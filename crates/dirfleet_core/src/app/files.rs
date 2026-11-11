//! File operations from the core, as egui's: new folder and file (renamed
//! once they appear), rename, duplicate, trash, delete, copy and move with a
//! choice when names clash, the clipboard (cut items dimmed), and one undo
//! history. The effects crate's worker runs each operation and reports
//! progress; finished operations refresh the folders they changed.
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crux_core::Command;
use crux_core::capability::Operation;
use crux_core::render::render;
use facet::Facet;
use serde::{Deserialize, Serialize};

use super::archives::Retry;
use super::settings::QueueMode;
use super::undo::Step;
use super::{DirFleet, Effect, Event, Model, OpenHistoryOp, Tab, ToastKind, tab_id};
use crate::fileops::{
    ConflictPolicy, FileOp, FileOpEvent, Origin, Outcome, Undo, UndoStack, validate_name,
};
use crate::ids::TabId;
use crate::listing::TimestampSeconds;
use crate::location::Location;

/// Run `op`, reporting its progress, then how it went.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunFileOp {
    pub id: u64,
    pub op: FileOp,
    pub origin: Origin,
    /// Read each copied file back and compare it with its original.
    pub verify: bool,
}

/// A file operation's report. Opaque to facet: it never leaves the core.
#[derive(Clone, Debug, PartialEq, Facet)]
#[facet(opaque)]
pub struct FileOpUpdate(pub FileOpEvent);

// Progress fractions are never NaN.
impl Eq for FileOpUpdate {}

impl Operation for RunFileOp {
    type Output = FileOpUpdate;
}

/// Stop the operation `id` (crux #622: streams are stopped by message).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CancelFileOp {
    pub id: u64,
}

impl Operation for CancelFileOp {
    type Output = ();
}

/// Hold the running operation `id` where it is, or let it go on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PauseFileOp {
    pub id: u64,
    pub paused: bool,
}

impl Operation for PauseFileOp {
    type Output = ();
}

/// The names among `srcs` that already exist in `dst`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FindConflicts {
    pub srcs: Vec<PathBuf>,
    pub dst: PathBuf,
}

/// Names that clash. Opaque to facet: it never leaves the core.
#[derive(Clone, Debug, PartialEq, Eq, Facet)]
#[facet(opaque)]
pub struct Conflicts(pub Vec<String>);

impl Operation for FindConflicts {
    type Output = Conflicts;
}

/// The system clipboard's files.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClipboardOp {
    Write { paths: Vec<PathBuf>, cut: bool },
    Read,
}

/// The clipboard's files and whether they were cut, or why it failed.
/// Opaque to facet: it never leaves the core.
#[derive(Clone, Debug, PartialEq, Eq, Facet)]
#[facet(opaque)]
pub struct ClipboardDone(pub Result<Option<(Vec<PathBuf>, bool)>, String>);

impl Operation for ClipboardOp {
    type Output = ClipboardDone;
}

/// How many times the system clipboard has changed, to tell when another
/// app replaced the items that were cut.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClipboardChanges;

impl Operation for ClipboardChanges {
    type Output = u64;
}

/// Where an operation is on its way.
#[derive(Facet, Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub enum OperationState {
    /// Not started: another operation uses its drive.
    Waiting,
    Running,
    /// Held by the user, started or not.
    Paused,
}

/// An operation under way or waiting its turn, for progress UI.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct OperationView {
    pub id: u64,
    pub description: String,
    /// Done so far, in thousandths.
    pub progress: u16,
    /// The item being worked on.
    pub current: String,
    pub state: OperationState,
    /// How it stands in words: "Waiting", "Paused", or how much is copied
    /// and how fast ("1.2 GB of 4.0 GB · 85.0 MB/s"). Empty when there is
    /// nothing to add.
    pub detail: String,
    /// It can be paused and resumed (copies, moves and deletions).
    pub can_pause: bool,
}

/// Names clash where items are copied or moved: the shell asks what to do.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ConflictView {
    pub count: u32,
    /// The first clashing name.
    pub name: String,
    /// The folder they go into.
    pub folder: String,
    pub copy: bool,
}

#[derive(Facet, Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub enum ConflictChoice {
    Replace,
    Skip,
    KeepBoth,
    Cancel,
}

/// A row being renamed in place.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct RenameView {
    pub row: u32,
    pub name: String,
    pub error: Option<String>,
}

/// A rename in progress, by path so it survives the rows moving.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Renaming {
    path: PathBuf,
    error: Option<String>,
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "independent facts about an operation"
)]
struct Running {
    description: String,
    progress: u16,
    current: String,
    /// The tab that started it: new items are selected there.
    tab: Option<TabId>,
    /// Rename the new item once it shows (New folder, New file).
    rename_new: bool,
    /// What to say when it is done ("Moved 3 items to Projects").
    done: String,
    /// The items it works on, to tell whether a drive is busy.
    paths: Vec<PathBuf>,
    /// The operation itself until the worker has it: `Some` while it waits.
    waiting: Option<(FileOp, Origin)>,
    /// Held by the user.
    paused: bool,
    /// It waits its turn on its drives ([`FileOp::is_bulk`]).
    bulk: bool,
    /// It can be held where it is ([`FileOp::can_pause`]).
    pausable: bool,
    /// An extraction, kept to run again once the archive's password is given.
    retry: Option<FileOp>,
    /// The drives it reads or writes, when it is one that waits its turn.
    drives: Vec<PathBuf>,
    /// Bytes written, bytes to write and bytes a second, while it copies.
    bytes: (u64, u64, u64),
}

