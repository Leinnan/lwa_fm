//! Checksums in the inspector: a file's CRC32, MD5, SHA-1, SHA-256 or
//! BLAKE3, worked out on request since it means reading the whole file,
//! copied as text, and compared with one the user pastes or with the one a
//! checksum file beside it (`name.sha256`, `SHA256SUMS`…) holds for it.
use std::collections::BTreeMap;

use crux_core::Command;
use crux_core::capability::Operation;
use crux_core::command::AbortHandle;
use crux_core::render::render;
use facet::Facet;
use serde::{Deserialize, Serialize};

use super::{DirFleet, Effect, Event, Model, SystemOp, Tab};
use crate::listing::DirEntryMetaData;

#[derive(
    Facet, Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[repr(C)]
pub enum Algorithm {
    Crc32,
    Md5,
    Sha1,
    Sha256,
    Blake3,
}

impl Algorithm {
    pub const ALL: [Self; 5] = [
        Self::Crc32,
        Self::Md5,
        Self::Sha1,
        Self::Sha256,
        Self::Blake3,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Crc32 => "CRC32",
            Self::Md5 => "MD5",
            Self::Sha1 => "SHA-1",
            Self::Sha256 => "SHA-256",
            Self::Blake3 => "BLAKE3",
        }
    }

    /// How many hexadecimal digits its checksums have.
    pub const fn hex_len(self) -> usize {
        match self {
            Self::Crc32 => 8,
            Self::Md5 => 32,
            Self::Sha1 => 40,
            Self::Sha256 | Self::Blake3 => 64,
        }
    }

    /// The names a checksum file made with it goes by: the extension of one
    /// beside a file, and a folder's list.
    const fn file_names(self) -> (&'static str, &'static str) {
        match self {
            Self::Crc32 => ("sfv", "CRC32SUMS"),
            Self::Md5 => ("md5", "MD5SUMS"),
            Self::Sha1 => ("sha1", "SHA1SUMS"),
            Self::Sha256 => ("sha256", "SHA256SUMS"),
            Self::Blake3 => ("b3", "B3SUMS"),
        }
    }
}

/// Read the file at `path` once and work out its checksums, saying how far
/// the read is. Stopped by [`StopHash`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HashFile {
    pub id: u64,
    pub path: String,
    pub algorithms: Vec<Algorithm>,
}

/// What a running [`HashFile`] reports. Opaque to facet: it never leaves
/// the core.
#[derive(Clone, Debug, PartialEq, Eq, Facet)]
#[facet(opaque)]
#[repr(C)]
pub enum HashUpdate {
    /// Bytes read so far, of how many.
    Progress {
        done: u64,
        total: u64,
    },
    /// Each checksum asked for, in lowercase hexadecimal.
    Done(Vec<(Algorithm, String)>),
    Failed(String),
}

impl Operation for HashFile {
    type Output = HashUpdate;
}

/// Stop the hashing `id` (crux #622: streams are stopped by message).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StopHash {
    pub id: u64,
}

impl Operation for StopHash {
    type Output = ();
}

/// Look in the checksum file `sidecar` for the checksum of the file called
/// `name`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadSidecar {
    pub sidecar: String,
    pub name: String,
}

impl Operation for ReadSidecar {
    /// The checksum as written there, or `None` when the file has none for
    /// that name or can't be read.
    type Output = Option<String>;
}

/// How the file's checksum compares with the one it should have.
#[derive(Facet, Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub enum ChecksumVerdict {
    /// Nothing to compare with.
    None,
    /// The file is still being read.
    Checking,
    Match,
    Mismatch,
    /// What was given is no checksum this app knows.
    Unknown,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ChecksumRow {
    pub algorithm: Algorithm,
    pub label: String,
    /// The checksum in hexadecimal; empty until it is worked out.
    pub value: String,
    pub computing: bool,
}

