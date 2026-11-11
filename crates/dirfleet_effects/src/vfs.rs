//! Places that are not folders on this machine.
//!
//! A [`Provider`] lists such a place and brings one of its items to this
//! machine as a real file, so that it can be opened, previewed or read
//! further. [`Vfs`] finds the provider of a [`Location`]. Archives are the
//! first: each open archive keeps its tree of entries, so listing one of
//! its folders reads nothing. Passwords given for archives are kept while
//! the app runs, and nowhere else.
use std::collections::HashMap;
use std::fmt;
use std::hash::BuildHasher;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::UNIX_EPOCH;

use dirfleet_archive::{ArchiveHandle, CoreError, Entry, open_archive};
use dirfleet_core::listing::{DirEntryMetaData, EntryType, Listing, ListingBuilder};
use dirfleet_core::location::Location;

/// Archives kept open: enough for a few tabs, with what they hold in memory
/// being their lists of entries.
const OPEN_ARCHIVES: usize = 6;

/// Why a place can't be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VfsError {
    /// It needs a password; `wrong` when the one given did not fit.
    Password { wrong: bool },
    /// It is on a server that is not connected: the connection's id.
    NotConnected(String),
    /// Anything else, in words for the user.
    Other(String),
}

impl fmt::Display for VfsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Password { wrong: false } => f.write_str("This archive is password protected."),
            Self::Password { wrong: true } => f.write_str("The password is incorrect."),
            Self::NotConnected(_) => f.write_str("The server is not connected."),
            Self::Other(reason) => f.write_str(reason),
        }
    }
}

impl VfsError {
    fn other(reason: impl fmt::Display) -> Self {
        Self::Other(reason.to_string())
    }

    /// An archive's own error; `had_password` says whether one was given.
    pub(crate) fn of(error: &CoreError, had_password: bool) -> Self {
        match error {
            CoreError::PasswordRequired => Self::Password { wrong: false },
            CoreError::BadPassword => Self::Password {
                wrong: had_password,
            },
            other => Self::other(other),
        }
    }
}

/// One item of a place that is not on this machine's disks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VEntry {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    /// Unix seconds, when known.
    pub modified: Option<i64>,
}

/// A kind of place other than this machine's disks.
pub trait Provider: Send + Sync {
    /// The items in the folder `dir` (`""` is the top).
    ///
    /// # Errors
    /// Why the folder can't be listed.
    fn list(&self, dir: &str) -> Result<Vec<VEntry>, VfsError>;

    /// A copy of the item at `path` as a file or folder below `into`, for
    /// opening or reading. Returns where it is.
    ///
    /// # Errors
    /// Why the item can't be brought here.
    fn materialize(&self, path: &str, into: &Path) -> Result<PathBuf, VfsError>;
}

/// An archive as a place: its entries by folder.
pub(crate) struct ArchiveProvider {
    pub(crate) handle: Arc<ArchiveHandle>,
    /// The password it was opened with, when one was given.
    pub(crate) password: Option<String>,
    entries: Vec<Entry>,
    /// Each folder's entries, by the folder's path inside the archive.
    children: HashMap<String, Vec<u32>>,
    /// Every entry's id, by its path inside the archive.
    ids: HashMap<String, u32>,
}

impl ArchiveProvider {
    fn open(file: &Path, password: Option<String>) -> Result<Self, VfsError> {
        let handle = open_archive(file.to_string_lossy().into_owned(), password.clone(), None)
            .map_err(|error| VfsError::of(&error, password.is_some()))?;
        let entries = handle.entries();
        let mut children: HashMap<String, Vec<u32>> = HashMap::new();
        let mut ids = HashMap::with_capacity(entries.len());
        for entry in &entries {
            let path = entry.path.replace('\\', "/");
            let folder = entry
                .parent_id
                .and_then(|parent| entries.get(parent as usize))
                .map(|parent| parent.path.replace('\\', "/"))
                .unwrap_or_default();
            children.entry(folder).or_default().push(entry.id);
            ids.insert(path, entry.id);
        }
        Ok(Self {
            handle,
            password,
            entries,
            children,
            ids,
        })
    }

    /// The id of the entry at `path` inside the archive.
    pub(crate) fn id_of(&self, path: &str) -> Option<u32> {
        self.ids.get(path.trim_matches('/')).copied()
    }
}

