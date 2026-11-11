//! SFTP through `russh`: a login by password or by a key file, the
//! server's key checked against the one the user accepted, and the files.
use std::io::Write as _;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use russh::client;
use russh::keys::{HashAlg, PrivateKeyWithHashAlg, PublicKeyOrCertificate};
use russh_sftp::client::SftpSession;
pub use russh_sftp::client::fs::File;
use tokio::io::AsyncReadExt as _;

use crate::{ConnectError, Endpoint, Entry, in_time, parts};

/// Files come over in pieces of this size.
const CHUNK: usize = 256 * 1024;

pub struct Client {
    /// Kept so the connection stays open while the session is used.
    handle: client::Handle<Keys>,
    sftp: SftpSession,
    /// The folder paths are below: the endpoint's root, or where the
    /// server puts the user.
    base: String,
}

/// Checks the server's key and remembers what it showed.
struct Keys {
    accepted: Option<String>,
    shown: Arc<Mutex<Option<String>>>,
}

fn fingerprint(key: &PublicKeyOrCertificate) -> String {
    match key {
        PublicKeyOrCertificate::PublicKey { key, .. } => {
            key.fingerprint(HashAlg::Sha256).to_string()
        }
        PublicKeyOrCertificate::Certificate(certificate) => certificate
            .public_key()
            .fingerprint(HashAlg::Sha256)
            .to_string(),
    }
}

impl client::Handler for Keys {
    type Error = russh::Error;

    #[allow(
        clippy::unused_async_trait_impl,
        reason = "the trait asks for an async function"
    )]
    async fn check_server_key(
        &mut self,
        server_public_key: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        let shown = fingerprint(server_public_key);
        let known = self.accepted.as_deref() == Some(shown.as_str());
        *self.shown.lock().unwrap_or_else(PoisonError::into_inner) = Some(shown);
        Ok(known)
    }
}

impl Client {
    pub async fn connect(endpoint: &Endpoint) -> Result<Self, ConnectError> {
        let failed = |reason: String| ConnectError::Failed(reason);
        let shown = Arc::new(Mutex::new(None));
        let keys = Keys {
            accepted: endpoint.host_key.clone(),
            shown: Arc::clone(&shown),
        };
        let port = if endpoint.port == 0 {
            22
        } else {
            endpoint.port
        };
        let config = Arc::new(client::Config::default());
        let address = (endpoint.host.as_str(), port);
        let connected = in_time(client::connect(config, address, keys)).await;
        let mut handle = match connected {
            Ok(handle) => handle,
            Err(reason) => {
                // Turned away by our own check: the key is what to ask about.
                let shown = shown.lock().unwrap_or_else(PoisonError::into_inner).take();
                return Err(match shown {
                    Some(fingerprint) if endpoint.host_key.as_deref() != Some(&fingerprint) => {
                        ConnectError::UnknownHostKey { fingerprint }
                    }
                    _ => failed(reason),
                });
            }
        };
        if endpoint.key_file.is_empty() {
            let login =
                handle.authenticate_password(endpoint.user.clone(), endpoint.secret.clone());
            if !in_time(login).await.map_err(failed)?.success() {
                return Err(ConnectError::NeedsSecret);
            }
        } else {
            // The secret, when there is one, unlocks the key.
            let phrase = (!endpoint.secret.is_empty()).then_some(endpoint.secret.as_str());
            let key = match russh::keys::load_secret_key(&endpoint.key_file, phrase) {
                Ok(key) => key,
                Err(russh::keys::Error::KeyIsEncrypted) => return Err(ConnectError::NeedsSecret),
                // With a passphrase given, the likeliest reason is a wrong one.
                Err(_) if phrase.is_some() => return Err(ConnectError::NeedsSecret),
                Err(error) => return Err(failed(format!("{}: {error}", endpoint.key_file))),
            };
            let hash = in_time(handle.best_supported_rsa_hash())
                .await
                .map_err(failed)?
                .flatten();
            let key = PrivateKeyWithHashAlg::new(Arc::new(key), hash);
            let login = handle.authenticate_publickey(endpoint.user.clone(), key);
            if !in_time(login).await.map_err(failed)?.success() {
                return Err(failed("The server did not accept the key".to_owned()));
            }
        }
        let channel = in_time(handle.channel_open_session())
            .await
            .map_err(failed)?;
        in_time(channel.request_subsystem(true, "sftp"))
            .await
            .map_err(failed)?;
        let sftp = in_time(SftpSession::new(channel.into_stream()))
            .await
            .map_err(failed)?;
        let base = if endpoint.root.is_empty() {
            in_time(sftp.canonicalize(".")).await.map_err(failed)?
        } else {
            endpoint.root.clone()
        };
        Ok(Self { handle, sftp, base })
    }

