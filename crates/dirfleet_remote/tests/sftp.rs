//! The SFTP client against a small SFTP server run in this process, which
//! serves a folder of this machine: the host key question, a wrong
//! password, a key file, listing, reading and changing what is there.
#![allow(
    clippy::unused_async_trait_impl,
    reason = "the server traits ask for async functions"
)]

use std::collections::HashMap;
use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, UNIX_EPOCH};

use dirfleet_remote::{ConnectError, Endpoint, Kind, Remote};
use russh::keys::ssh_key::{self, LineEnding};
use russh::keys::{Algorithm, PrivateKey};
use russh::server::{Auth, ChannelOpenHandle, Msg, Server as _, Session, VerifiedPublicKey};
use russh::{Channel, ChannelId};
use russh_sftp::protocol::{
    Attrs, Data, File, FileAttributes, Handle, Name, OpenFlags, Status, StatusCode, Version,
};
use tokio::sync::Mutex;

const README: &[u8] = b"hello from the server";

/// What is served, and whose key logs in without a password.
#[derive(Clone)]
struct Server {
    root: PathBuf,
    key: Option<ssh_key::PublicKey>,
}

impl russh::server::Server for Server {
    type Handler = Ssh;

    fn new_client(&mut self, _: Option<SocketAddr>) -> Ssh {
        Ssh {
            server: self.clone(),
            channels: Arc::default(),
        }
    }
}

struct Ssh {
    server: Server,
    channels: Arc<Mutex<HashMap<ChannelId, Channel<Msg>>>>,
}

impl russh::server::Handler for Ssh {
    type Error = russh::Error;

    async fn auth_password(&mut self, user: &str, password: &str) -> Result<Auth, Self::Error> {
        Ok(if (user, password) == ("ann", "secret") {
            Auth::Accept
        } else {
            Auth::reject()
        })
    }

    async fn auth_publickey(
        &mut self,
        user: &str,
        key: VerifiedPublicKey<'_>,
    ) -> Result<Auth, Self::Error> {
        let known = self
            .server
            .key
            .as_ref()
            .is_some_and(|known| known.key_data() == key.public_key.key_data());
        Ok(if user == "ann" && known {
            Auth::Accept
        } else {
            Auth::reject()
        })
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        reply: ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.channels.lock().await.insert(channel.id(), channel);
        reply.accept().await;
        Ok(())
    }

    async fn subsystem_request(
        &mut self,
        channel_id: ChannelId,
        name: &str,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        let channel = self.channels.lock().await.remove(&channel_id);
        match channel {
            Some(channel) if name == "sftp" => {
                session.channel_success(channel_id)?;
                let sftp = Sftp {
                    root: self.server.root.clone(),
                    open: HashMap::new(),
                    next: 0,
                };
                russh_sftp::server::run(channel.into_stream(), sftp).await;
            }
            _ => session.channel_failure(channel_id)?,
        }
        Ok(())
    }
}

/// Something a client has open.
enum Open {
    /// A folder, and whether it was read: a second read says there is no
    /// more.
    Dir(PathBuf, bool),
    File(std::fs::File),
}

struct Sftp {
    root: PathBuf,
    open: HashMap<String, Open>,
    next: u32,
}

fn ok(id: u32) -> Status {
    Status {
        id,
        status_code: StatusCode::Ok,
        error_message: "Ok".to_owned(),
        language_tag: "en-US".to_owned(),
    }
}

fn code(error: &std::io::Error) -> StatusCode {
    if error.kind() == std::io::ErrorKind::NotFound {
        StatusCode::NoSuchFile
    } else {
        StatusCode::Failure
    }
}

fn attrs_of(path: &Path) -> Result<FileAttributes, StatusCode> {
    let meta = std::fs::metadata(path).map_err(|error| code(&error))?;
    let mut attrs = FileAttributes::empty();
    attrs.mtime = meta
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .and_then(|since| u32::try_from(since.as_secs()).ok());
    if meta.is_dir() {
        attrs.permissions = Some(0o040_755);
    } else {
        attrs.permissions = Some(0o100_644);
        attrs.size = Some(meta.len());
    }
    Ok(attrs)
}

impl Sftp {
    /// Where the client's `path` is on this machine.
    fn real(&self, path: &str) -> Result<PathBuf, StatusCode> {
        let mut real = self.root.clone();
        for part in path
            .split('/')
            .filter(|part| !part.is_empty() && *part != ".")
        {
            if part == ".." {
                return Err(StatusCode::PermissionDenied);
            }
            real.push(part);
        }
        Ok(real)
    }