impl Running {
    const fn state(&self) -> OperationState {
        if self.paused {
            OperationState::Paused
        } else if self.waiting.is_some() {
            OperationState::Waiting
        } else {
            OperationState::Running
        }
    }

    fn detail(&self) -> String {
        let (done, total, rate) = self.bytes;
        match self.state() {
            OperationState::Paused => "Paused".to_owned(),
            OperationState::Waiting => "Waiting".to_owned(),
            OperationState::Running if total == 0 => String::new(),
            OperationState::Running if rate == 0 => {
                format!(
                    "{} of {}",
                    super::format::bytes(done),
                    super::format::bytes(total)
                )
            }
            OperationState::Running => format!(
                "{} of {} · {}/s",
                super::format::bytes(done),
                super::format::bytes(total),
                super::format::bytes(rate)
            ),
        }
    }
}

/// Items waiting to be copied or moved until the user picks how clashes go.
struct PendingTransfer {
    srcs: Vec<PathBuf>,
    dst: PathBuf,
    copy: bool,
    tab: TabId,
    conflicts: Vec<String>,
}

/// File operations in the model.
#[derive(Default)]
pub(super) struct Files {
    next_id: u64,
    running: BTreeMap<u64, Running>,
    undo: UndoStack<Step>,
    /// Items cut to the clipboard, dimmed until pasted.
    cut: std::collections::HashSet<PathBuf>,
    /// The clipboard's change count once they were cut.
    cut_changes: Option<u64>,
    pending: Option<PendingTransfer>,
    /// Finished operations, oldest first, at most [`ACTIVITY_KEPT`].
    activity: std::collections::VecDeque<Finished>,
}

/// A folder the selection can be copied or moved to from the Move to /
/// Copy to menus: another tab's folder, a favorite or a drive.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct TransferTarget {
    pub title: String,
    pub path: String,
    /// "Tab", "Favorite" or "Drive".
    pub detail: String,
}

/// Finished operations the Activity list keeps, as egui's.
const ACTIVITY_KEPT: usize = 30;

/// How a finished operation went.
#[derive(Facet, Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub enum ActivityState {
    Done,
    Failed,
    Stopped,
}

#[derive(Clone, Debug)]
struct Finished {
    id: u64,
    text: String,
    state: ActivityState,
    errors: Vec<String>,
    /// Unix seconds, or 0 when the time wasn't known.
    at: u64,
}

/// A finished operation in the Activity list.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ActivityView {
    pub id: u64,
    pub text: String,
    pub state: ActivityState,
    /// What went wrong, one per item.
    pub errors: Vec<String>,
    /// When, as "5 min ago" ("" when not known).
    pub when: String,
}

impl Files {
    /// Adds an operation that is over to the Activity list.
    fn record(&mut self, finished: Finished) {
        self.activity.push_back(finished);
        if self.activity.len() > ACTIVITY_KEPT {
            self.activity.pop_front();
        }
    }

    /// The Activity list, newest first.
    pub(super) fn activity(&self, now: u64) -> Vec<ActivityView> {
        self.activity
            .iter()
            .rev()
            .map(|finished| ActivityView {
                id: finished.id,
                text: finished.text.clone(),
                state: finished.state,
                errors: finished.errors.clone(),
                when: u32::try_from(finished.at)
                    .ok()
                    .filter(|&at| at > 0)
                    .map(|at| super::format::when(TimestampSeconds::from_unix(at), now))
                    .unwrap_or_default(),
            })
            .collect()
    }

    /// The running operations on the drive mounted at `mount`.
    pub(super) fn running_on(&self, mount: &Path) -> Vec<u64> {
        self.running
            .iter()
            .filter(|(_, running)| running.paths.iter().any(|path| path.starts_with(mount)))
            .map(|(&id, _)| id)
            .collect()
    }

    /// Items cut to the clipboard and not pasted yet.
    pub(super) fn cut_count(&self) -> u32 {
        u32::try_from(self.cut.len()).unwrap_or(u32::MAX)
    }

    pub(super) fn operations(&self) -> Vec<OperationView> {
        self.running
            .iter()
            .map(|(&id, running)| OperationView {
                id,
                description: running.description.clone(),
                progress: running.progress,
                current: running.current.clone(),
                state: running.state(),
                detail: running.detail(),
                can_pause: running.pausable,
            })
            .collect()
    }

    pub(super) fn undo_label(&self) -> Option<String> {
        self.undo.peek_undo().map(Step::label)
    }

    pub(super) fn redo_label(&self) -> Option<String> {
        self.undo.peek_redo().map(Step::label)
    }

    pub(super) fn conflict(&self) -> Option<ConflictView> {
        // Asked only once names are known to clash.
        let pending = self.pending.as_ref().filter(|p| !p.conflicts.is_empty())?;
        Some(ConflictView {
            count: u32::try_from(pending.conflicts.len()).unwrap_or(u32::MAX),
            name: pending.conflicts.first().cloned().unwrap_or_default(),
            folder: pending.dst.to_string_lossy().into_owned(),
            copy: pending.copy,
        })
    }

