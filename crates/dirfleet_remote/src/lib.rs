//! Servers as folders: SFTP, FTP, `WebDAV` and S3 behind one blocking interface.
//!
//! Everything underneath is asynchronous. One small runtime of this crate's
//! own runs it, and each call here waits for its answer, with a time limit,
//! so callers stay ordinary threads. FTP, `WebDAV` and S3 go through `opendal`;
//! SFTP goes through `russh`, since `opendal`'s own SFTP needs the system's
//! `ssh` and Unix.
use std::fmt;
use std::future::Future;
use std::io;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

mod sftp;
mod store;

/// How long a server may take to answer before the call gives up.
const ANSWER_WITHIN: Duration = Duration::from_secs(30);

/// The slowest a server may take data, in bytes a second, before the wait
/// for a piece of a file gives up.
const SLOWEST: u64 = 16 * 1024;

/// Pieces of a file go to an SFTP server in this size.
const SFTP_PIECE: usize = 256 * 1024;

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
    /// SFTP only: the private key file to log in with, in place of a
    /// password; the secret is then what unlocks the key. Empty for none.
    pub key_file: String,
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
    within(ANSWER_WITHIN, work).await
}

/// Waits for `work` no longer than `limit`.
async fn within<T, E: fmt::Display>(
    limit: Duration,
    work: impl Future<Output = Result<T, E>>,
) -> Result<T, String> {
    match tokio::time::timeout(limit, work).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(error)) => Err(error.to_string()),
        Err(_) => Err("The server did not answer in time".to_owned()),
    }
}

/// How long sending `bytes` may take.
fn time_for(bytes: u64) -> Duration {
    ANSWER_WITHIN.max(Duration::from_secs(bytes / SLOWEST))
}

/// A file on a server being read: what it holds comes through `Read`.
pub struct Reading {
    runtime: Arc<tokio::runtime::Runtime>,
    source: Source,
    /// The piece last brought over, and how much of it is handed out.
    held: Vec<u8>,
    at: usize,
}

enum Source {
    Store(store::Reading),
    Sftp(Box<sftp::File>),
}

impl io::Read for Reading {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.at == self.held.len() {
            let source = &mut self.source;
            let next = self.runtime.block_on(async {
                match source {
                    Source::Store(reading) => reading.next().await,
                    Source::Sftp(file) => {
                        use tokio::io::AsyncReadExt as _;
                        let mut piece = vec![0; SFTP_PIECE];
                        let read = in_time(file.read(&mut piece)).await?;
                        piece.truncate(read);
                        Ok(piece)
                    }
                }
            });
            self.held = next.map_err(io::Error::other)?;
            self.at = 0;
        }
        let count = buffer.len().min(self.held.len() - self.at);
        buffer[..count].copy_from_slice(&self.held[self.at..self.at + count]);
        self.at += count;
        Ok(count)
    }
}

/// A file on a server being written: what goes into `Write` is sent in
/// pieces, and the file is whole only once [`Writing::finish`] says so.
pub struct Writing {
    runtime: Arc<tokio::runtime::Runtime>,
    sink: Sink,
    /// What is not sent yet.
    held: Vec<u8>,
    /// How much is gathered before it is sent.
    piece: usize,
    sent: u64,
}

enum Sink {
    Store(opendal::Writer),
    Sftp(Box<sftp::File>),
}

impl Writing {
    fn send(&mut self) -> Result<(), String> {
        if self.held.is_empty() {
            return Ok(());
        }
        let piece = std::mem::take(&mut self.held);
        let limit = time_for(piece.len() as u64);
        self.sent += piece.len() as u64;
        let sink = &mut self.sink;
        self.runtime.block_on(async {
            match sink {
                Sink::Store(writer) => within(limit, writer.write(piece)).await,
                Sink::Sftp(file) => {
                    use tokio::io::AsyncWriteExt as _;
                    within(limit, file.write_all(&piece)).await
                }
            }
        })
    }