    fn handle(&mut self, id: u32, open: Open) -> Handle {
        self.next += 1;
        let handle = self.next.to_string();
        self.open.insert(handle.clone(), open);
        Handle { id, handle }
    }

    fn file(&mut self, handle: &str) -> Result<&mut std::fs::File, StatusCode> {
        match self.open.get_mut(handle) {
            Some(Open::File(file)) => Ok(file),
            _ => Err(StatusCode::Failure),
        }
    }
}

impl russh_sftp::server::Handler for Sftp {
    type Error = StatusCode;

    fn unimplemented(&self) -> Self::Error {
        StatusCode::OpUnsupported
    }

    async fn init(
        &mut self,
        _version: u32,
        _extensions: HashMap<String, String>,
    ) -> Result<Version, Self::Error> {
        Ok(Version::new())
    }

    async fn close(&mut self, id: u32, handle: String) -> Result<Status, Self::Error> {
        self.open.remove(&handle);
        Ok(ok(id))
    }

    async fn realpath(&mut self, id: u32, path: String) -> Result<Name, Self::Error> {
        let real = if path == "." { "/" } else { path.as_str() };
        Ok(Name {
            id,
            files: vec![File::dummy(real)],
        })
    }

    async fn opendir(&mut self, id: u32, path: String) -> Result<Handle, Self::Error> {
        let real = self.real(&path)?;
        if !real.is_dir() {
            return Err(StatusCode::NoSuchFile);
        }
        Ok(self.handle(id, Open::Dir(real, false)))
    }

    async fn readdir(&mut self, id: u32, handle: String) -> Result<Name, Self::Error> {
        let Some(Open::Dir(dir, read)) = self.open.get_mut(&handle) else {
            return Err(StatusCode::Failure);
        };
        if *read {
            return Err(StatusCode::Eof);
        }
        *read = true;
        let files = std::fs::read_dir(dir)
            .map_err(|error| code(&error))?
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let attrs = attrs_of(&entry.path()).ok()?;
                Some(File::new(entry.file_name().to_string_lossy(), attrs))
            })
            .collect();
        Ok(Name { id, files })
    }

    async fn open(
        &mut self,
        id: u32,
        filename: String,
        pflags: OpenFlags,
        _attrs: FileAttributes,
    ) -> Result<Handle, Self::Error> {
        let real = self.real(&filename)?;
        let file = std::fs::OpenOptions::from(pflags)
            .open(real)
            .map_err(|error| code(&error))?;
        Ok(self.handle(id, Open::File(file)))
    }

    async fn read(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        len: u32,
    ) -> Result<Data, Self::Error> {
        let file = self.file(&handle)?;
        file.seek(SeekFrom::Start(offset))
            .map_err(|error| code(&error))?;
        let mut data = vec![0; len as usize];
        let read = file.read(&mut data).map_err(|error| code(&error))?;
        if read == 0 {
            return Err(StatusCode::Eof);
        }
        data.truncate(read);
        Ok(Data { id, data })
    }

    async fn write(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        data: Vec<u8>,
    ) -> Result<Status, Self::Error> {
        let file = self.file(&handle)?;
        file.seek(SeekFrom::Start(offset))
            .map_err(|error| code(&error))?;
        file.write_all(&data).map_err(|error| code(&error))?;
        Ok(ok(id))
    }

    async fn stat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
        let attrs = attrs_of(&self.real(&path)?)?;
        Ok(Attrs { id, attrs })
    }

    async fn lstat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
        self.stat(id, path).await
    }

    async fn fstat(&mut self, id: u32, handle: String) -> Result<Attrs, Self::Error> {
        let meta = self
            .file(&handle)?
            .metadata()
            .map_err(|error| code(&error))?;
        let mut attrs = FileAttributes::empty();
        attrs.permissions = Some(0o100_644);
        attrs.size = Some(meta.len());
        Ok(Attrs { id, attrs })
    }

    async fn setstat(
        &mut self,
        id: u32,
        _path: String,
        _attrs: FileAttributes,
    ) -> Result<Status, Self::Error> {
        Ok(ok(id))
    }

    async fn fsetstat(
        &mut self,
        id: u32,
        _handle: String,
        _attrs: FileAttributes,
    ) -> Result<Status, Self::Error> {
        Ok(ok(id))
    }

    async fn mkdir(
        &mut self,
        id: u32,
        path: String,
        _attrs: FileAttributes,
    ) -> Result<Status, Self::Error> {
        std::fs::create_dir(self.real(&path)?).map_err(|error| code(&error))?;
        Ok(ok(id))
    }

    async fn rmdir(&mut self, id: u32, path: String) -> Result<Status, Self::Error> {
        std::fs::remove_dir(self.real(&path)?).map_err(|error| code(&error))?;
        Ok(ok(id))
    }

    async fn remove(&mut self, id: u32, filename: String) -> Result<Status, Self::Error> {
        std::fs::remove_file(self.real(&filename)?).map_err(|error| code(&error))?;
        Ok(ok(id))
    }

    async fn rename(
        &mut self,
        id: u32,
        oldpath: String,
        newpath: String,
    ) -> Result<Status, Self::Error> {
        std::fs::rename(self.real(&oldpath)?, self.real(&newpath)?)
            .map_err(|error| code(&error))?;
        Ok(ok(id))
    }
}

