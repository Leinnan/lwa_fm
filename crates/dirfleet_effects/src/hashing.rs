//! Checksums of files.
//!
//! Every algorithm asked for is fed from one read of the file, which says
//! how far it is and stops when told to. Checksum files (`name.sha256`,
//! `SHA256SUMS`, `.sfv`) are read for the checksum they hold for one name.
use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::{Duration, Instant};

use crux_core::Request;
use crux_core::effects::ResolveSink;
use dirfleet_core::app::{Algorithm, HashFile, HashUpdate, ReadSidecar};
use sha2::Digest as _;

use crate::access_time::PreserveAccessTime;

/// Files are read in pieces of this size, so progress and stopping stay live.
const CHUNK: usize = 1024 * 1024;
/// How often a read says how far it is.
const REPORT_INTERVAL: Duration = Duration::from_millis(100);
/// Checksum files larger than this are not read: they are lists of names.
const SIDECAR_LIMIT: u64 = 8 * 1024 * 1024;

enum Hasher {
    Crc32(crc32fast::Hasher),
    Md5(md5::Md5),
    Sha1(sha1::Sha1),
    Sha256(sha2::Sha256),
    Blake3(Box<blake3::Hasher>),
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(text, "{byte:02x}");
    }
    text
}

/// Several checksums worked out over the same bytes.
pub struct MultiHasher(Vec<(Algorithm, Hasher)>);

impl MultiHasher {
    pub fn new(algorithms: &[Algorithm]) -> Self {
        Self(
            algorithms
                .iter()
                .map(|&algorithm| {
                    let hasher = match algorithm {
                        Algorithm::Crc32 => Hasher::Crc32(crc32fast::Hasher::new()),
                        Algorithm::Md5 => Hasher::Md5(md5::Md5::new()),
                        Algorithm::Sha1 => Hasher::Sha1(sha1::Sha1::new()),
                        Algorithm::Sha256 => Hasher::Sha256(sha2::Sha256::new()),
                        Algorithm::Blake3 => Hasher::Blake3(Box::new(blake3::Hasher::new())),
                    };
                    (algorithm, hasher)
                })
                .collect(),
        )
    }

    pub fn update(&mut self, bytes: &[u8]) {
        for (_, hasher) in &mut self.0 {
            match hasher {
                Hasher::Crc32(hasher) => hasher.update(bytes),
                Hasher::Md5(hasher) => hasher.update(bytes),
                Hasher::Sha1(hasher) => hasher.update(bytes),
                Hasher::Sha256(hasher) => hasher.update(bytes),
                Hasher::Blake3(hasher) => {
                    hasher.update(bytes);
                }
            }
        }
    }

    /// Each checksum in lowercase hexadecimal, in the order asked for.
    pub fn finish(self) -> Vec<(Algorithm, String)> {
        self.0
            .into_iter()
            .map(|(algorithm, hasher)| {
                let text = match hasher {
                    Hasher::Crc32(hasher) => format!("{:08x}", hasher.finalize()),
                    Hasher::Md5(hasher) => hex(&hasher.finalize()),
                    Hasher::Sha1(hasher) => hex(&hasher.finalize()),
                    Hasher::Sha256(hasher) => hex(&hasher.finalize()),
                    Hasher::Blake3(hasher) => hasher.finalize().to_hex().to_string(),
                };
                (algorithm, text)
            })
            .collect()
    }
}

/// The checksums of everything `reader` gives, or `None` when `cancel` was
/// set meanwhile. `progress` hears the bytes read so far after each piece.
///
/// # Errors
/// What went wrong reading.
pub fn hash_reader(
    mut reader: impl Read,
    algorithms: &[Algorithm],
    cancel: &AtomicBool,
    mut progress: impl FnMut(u64),
) -> io::Result<Option<Vec<(Algorithm, String)>>> {
    let mut hasher = MultiHasher::new(algorithms);
    let mut buffer = vec![0; CHUNK];
    let mut done = 0_u64;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Ok(None);
        }
        let read = match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        hasher.update(&buffer[..read]);
        done += read as u64;
        progress(done);
    }
    Ok(Some(hasher.finish()))
}

/// The checksums of the file at `path`, read without counting as opening it.
///
/// # Errors
/// What went wrong opening or reading the file.
pub fn hash_file(
    path: &Path,
    algorithms: &[Algorithm],
    cancel: &AtomicBool,
    progress: impl FnMut(u64),
) -> io::Result<Option<Vec<(Algorithm, String)>>> {
    let _accessed = PreserveAccessTime::new(path);
    hash_reader(File::open(path)?, algorithms, cancel, progress)
}