    /// Sends what is left and closes the file.
    ///
    /// # Errors
    /// Why the file is not whole on the server, in words for the user.
    pub fn finish(mut self) -> Result<(), String> {
        let empty = self.sent == 0 && self.held.is_empty();
        self.send()?;
        // A server that takes the file in one piece takes it now.
        let limit = time_for(self.sent);
        let sink = &mut self.sink;
        self.runtime.block_on(async {
            match sink {
                Sink::Store(writer) => {
                    if empty {
                        // Some stores make no file that nothing was written to.
                        within(limit, writer.write(Vec::<u8>::new())).await?;
                    }
                    within(limit, writer.close()).await.map(|_| ())
                }
                Sink::Sftp(file) => {
                    use tokio::io::AsyncWriteExt as _;
                    within(limit, file.shutdown()).await
                }
            }
        })
    }

    /// Gives the file up. What reached the server may still be there: the
    /// caller removes it.
    pub fn abandon(mut self) {
        let sink = &mut self.sink;
        self.runtime.block_on(async {
            match sink {
                Sink::Store(writer) => {
                    let _ = within(ANSWER_WITHIN, writer.abort()).await;
                }
                Sink::Sftp(file) => {
                    use tokio::io::AsyncWriteExt as _;
                    let _ = within(ANSWER_WITHIN, file.shutdown()).await;
                }
            }
        });
    }
}

impl io::Write for Writing {
    fn write(&mut self, piece: &[u8]) -> io::Result<usize> {
        self.held.extend_from_slice(piece);
        if self.held.len() >= self.piece {
            self.send().map_err(io::Error::other)?;
        }
        Ok(piece.len())
    }

    /// Does nothing: pieces go when they are full, since some servers take
    /// no small ones. [`Writing::finish`] sends the rest.
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
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

    /// The file or folder at `path`; `None` when nothing is there.
    ///
    /// # Errors
    /// Why the server did not say, in words for the user.
    pub fn stat(&self, path: &str) -> Result<Option<Entry>, String> {
        self.runtime.block_on(async {
            match &self.backend {
                Backend::Store(store) => store.stat(path).await,
                Backend::Sftp(client) => client.stat(path).await,
            }
        })
    }

    /// Makes the folder `path`, in a folder that is there.
    ///
    /// # Errors
    /// Why the folder was not made, in words for the user.
    pub fn make_dir(&self, path: &str) -> Result<(), String> {
        self.runtime.block_on(async {
            match &self.backend {
                Backend::Store(store) => store.make_dir(path).await,
                Backend::Sftp(client) => client.make_dir(path).await,
            }
        })
    }

    /// Removes the file at `path`.
    ///
    /// # Errors
    /// Why the file is still there, in words for the user.
    pub fn remove_file(&self, path: &str) -> Result<(), String> {
        self.runtime.block_on(async {
            match &self.backend {
                Backend::Store(store) => store.remove_file(path).await,
                Backend::Sftp(client) => client.remove_file(path).await,
            }
        })
    }

    /// Removes the folder at `path`, which must hold nothing.
    ///
    /// # Errors
    /// Why the folder is still there, in words for the user.
    pub fn remove_dir(&self, path: &str) -> Result<(), String> {
        self.runtime.block_on(async {
            match &self.backend {
                Backend::Store(store) => store.remove_dir(path).await,
                Backend::Sftp(client) => client.remove_dir(path).await,
            }
        })
    }

    /// Gives the item at `from` the path `to`, where nothing is. `Ok(false)`
    /// when this kind of server can't rename: the item is then to be copied
    /// and its original removed.
    ///
    /// # Errors
    /// Why the item was not renamed, in words for the user.
    pub fn rename(&self, from: &str, to: &str, is_dir: bool) -> Result<bool, String> {
        self.runtime.block_on(async {
            match &self.backend {
                Backend::Store(store) => store.rename(from, to, is_dir).await,
                Backend::Sftp(client) => client.rename(from, to).await.map(|()| true),
            }
        })
    }

    /// Has the server copy the file `from` to `to` itself, without the
    /// data coming here. `Ok(false)` when this kind of server can't.
    ///
    /// # Errors
    /// Why the file was not copied, in words for the user.
    pub fn copy_file(&self, from: &str, to: &str) -> Result<bool, String> {
        self.runtime.block_on(async {
            match &self.backend {
                Backend::Store(store) => store.copy_file(from, to).await,
                Backend::Sftp(_) => Ok(false),
            }
        })
    }

