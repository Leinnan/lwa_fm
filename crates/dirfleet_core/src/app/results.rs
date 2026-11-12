//! Tabs that show what a scan found rather than a folder: the files that
//! hold the same bytes as another (duplicates), told of here, and what
//! differs between two folders (`compare`).
//!
//! A results tab has no folder. Its listing is made by the scan, each entry
//! with the folder it is in, so its rows are selected, previewed, opened and
//! deleted like a search's. The rows are kept set by set, each set under a
//! heading; within a set the tab's sort holds. Nothing is read again when a
//! file operation ends: the files it may have touched are checked for still
//! being there, and a set left with one file is no longer shown.
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::sync::{Arc, Mutex, PoisonError};

use crux_core::Command;
use crux_core::capability::Operation;
use crux_core::command::AbortHandle;
use crux_core::render::render;
use facet::Facet;
use serde::{Deserialize, Serialize};

use super::compare::{COMPARE_VIEW, Compare, CompareView, StopCompare};
use super::{DirFleet, Effect, Event, ListDirOutput, ListView, Model, Tab, format, tab_id};
use crate::DisplayType;
use crate::ids::TabId;
use crate::listing::group::GroupSpan;
use crate::listing::{EntryType, Listing};
use crate::location::Location;

/// The key the view of duplicates is kept under among folder views.
pub(super) const RESULTS_VIEW: &str = ":duplicates";
/// The key the view of a tag's items is kept under.
const TAGS_VIEW: &str = ":tags";

/// How many sets get a heading of their own; the rest share a last one.
pub const SETS_HEADED: usize = 2000;

/// Find the files under `roots` that hold the same bytes as another, saying
/// how far the scan is, then what it found. Stopped by [`StopDuplicates`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FindDuplicates {
    pub id: u64,
    pub roots: Vec<String>,
    /// How the files found are ordered; hidden files count only when shown.
    pub view: ListView,
}

impl Operation for FindDuplicates {
    type Output = DuplicatesUpdate;
}

/// How far a scan for duplicates is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct ScanProgress {
    /// Files and folders found so far.
    pub found: u64,
    /// Files that share their size with another, once the folders are read.
    pub candidates: u64,
    /// Bytes read of the files that are read whole, and how many there are
    /// to read, once that is known.
    pub compared: u64,
    pub to_compare: u64,
}

/// What a scan for duplicates found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DuplicateSets {
    /// The files, as a listing whose entries know their folders.
    pub output: ListDirOutput,
    /// The set each entry of the listing is in; the set that wastes the
    /// most space is 0.
    pub set_of: Arc<Vec<u32>>,
    /// The size of one file of each set.
    pub sizes: Arc<Vec<u64>>,
    /// How many files could not be read, and so were left out.
    pub unread: u32,
    /// How many files were looked at.
    pub files: u64,
}

/// Opaque to facet: it never leaves the core.
#[derive(Clone, Debug, PartialEq, Eq, Facet)]
#[facet(opaque)]
#[repr(C)]
pub enum DuplicatesUpdate {
    Progress(ScanProgress),
    /// The scan is done.
    Found(DuplicateSets),
}

/// Stop the scan `id`, telling the handler too (crux #622).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StopDuplicates {
    pub id: u64,
}

impl Operation for StopDuplicates {
    type Output = ();
}

/// Whether each of `paths` is still there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StillThere {
    pub paths: Vec<String>,
}

impl Operation for StillThere {
    type Output = Vec<bool>;
}

/// Which file of each set of duplicates stays unselected.
#[derive(Facet, Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub enum Keep {
    /// The one changed last.
    Newest,
    /// The one changed first.
    Oldest,
    /// The one shown first in its set, so the tab's sort decides.
    First,
}

