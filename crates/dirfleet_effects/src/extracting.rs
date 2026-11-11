//! Taking items out of an archive, as a file operation.
//!
//! The archive crate extracts into a hidden folder beside the destination
//! and moves the result into place at the end, so a failure or a stop
//! leaves nothing behind. It runs on a thread of its own here; this thread
//! passes its progress on and tells it to stop when the operation is
//! stopped.
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use dirfleet_archive::{CancelToken, ExtractRequest, ExtractSummary, ProgressObserver};
use dirfleet_core::fileops::{ConflictPolicy, FileOp, FileOpEvent, Origin, Outcome, Undo};
use dirfleet_core::location::Location;

use crate::fileops::OpControl;
use crate::vfs::{Vfs, VfsError};

/// How often a stop is looked for while nothing is reported.
const POLL: Duration = Duration::from_millis(50);

/// Sends what the archive crate reports over to the operation's thread.
struct Reports(Mutex<Sender<(u64, u64, String)>>);

impl ProgressObserver for Reports {
    fn on_progress(&self, done_bytes: u64, total_bytes: u64, current_file: String) {
        let _ = self.0.lock().unwrap_or_else(PoisonError::into_inner).send((
            done_bytes,
            total_bytes,
            current_file,
        ));
    }
}

const fn conflict(policy: ConflictPolicy) -> dirfleet_archive::ConflictPolicy {
    match policy {
        ConflictPolicy::Replace => dirfleet_archive::ConflictPolicy::Overwrite,
        ConflictPolicy::Skip => dirfleet_archive::ConflictPolicy::Skip,
        ConflictPolicy::KeepBoth => dirfleet_archive::ConflictPolicy::KeepBoth,
    }
}

/// What an extraction is asked to do, from [`FileOp::Extract`].
struct Job<'a> {
    archive: &'a Path,
    members: &'a [String],
    dst: &'a Path,
    policy: ConflictPolicy,
    own_folder: bool,
}

/// Extracts, reporting through `report`; `Err` says why nothing came out.
fn extract(
    vfs: &Vfs,
    id: u64,
    job: &Job<'_>,
    control: &OpControl,
    report: &dyn Fn(FileOpEvent),
) -> Result<ExtractSummary, VfsError> {
    let archive = Location::parse(&job.archive.to_string_lossy());
    let provider = vfs.open_archive(&archive)?;
    let selection = if job.members.is_empty() {
        None
    } else {
        let ids: Vec<u32> = job
            .members
            .iter()
            .filter_map(|member| provider.id_of(member))
            .collect();
        if ids.is_empty() {
            return Err(VfsError::Other(
                "What was to be extracted is no longer in the archive".to_owned(),
            ));
        }
        Some(ids)
    };
    let parent = job.dst.to_string_lossy().into_owned();
    let output_dir = if job.own_folder {
        provider.handle.plan_destination(parent).output_dir
    } else {
        parent
    };
    let request = ExtractRequest {
        output_dir,
        selection,
        password: provider.password.clone(),
        conflict: conflict(job.policy),
        restore_metadata: true,
    };
    let (reports, received) = channel();
    let observer: Arc<dyn ProgressObserver> = Arc::new(Reports(Mutex::new(reports)));
    let token = CancelToken::new();
    let result = std::thread::scope(|scope| {
        let handle = Arc::clone(&provider.handle);
        let stop = Arc::clone(&token);
        let extraction = scope.spawn(move || handle.extract(request, observer, stop));
        loop {
            match received.recv_timeout(POLL) {
                Ok((done, total, current)) => report(FileOpEvent::Progress {
                    id,
                    fraction: if total == 0 {
                        0.0
                    } else {
                        (done as f64 / total as f64).min(1.0) as f32
                    },
                    current,
                    bytes_done: done,
                    bytes_total: total,
                    bytes_per_second: 0,
                }),
                Err(RecvTimeoutError::Timeout) => {}
                // The extraction is over: its observer is gone.
                Err(RecvTimeoutError::Disconnected) => break,
            }
            if control.is_cancelled() {
                token.cancel();
            }
        }
        extraction.join()
    });
    // A thread that panicked read the archive no further.
    result
        .unwrap_or_else(|_| {
            Err(dirfleet_archive::CoreError::Corrupt {
                message: "it could not be read to its end".to_owned(),
            })
        })
        .map_err(|error| VfsError::of(&error, provider.password.is_some()))
}