/// Starts a server for `root` on a free port of this machine and returns
/// the port.
fn serve(runtime: &tokio::runtime::Runtime, root: &Path, key: Option<ssh_key::PublicKey>) -> u16 {
    let port = std::net::TcpListener::bind(("127.0.0.1", 0))
        .and_then(|listener| listener.local_addr())
        .expect("a free port")
        .port();
    let config = russh::server::Config {
        auth_rejection_time: Duration::from_millis(10),
        auth_rejection_time_initial: Some(Duration::ZERO),
        keys: vec![PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).expect("a host key")],
        ..Default::default()
    };
    let mut server = Server {
        root: root.to_path_buf(),
        key,
    };
    runtime.spawn(async move {
        let _ = server
            .run_on_address(Arc::new(config), ("127.0.0.1", port))
            .await;
    });
    // Until it listens.
    for _ in 0..200 {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    port
}

fn endpoint(port: u16) -> Endpoint {
    Endpoint {
        kind: Kind::Sftp,
        host: "127.0.0.1".to_owned(),
        port,
        user: "ann".to_owned(),
        secret: "secret".to_owned(),
        root: String::new(),
        secure: false,
        bucket: String::new(),
        region: String::new(),
        host_key: None,
        key_file: String::new(),
    }
}

/// Connects with the server's key accepted, whatever it is.
fn trusting(
    remote: &Remote,
    mut endpoint: Endpoint,
) -> Result<dirfleet_remote::Session, ConnectError> {
    if let Err(ConnectError::UnknownHostKey { fingerprint }) = remote.connect(&endpoint) {
        endpoint.host_key = Some(fingerprint);
    }
    remote.connect(&endpoint)
}

#[test]
fn an_sftp_server_is_asked_about_trusted_logged_into_listed_and_read() {
    let served = tempfile::tempdir().expect("temp dir");
    std::fs::create_dir(served.path().join("docs")).expect("folder");
    std::fs::write(served.path().join("docs/a.md"), b"# a").expect("write");
    let readme = served.path().join("readme.txt");
    std::fs::write(&readme, README).expect("write");
    std::fs::File::options()
        .write(true)
        .open(&readme)
        .and_then(|file| file.set_modified(UNIX_EPOCH + Duration::from_secs(1_700_000_000)))
        .expect("time");
    let runtime = tokio::runtime::Runtime::new().expect("the server's runtime");
    let port = serve(&runtime, served.path(), None);
    let remote = Remote::new().expect("runtime");
    let mut endpoint = Endpoint {
        secret: "wrong".to_owned(),
        ..endpoint(port)
    };

    // A server never seen before: its key is what to ask about.
    let Err(ConnectError::UnknownHostKey { fingerprint }) = remote.connect(&endpoint) else {
        panic!("the key is not known yet");
    };
    assert!(fingerprint.starts_with("SHA256:"), "{fingerprint}");
    // Another key than the one accepted is asked about again.
    endpoint.host_key = Some("SHA256:someone-else".to_owned());
    assert!(matches!(
        remote.connect(&endpoint),
        Err(ConnectError::UnknownHostKey { .. })
    ));

    // The key accepted, the password is wrong, then right.
    endpoint.host_key = Some(fingerprint);
    assert!(matches!(
        remote.connect(&endpoint).err(),
        Some(ConnectError::NeedsSecret)
    ));
    endpoint.secret = "secret".to_owned();
    let session = remote.connect(&endpoint).expect("connects");
    assert!(!session.is_closed());

    let mut top = session.list("/").expect("lists");
    top.sort_by(|a, b| a.name.cmp(&b.name));
    let seen: Vec<(&str, bool, u64)> = top
        .iter()
        .map(|entry| (entry.name.as_str(), entry.is_dir, entry.size))
        .collect();
    assert_eq!(
        seen,
        [
            ("docs", true, 0),
            ("readme.txt", false, README.len() as u64)
        ]
    );
    assert_eq!(top[1].modified, Some(1_700_000_000));
    let docs = session.list("docs").expect("lists");
    assert_eq!(docs.len(), 1);
    assert_eq!(docs[0].name, "a.md");
    assert!(session.list("nowhere").is_err());

    let dir = tempfile::tempdir().expect("temp dir");
    let copy = dir.path().join("readme.txt");
    session
        .download("readme.txt", &copy, &AtomicBool::new(false))
        .expect("downloads");
    assert_eq!(std::fs::read(&copy).expect("read"), README);
    let missing = dir.path().join("missing.txt");
    assert!(
        session
            .download("missing.txt", &missing, &AtomicBool::new(false))
            .is_err()
    );
    assert!(!missing.exists());
}