    pub(super) fn cut_is_empty(&self) -> bool {
        self.cut.is_empty()
    }

    pub(super) fn is_cut(&self, path: &Path) -> bool {
        self.cut.contains(path)
    }
}

impl Tab {
    /// The selected rows' paths, or the cursor's when nothing is selected.
    pub(super) fn targets(&self) -> Vec<PathBuf> {
        let rows: Vec<usize> = if self.selection.is_empty() {
            self.selection.cursor().into_iter().collect()
        } else {
            self.selection.rows().collect()
        };
        rows.into_iter()
            .filter_map(|row| self.row_path(row))
            .collect()
    }

    /// The rename shown in the tab, with the row it is on now.
    pub(super) fn rename_view(&self) -> Option<RenameView> {
        let renaming = self.renaming.as_ref()?;
        let row = (0..self.order.len())
            .find(|&row| self.row_path(row).as_deref() == Some(&renaming.path))?;
        Some(RenameView {
            row: u32::try_from(row).ok()?,
            name: renaming
                .path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default(),
            error: renaming.error.clone(),
        })
    }

    /// Selects the items an operation made once the listing has them, and
    /// starts renaming the first when asked.
    pub(super) fn select_new(&mut self) {
        if self.select_after.is_empty() {
            return;
        }
        let wanted: std::collections::HashSet<&PathBuf> = self.select_after.iter().collect();
        let rows: Vec<usize> = (0..self.order.len())
            .filter(|&row| {
                self.row_path(row)
                    .is_some_and(|path| wanted.contains(&path))
            })
            .collect();
        let Some(&first) = rows.first() else {
            return;
        };
        self.selection.set_rows(rows);
        self.scroll_to = Some(u32::try_from(first).unwrap_or(0));
        self.scroll_count = self.scroll_count.wrapping_add(1);
        if self.rename_after {
            self.renaming = self
                .row_path(first)
                .map(|path| Renaming { path, error: None });
        }
        self.select_after.clear();
        self.rename_after = false;
    }
}

impl DirFleet {
    /// Records a change to the saved searches, so Undo brings `before` back.
    pub(super) fn record_saved(
        model: &mut Model,
        before: Vec<crate::search::saved::SavedSearch>,
        label: String,
    ) {
        if before != model.saved.searches {
            let after = model.saved.searches.clone();
            model.files.undo.record(
                Origin::User,
                Step::SavedSearches {
                    before,
                    after,
                    label,
                },
            );
        }
    }

    /// Records a tab's search change, when its search was committed.
    pub(super) fn record_search(
        files: &mut Files,
        tab: TabId,
        change: Option<(Option<crate::search::Search>, Option<crate::search::Search>)>,
    ) {
        if let Some((before, after)) = change {
            let label = super::undo::describe(before.as_ref(), after.as_ref());
            files.undo.record(
                Origin::User,
                Step::Search {
                    tab,
                    before,
                    after,
                    label,
                },
            );
        }
    }

    /// Undoes (or redoes) a search or saved-search step: the state it leads
    /// back to is applied, and the step goes to the other stack.
    fn undo_step(model: &mut Model, step: Step, origin: Origin) -> Command<Effect, Event> {
        let undoing = origin == Origin::Undo;
        let verb = if undoing { "Undid" } else { "Redid" };
        let label = step.label();
        let command = match &step {
            Step::Search {
                tab, before, after, ..
            } => {
                let search = if undoing { before } else { after }.clone();
                let Model {
                    tabs, generation, ..
                } = model;
                tabs.get_mut(tab).map_or_else(Command::done, |state| {
                    state.restore_search(search);
                    Self::refilter(state, tab.number(), generation)
                })
            }
            Step::SavedSearches { before, after, .. } => {
                model
                    .saved
                    .searches
                    .clone_from(if undoing { before } else { after });
                Command::done()
            }
            Step::File(_) => Command::done(),
        };
        model.files.undo.record(origin, step);
        command.and(render()).and(Self::toast(
            model,
            ToastKind::Info,
            format!("{verb} {label}"),
        ))
    }

    /// Where the Move to / Copy to menus offer to send a tab's selection, as
    /// egui's: other tabs' folders, then favorites, then drives; never the
    /// tab's own folder, and each folder once.
    pub(super) fn transfer_targets(model: &Model, own: Option<&str>) -> Vec<TransferTarget> {
        let tabs = model
            .windows
            .iter()
            .flat_map(|window| window.layout.tabs())
            .filter_map(|id| {
                model
                    .tabs
                    .get(&id)
                    .filter(|tab| tab.is_local())?
                    .path
                    .clone()
            })
            .map(|path| (super::title_of(&path), path, "Tab"));
        let favorites = model
            .sidebar
            .favorites
            .iter()
            .map(|favorite| (favorite.name.clone(), favorite.path.clone(), "Favorite"));
        let drives = model.sidebar.drives.iter().map(|drive| {
            (
                drive.display_name(),
                drive.mount.to_string_lossy().into_owned(),
                "Drive",
            )
        });
        let mut targets: Vec<TransferTarget> = Vec::new();
        for (title, path, detail) in tabs.chain(favorites).chain(drives) {
            if Some(path.as_str()) != own && !targets.iter().any(|target| target.path == path) {
                targets.push(TransferTarget {
                    title,
                    path,
                    detail: detail.to_owned(),
                });
            }
        }
        targets
    }