/// Runs a [`FileOp::Extract`] to its end on the calling thread.
pub fn run(
    vfs: &Vfs,
    id: u64,
    op: &FileOp,
    origin: Origin,
    control: &OpControl,
    report: &dyn Fn(FileOpEvent),
) -> Outcome {
    let description = op.describe();
    report(FileOpEvent::Started {
        id,
        description: description.clone(),
    });
    let mut outcome = Outcome {
        id,
        origin,
        description,
        errors: Vec::new(),
        cancelled: false,
        undo: None,
        affected_dirs: Vec::new(),
        created: Vec::new(),
        trash_changed: false,
        verified: false,
        needs_password: None,
    };
    let FileOp::Extract {
        archive,
        members,
        dst,
        policy,
        own_folder,
    } = op
    else {
        outcome
            .errors
            .push("Only an extraction is run here".to_owned());
        return outcome;
    };
    outcome.affected_dirs.push(dst.clone());
    let job = Job {
        archive,
        members,
        dst,
        policy: *policy,
        own_folder: *own_folder,
    };
    match extract(vfs, id, &job, control, report) {
        Ok(summary) => {
            let created: Vec<PathBuf> = summary.top_level_paths.iter().map(PathBuf::from).collect();
            // Where things landed: the destination, or the folder made in it.
            let folders: BTreeSet<PathBuf> = created
                .iter()
                .filter_map(|path| path.parent().map(Path::to_path_buf))
                .chain(std::iter::once(dst.clone()))
                .collect();
            outcome.affected_dirs = folders.into_iter().collect();
            if !summary.skipped_unsafe.is_empty() {
                outcome.errors.push(format!(
                    "{} left in the archive: their names lead outside the folder",
                    count(summary.skipped_unsafe.len())
                ));
            }
            if !summary.skipped_links.is_empty() {
                outcome.errors.push(format!(
                    "{} not made: links that lead outside the folder",
                    count(summary.skipped_links.len())
                ));
            }
            outcome.undo = (!created.is_empty()).then(|| Undo::Created(created.clone()));
            outcome.created = created;
        }
        Err(VfsError::Password { wrong }) => {
            outcome.needs_password = Some(wrong);
            outcome
                .errors
                .push(VfsError::Password { wrong }.to_string());
        }
        Err(VfsError::Other(_)) if control.is_cancelled() => outcome.cancelled = true,
        Err(error) => outcome.errors.push(error.to_string()),
    }
    outcome
}

