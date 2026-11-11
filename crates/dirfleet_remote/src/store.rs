//! FTP, `WebDAV` and S3 (and a folder standing in for a server) through
//! `opendal`.
use std::io::Write as _;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use opendal::{ErrorKind, Operator, services};

use crate::{ConnectError, Endpoint, Entry, Kind, in_time, parts};

/// Files come over in pieces of this size.
const CHUNK: u64 = 4 * 1024 * 1024;

pub struct Store {
    operator: Operator,
}

/// `host`, with `port` when one is given.
fn authority(endpoint: &Endpoint) -> String {
    if endpoint.port == 0 {
        endpoint.host.clone()
    } else {
        format!("{}:{}", endpoint.host, endpoint.port)
    }
}

fn operator(endpoint: &Endpoint) -> opendal::Result<Operator> {
    let root = if endpoint.root.is_empty() {
        "/"
    } else {
        endpoint.root.as_str()
    };
    let web = if endpoint.secure { "https" } else { "http" };
    let builder = match endpoint.kind {
        Kind::Ftp => {
            let scheme = if endpoint.secure { "ftps" } else { "ftp" };
            let mut ftp = services::Ftp::default()
                .endpoint(&format!("{scheme}://{}", authority(endpoint)))
                .root(root);
            if !endpoint.user.is_empty() {
                ftp = ftp.user(&endpoint.user).password(&endpoint.secret);
            }
            Operator::new(ftp)?
        }
        Kind::WebDav => {
            let mut dav = services::Webdav::default()
                .endpoint(&format!("{web}://{}", authority(endpoint)))
                .root(root);
            if !endpoint.user.is_empty() {
                dav = dav.username(&endpoint.user).password(&endpoint.secret);
            }
            Operator::new(dav)?
        }
        Kind::S3 => {
            let mut s3 = services::S3::default()
                .bucket(&endpoint.bucket)
                .root(root)
                .access_key_id(&endpoint.user)
                .secret_access_key(&endpoint.secret)
                // Only what the user gave: no keys picked up from the machine.
                .disable_config_load();
            if !endpoint.region.is_empty() {
                s3 = s3.region(&endpoint.region);
            }
            if !endpoint.host.is_empty() {
                s3 = s3.endpoint(&format!("{web}://{}", authority(endpoint)));
            }
            Operator::new(s3)?
        }
        Kind::Folder | Kind::Sftp => Operator::new(services::Fs::default().root(&endpoint.host))?,
    };
    Ok(builder)
}

/// `dir` as `opendal` wants a folder: below the root, ending with `/`.
fn folder_path(dir: &str) -> String {
    let mut path: String = parts(dir).flat_map(|part| [part, "/"]).collect();
    if path.is_empty() {
        path.push('/');
    }
    path
}

fn file_path(path: &str) -> String {
    parts(path).collect::<Vec<_>>().join("/")
}

impl Store {
    pub async fn connect(endpoint: &Endpoint) -> Result<Self, ConnectError> {
        // A folder standing in for a server is made by `opendal` when it is
        // not there; a server that is not there should say so.
        if endpoint.kind == Kind::Folder && !Path::new(&endpoint.host).is_dir() {
            return Err(ConnectError::Failed(format!(
                "{} is not a folder",
                endpoint.host
            )));
        }
        let operator =
            operator(endpoint).map_err(|error| ConnectError::Failed(error.to_string()))?;
        // The first look says whether the address and the password are right.
        let looked = tokio::time::timeout(crate::ANSWER_WITHIN, operator.list("/")).await;
        match looked {
            Ok(Ok(_)) => Ok(Self { operator }),
            Ok(Err(error)) if error.kind() == ErrorKind::PermissionDenied => {
                Err(ConnectError::NeedsSecret)
            }
            Ok(Err(error)) => Err(ConnectError::Failed(error.to_string())),
            Err(_) => Err(ConnectError::Failed(
                "The server did not answer in time".to_owned(),
            )),
        }
    }

    pub async fn list(&self, dir: &str) -> Result<Vec<Entry>, String> {
        let path = folder_path(dir);
        let listed = in_time(self.operator.list(&path)).await?;
        // Stores list a folder that is not there as an empty one.
        if listed.is_empty() && path != "/" {
            in_time(self.operator.stat(&path)).await?;
        }
        Ok(listed
            .into_iter()
            // Some services list the folder itself among its items.
            .filter(|entry| entry.path() != path && !entry.name().trim_matches('/').is_empty())
            .map(|entry| {
                let meta = entry.metadata();
                Entry {
                    name: entry.name().trim_end_matches('/').to_owned(),
                    is_dir: meta.is_dir(),
                    size: meta.content_length(),
                    modified: meta
                        .last_modified()
                        .map(|time| time.into_inner().as_second()),
                }
            })
            .collect())
    }

    pub async fn download(&self, path: &str, to: &Path, cancel: &AtomicBool) -> Result<(), String> {
        let path = file_path(path);
        let size = in_time(self.operator.stat(&path)).await?.content_length();
        let reader = in_time(self.operator.reader(&path)).await?;
        let mut file = std::fs::File::create_new(to).map_err(|error| error.to_string())?;
        let mut done = 0;
        while done < size {
            if cancel.load(Ordering::Relaxed) {
                return Err("Stopped".to_owned());
            }
            let end = (done + CHUNK).min(size);
            let piece = in_time(reader.read(done..end)).await?;
            if piece.is_empty() {
                return Err("The server sent less than the file holds".to_owned());
            }
            done += piece.len() as u64;
            file.write_all(&piece.to_vec())
                .map_err(|error| error.to_string())?;
        }
        if cancel.load(Ordering::Relaxed) {
            return Err("Stopped".to_owned());
        }
        file.flush().map_err(|error| error.to_string())
    }
}