impl Provider for ArchiveProvider {
    fn list(&self, dir: &str) -> Result<Vec<VEntry>, VfsError> {
        let dir = dir.trim_matches('/');
        if !dir.is_empty()
            && !self
                .id_of(dir)
                .and_then(|id| self.entries.get(id as usize))
                .is_some_and(|entry| entry.is_dir)
        {
            return Err(VfsError::Other(format!(
                "{dir}: no such folder in the archive"
            )));
        }
        Ok(self
            .children
            .get(dir)
            .into_iter()
            .flatten()
            .filter_map(|&id| self.entries.get(id as usize))
            .map(|entry| VEntry {
                name: entry.name.clone(),
                is_dir: entry.is_dir,
                size: entry.size,
                modified: entry.modified,
            })
            .collect())
    }

    fn materialize(&self, path: &str, into: &Path) -> Result<PathBuf, VfsError> {
        let id = self
            .id_of(path)
            .ok_or_else(|| VfsError::Other(format!("{path}: not in the archive")))?;
        std::fs::create_dir_all(into).map_err(VfsError::other)?;
        let extracted = self
            .handle
            .extract_to_temp(
                vec![id],
                into.to_string_lossy().into_owned(),
                self.password.clone(),
            )
            .map_err(|error| VfsError::of(&error, self.password.is_some()))?;
        extracted
            .into_iter()
            .next()
            .map(PathBuf::from)
            .ok_or_else(|| VfsError::Other(format!("{path}: nothing was extracted")))
    }
}

/// An open archive and what its file looked like when it was opened.
struct Opened {
    stamp: (u64, u128),
    provider: Arc<ArchiveProvider>,
}

/// Finds the provider of a place, keeping archives open between listings.
pub struct Vfs {
    archives: Mutex<lru::LruCache<PathBuf, Opened>>,
    /// Passwords given for archives, by the archive's file.
    passwords: Mutex<HashMap<PathBuf, String>>,
    /// Where items brought to this machine are kept.
    cache: PathBuf,
    /// What connects to servers, started when the first one is asked for.
    remote: OnceLock<Option<dirfleet_remote::Remote>>,
    /// Open connections, by the saved connection's id.
    sessions: Mutex<HashMap<String, Arc<dirfleet_remote::Session>>>,
}

/// A server as a place.
struct ServerProvider(Arc<dirfleet_remote::Session>);

impl Provider for ServerProvider {
    fn list(&self, dir: &str) -> Result<Vec<VEntry>, VfsError> {
        let listed = self.0.list(dir).map_err(VfsError::Other)?;
        Ok(listed
            .into_iter()
            .map(|entry| VEntry {
                name: entry.name,
                is_dir: entry.is_dir,
                size: entry.size,
                modified: entry.modified,
            })
            .collect())
    }

    fn materialize(&self, path: &str, into: &Path) -> Result<PathBuf, VfsError> {
        let name = path
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .filter(|name| !name.is_empty())
            .ok_or_else(|| VfsError::Other(format!("{path}: not a file")))?;
        std::fs::create_dir_all(into).map_err(VfsError::other)?;
        let copy = into.join(name);
        // An earlier copy may be of an earlier version.
        let _ = std::fs::remove_file(&copy);
        self.0
            .download(path, &copy, &AtomicBool::new(false))
            .map_err(VfsError::Other)?;
        Ok(copy)
    }
}

impl Default for Vfs {
    fn default() -> Self {
        Self::new(std::env::temp_dir().join("dirfleet").join("places"))
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The file's size and modification time: an archive written again is
/// opened again.
fn stamp_of(file: &Path) -> Result<(u64, u128), VfsError> {
    let meta = file
        .metadata()
        .map_err(|error| VfsError::Other(format!("{}: {error}", file.display())))?;
    let modified = meta
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |since| since.as_nanos());
    Ok((meta.len(), modified))
}

impl Vfs {
    /// A `Vfs` that keeps what it brings to this machine under `cache`.
    pub fn new(cache: PathBuf) -> Self {
        let capacity = NonZeroUsize::new(OPEN_ARCHIVES).unwrap_or(NonZeroUsize::MIN);
        Self {
            archives: Mutex::new(lru::LruCache::new(capacity)),
            passwords: Mutex::default(),
            cache,
            remote: OnceLock::new(),
            sessions: Mutex::default(),
        }
    }