fn count(items: usize) -> String {
    if items == 1 {
        "1 item".to_owned()
    } else {
        format!("{items} items")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vfs::tests::{fixture, sample, top_of};

    fn extract_op(archive: &Path, members: &[&str], dst: &Path, own_folder: bool) -> FileOp {
        FileOp::Extract {
            archive: archive.to_path_buf(),
            members: members.iter().map(|member| (*member).to_owned()).collect(),
            dst: dst.to_path_buf(),
            policy: ConflictPolicy::KeepBoth,
            own_folder,
        }
    }

    fn run_op(vfs: &Vfs, op: &FileOp) -> Outcome {
        run(vfs, 1, op, Origin::User, &OpControl::default(), &|_| {})
    }

    #[test]
    fn a_whole_archive_comes_out_into_a_folder_of_its_own_and_can_be_undone() {
        let dir = tempfile::tempdir().expect("temp dir");
        let zip = sample(dir.path());
        let vfs = Vfs::new(dir.path().join("cache"));
        let out = dir.path().join("out");
        std::fs::create_dir(&out).expect("out");

        let outcome = run_op(&vfs, &extract_op(&zip, &[], &out, true));
        assert!(outcome.succeeded(), "{outcome:?}");
        // Two items at its top: they stay together, in a folder named after it.
        let folder = out.join("sample");
        assert_eq!(
            std::fs::read_to_string(folder.join("readme.txt")).expect("read"),
            "hello"
        );
        assert_eq!(
            std::fs::read_to_string(folder.join("docs/deep/notes.md")).expect("read"),
            "# notes"
        );
        assert!(outcome.affected_dirs.contains(&out));
        assert!(matches!(&outcome.undo, Some(Undo::Created(paths)) if !paths.is_empty()));
        assert_eq!(outcome.created.len(), 2, "{:?}", outcome.created);

        // Again: nothing is overwritten, a second folder is made.
        let again = run_op(&vfs, &extract_op(&zip, &[], &out, true));
        assert!(again.succeeded());
        assert!(out.join("sample 2").join("readme.txt").is_file());
    }

    #[test]
    fn picked_items_come_out_as_they_are() {
        let dir = tempfile::tempdir().expect("temp dir");
        let zip = sample(dir.path());
        let vfs = Vfs::new(dir.path().join("cache"));
        let out = dir.path().join("out");
        std::fs::create_dir(&out).expect("out");

        let outcome = run_op(&vfs, &extract_op(&zip, &["docs/deep"], &out, false));
        assert!(outcome.succeeded(), "{outcome:?}");
        assert!(out.join("docs/deep/notes.md").is_file());
        assert!(!out.join("readme.txt").exists(), "only what was picked");

        let gone = run_op(&vfs, &extract_op(&zip, &["nowhere.txt"], &out, false));
        assert_eq!(gone.errors.len(), 1, "{gone:?}");

        // The archive may itself be inside another.
        let nested_dir = tempfile::tempdir().expect("temp dir");
        let _ = top_of(&zip);
        let outcome = run_op(
            &vfs,
            &extract_op(&zip, &["readme.txt"], nested_dir.path(), false),
        );
        assert!(outcome.succeeded());
        assert!(nested_dir.path().join("readme.txt").is_file());
    }

    #[test]
    fn a_locked_archive_says_it_wants_its_password_and_extracts_with_it() {
        let dir = tempfile::tempdir().expect("temp dir");
        let vfs = Vfs::new(dir.path().join("cache"));
        let archive = fixture("password_aes.rar");
        let out = dir.path().join("out");
        std::fs::create_dir(&out).expect("out");
        let op = extract_op(&archive, &[], &out, true);

        let locked = run_op(&vfs, &op);
        assert_eq!(locked.needs_password, Some(false), "{locked:?}");
        assert!(!locked.succeeded());
        let file = Location::Local(archive.to_string_lossy().into_owned());
        vfs.set_password(&file, "wrong".into()).expect("kept");
        assert_eq!(run_op(&vfs, &op).needs_password, Some(true));
        // Nothing was left behind by the tries that failed.
        assert_eq!(std::fs::read_dir(&out).expect("read").count(), 0);

        vfs.set_password(&file, "password".into()).expect("kept");
        let outcome = run_op(&vfs, &op);
        assert!(outcome.succeeded(), "{outcome:?}");
        assert_eq!(outcome.needs_password, None);
        assert!(std::fs::read_dir(&out).expect("read").count() > 0);
    }

    #[test]
    fn a_stopped_extraction_leaves_nothing() {
        let dir = tempfile::tempdir().expect("temp dir");
        let zip = sample(dir.path());
        let vfs = Vfs::new(dir.path().join("cache"));
        let out = dir.path().join("out");
        std::fs::create_dir(&out).expect("out");
        let control = OpControl::default();
        control.cancel();
        let outcome = run(
            &vfs,
            1,
            &extract_op(&zip, &[], &out, true),
            Origin::User,
            &control,
            &|_| {},
        );
        // Either it was stopped in time, or it was already done: never half.
        if outcome.cancelled {
            assert_eq!(std::fs::read_dir(&out).expect("read").count(), 0);
        } else {
            assert!(outcome.succeeded(), "{outcome:?}");
        }
    }
}