/// A file's checksums, for the inspector.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ChecksumsView {
    /// One row for each algorithm, worked out or not.
    pub rows: Vec<ChecksumRow>,
    /// While the file is read: how far, in thousandths.
    pub progress: Option<u16>,
    /// The checksum the file is compared with, as hexadecimal digits.
    pub expected: String,
    pub verdict: ChecksumVerdict,
    /// The verdict in words, or why the file couldn't be read.
    pub message: String,
    /// Checksum files in the folder that may hold this file's checksum.
    pub sidecars: Vec<String>,
}

struct Job {
    id: u64,
    algorithms: Vec<Algorithm>,
    /// Read so far, in thousandths.
    progress: u16,
    abort: AbortHandle,
}

/// What is known of one file's checksums.
#[derive(Default)]
struct FileSums {
    /// The file's size and modification time when they were worked out:
    /// checksums of an earlier version are not shown.
    stamp: Option<(u64, u32)>,
    values: BTreeMap<Algorithm, String>,
    job: Option<Job>,
    expected: String,
    /// What was pasted held no checksum.
    unknown: bool,
    error: String,
}

/// Checksums asked for, by path.
#[derive(Default)]
pub(super) struct Checksums {
    next_id: u64,
    files: BTreeMap<String, FileSums>,
}

/// The longest run of hexadecimal digits in `text`, in lowercase: people
/// paste checksums with names, labels and spaces around them.
fn hex_run(text: &str) -> String {
    text.split(|c: char| !c.is_ascii_hexdigit())
        .max_by_key(|run| run.len())
        .unwrap_or_default()
        .to_ascii_lowercase()
}

/// The algorithms whose checksums are `len` digits long.
fn with_hex_len(len: usize) -> Vec<Algorithm> {
    Algorithm::ALL
        .into_iter()
        .filter(|algorithm| algorithm.hex_len() == len)
        .collect()
}

fn stamp_of(meta: &DirEntryMetaData) -> (u64, u32) {
    (meta.size, *meta.modified_at)
}

impl FileSums {
    fn verdict(&self) -> (ChecksumVerdict, String) {
        if !self.error.is_empty() {
            return (ChecksumVerdict::None, self.error.clone());
        }
        if self.unknown {
            return (
                ChecksumVerdict::Unknown,
                "That is not a checksum of a kind known here".to_owned(),
            );
        }
        if self.expected.is_empty() {
            return (ChecksumVerdict::None, String::new());
        }
        let candidates = with_hex_len(self.expected.len());
        let matching = candidates
            .iter()
            .find(|algorithm| self.values.get(algorithm) == Some(&self.expected));
        if let Some(algorithm) = matching {
            return (
                ChecksumVerdict::Match,
                format!("Matches the file's {}", algorithm.label()),
            );
        }
        if candidates
            .iter()
            .all(|algorithm| self.values.contains_key(algorithm))
        {
            let names: Vec<&str> = candidates.iter().map(|a| a.label()).collect();
            return (
                ChecksumVerdict::Mismatch,
                format!("Does not match the file's {}", names.join(" or ")),
            );
        }
        (ChecksumVerdict::Checking, "Reading the file…".to_owned())
    }
}

impl Checksums {
    /// The inspector's checksums for the file at `path`, as it is now.
    fn view(&self, path: &str, meta: &DirEntryMetaData, sidecars: Vec<String>) -> ChecksumsView {
        let empty = FileSums::default();
        let sums = self
            .files
            .get(path)
            // Worked out for an earlier version of the file: as good as none.
            .filter(|sums| sums.stamp.is_none_or(|stamp| stamp == stamp_of(meta)))
            .unwrap_or(&empty);
        let (verdict, message) = sums.verdict();
        ChecksumsView {
            rows: Algorithm::ALL
                .into_iter()
                .map(|algorithm| ChecksumRow {
                    algorithm,
                    label: algorithm.label().to_owned(),
                    value: sums.values.get(&algorithm).cloned().unwrap_or_default(),
                    computing: sums
                        .job
                        .as_ref()
                        .is_some_and(|job| job.algorithms.contains(&algorithm)),
                })
                .collect(),
            progress: sums.job.as_ref().map(|job| job.progress),
            expected: sums.expected.clone(),
            verdict,
            message,
            sidecars,
        }
    }
}