#[test]
fn an_sftp_server_takes_files_and_folders_and_renames_and_removes_them() {
    let served = tempfile::tempdir().expect("temp dir");
    let runtime = tokio::runtime::Runtime::new().expect("the server's runtime");
    let port = serve(&runtime, served.path(), None);
    let remote = Remote::new().expect("runtime");
    let session = trusting(&remote, endpoint(port)).expect("connects");

    session.make_dir("docs").expect("makes a folder");
    assert!(served.path().join("docs").is_dir());
    // Larger than one piece, and not a whole number of them.
    let data: Vec<u8> = (0..700_001_u32).map(|n| (n % 251) as u8).collect();
    let mut writing = session
        .write("docs/data.bin", data.len() as u64)
        .expect("opens");
    writing.write_all(&data).expect("writes");
    writing.finish().expect("finishes");
    assert_eq!(
        std::fs::read(served.path().join("docs/data.bin")).expect("read"),
        data
    );
    session
        .write("docs/empty.txt", 0)
        .expect("opens")
        .finish()
        .expect("finishes");
    assert_eq!(
        std::fs::read(served.path().join("docs/empty.txt")).expect("read"),
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
    assert!(served.path().join("papers/renamed.bin").is_file());
    // SFTP has no copy of its own: the data has to come here and go back.
    assert_eq!(
        session.copy_file("papers/renamed.bin", "papers/copy.bin"),
        Ok(false)
    );

    // A folder that holds something stays.
    assert!(session.remove_dir("papers").is_err());
    session
        .remove_file("papers/renamed.bin")
        .expect("removes a file");
    session
        .remove_file("papers/empty.txt")
        .expect("removes a file");
    session.remove_dir("papers").expect("removes a folder");
    assert!(!served.path().join("papers").exists());
}

#[test]
fn an_sftp_server_is_logged_into_with_a_key_file() {
    let served = tempfile::tempdir().expect("temp dir");
    std::fs::write(served.path().join("readme.txt"), README).expect("write");
    let keys = tempfile::tempdir().expect("temp dir");
    let save = |name: &str| {
        let key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).expect("a key");
        let file = keys.path().join(name);
        std::fs::write(
            &file,
            key.to_openssh(LineEnding::LF).expect("text").as_bytes(),
        )
        .expect("write");
        (
            key.public_key().clone(),
            file.to_string_lossy().into_owned(),
        )
    };
    let (known, mine) = save("mine");
    let (_, other) = save("other");
    let runtime = tokio::runtime::Runtime::new().expect("the server's runtime");
    let port = serve(&runtime, served.path(), Some(known));
    let remote = Remote::new().expect("runtime");
    let with = |key_file: &str| Endpoint {
        secret: String::new(),
        key_file: key_file.to_owned(),
        ..endpoint(port)
    };

    let session = trusting(&remote, with(&mine)).expect("the key logs in");
    assert_eq!(session.list("/").expect("lists").len(), 1);
    // A key the server does not know, and a file that is no key.
    assert!(matches!(
        trusting(&remote, with(&other)),
        Err(ConnectError::Failed(_))
    ));
    let no_key = served.path().join("readme.txt");
    assert!(matches!(
        trusting(&remote, with(&no_key.to_string_lossy())),
        Err(ConnectError::Failed(_))
    ));
}