    /// Starts `op` for `tab`, recording its inverse when it finishes.
    pub(super) fn run(
        model: &mut Model,
        op: FileOp,
        origin: Origin,
        tab: Option<TabId>,
        rename_new: bool,
    ) -> Command<Effect, Event> {
        // Nothing is written where nothing can be: inside an archive.
        let unchangeable = op.paths().into_iter().find_map(|path| {
            super::archives::why_unchangeable(&Location::parse(&path.to_string_lossy()))
        });
        if let Some(reason) = unchangeable {
            return Self::notify(model, reason.to_owned());
        }
        model.files.next_id += 1;
        let id = model.files.next_id;
        let paths: Vec<PathBuf> = op.paths().into_iter().map(Path::to_path_buf).collect();
        let bulk = op.is_bulk();
        let pausable = op.can_pause();
        let retry = matches!(op, FileOp::Extract { .. }).then(|| op.clone());
        let drives = if bulk {
            let mounts = Self::mounts(model);
            let mut drives: Vec<PathBuf> = Vec::new();
            for path in &paths {
                let drive = drive_of(path, &mounts);
                if !drives.contains(&drive) {
                    drives.push(drive);
                }
            }
            drives
        } else {
            Vec::new()
        };
        model.files.running.insert(
            id,
            Running {
                description: op.describe(),
                done: op.describe_done(),
                progress: 0,
                current: String::new(),
                tab,
                rename_new,
                paths,
                waiting: Some((op, origin)),
                paused: false,
                bulk,
                pausable,
                retry,
                drives,
                bytes: (0, 0, 0),
            },
        );
        Self::pump(model).and(render())
    }

    /// Where the drives are mounted.
    fn mounts(model: &Model) -> Vec<PathBuf> {
        model
            .sidebar
            .drives
            .iter()
            .map(|drive| drive.mount.clone())
            .collect()
    }

    /// Hands the worker every waiting operation whose turn it is. Small
    /// operations go at once. Copies, moves and deletions go by the queue
    /// setting: one at a time on each drive they use, one at a time in all,
    /// or all at once. On a drive they go in the order they were asked for;
    /// a paused one lets those behind it pass.
    pub(super) fn pump(model: &mut Model) -> Command<Effect, Event> {
        let mode = model.settings.queue_mode;
        // The drives of the operations ahead: running, or waiting first.
        let mut ahead: Vec<&Path> = Vec::new();
        let mut any_ahead = false;
        let mut start: Vec<u64> = Vec::new();
        for (&id, operation) in &model.files.running {
            if operation.paused {
                continue;
            }
            if operation.waiting.is_none() {
                if operation.bulk {
                    ahead.extend(operation.drives.iter().map(PathBuf::as_path));
                    any_ahead = true;
                }
                continue;
            }
            let free = !operation.bulk
                || match mode {
                    QueueMode::Unlimited => true,
                    QueueMode::OneAtATime => !any_ahead,
                    QueueMode::PerVolume => !operation
                        .drives
                        .iter()
                        .any(|drive| ahead.contains(&drive.as_path())),
                };
            if operation.bulk {
                ahead.extend(operation.drives.iter().map(PathBuf::as_path));
                any_ahead = true;
            }
            if free {
                start.push(id);
            }
        }
        let started: Vec<_> = start
            .into_iter()
            .map(|id| Self::start_operation(model, id))
            .collect();
        Command::all(started)
    }

    /// Hands the waiting operation `id` to the worker.
    fn start_operation(model: &mut Model, id: u64) -> Command<Effect, Event> {
        let Some((op, origin)) = model
            .files
            .running
            .get_mut(&id)
            .and_then(|operation| operation.waiting.take())
        else {
            return Command::done();
        };
        let verify = model.settings.verify_copies;
        Command::stream_from_shell(RunFileOp {
            id,
            op,
            origin,
            verify,
        })
        .then_send(move |update| Event::FileOpUpdated { id, update })
    }

    /// Stops the operation `id`: one that waits is dropped, one that runs
    /// is told to stop and says so when it has.
    pub(super) fn cancel_operation(model: &mut Model, id: u64) -> Command<Effect, Event> {
        let waits = model
            .files
            .running
            .get(&id)
            .is_some_and(|operation| operation.waiting.is_some());
        if !waits {
            return Command::notify_shell(CancelFileOp { id }).into();
        }
        if let Some(operation) = model.files.running.remove(&id) {
            model.files.record(Finished {
                id,
                text: format!("Stopped: {}", operation.description),
                state: ActivityState::Stopped,
                errors: Vec::new(),
                at: model.now,
            });
        }
        Self::pump(model).and(render())
    }

    /// Pauses or resumes the operation `id`, when it is one that can be.
    /// The worker is told only about one it already has.
    fn set_paused(model: &mut Model, id: u64, paused: bool) -> Command<Effect, Event> {
        let Some(operation) = model
            .files
            .running
            .get_mut(&id)
            .filter(|operation| operation.pausable && operation.paused != paused)
        else {
            return Command::done();
        };
        operation.paused = paused;
        if operation.waiting.is_some() {
            return Command::done();
        }
        Command::notify_shell(PauseFileOp { id, paused }).into()
    }