impl Tab {
    /// The checksum files beside the entry called `name` that may hold its
    /// checksum: `name.sha256` and the like, and the folder's lists.
    fn sidecars_of(&self, name: &str) -> Vec<String> {
        // Results from several folders: their checksum files aren't listed.
        if self.shows_folders() {
            return Vec::new();
        }
        let beside: Vec<String> = Algorithm::ALL
            .into_iter()
            .map(|algorithm| format!("{name}.{}", algorithm.file_names().0))
            .collect();
        let mut found: Vec<String> = (0..self.listing.len())
            .map(|entry| self.listing.name(entry))
            .filter(|other| {
                beside.iter().any(|file| file.eq_ignore_ascii_case(other))
                    || Algorithm::ALL
                        .into_iter()
                        .any(|algorithm| algorithm.file_names().1.eq_ignore_ascii_case(other))
            })
            .map(str::to_owned)
            .collect();
        found.sort_unstable();
        found
    }

    /// The checksums the inspector shows for the file at `entry`.
    pub(super) fn checksums_view(
        &self,
        checksums: &Checksums,
        entry: usize,
        path: &str,
    ) -> ChecksumsView {
        checksums.view(
            path,
            self.listing.meta(entry),
            self.sidecars_of(self.listing.name(entry)),
        )
    }
}

impl DirFleet {
    /// The size and modification time of the file at `path`, when a tab's
    /// inspector shows it.
    fn checksum_stamp(model: &Model, path: &str) -> Option<(u64, u32)> {
        model.tabs.values().find_map(|tab| {
            let row = tab.single_item()?;
            let entry = *tab.order.get(row)? as usize;
            let shown = tab.row_path(row)?;
            (shown.to_str() == Some(path)).then(|| stamp_of(tab.listing.meta(entry)))
        })
    }

    /// Works out the checksums of `path` it doesn't have yet.
    fn compute_checksums(
        model: &mut Model,
        path: &str,
        wanted: &[Algorithm],
    ) -> Command<Effect, Event> {
        let stamp = Self::checksum_stamp(model, path);
        let sums = model.checksums.files.entry(path.to_owned()).or_default();
        if stamp.is_some() && sums.stamp != stamp {
            // The file changed since: what was worked out is of no use.
            sums.values.clear();
            sums.stamp = stamp;
        }
        sums.error.clear();
        let mut algorithms: Vec<Algorithm> = wanted
            .iter()
            .copied()
            .filter(|algorithm| !sums.values.contains_key(algorithm))
            .collect();
        if algorithms.is_empty() {
            return render();
        }
        // One read of the file serves every checksum asked for meanwhile.
        let stop = match sums.job.take() {
            Some(job) if algorithms.iter().all(|a| job.algorithms.contains(a)) => {
                sums.job = Some(job);
                return render();
            }
            Some(job) => {
                for algorithm in &job.algorithms {
                    if !algorithms.contains(algorithm) {
                        algorithms.push(*algorithm);
                    }
                }
                job.abort.abort();
                Command::notify_shell(StopHash { id: job.id }).into()
            }
            None => Command::done(),
        };
        model.checksums.next_id += 1;
        let id = model.checksums.next_id;
        let sent = path.to_owned();
        let hash = Command::stream_from_shell(HashFile {
            id,
            path: path.to_owned(),
            algorithms: algorithms.clone(),
        })
        .then_send(move |update| Event::ChecksumUpdated {
            id,
            path: sent.clone(),
            update,
        });
        sums.job = Some(Job {
            id,
            algorithms,
            progress: 0,
            abort: hash.abort_handle(),
        });
        stop.and(hash).and(render())
    }

    /// Compares the file at `path` with the checksum in `text`.
    fn verify_checksum(model: &mut Model, path: &str, text: &str) -> Command<Effect, Event> {
        let expected = hex_run(text);
        let candidates = with_hex_len(expected.len());
        let sums = model.checksums.files.entry(path.to_owned()).or_default();
        sums.error.clear();
        sums.unknown = candidates.is_empty() && !text.trim().is_empty();
        sums.expected = if candidates.is_empty() {
            String::new()
        } else {
            expected
        };
        Self::compute_checksums(model, path, &candidates)
    }