/// A results tab's bar: what was looked for, how far the scan is, and what
/// it found.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
pub struct ResultsView {
    /// "Duplicates in Downloads".
    pub title: String,
    /// The scan is running; `StopResults` stops it.
    pub running: bool,
    /// How far the scan is, while it runs.
    pub progress: String,
    /// What the scan found, once it is done.
    pub summary: String,
    /// How many sets of duplicates are left.
    pub sets: u32,
    /// How many sets have every one of their files selected: deleting the
    /// selection would leave no copy of those, so shells ask first.
    pub whole_sets: u32,
    /// The tab compares two folders rather than looking for duplicates.
    pub compare: Option<CompareView>,
    /// The tab shows the items that have a tag: nothing is scanned.
    pub tag: Option<super::TagPlaceView>,
}

/// What a results tab shows besides its listing.
#[derive(Default)]
pub(super) struct Results {
    /// The folders scanned; when they are compared, the left one and the
    /// right one.
    pub(super) roots: Vec<String>,
    /// The running scan: its id and the handle stopping its stream.
    pub(super) job: Option<(u64, AbortHandle)>,
    pub(super) progress: ScanProgress,
    /// The last scan was stopped before it was done.
    pub(super) stopped: bool,
    /// A scan was done, so there is something to say about it.
    pub(super) done: bool,
    /// The folders are compared, not looked through for duplicates.
    pub(super) compare: Option<Compare>,
    /// A tag's items are shown: there are no folders, and nothing is
    /// scanned.
    pub(super) tag: Option<super::tags::TagPlace>,
    set_of: Arc<Vec<u32>>,
    sizes: Arc<Vec<u64>>,
    /// Entries of the listing found gone since the scan.
    gone: HashSet<u32>,
    /// How many files of each set are still there.
    live: Vec<u32>,
    pub(super) unread: u32,
    files: u64,
    /// How many sets are wholly selected, for the selection it was counted
    /// for.
    whole: Mutex<WholeSets>,
}

/// How many sets are wholly selected, and the selection that was counted.
type WholeSets = Option<(super::selection::Stamp, u32)>;

impl Results {
    /// Puts `order` set by set, the set that wastes the most first, keeping
    /// the order within each set; files that are gone, and sets left with
    /// one file, drop out. Returns each set's heading.
    pub(super) fn arrange(&mut self, listing: &Listing, order: &mut Vec<u32>) -> Vec<GroupSpan> {
        // What differs between two folders is kept kind by kind.
        if let Some(compare) = &mut self.compare {
            return compare.arrange(listing, order);
        }
        // Until the scan's own listing comes there are no sets.
        if self.set_of.len() != listing.len() {
            self.live.clear();
            return Vec::new();
        }
        let sets = self.sizes.len();
        let set_at = |entry: u32| self.set_of.get(entry as usize).map(|&set| set as usize);
        let mut live = vec![0_u32; sets];
        for entry in 0..self.set_of.len() as u32 {
            if !self.gone.contains(&entry)
                && let Some(count) = set_at(entry).and_then(|set| live.get_mut(set))
            {
                *count += 1;
            }
        }
        order.retain(|&entry| {
            !self.gone.contains(&entry)
                && set_at(entry)
                    .and_then(|set| live.get(set))
                    .is_some_and(|&count| count >= 2)
        });
        // Each set's rows together, by counting: the order within stays.
        let mut counts = vec![0_u32; sets];
        for &entry in order.iter() {
            if let Some(set) = set_at(entry) {
                counts[set] += 1;
            }
        }
        let mut starts = Vec::with_capacity(sets);
        let mut at = 0_u32;
        for &count in &counts {
            starts.push(at);
            at += count;
        }
        let mut next = starts.clone();
        let mut placed = vec![0_u32; order.len()];
        for &entry in order.iter() {
            if let Some(set) = set_at(entry) {
                placed[next[set] as usize] = entry;
                next[set] += 1;
            }
        }
        *order = placed;
        let mut groups = Vec::new();
        for (set, &count) in counts.iter().enumerate() {
            if count == 0 {
                continue;
            }
            if groups.len() == SETS_HEADED {
                groups.push(GroupSpan {
                    label: "More duplicates".to_owned(),
                    first: starts[set],
                    len: at - starts[set],
                });
                break;
            }
            groups.push(GroupSpan {
                label: format!("{} each", format::bytes(self.sizes[set])),
                first: starts[set],
                len: count,
            });
        }
        self.live = live;
        groups
    }