    /// The events that pause, resume and reorder operations.
    fn queue_event(model: &mut Model, event: &Event) -> Command<Effect, Event> {
        let told = match *event {
            Event::PauseOperation { id } => Self::set_paused(model, id, true),
            Event::ResumeOperation { id } => Self::set_paused(model, id, false),
            Event::PauseAllOperations | Event::ResumeAllOperations => {
                let paused = matches!(event, Event::PauseAllOperations);
                // Collected first: pausing needs the model.
                #[allow(clippy::needless_collect)]
                let ids: Vec<u64> = model.files.running.keys().copied().collect();
                let told: Vec<_> = ids
                    .into_iter()
                    .map(|id| Self::set_paused(model, id, paused))
                    .collect();
                Command::all(told)
            }
            Event::StartOperationNow { id } => {
                if let Some(operation) = model
                    .files
                    .running
                    .get_mut(&id)
                    .filter(|operation| operation.waiting.is_some())
                {
                    operation.paused = false;
                }
                Self::start_operation(model, id)
            }
            _ => return Command::done(),
        };
        // A pause frees a drive for those waiting; a resume joins the line.
        told.and(Self::pump(model)).and(render())
    }

    /// Copies or moves `srcs` into `dst`, asking first when names clash.
    /// Copying into the folder the items are in makes copies beside them.
    fn transfer(
        model: &mut Model,
        tab: TabId,
        srcs: Vec<PathBuf>,
        dst: PathBuf,
        copy: bool,
    ) -> Command<Effect, Event> {
        let unchangeable =
            super::archives::why_unchangeable(&Location::parse(&dst.to_string_lossy()));
        if let Some(reason) = unchangeable {
            return Self::notify(model, reason.to_owned());
        }
        let srcs: Vec<PathBuf> = srcs
            .into_iter()
            // A folder never goes into itself.
            .filter(|src| !dst.starts_with(src))
            .collect();
        if srcs.is_empty() {
            return Command::done();
        }
        if srcs.iter().all(|src| src.parent() == Some(dst.as_path())) {
            if copy {
                return Self::run(
                    model,
                    FileOp::Duplicate(srcs),
                    Origin::User,
                    Some(tab),
                    false,
                );
            }
            return Command::done();
        }
        let check = FindConflicts {
            srcs: srcs.clone(),
            dst: dst.clone(),
        };
        model.files.pending = Some(PendingTransfer {
            srcs,
            dst,
            copy,
            tab,
            conflicts: Vec::new(),
        });
        Command::request_from_shell(check)
            .then_send(|conflicts| Event::ConflictsFound { conflicts })
    }

    fn start_pending(model: &mut Model, policy: ConflictPolicy) -> Command<Effect, Event> {
        let Some(pending) = model.files.pending.take() else {
            return render();
        };
        let op = if pending.copy {
            FileOp::Copy {
                srcs: pending.srcs,
                dst: pending.dst,
                policy,
            }
        } else {
            FileOp::Move {
                srcs: pending.srcs,
                dst: pending.dst,
                policy,
            }
        };
        Self::run(model, op, Origin::User, Some(pending.tab), false)
    }

