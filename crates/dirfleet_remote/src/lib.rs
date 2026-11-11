//! Servers as folders: SFTP, FTP, `WebDAV` and S3 behind one blocking interface.
//!
//! Everything underneath is asynchronous. One small runtime of this crate's
//! own runs it, and each call here waits for its answer, with a time limit,
//! so callers stay ordinary threads. FTP, `WebDAV` and S3 go through `opendal`;
//! SFTP goes through `russh`, since `opendal`'s own SFTP needs the system's
//! `ssh` and Unix.
use std::fmt;
use std::future::Future;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

mod sftp;
mod store;

/// How long a server may take to answer before the call gives up.
const ANSWER_WITHIN: Duration = Duration::from_secs(30);

/// How a server is talked to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Sftp,
    Ftp,
    WebDav,
    S3,
    /// A folder on this machine treated as a server: for trying things out
    /// without one. `host` is the folder.
    Folder,
}

/// Where to connect, and as whom.
#[derive(Clone, PartialEq, Eq)]
pub struct Endpoint {
    pub kind: Kind,
    /// The server's name or address. For S3, the endpoint when it is not
    /// Amazon's own.
    pub host: String,
    /// 0 for the protocol's usual port.
    pub port: u16,
    /// The user; for S3 the access key's id. Empty for no one in particular.
    pub user: String,
    /// The password; for S3 the secret access key.
    pub secret: String,
    /// The folder to start in; empty for the server's own choice.
    pub root: String,
    /// TLS: FTPS, and `https` for `WebDAV` and S3 endpoints.
    pub secure: bool,
    /// S3 only.
    pub bucket: String,
    pub region: String,
    /// SFTP only: the fingerprint of the host key the user has accepted.
    pub host_key: Option<String>,
}

impl fmt::Debug for Endpoint {
    /// Without the secret.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Endpoint")
            .field("kind", &self.kind)
            .field("host", &self.host)
            .field("port", &self.port)
            .field("user", &self.user)
            .field("root", &self.root)
            .finish_non_exhaustive()
    }
}

/// Why a connection was not made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectError {
    /// The server wants a password (or another one).
    NeedsSecret,
    /// The server showed a key nobody has accepted for it yet, or another
    /// than the accepted one.
    UnknownHostKey { fingerprint: String },
    /// Anything else, in words for the user.
    Failed(String),
}

impl fmt::Display for ConnectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NeedsSecret => f.write_str("The server did not accept the password"),
            Self::UnknownHostKey { fingerprint } => {
                write!(f, "The server's key is not known: {fingerprint}")
            }
            Self::Failed(reason) => f.write_str(reason),
        }
    }
}

/// One item of a folder on a server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    /// Unix seconds, when the server says.
    pub modified: Option<i64>,
}

/// Makes connections; holds the runtime they all run on.
pub struct Remote {
    runtime: Arc<tokio::runtime::Runtime>,
}

/// An open connection to one server.
pub struct Session {
    runtime: Arc<tokio::runtime::Runtime>,
    backend: Backend,
}

enum Backend {
    Store(store::Store),
    Sftp(sftp::Client),
}

/// Waits for `work`, but not for ever.
async fn in_time<T, E: fmt::Display>(
    work: impl Future<Output = Result<T, E>>,
) -> Result<T, String> {
    match tokio::time::timeout(ANSWER_WITHIN, work).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(error)) => Err(error.to_string()),
        Err(_) => Err("The server did not answer in time".to_owned()),
    }
}

impl Remote {
    /// Starts the runtime connections run on.
    ///
    /// # Errors
    /// When the system gives no threads for it.
    pub fn new() -> std::io::Result<Self> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("dirfleet_remote")
            .enable_all()
            .build()?;
        Ok(Self {
            runtime: Arc::new(runtime),
        })
    }

    /// Connects to `endpoint` and looks at the folder it starts in, so a
    /// wrong password or address is found out now.
    ///
    /// # Errors
    /// Why the connection was not made.
    pub fn connect(&self, endpoint: &Endpoint) -> Result<Session, ConnectError> {
        let backend = self.runtime.block_on(async {
            match endpoint.kind {
                Kind::Sftp => sftp::Client::connect(endpoint).await.map(Backend::Sftp),
                Kind::Ftp | Kind::WebDav | Kind::S3 | Kind::Folder => {
                    store::Store::connect(endpoint).await.map(Backend::Store)
                }
            }
        })?;
        Ok(Session {
            runtime: Arc::clone(&self.runtime),
            backend,
        })
    }
}

