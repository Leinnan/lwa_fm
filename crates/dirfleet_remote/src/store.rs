//! FTP, `WebDAV` and S3 (and a folder standing in for a server) through
//! `opendal`.
use std::io::Write as _;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use opendal::{ErrorKind, Operator, services};

use crate::{ANSWER_WITHIN, ConnectError, Endpoint, Entry, Kind, in_time, parts, within};

/// Files come over in pieces of this size.
const CHUNK: u64 = 4 * 1024 * 1024;

/// Files go over in pieces of at least this size: S3 takes no smaller.
const PIECE: u64 = 8 * 1024 * 1024;

/// S3 takes a file in at most 10 000 pieces; larger files go in larger ones.
const MOST_PIECES: u64 = 9000;

/// More than this is not sent to a server that takes a file in one piece
/// (`WebDAV`): all of it is held in memory on the way.
const WHOLE_FILE_LIMIT: u64 = 1024 * 1024 * 1024;

/// A server may take this long to copy a file from one of its places to
/// another.
const COPY_WITHIN: std::time::Duration = std::time::Duration::from_secs(600);

/// A file on a store, read a piece at a time.
pub struct Reading {
    reader: opendal::Reader,
    at: u64,
    size: u64,
}

impl Reading {
    /// The next piece; empty at the file's end.
    pub async fn next(&mut self) -> Result<Vec<u8>, String> {
        if self.at >= self.size {
            return Ok(Vec::new());
        }
        let end = (self.at + CHUNK).min(self.size);
        let piece = in_time(self.reader.read(self.at..end)).await?;
        if piece.is_empty() {
            return Err("The server sent less than the file holds".to_owned());
        }
        self.at += piece.len() as u64;
        Ok(piece.to_vec())
    }
}

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

    /// What the server says about `path` as it is written; `None` when
    /// nothing is there.
    async fn looked(&self, path: &str) -> Result<Option<opendal::Metadata>, String> {
        match tokio::time::timeout(ANSWER_WITHIN, self.operator.stat(path)).await {
            Ok(Ok(meta)) => Ok(Some(meta)),
            Ok(Err(error)) if error.kind() == ErrorKind::NotFound => Ok(None),
            Ok(Err(error)) => Err(error.to_string()),
            Err(_) => Err("The server did not answer in time".to_owned()),
        }
    }

    /// The file or folder at `path`; `None` when nothing is there.
    pub async fn stat(&self, path: &str) -> Result<Option<Entry>, String> {
        let Some(name) = parts(path).last() else {
            return Ok(Some(Entry {
                name: String::new(),
                is_dir: true,
                size: 0,
                modified: None,
            }));
        };
        // Stores tell a folder from a file by how the path ends.
        let found = match self.looked(&file_path(path)).await? {
            Some(meta) => Some(meta),
            None => self.looked(&folder_path(path)).await?,
        };
        Ok(found.map(|meta| Entry {
            name: name.to_owned(),
            is_dir: meta.is_dir(),
            size: meta.content_length(),
            modified: meta
                .last_modified()
                .map(|time| time.into_inner().as_second()),
        }))
    }

    pub async fn make_dir(&self, path: &str) -> Result<(), String> {
        in_time(self.operator.create_dir(&folder_path(path))).await
    }

    pub async fn remove_file(&self, path: &str) -> Result<(), String> {
        in_time(self.operator.delete(&file_path(path))).await
    }

    /// Removes the folder at `path`, which holds nothing any more.
    pub async fn remove_dir(&self, path: &str) -> Result<(), String> {
        in_time(self.operator.delete(&folder_path(path))).await
    }

    /// Gives `from` the path `to`. `Ok(false)` when this kind of server
    /// can't: the item is then copied and its original removed.
    pub async fn rename(&self, from: &str, to: &str, is_dir: bool) -> Result<bool, String> {
        if !self.operator.info().capability().rename {
            return Ok(false);
        }
        let (from, to) = (file_path(from), file_path(to));
        let renamed = self.operator.rename(&from, &to);
        match tokio::time::timeout(ANSWER_WITHIN, renamed).await {
            Ok(Ok(())) => Ok(true),
            // Some servers move a folder only item by item.
            Ok(Err(error))
                if is_dir
                    && matches!(
                        error.kind(),
                        ErrorKind::Unsupported | ErrorKind::IsADirectory | ErrorKind::NotFound
                    ) =>
            {
                Ok(false)
            }
            Ok(Err(error)) => Err(error.to_string()),
            Err(_) => Err("The server did not answer in time".to_owned()),
        }
    }

    /// Has the server copy the file `from` to `to` itself. `Ok(false)` when
    /// this kind of server can't.
    pub async fn copy_file(&self, from: &str, to: &str) -> Result<bool, String> {
        if !self.operator.info().capability().copy {
            return Ok(false);
        }
        let (from, to) = (file_path(from), file_path(to));
        let copied = self.operator.copy(&from, &to);
        within(COPY_WITHIN, copied).await.map(|_| true)
    }

    pub async fn read(&self, path: &str) -> Result<Reading, String> {
        let path = file_path(path);
        let size = in_time(self.operator.stat(&path)).await?.content_length();
        let reader = in_time(self.operator.reader(&path)).await?;
        Ok(Reading {
            reader,
            at: 0,
            size,
        })
    }

    /// Opens the file at `path` for writing `size` bytes, and says how
    /// large the pieces handed over are to be.
    pub async fn write(&self, path: &str, size: u64) -> Result<(opendal::Writer, u64), String> {
        let whole = !self.operator.info().capability().write_can_multi;
        if whole && size > WHOLE_FILE_LIMIT {
            return Err(
                "This kind of server takes a file in one piece, and this one is over 1 GB"
                    .to_owned(),
            );
        }
        let writer = in_time(self.operator.writer(&file_path(path))).await?;
        let piece = if whole {
            u64::MAX
        } else {
            PIECE.max(size / MOST_PIECES)
        };
        Ok((writer, piece))
    }
}