    /// The events of file operations, renames and the clipboard.
    #[allow(clippy::too_many_lines)]
    pub(super) fn file_event(model: &mut Model, event: Event) -> Command<Effect, Event> {
        if matches!(
            event,
            Event::StartBatchRename { .. }
                | Event::SetBatchRename { .. }
                | Event::ApplyBatchRename { .. }
                | Event::CancelBatchRename { .. }
        ) {
            return Self::batch_event(model, event);
        }
        let tab_of = |tab: u32| tab_id(tab).filter(|id| model.tabs.contains_key(id));
        match event {
            Event::NewFolder { tab } | Event::NewTextFile { tab } => {
                let Some(id) = tab_of(tab) else {
                    return Command::done();
                };
                let Some(parent) = model.tabs[&id].path.clone().map(PathBuf::from) else {
                    return Command::done();
                };
                let op = if matches!(event, Event::NewFolder { .. }) {
                    FileOp::NewFolder { parent }
                } else {
                    FileOp::NewFile { parent }
                };
                Self::run(model, op, Origin::User, Some(id), true)
            }
            Event::StartRename { tab } => {
                let Some(state) = tab_id(tab).and_then(|id| model.tabs.get_mut(&id)) else {
                    return Command::done();
                };
                // Several selected: batch rename. Nothing renames in the Trash.
                if state.trash {
                    return Command::done();
                }
                if state.selection.rows().nth(1).is_some() {
                    state.batch = state.start_batch();
                    return render();
                }
                let target = state
                    .selection
                    .cursor()
                    .or_else(|| state.selection.rows().next())
                    .and_then(|row| state.row_path(row));
                state.renaming = target.map(|path| Renaming { path, error: None });
                render()
            }
            Event::CancelRename { tab } => {
                if let Some(state) = tab_id(tab).and_then(|id| model.tabs.get_mut(&id)) {
                    state.renaming = None;
                }
                render()
            }
            Event::Rename { tab, name } => {
                let Some(id) = tab_of(tab) else {
                    return Command::done();
                };
                let Some(state) = model.tabs.get_mut(&id) else {
                    return Command::done();
                };
                let Some(renaming) = state.renaming.as_mut() else {
                    return Command::done();
                };
                if let Err(error) = validate_name(&name) {
                    renaming.error = Some(error.to_owned());
                    return render();
                }
                let from = renaming.path.clone();
                state.renaming = None;
                if from.file_name().is_some_and(|old| old == name.as_str()) {
                    return render();
                }
                let to = from.with_file_name(&name);
                state.select_after = vec![to.clone()];
                Self::run(
                    model,
                    FileOp::Rename { from, to },
                    Origin::User,
                    Some(id),
                    false,
                )
            }
            Event::Duplicate { tab }
            | Event::TrashSelection { tab }
            | Event::DeleteSelection { tab } => {
                let Some(id) = tab_of(tab) else {
                    return Command::done();
                };
                let paths = model.tabs[&id].targets();
                if paths.is_empty() || model.tabs[&id].trash {
                    return Command::done();
                }
                let op = match event {
                    Event::Duplicate { .. } => FileOp::Duplicate(paths),
                    Event::TrashSelection { .. } => FileOp::Trash(paths),
                    _ => FileOp::Delete(paths),
                };
                Self::run(model, op, Origin::User, Some(id), false)
            }
            Event::CopyTo { tab, folder } => Self::send_selection(model, tab, folder, true),
            Event::MoveTo { tab, folder } => Self::send_selection(model, tab, folder, false),
            Event::PasteInto { tab, folder } => Command::request_from_shell(ClipboardOp::Read)
                .then_send(move |done| Event::PastedInto { tab, folder, done }),
            Event::PastedInto { tab, folder, done } => match done.0 {
                Err(error) => Self::notify(model, error),
                Ok(None) => Command::done(),
                Ok(Some((paths, cut))) => {
                    let Some(id) = tab_of(tab) else {
                        return Command::done();
                    };
                    if cut {
                        model.files.cut.clear();
                    }
                    Self::transfer(model, id, paths, PathBuf::from(folder), !cut)
                }
            },
            Event::ConflictsFound { conflicts } => {
                let clash = !conflicts.0.is_empty();
                match model.files.pending.as_mut() {
                    Some(pending) if clash => {
                        pending.conflicts = conflicts.0;
                        render()
                    }
                    Some(_) => Self::start_pending(model, ConflictPolicy::KeepBoth),
                    None => Command::done(),
                }
            }
            Event::ResolveConflict { choice } => {
                let policy = match choice {
                    ConflictChoice::Replace => ConflictPolicy::Replace,
                    ConflictChoice::Skip => ConflictPolicy::Skip,
                    ConflictChoice::KeepBoth => ConflictPolicy::KeepBoth,
                    ConflictChoice::Cancel => {
                        model.files.pending = None;
                        return render();
                    }
                };
                Self::start_pending(model, policy)
            }
            Event::Undo | Event::Redo => {
                let (undo, origin) = if matches!(event, Event::Undo) {
                    (model.files.undo.pop_undo(), Origin::Undo)
                } else {
                    (model.files.undo.pop_redo(), Origin::Redo)
                };
                match undo {
                    None => Command::done(),
                    Some(Step::File(undo)) => {
                        Self::run(model, FileOp::Undo(undo), origin, None, false)
                    }
                    Some(step) => Self::undo_step(model, step, origin),
                }
            }
            Event::CancelOperation { id } => Self::cancel_operation(model, id),
            Event::PauseOperation { .. }
            | Event::ResumeOperation { .. }
            | Event::PauseAllOperations
            | Event::ResumeAllOperations
            | Event::StartOperationNow { .. } => Self::queue_event(model, &event),
            Event::CopySelection { tab } | Event::CutSelection { tab } => {
                let Some(id) = tab_of(tab) else {
                    return Command::done();
                };
                let paths = model.tabs[&id].targets();
                // Items leave the Trash only by being put back.
                if paths.is_empty() || model.tabs[&id].trash {
                    return Command::done();
                }
                let cut = matches!(event, Event::CutSelection { .. });
                model.files.cut = if cut {
                    paths.iter().cloned().collect()
                } else {
                    std::collections::HashSet::new()
                };
                model.files.cut_changes = None;
                Command::request_from_shell(ClipboardOp::Write { paths, cut })
                    .then_send(move |done| Event::ClipboardDone { tab, done })
                    .and(render())
            }
            Event::Paste { tab } => Command::request_from_shell(ClipboardOp::Read)
                .then_send(move |done| Event::ClipboardDone { tab, done }),
            Event::ClipboardDone { tab, done } => match done.0 {
                Err(error) => Self::notify(model, error),
                // A write went through: note the clipboard's count for a cut.
                Ok(None) if model.files.cut.is_empty() => Command::done(),
                Ok(None) => Command::request_from_shell(ClipboardChanges)
                    .then_send(|count| Event::ClipboardCounted { count, cut: true }),
                Ok(Some((paths, cut))) => {
                    let Some(id) = tab_of(tab) else {
                        return Command::done();
                    };
                    let Some(dst) = model.tabs[&id].path.clone().map(PathBuf::from) else {
                        return Command::done();
                    };
                    if cut {
                        model.files.cut.clear();
                    }
                    Self::transfer(model, id, paths, dst, !cut)
                }
            },
            Event::ClipboardCounted { count, cut } => {
                let files = &mut model.files;
                if cut {
                    files.cut_changes = Some(count);
                    return Command::done();
                }
                if files.cut_changes.is_none_or(|then| then == count) {
                    return Command::done();
                }
                files.cut.clear();
                files.cut_changes = None;
                render()
            }
            Event::FileOpUpdated { id, update } => Self::file_op_updated(model, id, update.0),
            Event::DropItems {
                tab,
                paths,
                target,
                copy,
                force_move,
            } => Self::drop_items(model, tab, paths, target, copy, force_move),
            Event::DropOnTrash { tab, paths } => Self::drop_on_trash(model, tab, paths),
            _ => Command::done(),
        }
    }