/// The BLAKE3 of the file at `path`, for telling whether two files hold
/// the same bytes; `None` when `stop` says to give up.
///
/// # Errors
/// What went wrong opening or reading the file.
pub fn fingerprint(
    path: &Path,
    mut stop: impl FnMut() -> bool,
) -> io::Result<Option<blake3::Hash>> {
    let mut file = File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = vec![0; CHUNK];
    loop {
        if stop() {
            return Ok(None);
        }
        let read = match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        hasher.update(&buffer[..read]);
    }
    Ok(Some(hasher.finalize()))
}

/// The checksum a checksum file's `text` holds for the file called `name`.
///
/// Read are `<hex>  name` and `<hex> *name` (the `sha256sum` family),
/// `ALGO (name) = <hex>` (BSD), `name <hex>` (SFV), and the lone checksum of
/// a file that holds nothing else.
pub fn checksum_in(text: &str, name: &str) -> Option<String> {
    let is_hex = |word: &str| word.len() >= 8 && word.chars().all(|c| c.is_ascii_hexdigit());
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with([';', '#']))
        .collect();
    let named = lines.iter().find_map(|line| {
        let (first, rest) = line.split_once(char::is_whitespace)?;
        let rest = rest.trim();
        // `<hex>  name`, the name maybe with a `*` (binary) or a `./` before it.
        if is_hex(first) {
            let listed = rest.trim_start_matches('*');
            let listed = listed.strip_prefix("./").unwrap_or(listed);
            return (listed == name).then(|| first.to_owned());
        }
        // `ALGO (name) = <hex>`
        if let Some((listed, sum)) = rest
            .strip_prefix('(')
            .and_then(|rest| rest.rsplit_once(") = "))
        {
            return (listed == name && is_hex(sum.trim())).then(|| sum.trim().to_owned());
        }
        // `name <hex>`
        let (listed, sum) = line.rsplit_once(char::is_whitespace)?;
        (listed.trim() == name && is_hex(sum)).then(|| sum.to_owned())
    });
    named.or_else(|| match lines.as_slice() {
        [only] if is_hex(only) => Some((*only).to_owned()),
        _ => None,
    })
}