    /// What a tab shows while `roots` are scanned: compared, or looked
    /// through for duplicates.
    pub(super) fn of(roots: Vec<String>, compare: Option<Compare>) -> Self {
        Self {
            roots,
            compare,
            ..Self::default()
        }
    }

    /// The key the view of such results is kept under among folder views.
    pub(super) const fn view_key(&self) -> &'static str {
        if self.compare.is_some() {
            COMPARE_VIEW
        } else if self.tag.is_some() {
            TAGS_VIEW
        } else {
            RESULTS_VIEW
        }
    }

    /// Whether these are sets of duplicates, which rows are kept by.
    pub(super) const fn is_duplicates(&self) -> bool {
        self.compare.is_none() && self.tag.is_none()
    }

    /// Whether the rows are kept in an order of the results' own: sets, or
    /// kinds of difference. A tag's items are grouped as a folder's.
    pub(super) const fn arranges(&self) -> bool {
        self.tag.is_none()
    }

    /// How many sets of two files or more are left.
    fn sets_left(&self) -> usize {
        self.live.iter().filter(|&&count| count >= 2).count()
    }

    fn progress_text(&self) -> String {
        if self.job.is_none() {
            return String::new();
        }
        let progress = self.progress;
        if progress.candidates == 0 {
            return format!("Reading folders… {} items", progress.found);
        }
        // Until it is known how much there is to read, only how many files.
        let read = progress.compared.min(progress.to_compare);
        read.saturating_mul(100)
            .checked_div(progress.to_compare)
            .map_or_else(
                || format!("Comparing {} files…", progress.candidates),
                |percent| {
                    format!(
                        "Comparing files… {percent}% of {}",
                        format::bytes(progress.to_compare)
                    )
                },
            )
    }

    /// The sets of duplicates left, in a line.
    fn sets_text(&self) -> String {
        let sets = self.sets_left();
        if sets == 0 {
            return format!("No duplicates among {} files", self.files);
        }
        let (files, extra) = self
            .live
            .iter()
            .zip(self.sizes.iter())
            .filter(|(count, _)| **count >= 2)
            .fold((0_u64, 0_u64), |(files, extra), (&count, &size)| {
                (
                    files + u64::from(count),
                    extra.saturating_add(size.saturating_mul(u64::from(count) - 1)),
                )
            });
        format!(
            "{sets} set{} of duplicates, {files} files. The extra copies take {}",
            if sets == 1 { "" } else { "s" },
            format::bytes(extra)
        )
    }

    fn summary(&self) -> String {
        if self.job.is_some() {
            return String::new();
        }
        if !self.done {
            return if self.stopped {
                "Stopped before the scan was done".to_owned()
            } else {
                String::new()
            };
        }
        let mut text = self
            .compare
            .as_ref()
            .map_or_else(|| self.sets_text(), Compare::found_text);
        if self.unread > 0 {
            let _ = write!(
                text,
                ". {} file{} could not be read",
                self.unread,
                if self.unread == 1 { "" } else { "s" }
            );
        }
        if self.stopped {
            text.push_str(". The new scan was stopped");
        }
        text
    }
}

/// `dir` written one way, to tell whether two names mean the same folder.
pub(super) fn same_form(dir: &str) -> String {
    dir.replace('\\', "/").trim_end_matches('/').to_lowercase()
}

impl Tab {
    /// Stops the running scan, telling the handler too (crux #622).
    pub(super) fn stop_scan(&mut self) -> Command<Effect, Event> {
        let Some(results) = self.results.as_mut() else {
            return Command::done();
        };
        match results.job.take() {
            Some((id, handle)) => {
                handle.abort();
                if results.compare.is_some() {
                    Command::notify_shell(StopCompare { id }).into()
                } else {
                    Command::notify_shell(StopDuplicates { id }).into()
                }
            }
            None => Command::done(),
        }
    }