    /// `path` below the folder the connection starts in.
    fn below_base(&self, path: &str) -> String {
        let mut whole = self.base.trim_end_matches('/').to_owned();
        for part in parts(path) {
            whole.push('/');
            whole.push_str(part);
        }
        if whole.is_empty() {
            whole.push('/');
        }
        whole
    }

    pub async fn list(&self, dir: &str) -> Result<Vec<Entry>, String> {
        let listed = in_time(self.sftp.read_dir(self.below_base(dir))).await?;
        Ok(listed
            .filter(|entry| !matches!(entry.file_name().as_str(), "." | ".."))
            .map(|entry| {
                let meta = entry.metadata();
                Entry {
                    name: entry.file_name(),
                    is_dir: meta.is_dir(),
                    size: meta.size.unwrap_or(0),
                    modified: meta.mtime.map(i64::from),
                }
            })
            .collect())
    }

    pub async fn download(&self, path: &str, to: &Path, cancel: &AtomicBool) -> Result<(), String> {
        let mut remote = in_time(self.sftp.open(self.below_base(path))).await?;
        let mut file = std::fs::File::create_new(to).map_err(|error| error.to_string())?;
        let mut piece = vec![0; CHUNK];
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err("Stopped".to_owned());
            }
            let read = in_time(remote.read(&mut piece)).await?;
            if read == 0 {
                break;
            }
            file.write_all(&piece[..read])
                .map_err(|error| error.to_string())?;
        }
        file.flush().map_err(|error| error.to_string())
    }

    pub fn is_closed(&self) -> bool {
        self.handle.is_closed()
    }

    /// The file or folder at `path`; `None` when nothing is there.
    pub async fn stat(&self, path: &str) -> Result<Option<Entry>, String> {
        let whole = self.below_base(path);
        if !in_time(self.sftp.try_exists(whole.clone())).await? {
            return Ok(None);
        }
        let meta = in_time(self.sftp.metadata(whole)).await?;
        Ok(Some(Entry {
            name: parts(path).last().unwrap_or_default().to_owned(),
            is_dir: meta.is_dir(),
            size: meta.size.unwrap_or(0),
            modified: meta.mtime.map(i64::from),
        }))
    }

    pub async fn make_dir(&self, path: &str) -> Result<(), String> {
        in_time(self.sftp.create_dir(self.below_base(path))).await
    }

    pub async fn remove_file(&self, path: &str) -> Result<(), String> {
        in_time(self.sftp.remove_file(self.below_base(path))).await
    }

    /// Removes the folder at `path`, which holds nothing any more.
    pub async fn remove_dir(&self, path: &str) -> Result<(), String> {
        in_time(self.sftp.remove_dir(self.below_base(path))).await
    }

    pub async fn rename(&self, from: &str, to: &str) -> Result<(), String> {
        in_time(self.sftp.rename(self.below_base(from), self.below_base(to))).await
    }

    pub async fn read(&self, path: &str) -> Result<File, String> {
        in_time(self.sftp.open(self.below_base(path))).await
    }

    /// Opens the file at `path` for writing, emptied when it is there.
    pub async fn write(&self, path: &str) -> Result<File, String> {
        in_time(self.sftp.create(self.below_base(path))).await
    }
}
