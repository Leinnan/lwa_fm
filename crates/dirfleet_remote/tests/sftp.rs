//! The SFTP client against a small SFTP server run in this process: the
//! host key question, a wrong password, listing and downloading.
#![allow(
    clippy::unused_async_trait_impl,
    reason = "the server traits ask for async functions"
)]

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use dirfleet_remote::{ConnectError, Endpoint, Kind, Remote};
use russh::keys::{Algorithm, PrivateKey};
use russh::server::{Auth, ChannelOpenHandle, Msg, Server as _, Session};
use russh::{Channel, ChannelId};
use russh_sftp::protocol::{
    Attrs, Data, File, FileAttributes, Handle, Name, OpenFlags, Status, StatusCode, Version,
};
use tokio::sync::Mutex;

const README: &[u8] = b"hello from the server";

/// What the server holds: `/readme.txt`, and `/docs` with `a.md` in it.
fn attrs_of(path: &str) -> Option<FileAttributes> {
    let mut attrs = FileAttributes::empty();
    attrs.mtime = Some(1_700_000_000);
    match path.trim_end_matches('/') {
        "" | "/docs" => attrs.permissions = Some(0o040_755),
        "/readme.txt" => {
            attrs.permissions = Some(0o100_644);
            attrs.size = Some(README.len() as u64);
        }
        "/docs/a.md" => {
            attrs.permissions = Some(0o100_644);
            attrs.size = Some(3);
        }
        _ => return None,
    }
    Some(attrs)
}

fn children(dir: &str) -> Option<Vec<&'static str>> {
    match dir.trim_end_matches('/') {
        "" => Some(vec!["readme.txt", "docs"]),
        "/docs" => Some(vec!["a.md"]),
        _ => None,
    }
}

#[derive(Clone)]
struct Server;

impl russh::server::Server for Server {
    type Handler = Ssh;

    fn new_client(&mut self, _: Option<SocketAddr>) -> Ssh {
        Ssh::default()
    }
}

#[derive(Default)]
struct Ssh {
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
                russh_sftp::server::run(channel.into_stream(), Sftp::default()).await;
            }
            _ => session.channel_failure(channel_id)?,
        }
        Ok(())
    }
}

/// Folders already read: a second read of one says there is no more.
#[derive(Default)]
struct Sftp {
    read: Vec<String>,
}

fn ok(id: u32) -> Status {
    Status {
        id,
        status_code: StatusCode::Ok,
        error_message: "Ok".to_owned(),
        language_tag: "en-US".to_owned(),
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
        self.read.retain(|read| read != &handle);
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
        children(&path).ok_or(StatusCode::NoSuchFile)?;
        Ok(Handle { id, handle: path })
    }

    async fn readdir(&mut self, id: u32, handle: String) -> Result<Name, Self::Error> {
        if self.read.contains(&handle) {
            return Err(StatusCode::Eof);
        }
        let names = children(&handle).ok_or(StatusCode::NoSuchFile)?;
        self.read.push(handle.clone());
        let dir = handle.trim_end_matches('/');
        Ok(Name {
            id,
            files: names
                .into_iter()
                .filter_map(|name| Some(File::new(name, attrs_of(&format!("{dir}/{name}"))?)))
                .collect(),
        })
    }

    async fn open(
        &mut self,
        id: u32,
        filename: String,
        _pflags: OpenFlags,
        _attrs: FileAttributes,
    ) -> Result<Handle, Self::Error> {
        attrs_of(&filename).ok_or(StatusCode::NoSuchFile)?;
        Ok(Handle {
            id,
            handle: filename,
        })
    }

    async fn read(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        len: u32,
    ) -> Result<Data, Self::Error> {
        let bytes: &[u8] = match handle.as_str() {
            "/readme.txt" => README,
            "/docs/a.md" => b"# a",
            _ => return Err(StatusCode::NoSuchFile),
        };
        let start = usize::try_from(offset).unwrap_or(usize::MAX);
        if start >= bytes.len() {
            return Err(StatusCode::Eof);
        }
        let end = bytes.len().min(start + len as usize);
        Ok(Data {
            id,
            data: bytes[start..end].to_vec(),
        })
    }

    async fn stat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
        let attrs = attrs_of(&path).ok_or(StatusCode::NoSuchFile)?;
        Ok(Attrs { id, attrs })
    }

    async fn lstat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
        self.stat(id, path).await
    }

    async fn fstat(&mut self, id: u32, handle: String) -> Result<Attrs, Self::Error> {
        self.stat(id, handle).await
    }
}

/// Starts the server on a free port of this machine and returns the port.
fn serve(runtime: &tokio::runtime::Runtime) -> u16 {
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
    runtime.spawn(async move {
        let _ = Server
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

#[test]
fn an_sftp_server_is_asked_about_trusted_logged_into_listed_and_read() {
    let runtime = tokio::runtime::Runtime::new().expect("the server's runtime");
    let port = serve(&runtime);
    let remote = Remote::new().expect("runtime");
    let mut endpoint = Endpoint {
        kind: Kind::Sftp,
        host: "127.0.0.1".to_owned(),
        port,
        user: "ann".to_owned(),
        secret: "wrong".to_owned(),
        root: String::new(),
        secure: false,
        bucket: String::new(),
        region: String::new(),
        host_key: None,
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