    /// Opens the file at `path` for reading.
    ///
    /// # Errors
    /// Why the file can't be read, in words for the user.
    pub fn read(&self, path: &str) -> Result<Reading, String> {
        let source = self.runtime.block_on(async {
            match &self.backend {
                Backend::Store(store) => store.read(path).await.map(Source::Store),
                Backend::Sftp(client) => client
                    .read(path)
                    .await
                    .map(|file| Source::Sftp(Box::new(file))),
            }
        })?;
        Ok(Reading {
            runtime: Arc::clone(&self.runtime),
            source,
            held: Vec::new(),
            at: 0,
        })
    }

    /// Opens the file at `path` for writing `size` bytes, in place of what
    /// is there.
    ///
    /// # Errors
    /// Why the file can't be written, in words for the user.
    pub fn write(&self, path: &str, size: u64) -> Result<Writing, String> {
        let (sink, piece) = self.runtime.block_on(async {
            match &self.backend {
                Backend::Store(store) => store
                    .write(path, size)
                    .await
                    .map(|(writer, piece)| (Sink::Store(writer), piece)),
                Backend::Sftp(client) => client
                    .write(path)
                    .await
                    .map(|file| (Sink::Sftp(Box::new(file)), SFTP_PIECE as u64)),
            }
        })?;
        Ok(Writing {
            runtime: Arc::clone(&self.runtime),
            sink,
            held: Vec::new(),
            piece: usize::try_from(piece).unwrap_or(usize::MAX),
            sent: 0,
        })
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
            key_file: String::new(),
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
    fn a_store_takes_files_and_folders_and_renames_and_removes_them() {
        use std::io::{Read as _, Write as _};
        let dir = tempfile::tempdir().expect("temp dir");
        let remote = Remote::new().expect("runtime");
        let session = remote.connect(&folder(dir.path())).expect("connects");

        session.make_dir("docs").expect("makes a folder");
        assert!(dir.path().join("docs").is_dir());
        let data: Vec<u8> = (0..3_000_001_u32).map(|n| (n % 251) as u8).collect();
        let mut writing = session
            .write("docs/data.bin", data.len() as u64)
            .expect("opens");
        // In pieces, as a copy hands them over.
        for piece in data.chunks(1024 * 1024) {
            writing.write_all(piece).expect("writes");
        }
        writing.finish().expect("finishes");
        assert_eq!(
            std::fs::read(dir.path().join("docs/data.bin")).expect("read"),
            data
        );
        session
            .write("docs/empty.txt", 0)
            .expect("opens")
            .finish()
            .expect("finishes");
        assert_eq!(
            std::fs::read(dir.path().join("docs/empty.txt")).expect("read"),
            b""
        );

        let found = session
            .stat("docs/data.bin")
            .expect("answers")
            .expect("there");
        assert_eq!(
            (found.name.as_str(), found.is_dir, found.size),
            ("data.bin", false, data.len() as u64)
        );
        assert!(
            session
                .stat("docs")
                .expect("answers")
                .expect("there")
                .is_dir
        );
        assert!(session.stat("/").expect("answers").expect("there").is_dir);
        assert_eq!(session.stat("docs/nothing").expect("answers"), None);

        let mut back = Vec::new();
        session
            .read("docs/data.bin")
            .expect("opens")
            .read_to_end(&mut back)
            .expect("reads");
        assert_eq!(back, data);

        assert_eq!(
            session.rename("docs/data.bin", "docs/renamed.bin", false),
            Ok(true)
        );
        assert_eq!(session.rename("docs", "papers", true), Ok(true));
        assert!(dir.path().join("papers/renamed.bin").is_file());
        assert_eq!(
            session.copy_file("papers/renamed.bin", "papers/copy.bin"),
            Ok(true)
        );
        assert_eq!(
            std::fs::read(dir.path().join("papers/copy.bin")).expect("read"),
            data
        );

        for name in ["renamed.bin", "copy.bin", "empty.txt"] {
            session
                .remove_file(&format!("papers/{name}"))
                .expect("removes a file");
        }
        session.remove_dir("papers").expect("removes a folder");
        assert!(!dir.path().join("papers").exists());
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