    /// The tab goes on to show a folder: its scan stops and its results go.
    pub(super) fn leave_results(&mut self) -> Command<Effect, Event> {
        let stop = self.stop_scan();
        self.results = None;
        stop
    }

    /// What the tab is called while it shows results.
    pub(super) fn results_title(&self) -> Option<String> {
        let results = self.results.as_ref()?;
        if let Some(compare) = &results.compare {
            return Some(compare.title());
        }
        if let Some(place) = &results.tag {
            return Some(place.name.clone());
        }
        Some(match results.roots.as_slice() {
            [root] => format!("Duplicates in {}", Location::parse(root).title()),
            roots => format!("Duplicates in {} folders", roots.len()),
        })
    }

    /// What the tab is busy with, for its header: how far a search through
    /// subfolders or a scan is.
    pub(super) fn busy_text(&self, results: Option<&ResultsView>) -> String {
        if self.walk.is_some() {
            let folders = self.listing.dirs().len();
            return format!(
                "Scanning {folders} folder{}…",
                if folders == 1 { "" } else { "s" }
            );
        }
        results
            .map(|results| results.progress.clone())
            .unwrap_or_default()
    }

    pub(super) fn results_view(&self) -> Option<ResultsView> {
        let results = self.results.as_ref()?;
        Some(ResultsView {
            title: self.results_title().unwrap_or_default(),
            running: results.job.is_some(),
            progress: results.progress_text(),
            summary: results.summary(),
            sets: u32::try_from(results.sets_left()).unwrap_or(u32::MAX),
            whole_sets: self.whole_sets_selected(),
            compare: self.compare_view(),
            // Filled in by whoever knows the tags.
            tag: None,
        })
    }

    /// How many sets have every file they have left selected. Counted once
    /// for each selection.
    fn whole_sets_selected(&self) -> u32 {
        let Some(results) = &self.results else {
            return 0;
        };
        // A set has two files at least.
        if self.selection.len() < 2 {
            return 0;
        }
        let stamp = self.selection_stamp();
        let mut whole = results.whole.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((at, count)) = *whole
            && at == stamp
        {
            return count;
        }
        let mut selected: HashMap<u32, u32> = HashMap::new();
        for row in self.selection.rows() {
            if let Some(&set) = self
                .order
                .get(row)
                .and_then(|&entry| results.set_of.get(entry as usize))
            {
                *selected.entry(set).or_default() += 1;
            }
        }
        let count = selected
            .iter()
            .filter(|&(&set, count)| results.live.get(set as usize) == Some(count))
            .count();
        let count = u32::try_from(count).unwrap_or(u32::MAX);
        *whole = Some((stamp, count));
        count
    }

    /// Selects every shown file of each set but the one `keep` names. A set
    /// showing one file keeps it.
    fn select_copies(&mut self, keep: Keep) {
        let Some(results) = &self.results else {
            return;
        };
        let set_at = |row: usize| {
            self.order
                .get(row)
                .and_then(|&entry| results.set_of.get(entry as usize))
                .copied()
        };
        let modified = |row: usize| {
            self.order
                .get(row)
                .map_or(0, |&entry| *self.listing.meta(entry as usize).modified_at)
        };
        let mut selected = Vec::new();
        let mut row = 0;
        while row < self.order.len() {
            let set = set_at(row);
            let mut end = row + 1;
            while end < self.order.len() && set_at(end) == set {
                end += 1;
            }
            // Of files changed at the same time, the one shown first stays.
            let kept = match keep {
                Keep::First => Some(row),
                Keep::Newest => (row..end).rev().max_by_key(|&row| modified(row)),
                Keep::Oldest => (row..end).min_by_key(|&row| modified(row)),
            };
            if end - row >= 2 {
                selected.extend((row..end).filter(|&row| Some(row) != kept));
            }
            row = end;
        }
        self.selection.set_rows(selected);
    }