    /// Connects to `endpoint` and keeps the connection under `id`, in place
    /// of an earlier one.
    ///
    /// # Errors
    /// Why the connection was not made.
    pub fn connect(
        &self,
        id: &str,
        endpoint: &dirfleet_remote::Endpoint,
    ) -> Result<(), dirfleet_remote::ConnectError> {
        let remote = self
            .remote
            .get_or_init(|| {
                dirfleet_remote::Remote::new()
                    .map_err(|error| log::error!("servers can't be connected to: {error}"))
                    .ok()
            })
            .as_ref()
            .ok_or_else(|| {
                dirfleet_remote::ConnectError::Failed(
                    "Connections can't be made on this system".to_owned(),
                )
            })?;
        let session = remote.connect(endpoint)?;
        lock(&self.sessions).insert(id.to_owned(), Arc::new(session));
        Ok(())
    }

    /// Closes the connection kept under `id`.
    pub fn disconnect(&self, id: &str) {
        lock(&self.sessions).remove(id);
    }

    /// The open connection `id`; one that has ended counts as not there.
    fn session(&self, id: &str) -> Result<Arc<dirfleet_remote::Session>, VfsError> {
        let open = lock(&self.sessions)
            .get(id)
            .filter(|session| !session.is_closed())
            .cloned();
        open.ok_or_else(|| {
            lock(&self.sessions).remove(id);
            VfsError::NotConnected(id.to_owned())
        })
    }

    /// The archive that is the file at `file`, opened once while the file
    /// stays as it is, with the password given for it if any.
    fn archive(&self, file: &Path) -> Result<Arc<ArchiveProvider>, VfsError> {
        let stamp = stamp_of(file)?;
        let known = lock(&self.archives)
            .get(file)
            .filter(|opened| opened.stamp == stamp)
            .map(|opened| Arc::clone(&opened.provider));
        if let Some(provider) = known {
            return Ok(provider);
        }
        let password = lock(&self.passwords).get(file).cloned();
        // Opened outside the lock: a large archive takes a while to read.
        let provider = Arc::new(ArchiveProvider::open(file, password)?);
        lock(&self.archives).put(
            file.to_path_buf(),
            Opened {
                stamp,
                provider: Arc::clone(&provider),
            },
        );
        Ok(provider)
    }

    /// Remembers `password` for the archive that is the file at `archive`,
    /// until the app ends. The archive is opened again with it.
    ///
    /// # Errors
    /// Why the archive's file can't be reached.
    pub fn set_password(&self, archive: &Location, password: String) -> Result<(), VfsError> {
        let file = self.local_file(archive)?;
        lock(&self.archives).pop(&file);
        lock(&self.passwords).insert(file, password);
        Ok(())
    }

    /// The archive that is the file at `archive`, open.
    pub(crate) fn open_archive(
        &self,
        archive: &Location,
    ) -> Result<Arc<ArchiveProvider>, VfsError> {
        self.archive(&self.local_file(archive)?)
    }

    /// Where copies of the items of `place` are kept: a folder named after
    /// the place, so two archives never share one.
    fn cache_for(&self, place: &Location) -> PathBuf {
        let named = foldhash::fast::FixedState::default().hash_one(place.to_string());
        self.cache.join(format!("{named:016x}"))
    }

    /// The provider of the place `location` is in, and its path there.
    ///
    /// # Errors
    /// Why the place can't be reached.
    pub fn resolve(&self, location: &Location) -> Result<(Arc<dyn Provider>, String), VfsError> {
        match location {
            Location::Local(path) => Err(VfsError::Other(format!(
                "{path} is a folder on this machine"
            ))),
            Location::Archive { outer, inner } => {
                // An archive inside an archive, or on a server, is brought here first.
                let provider: Arc<dyn Provider> = self.open_archive(outer)?;
                Ok((provider, inner.clone()))
            }
            Location::Remote {
                connection, path, ..
            } => {
                let provider: Arc<dyn Provider> =
                    Arc::new(ServerProvider(self.session(connection)?));
                Ok((provider, path.clone()))
            }
        }
    }

    /// A real file holding what is at `location`: the file itself when it
    /// is on this machine, a copy otherwise.
    ///
    /// # Errors
    /// Why the item can't be brought here.
    pub fn local_file(&self, location: &Location) -> Result<PathBuf, VfsError> {
        match location {
            Location::Local(path) => Ok(PathBuf::from(path)),
            Location::Archive { outer, .. } => {
                let (provider, inner) = self.resolve(location)?;
                let place = Location::Archive {
                    outer: outer.clone(),
                    inner: String::new(),
                };
                provider.materialize(&inner, &self.cache_for(&place))
            }
            Location::Remote { .. } => {
                let (provider, path) = self.resolve(location)?;
                // Each file's copy in a folder of its own: two folders may
                // hold files of one name.
                provider.materialize(&path, &self.cache_for(location))
            }
        }
    }