    /// The events of the inspector's checksums.
    pub(super) fn checksum_event(model: &mut Model, event: Event) -> Command<Effect, Event> {
        match event {
            Event::ComputeChecksum { path, algorithm } => {
                Self::compute_checksums(model, &path, &[algorithm])
            }
            Event::CancelChecksum { path } => {
                let job = model
                    .checksums
                    .files
                    .get_mut(&path)
                    .and_then(|sums| sums.job.take());
                let Some(job) = job else {
                    return Command::done();
                };
                job.abort.abort();
                let stop: Command<Effect, Event> =
                    Command::notify_shell(StopHash { id: job.id }).into();
                stop.and(render())
            }
            Event::VerifyChecksum { path, expected } => {
                Self::verify_checksum(model, &path, &expected)
            }
            Event::VerifySidecar { path, sidecar } => {
                let name = crate::location::Location::parse(&path)
                    .file_name()
                    .unwrap_or_default()
                    .to_owned();
                let beside = crate::location::Location::parse(&path)
                    .parent()
                    .map(|folder| folder.join(&sidecar).to_string())
                    .unwrap_or_default();
                Command::request_from_shell(ReadSidecar {
                    sidecar: beside,
                    name,
                })
                .then_send(move |expected| Event::SidecarRead {
                    path,
                    sidecar,
                    expected,
                })
            }
            Event::SidecarRead {
                path,
                sidecar,
                expected,
            } => {
                if let Some(expected) = expected {
                    return Self::verify_checksum(model, &path, &expected);
                }
                let sums = model.checksums.files.entry(path).or_default();
                sums.expected.clear();
                sums.unknown = false;
                sums.error = format!("{sidecar} has no checksum for this file");
                render()
            }
            Event::CopyChecksum { path, algorithm } => model
                .checksums
                .files
                .get(&path)
                .and_then(|sums| sums.values.get(&algorithm))
                .map_or_else(Command::done, |value| {
                    Self::system(SystemOp::CopyText(value.clone()))
                }),
            Event::ChecksumUpdated { id, path, update } => {
                let Some(sums) = model
                    .checksums
                    .files
                    .get_mut(&path)
                    .filter(|sums| sums.job.as_ref().is_some_and(|job| job.id == id))
                else {
                    return Command::done();
                };
                match update {
                    HashUpdate::Progress { done, total } => {
                        if let Some(job) = &mut sums.job {
                            let part = (done as f64 / total.max(1) as f64).clamp(0.0, 1.0);
                            job.progress = (part * 1000.0).round() as u16;
                        }
                    }
                    HashUpdate::Done(values) => {
                        sums.values.extend(values);
                        if let Some(job) = sums.job.take() {
                            job.abort.abort();
                        }
                    }
                    HashUpdate::Failed(error) => {
                        sums.error = error;
                        if let Some(job) = sums.job.take() {
                            job.abort.abort();
                        }
                    }
                }
                render()
            }
            _ => Command::done(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pasted_checksum_is_the_longest_run_of_hexadecimal_digits() {
        assert_eq!(
            hex_run("  D41D8CD98F00B204E9800998ECF8427E  "),
            "d41d8cd98f00b204e9800998ecf8427e"
        );
        assert_eq!(
            hex_run("SHA1 (a.txt) = da39a3ee5e6b4b0d3255bfef95601890afd80709"),
            "da39a3ee5e6b4b0d3255bfef95601890afd80709"
        );
        assert_eq!(hex_run("not one"), "e");
        assert_eq!(hex_run(""), "");
    }

    #[test]
    fn a_checksum_s_length_says_which_algorithms_it_may_be() {
        assert_eq!(with_hex_len(8), [Algorithm::Crc32]);
        assert_eq!(with_hex_len(32), [Algorithm::Md5]);
        assert_eq!(with_hex_len(40), [Algorithm::Sha1]);
        assert_eq!(with_hex_len(64), [Algorithm::Sha256, Algorithm::Blake3]);
        assert_eq!(with_hex_len(12), []);
    }
}