    /// Asks whether the tab's files in the folders `affected` are still
    /// there, when it shows results and a file operation ended.
    pub(super) fn check_results(
        &self,
        number: u32,
        affected: &[String],
    ) -> Option<Command<Effect, Event>> {
        let results = self.results.as_ref()?;
        // Compared folders are compared again where they were touched.
        if results.compare.is_some() {
            return self.check_compare(number, affected);
        }
        // A tag's items are listed again instead.
        if results.tag.is_some() {
            return None;
        }
        if results.job.is_some() || affected.is_empty() || self.listing.is_empty() {
            return None;
        }
        let affected: HashSet<String> = affected.iter().map(|dir| same_form(dir)).collect();
        let touched: Vec<bool> = self
            .listing
            .dirs()
            .iter()
            .map(|dir| affected.contains(&same_form(dir)))
            .collect();
        // Files found gone are asked about too: Undo may have put them back.
        let entries: Vec<u32> = (0..self.listing.len())
            .filter(|&entry| touched.get(self.listing.dir_index(entry)) == Some(&true))
            .map(|entry| entry as u32)
            .collect();
        if entries.is_empty() {
            return None;
        }
        let paths = entries
            .iter()
            .map(|&entry| {
                // As rows name their files.
                let found = self.listing.entry(entry as usize);
                let (dir, name) = found.get_splitted_path();
                Location::parse(dir).join(name).to_string()
            })
            .collect();
        let generation = self.generation;
        Some(
            Command::request_from_shell(StillThere { paths }).then_send(move |there| {
                Event::ResultsChecked {
                    tab: number,
                    generation,
                    entries,
                    there,
                }
            }),
        )
    }
}

impl DirFleet {
    /// The events of results tabs.
    pub(super) fn scan_event(model: &mut Model, event: Event) -> Command<Effect, Event> {
        match event {
            Event::FindDuplicates { tab, selected } => Self::find_duplicates(model, tab, selected),
            Event::StopResults { tab } => {
                let Some(state) = tab_id(tab).and_then(|id| model.tabs.get_mut(&id)) else {
                    return Command::done();
                };
                // Only a scan that runs is stopped.
                let Some(results) = state
                    .results
                    .as_mut()
                    .filter(|results| results.job.is_some())
                else {
                    return Command::done();
                };
                results.stopped = true;
                state.loading = false;
                state.stop_scan().and(render())
            }
            Event::SelectDuplicates { tab, keep } => {
                let Some(state) = tab_id(tab).and_then(|id| model.tabs.get_mut(&id)) else {
                    return Command::done();
                };
                // Only duplicates come in sets.
                if state
                    .results
                    .as_ref()
                    .is_none_or(|results| !results.is_duplicates())
                {
                    return Command::done();
                }
                state.select_copies(keep);
                render().and(Self::selection_moved(model, tab))
            }
            Event::DuplicatesScanned {
                tab,
                generation,
                update,
            } => Self::scanned(model, tab, generation, update),
            Event::ResultsChecked {
                tab,
                generation,
                entries,
                there,
            } => {
                let Some(state) = Self::current(model, tab, generation) else {
                    return Command::done();
                };
                let Some(results) = state.results.as_mut() else {
                    return Command::done();
                };
                let mut changed = false;
                for (&entry, &there) in entries.iter().zip(&there) {
                    changed |= if there {
                        results.gone.remove(&entry)
                    } else {
                        results.gone.insert(entry)
                    };
                }
                if !changed {
                    return Command::done();
                }
                // The whole order again: a file put back takes its place.
                Self::reorder(state, tab).and(render())
            }
            event => Self::compare_event(model, event),
        }
    }