impl Session {
    /// The items of the folder `dir`, a `/`-separated path below where the
    /// connection starts (`""` or `"/"` is that folder itself).
    ///
    /// # Errors
    /// Why the folder can't be listed, in words for the user.
    pub fn list(&self, dir: &str) -> Result<Vec<Entry>, String> {
        self.runtime.block_on(async {
            match &self.backend {
                Backend::Store(store) => store.list(dir).await,
                Backend::Sftp(client) => client.list(dir).await,
            }
        })
    }

    /// Copies the file at `path` to the new file `to` on this machine,
    /// giving up when `cancel` is set. A copy that fails is removed.
    ///
    /// # Errors
    /// Why the file was not copied, in words for the user.
    pub fn download(&self, path: &str, to: &Path, cancel: &AtomicBool) -> Result<(), String> {
        let copied = self.runtime.block_on(async {
            match &self.backend {
                Backend::Store(store) => store.download(path, to, cancel).await,
                Backend::Sftp(client) => client.download(path, to, cancel).await,
            }
        });
        if copied.is_err() {
            let _ = std::fs::remove_file(to);
        }
        copied
    }

    /// Whether the connection is known to be over.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        match &self.backend {
            Backend::Store(_) => false,
            Backend::Sftp(client) => client.is_closed(),
        }
    }
}

/// `path` as the parts between its `/`s, without empty ones.
fn parts(path: &str) -> impl Iterator<Item = &str> {
    path.split('/').filter(|part| !part.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    pub fn folder(dir: &Path) -> Endpoint {
        Endpoint {
            kind: Kind::Folder,
            host: dir.to_string_lossy().into_owned(),
            port: 0,
            user: String::new(),
            secret: String::new(),
            root: String::new(),
            secure: false,
            bucket: String::new(),
            region: String::new(),
            host_key: None,
        }
    }

    fn names(entries: &[Entry]) -> Vec<&str> {
        let mut names: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
        names.sort_unstable();
        names
    }

    #[test]
    fn a_store_lists_its_folders_and_hands_over_its_files() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::create_dir_all(dir.path().join("docs/deep")).expect("folders");
        std::fs::write(dir.path().join("readme.txt"), b"hello").expect("write");
        std::fs::write(dir.path().join("docs/deep/notes.md"), vec![b'n'; 300_000]).expect("write");
        let remote = Remote::new().expect("runtime");
        let session = remote.connect(&folder(dir.path())).expect("connects");

        let top = session.list("/").expect("lists");
        assert_eq!(names(&top), ["docs", "readme.txt"]);
        let readme = top
            .iter()
            .find(|entry| entry.name == "readme.txt")
            .expect("listed");
        assert_eq!((readme.is_dir, readme.size), (false, 5));
        assert!(readme.modified.is_some());
        assert!(top.iter().any(|entry| entry.name == "docs" && entry.is_dir));
        assert_eq!(
            names(&session.list("docs/deep").expect("lists")),
            ["notes.md"]
        );
        assert!(session.list("nowhere").is_err());

        let out = dir.path().join("copy.md");
        let cancel = AtomicBool::new(false);
        session
            .download("/docs/deep/notes.md", &out, &cancel)
            .expect("downloads");
        assert_eq!(std::fs::read(&out).expect("read"), vec![b'n'; 300_000]);

        // Stopped: nothing is left where the copy was going.
        let stopped = dir.path().join("stopped.md");
        cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        assert!(
            session
                .download("docs/deep/notes.md", &stopped, &cancel)
                .is_err()
        );
        assert!(!stopped.exists());
    }

    #[test]
    fn a_folder_that_is_not_there_is_no_connection() {
        let dir = tempfile::tempdir().expect("temp dir");
        let remote = Remote::new().expect("runtime");
        let gone = folder(&dir.path().join("gone"));
        assert!(matches!(
            remote.connect(&gone),
            Err(ConnectError::Failed(_))
        ));
        assert!(
            !format!(
                "{:?}",
                Endpoint {
                    secret: "hunter2".into(),
                    ..gone
                }
            )
            .contains("hunter2")
        );
    }
}