    /// Copies or moves the tab's selection into `folder`.
    fn send_selection(
        model: &mut Model,
        tab: u32,
        folder: String,
        copy: bool,
    ) -> Command<Effect, Event> {
        let Some(id) = tab_id(tab).filter(|id| model.tabs.contains_key(id)) else {
            return Command::done();
        };
        // Items leave the Trash only by being put back.
        if model.tabs[&id].trash {
            return Command::done();
        }
        let srcs = model.tabs[&id].targets();
        Self::transfer(model, id, srcs, PathBuf::from(folder), copy)
    }

    /// Un-dims cut items once another app replaced them on the clipboard,
    /// as egui does; checked when a window comes back to the front.
    pub(super) fn check_cut(model: &Model) -> Command<Effect, Event> {
        if model.files.cut.is_empty() || model.files.cut_changes.is_none() {
            return Command::done();
        }
        Command::request_from_shell(ClipboardChanges)
            .then_send(|count| Event::ClipboardCounted { count, cut: false })
    }

    pub(super) fn notify(model: &mut Model, text: String) -> Command<Effect, Event> {
        Self::toast(model, ToastKind::Error, text)
    }

    fn file_op_updated(model: &mut Model, id: u64, event: FileOpEvent) -> Command<Effect, Event> {
        match event {
            FileOpEvent::Started { description, .. } => {
                if let Some(running) = model.files.running.get_mut(&id) {
                    running.description = description;
                }
                render()
            }
            FileOpEvent::Progress {
                fraction,
                current,
                bytes_done,
                bytes_total,
                bytes_per_second,
                ..
            } => {
                if let Some(running) = model.files.running.get_mut(&id) {
                    running.progress = (fraction.clamp(0.0, 1.0) * 1000.0).round() as u16;
                    running.current = current;
                    running.bytes = (bytes_done, bytes_total, bytes_per_second);
                }
                render()
            }
            FileOpEvent::Finished(outcome) => Self::finished(model, id, &outcome),
        }
    }

    /// An extraction that stopped because its archive wants a password:
    /// the password is asked for, and the extraction runs again once it is
    /// given. Says whether it asked.
    fn ask_if_locked(model: &mut Model, running: Option<&mut Running>, outcome: &Outcome) -> bool {
        let (Some(wrong), Some(running)) = (outcome.needs_password, running) else {
            return false;
        };
        let Some(op) = running.retry.take() else {
            return false;
        };
        let tab = running.tab;
        Self::ask_archive_password(model, wrong, Retry::Extract { op, tab })
    }

    /// Records the inverse, says what failed, selects what was made and
    /// reads the changed folders again.
    fn finished(model: &mut Model, id: u64, outcome: &Outcome) -> Command<Effect, Event> {
        let mut running = model.files.running.remove(&id);
        let (state, text) = if !outcome.errors.is_empty() {
            (ActivityState::Failed, outcome.description.clone())
        } else if outcome.cancelled {
            (
                ActivityState::Stopped,
                format!("Stopped: {}", outcome.description),
            )
        } else {
            let done = running
                .as_ref()
                .map_or_else(|| outcome.description.clone(), |r| r.done.clone());
            (
                ActivityState::Done,
                if outcome.verified {
                    format!("{done} (verified)")
                } else {
                    done
                },
            )
        };
        model.files.record(Finished {
            id,
            text,
            state,
            errors: outcome.errors.clone(),
            at: model.now,
        });
        if let Some(undo) = outcome.undo.clone() {
            model.files.undo.record(outcome.origin, Step::File(undo));
        }
        let asked = Self::ask_if_locked(model, running.as_mut(), outcome);
        let toast = if asked {
            Command::done()
        } else if !outcome.errors.is_empty() {
            let more = outcome.errors.len() - 1;
            let mut text = outcome.errors[0].clone();
            if more > 0 {
                let _ = write!(text, " (and {more} more)");
            }
            Self::toast(model, ToastKind::Error, text)
        } else if outcome.cancelled {
            Self::toast(
                model,
                ToastKind::Info,
                format!("Stopped: {}", outcome.description),
            )
        } else {
            // New items are renamed next: that says enough.
            running
                .as_ref()
                .filter(|running| !running.rename_new)
                .map_or_else(Command::done, |running| {
                    let text = if outcome.verified {
                        format!("{} (verified)", running.done)
                    } else {
                        running.done.clone()
                    };
                    Self::toast(model, ToastKind::Success, text)
                })
        };
        if let Some(running) = &running
            && let Some(state) = running.tab.and_then(|tab| model.tabs.get_mut(&tab))
            && !outcome.created.is_empty()
        {
            state.select_after.clone_from(&outcome.created);
            state.rename_after = running.rename_new;
        }
        let affected: Vec<String> = outcome
            .affected_dirs
            .iter()
            .map(|dir| dir.to_string_lossy().into_owned())
            .collect();
        // Collected first: refreshing needs the model.
        #[allow(clippy::needless_collect)]
        let refresh: Vec<u32> = model
            .tabs
            .iter()
            .filter(|(_, tab)| {
                (outcome.trash_changed && tab.trash)
                    || tab
                        .path
                        .as_ref()
                        .is_some_and(|path| affected.contains(path) || tab.shows_folders())
            })
            .map(|(id, _)| id.number())
            .collect();
        let refreshes: Vec<_> = refresh
            .into_iter()
            .map(|tab| Self::refresh(model, tab))
            .collect();
        let told = Self::tell_plugins_file_op(model, outcome);
        // Its drives are free for whatever waited behind it.
        let next = Self::pump(model);
        render()
            .and(toast)
            .and(told)
            .and(Self::follow_renames(outcome))
            .and(Command::all(refreshes))
            .and(next)
    }