    /// Opens a tab beside `tab` that looks for duplicates: in the folders
    /// selected there when `selected`, in the tab's own folder otherwise.
    fn find_duplicates(model: &mut Model, tab: u32, selected: bool) -> Command<Effect, Event> {
        let Some(id) = tab_id(tab) else {
            return Command::done();
        };
        let Some(state) = model.tabs.get(&id) else {
            return Command::done();
        };
        // Only folders on this machine are read through.
        let Some(path) = state.path.clone().filter(|_| state.is_local()) else {
            return Command::done();
        };
        let roots: Vec<String> = if selected {
            state
                .selection
                .rows()
                .filter(|&row| {
                    state.order.get(row).is_some_and(|&entry| {
                        state.listing.meta(entry as usize).entry_type == EntryType::Directory
                    })
                })
                .filter_map(|row| state.row_path(row))
                .map(|path| path.to_string_lossy().into_owned())
                .collect()
        } else {
            vec![path]
        };
        if roots.is_empty() {
            return Command::done();
        }
        let Some(index) = model.window_of(id) else {
            return Command::done();
        };
        let new = TabId::new(model.tab_ids.next_id());
        model.tabs.insert(new, Tab::default());
        let window = &mut model.windows[index];
        window.layout.insert_after(id, new);
        window.focused = new;
        Self::show_scan(model, new, Results::of(roots, None))
    }

    /// Shows what the scan of `results` finds in the tab, dropping what it
    /// showed before.
    pub(super) fn show_scan(
        model: &mut Model,
        id: TabId,
        results: Results,
    ) -> Command<Effect, Event> {
        let key = results.view_key();
        let mut view = model.view_for(key);
        // Sets read best as a list, until the user says otherwise.
        if !model.folder_views.contains_key(key) {
            view.display = DisplayType::List;
        }
        let Some(tab) = model.tabs.get_mut(&id) else {
            return Command::done();
        };
        tab.view = view;
        model.generation += 1;
        let generation = model.generation;
        tab.reset_for(generation);
        tab.path = None;
        tab.trash = false;
        tab.committed = None;
        tab.set_search(None);
        let stop_old = tab.stop_watching().and(tab.stop_scan());
        tab.results = Some(results);
        render().and(stop_old).and(Self::load(tab, id.number()))
    }

    /// Starts the tab's scan over, stopping the one that runs.
    pub(super) fn scan(tab: &mut Tab, number: u32) -> Command<Effect, Event> {
        if tab
            .results
            .as_ref()
            .is_some_and(|results| results.compare.is_some())
        {
            return Self::compare_scan(tab, number);
        }
        if tab.shown_tag().is_some() {
            return Self::list_tagged(tab, number);
        }
        let stop = tab.stop_scan();
        let generation = tab.generation;
        let view = tab.list_view();
        let Some(results) = tab.results.as_mut() else {
            return stop;
        };
        results.progress = ScanProgress::default();
        results.stopped = false;
        let scan = Command::stream_from_shell(FindDuplicates {
            id: generation,
            roots: results.roots.clone(),
            view,
        })
        .then_send(move |update| Event::DuplicatesScanned {
            tab: number,
            generation,
            update,
        });
        results.job = Some((generation, scan.abort_handle()));
        tab.loading = true;
        stop.and(scan)
    }

    /// How far a scan is, or what it found.
    fn scanned(
        model: &mut Model,
        tab: u32,
        generation: u64,
        update: DuplicatesUpdate,
    ) -> Command<Effect, Event> {
        let Some(results) =
            Self::current(model, tab, generation).and_then(|state| state.results.as_mut())
        else {
            return Command::done();
        };
        match update {
            DuplicatesUpdate::Progress(progress) => {
                results.progress = progress;
                render()
            }
            DuplicatesUpdate::Found(found) => {
                results.job = None;
                results.done = true;
                results.set_of = found.set_of;
                results.sizes = found.sizes;
                results.unread = found.unread;
                results.files = found.files;
                results.gone.clear();
                Self::listed(model, tab, generation, found.output)
            }
        }
    }
}