    /// The folder at `location` as a listing whose folder is `location`'s
    /// text, so rows are found below it like any folder's.
    ///
    /// # Errors
    /// Why the folder can't be listed.
    pub fn listing(&self, location: &Location) -> Result<Listing, VfsError> {
        let (provider, inner) = self.resolve(location)?;
        let entries = provider.list(&inner)?;
        let dir: Arc<str> = Arc::from(location.to_string());
        let mut builder = ListingBuilder::for_dir(&dir, entries.len(), false);
        for entry in entries {
            let mut meta = DirEntryMetaData::from(if entry.is_dir {
                EntryType::Directory
            } else {
                EntryType::File
            });
            meta.size = entry.size;
            let modified = entry
                .modified
                .and_then(|seconds| u32::try_from(seconds).ok())
                .unwrap_or(0);
            meta.modified_at = dirfleet_core::listing::TimestampSeconds::from_unix(modified);
            builder.push(&dir, &entry.name, meta);
        }
        Ok(builder.finish())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::io::Write as _;

    use super::*;

    /// A zip with a file at the top and one two folders down.
    pub fn sample(dir: &Path) -> PathBuf {
        let path = dir.join("sample.zip");
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).expect("create"));
        let options = zip::write::SimpleFileOptions::default();
        zip.start_file("readme.txt", options).expect("entry");
        zip.write_all(b"hello").expect("write");
        zip.start_file("docs/deep/notes.md", options)
            .expect("entry");
        zip.write_all(b"# notes").expect("write");
        zip.finish().expect("finish");
        path
    }