/// Answers [`ReadSidecar`]: `None` when the file can't be read, is too
/// large to be a checksum file, or holds nothing for the name.
pub fn read_sidecar(request: &ReadSidecar) -> Option<String> {
    let path = Path::new(&request.sidecar);
    if path.metadata().ok()?.len() > SIDECAR_LIMIT {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    checksum_in(&String::from_utf8_lossy(&bytes), &request.name)
}

type Start = dyn Fn(Request<HashFile>, Arc<AtomicBool>) + Send + Sync;
type Running = Mutex<HashMap<u64, Arc<AtomicBool>>>;

/// Reads files for their checksums, each on a thread of its own, until it
/// is done or stopped.
pub struct HashHandler {
    running: Arc<Running>,
    start: Box<Start>,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl HashHandler {
    pub fn new<R>(sink: Weak<R>) -> Self
    where
        R: ResolveSink<HashFile> + Send + Sync + 'static,
    {
        let running: Arc<Running> = Arc::default();
        let finished = Arc::downgrade(&running);
        Self {
            running,
            start: Box::new(move |request, cancel| {
                spawn_hash(request, cancel, sink.clone(), finished.clone());
            }),
        }
    }

    pub fn start(&self, request: Request<HashFile>) {
        let cancel = Arc::new(AtomicBool::new(false));
        lock(&self.running).insert(request.operation.id, Arc::clone(&cancel));
        (self.start)(request, cancel);
    }

    pub fn stop(&self, id: u64) {
        let cancel = lock(&self.running).remove(&id);
        if let Some(cancel) = cancel {
            cancel.store(true, Ordering::Relaxed);
        }
    }
}

fn spawn_hash<R>(
    mut request: Request<HashFile>,
    cancel: Arc<AtomicBool>,
    sink: Weak<R>,
    running: Weak<Running>,
) where
    R: ResolveSink<HashFile> + Send + Sync + 'static,
{
    let id = request.operation.id;
    let spawned = std::thread::Builder::new()
        .name(format!("dirfleet_hash_{id}"))
        .spawn(move || {
            let operation = request.operation.clone();
            let path = Path::new(&operation.path);
            let total = path.metadata().map_or(0, |meta| meta.len());
            let mut tell = |update: HashUpdate| {
                if let Some(sink) = sink.upgrade() {
                    let _ = sink.resolve_request(&mut request, update);
                }
            };
            let mut last = Instant::now();
            let result = hash_file(path, &operation.algorithms, &cancel, |done| {
                if last.elapsed() >= REPORT_INTERVAL {
                    last = Instant::now();
                    tell(HashUpdate::Progress { done, total });
                }
            });
            match result {
                Ok(Some(values)) => tell(HashUpdate::Done(values)),
                // Stopped: whoever stopped it listens no more.
                Ok(None) => {}
                Err(error) => tell(HashUpdate::Failed(format!(
                    "{} can't be read: {error}",
                    dirfleet_core::fileops::display_name(path)
                ))),
            }
            if let Some(running) = running.upgrade() {
                lock(&running).remove(&id);
            }
        });
    if let Err(error) = spawned {
        log::error!("could not start a checksum's thread: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sums(bytes: &[u8]) -> Vec<String> {
        let cancel = AtomicBool::new(false);
        hash_reader(bytes, &Algorithm::ALL, &cancel, |_| {})
            .expect("reads")
            .expect("not stopped")
            .into_iter()
            .map(|(_, value)| value)
            .collect()
    }

    #[test]
    fn each_algorithm_gives_its_known_checksum() {
        assert_eq!(
            sums(b""),
            [
                "00000000",
                "d41d8cd98f00b204e9800998ecf8427e",
                "da39a3ee5e6b4b0d3255bfef95601890afd80709",
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
                "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262",
            ]
        );
        assert_eq!(
            sums(b"abc"),
            [
                "352441c2",
                "900150983cd24fb0d6963f7d28e17f72",
                "a9993e364706816aba3e25717850c26c9cd0d89d",
                "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
                "6437b3ac38465133ffb63b75273a8db548c558465d79db03fd359c6cd5bd9d85",
            ]
        );
    }

    #[test]
    fn a_file_is_read_once_for_all_its_checksums_and_says_how_far_it_is() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("data.bin");
        std::fs::write(&path, vec![7_u8; 3 * CHUNK + 5]).expect("write");
        let cancel = AtomicBool::new(false);
        let mut seen = Vec::new();
        let values = hash_file(
            &path,
            &[Algorithm::Sha256, Algorithm::Crc32],
            &cancel,
            |done| seen.push(done),
        )
        .expect("reads")
        .expect("not stopped");
        assert_eq!(values.len(), 2);
        assert_eq!(values[0].0, Algorithm::Sha256);
        assert_eq!(seen.last(), Some(&(3 * CHUNK as u64 + 5)));
        assert!(seen.len() >= 4, "one report for each piece: {seen:?}");
        assert_eq!(
            fingerprint(&path, || false).expect("reads"),
            fingerprint(&path, || false).expect("reads")
        );
    }

    #[test]
    fn a_stopped_read_gives_nothing() {
        let cancel = AtomicBool::new(false);
        let bytes = vec![1_u8; 4 * CHUNK];
        let stopped = hash_reader(&bytes[..], &[Algorithm::Md5], &cancel, |_| {
            cancel.store(true, Ordering::Relaxed);
        })
        .expect("reads");
        assert_eq!(stopped, None);
    }

    #[test]
    fn a_checksum_file_is_read_in_each_of_its_forms() {
        let sha = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        let list = format!("; made by hand\n{sha}  a.txt\n0000000000000000 *b.txt\n");
        assert_eq!(checksum_in(&list, "a.txt").as_deref(), Some(sha));
        assert_eq!(
            checksum_in(&list, "b.txt").as_deref(),
            Some("0000000000000000")
        );
        assert_eq!(checksum_in(&list, "c.txt"), None);
        let bsd = format!("SHA256 (my file (1).txt) = {sha}\n");
        assert_eq!(checksum_in(&bsd, "my file (1).txt").as_deref(), Some(sha));
        assert_eq!(
            checksum_in("; sfv\nmy movie.mkv 352441C2\n", "my movie.mkv").as_deref(),
            Some("352441C2")
        );
        // A file with one checksum and no name is about the file beside it.
        assert_eq!(
            checksum_in(&format!("{sha}\n"), "a.txt").as_deref(),
            Some(sha)
        );
        assert_eq!(checksum_in("hello world\n", "a.txt"), None);
    }
}