    /// Plugins subscribed to `file_op_finished` hear how it went, as egui's.
    fn tell_plugins_file_op(model: &Model, outcome: &Outcome) -> Command<Effect, Event> {
        let text = |paths: &[PathBuf]| -> Vec<String> {
            paths
                .iter()
                .map(|path| path.to_string_lossy().into_owned())
                .collect()
        };
        Self::emit_plugin_event(
            model,
            "file_op_finished",
            &crate::plugins::CallContext::default(),
            &serde_json::json!({
                "description": outcome.description,
                "errors": outcome.errors,
                "cancelled": outcome.cancelled,
                "affected_dirs": text(&outcome.affected_dirs),
                "created": text(&outcome.created),
            }),
        )
    }

    /// Open counts and chosen thumbnails follow renamed and moved items, as
    /// egui's do; before the folders are read again.
    fn follow_renames(outcome: &Outcome) -> Command<Effect, Event> {
        let text = |path: &Path| path.to_string_lossy().into_owned();
        let renamed: Vec<(String, String)> = match &outcome.undo {
            Some(Undo::Rename { from, to }) => vec![(text(from), text(to))],
            Some(Undo::Moves(pairs)) => pairs
                .iter()
                .map(|(now, was)| (text(was), text(now)))
                .collect(),
            _ => return Command::done(),
        };
        Self::history(OpenHistoryOp::Rename(renamed.clone())).and(Self::rename_choices(renamed))
    }
}

/// The mount point `path` is on: the longest of `mounts` it is under, or its
/// first component when no drive is known.
fn drive_of(path: &Path, mounts: &[PathBuf]) -> PathBuf {
    mounts
        .iter()
        .filter(|mount| path.starts_with(mount))
        .max_by_key(|mount| mount.as_os_str().len())
        .cloned()
        .unwrap_or_else(|| path.components().take(1).collect())
}

impl DirFleet {
    /// Items dropped on `target` (a folder or an app; the tab's folder when
    /// `None`). An app opens them; otherwise they move within a drive and are
    /// copied to another, unless `copy` or `force_move` says (the shell maps
    /// its keys: ⌥ and ⌘ on the Mac). Drops into an item's own folder, or a
    /// folder into itself, do nothing.
    pub(super) fn drop_items(
        model: &mut Model,
        tab: u32,
        paths: Vec<String>,
        target: Option<String>,
        copy: bool,
        force_move: bool,
    ) -> Command<Effect, Event> {
        let Some(id) = tab_id(tab).filter(|id| model.tabs.contains_key(id)) else {
            return Command::done();
        };
        let Some(dst) = target
            .or_else(|| model.tabs[&id].path.clone())
            .map(PathBuf::from)
        else {
            return Command::done();
        };
        if dst
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("app"))
        {
            return Command::request_from_shell(super::SystemOp::OpenWith {
                app: dst.to_string_lossy().into_owned(),
                paths,
            })
            .then_send(|done| Event::SystemDone { done });
        }
        let srcs: Vec<PathBuf> = paths
            .into_iter()
            .map(PathBuf::from)
            .filter(|src| src.parent() != Some(dst.as_path()) && !dst.starts_with(src))
            .collect();
        if srcs.is_empty() {
            return Command::done();
        }
        let mounts: Vec<PathBuf> = model
            .sidebar
            .drives
            .iter()
            .map(|drive| drive.mount.clone())
            .collect();
        let same_drive = srcs
            .iter()
            .all(|src| drive_of(src, &mounts) == drive_of(&dst, &mounts));
        let moves = force_move || (!copy && same_drive);
        Self::transfer(model, id, srcs, dst, !moves)
    }

    /// Items dropped on the Trash go there.
    pub(super) fn drop_on_trash(
        model: &mut Model,
        tab: u32,
        paths: Vec<String>,
    ) -> Command<Effect, Event> {
        if paths.is_empty() {
            return Command::done();
        }
        let id = tab_id(tab).filter(|id| model.tabs.contains_key(id));
        let paths = paths.into_iter().map(PathBuf::from).collect();
        Self::run(model, FileOp::Trash(paths), Origin::User, id, false)
    }
}