    /// One of the archive crate's own test archives.
    pub fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../dirfleet_archive/tests/fixtures")
            .join(name)
    }

    pub fn top_of(file: &Path) -> Location {
        Location::Local(file.to_string_lossy().into_owned()).into_archive()
    }

    fn names(listing: &Listing) -> Vec<String> {
        let mut names: Vec<String> = (0..listing.len())
            .map(|entry| listing.name(entry).to_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn an_archive_lists_as_folders_and_its_items_come_out_as_files() {
        let dir = tempfile::tempdir().expect("temp dir");
        let zip = sample(dir.path());
        let vfs = Vfs::new(dir.path().join("cache"));
        let top = top_of(&zip);

        let listing = vfs.listing(&top).expect("the top lists");
        assert_eq!(names(&listing), ["docs", "readme.txt"]);
        let readme = listing.position("readme.txt").expect("listed");
        assert_eq!(listing.meta(readme).size, 5);
        assert!(listing.meta(readme).entry_type.is_leaf());
        let docs = listing.position("docs").expect("listed");
        assert_eq!(listing.meta(docs).entry_type, EntryType::Directory);

        // A folder the archive only implies lists too.
        let deep = top.join("docs").join("deep");
        assert_eq!(names(&vfs.listing(&deep).expect("lists")), ["notes.md"]);
        assert!(vfs.listing(&top.join("nowhere")).is_err());
        assert!(
            vfs.listing(&top.join("readme.txt")).is_err(),
            "a file is no folder"
        );

        let file = vfs.local_file(&deep.join("notes.md")).expect("comes out");
        assert_eq!(std::fs::read_to_string(file).expect("read"), "# notes");
    }

    #[test]
    fn an_archive_inside_an_archive_opens_too_and_one_written_again_is_read_again() {
        let dir = tempfile::tempdir().expect("temp dir");
        let inner = sample(dir.path());
        let outer_path = dir.path().join("outer.zip");
        let write_outer = |extra: bool| {
            let mut zip = zip::ZipWriter::new(std::fs::File::create(&outer_path).expect("create"));
            let options = zip::write::SimpleFileOptions::default();
            zip.start_file("inner/sample.zip", options).expect("entry");
            zip.write_all(&std::fs::read(&inner).expect("read"))
                .expect("write");
            if extra {
                zip.start_file("added.txt", options).expect("entry");
                zip.write_all(b"new").expect("write");
            }
            zip.finish().expect("finish");
        };
        write_outer(false);
        let vfs = Vfs::new(dir.path().join("cache"));
        let outer = top_of(&outer_path);
        assert_eq!(names(&vfs.listing(&outer).expect("lists")), ["inner"]);
        let nested = outer.join("inner").join("sample.zip").into_archive();
        assert_eq!(
            names(&vfs.listing(&nested).expect("the inner archive lists")),
            ["docs", "readme.txt"]
        );

        write_outer(true);
        assert_eq!(
            names(&vfs.listing(&outer).expect("lists again")),
            ["added.txt", "inner"]
        );
    }

    #[test]
    fn a_file_that_is_no_archive_says_so() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("notes.zip");
        std::fs::write(&path, b"just text").expect("write");
        let vfs = Vfs::new(dir.path().join("cache"));
        assert!(matches!(
            vfs.listing(&top_of(&path)),
            Err(VfsError::Other(_))
        ));
        assert!(vfs.listing(&Location::Local("/x".into())).is_err());
    }

    #[test]
    fn an_archive_with_a_hidden_list_asks_for_its_password() {
        let dir = tempfile::tempdir().expect("temp dir");
        let vfs = Vfs::new(dir.path().join("cache"));
        let file = Location::Local(
            fixture("header_encrypted.rar")
                .to_string_lossy()
                .into_owned(),
        );
        let top = file.clone().into_archive();
        assert_eq!(
            vfs.listing(&top).err(),
            Some(VfsError::Password { wrong: false })
        );
        vfs.set_password(&file, "nope".into()).expect("kept");
        assert_eq!(
            vfs.listing(&top).err(),
            Some(VfsError::Password { wrong: true })
        );
        vfs.set_password(&file, "password".into()).expect("kept");
        assert_ne!(vfs.listing(&top).expect("lists now").len(), 0);
    }

    #[test]
    fn a_locked_file_in_an_archive_comes_out_once_the_password_is_given() {
        let dir = tempfile::tempdir().expect("temp dir");
        let vfs = Vfs::new(dir.path().join("cache"));
        let file = Location::Local(fixture("password_aes.rar").to_string_lossy().into_owned());
        let top = file.clone().into_archive();
        // Its list is open to all; what the files hold is not.
        let listing = vfs.listing(&top).expect("lists without a password");
        let name = (0..listing.len())
            .find(|&entry| listing.meta(entry).entry_type.is_leaf())
            .map(|entry| listing.name(entry).to_owned())
            .expect("a file");
        assert_eq!(
            vfs.local_file(&top.join(&name)).err(),
            Some(VfsError::Password { wrong: false })
        );
        vfs.set_password(&file, "password".into()).expect("kept");
        assert!(
            vfs.local_file(&top.join(&name))
                .expect("comes out")
                .is_file()
        );
    }

    #[test]
    fn a_connected_server_lists_and_hands_over_files_until_it_is_left() {
        let dir = tempfile::tempdir().expect("temp dir");
        let served = dir.path().join("served");
        std::fs::create_dir_all(served.join("docs")).expect("folders");
        std::fs::write(served.join("docs/a.md"), b"# a").expect("write");
        std::fs::write(served.join("readme.txt"), b"hello").expect("write");
        let vfs = Vfs::new(dir.path().join("cache"));
        let top = Location::parse("sftp://c1/");

        // Not connected yet: the listing says which connection it needs.
        assert_eq!(
            vfs.listing(&top).err(),
            Some(VfsError::NotConnected("c1".into()))
        );
        // A folder stands in for the server.
        let endpoint = dirfleet_remote::Endpoint {
            kind: dirfleet_remote::Kind::Folder,
            host: served.to_string_lossy().into_owned(),
            port: 0,
            user: String::new(),
            secret: String::new(),
            root: String::new(),
            secure: false,
            bucket: String::new(),
            region: String::new(),
            host_key: None,
        };
        vfs.connect("c1", &endpoint).expect("connects");
        assert_eq!(
            names(&vfs.listing(&top).expect("lists")),
            ["docs", "readme.txt"]
        );
        let docs = top.join("docs");
        assert_eq!(names(&vfs.listing(&docs).expect("lists")), ["a.md"]);
        let copy = vfs.local_file(&docs.join("a.md")).expect("comes over");
        assert_eq!(std::fs::read_to_string(copy).expect("read"), "# a");
        // Another connection's id is another server.
        assert!(matches!(
            vfs.listing(&Location::parse("sftp://c2/")),
            Err(VfsError::NotConnected(_))
        ));

        vfs.disconnect("c1");
        assert!(matches!(vfs.listing(&top), Err(VfsError::NotConnected(_))));
    }
}
