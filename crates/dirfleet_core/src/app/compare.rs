//! Comparing two folders: a results tab that shows the files which are in
//! one of them only or differ between them, each kind under its heading,
//! and copies the ones chosen to the other folder.
//!
//! The scan pairs files by their path below each folder. A row is one such
//! path, shown by the file of one side: the left folder's where it is the
//! only or the newer one, or as old as the other; the right folder's
//! otherwise. After a file operation the paths it may have touched are
//! compared again, so rows leave as the folders come to agree, and come
//! back when Undo makes them differ again.
use std::collections::HashSet;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use crux_core::Command;
use crux_core::capability::Operation;
use crux_core::render::render;
use facet::Facet;
use serde::{Deserialize, Serialize};

use super::files::ConflictChoice;
use super::results::{Results, ScanProgress, same_form};
use super::selection::Stamp;
use super::{DirFleet, Effect, Event, ListDirOutput, ListView, Model, Tab, Window, tab_id};
use crate::fileops::{FileOp, Origin};
use crate::ids::TabId;
use crate::listing::Listing;
use crate::listing::group::GroupSpan;
use crate::location::Location;

/// The key the view of compared folders is kept under among folder views.
pub(super) const COMPARE_VIEW: &str = ":compare";

/// How many seconds apart two files may have been changed and still count
/// as changed at the same time: some disks keep times to two seconds.
pub const SAME_TIME: u64 = 2;

/// How a file differs between the two folders compared. Opaque to facet:
/// it never leaves the core.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Facet)]
#[facet(opaque)]
#[repr(C)]
pub enum Difference {
    /// In both folders and alike, or in neither: not shown.
    Same,
    OnlyLeft,
    OnlyRight,
    /// In both, not alike, and the left folder's was changed later.
    NewerLeft,
    NewerRight,
    /// In both and changed at the same time, yet not alike.
    Different,
}

impl Difference {
    /// The differences shown, in the order of their headings.
    pub const SHOWN: [Self; 5] = [
        Self::OnlyLeft,
        Self::OnlyRight,
        Self::NewerLeft,
        Self::NewerRight,
        Self::Different,
    ];

    /// How two files that are both there differ: each one's size and when
    /// it was changed, in seconds. `alike` is whether they hold the same
    /// bytes, where they were read; files not read are alike when they are
    /// of one size and were changed at the same time.
    pub const fn of(left: (u64, u64), right: (u64, u64), alike: Option<bool>) -> Self {
        let same_time = left.1.abs_diff(right.1) <= SAME_TIME;
        let same_size = left.0 == right.0;
        let alike = match alike {
            Some(alike) => alike && same_size,
            None => same_size && same_time,
        };
        if alike {
            Self::Same
        } else if same_time {
            Self::Different
        } else if left.1 > right.1 {
            Self::NewerLeft
        } else {
            Self::NewerRight
        }
    }

    /// Whether copying towards the left folder (`to_left`) or the right one
    /// copies this file, and whether that replaces a file there.
    const fn copies(self, to_left: bool) -> (bool, bool) {
        match self {
            Self::Same => (false, false),
            Self::OnlyLeft => (!to_left, false),
            Self::OnlyRight => (to_left, false),
            Self::NewerLeft | Self::NewerRight | Self::Different => (true, true),
        }
    }
}

/// Compare the folders `left` and `right`, saying how far the scan is, then
/// what differs. Stopped by [`StopCompare`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompareFolders {
    pub id: u64,
    pub left: String,
    pub right: String,
    /// Read files of one size to tell whether they hold the same bytes,
    /// whenever they were changed.
    pub contents: bool,
    /// How the files found are ordered; hidden files count only when shown.
    pub view: ListView,
}

impl Operation for CompareFolders {
    type Output = CompareUpdate;
}

/// What comparing two folders found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Differences {
    /// One file for each path that differs, as a listing whose entries know
    /// their folders.
    pub output: ListDirOutput,
    /// How each entry of the listing differs.
    pub kinds: Vec<Difference>,
    /// For each folder of the listing (`Listing::dirs`), the folder at the
    /// same place on the other side.
    pub other_dirs: Arc<Vec<String>>,
    /// For each folder of the listing, whether it is on the left side.
    pub on_left: Arc<Vec<bool>>,
    /// How many files are in both folders and alike.
    pub alike: u64,
    /// How many files could not be read, and so were told apart by size
    /// and time alone.
    pub unread: u32,
}

/// Opaque to facet: it never leaves the core.
#[derive(Clone, Debug, PartialEq, Eq, Facet)]
#[facet(opaque)]
#[repr(C)]
pub enum CompareUpdate {
    Progress(ScanProgress),
    /// The scan is done.
    Found(Differences),
}

/// Stop the scan `id`, telling the handler too (crux #622).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StopCompare {
    pub id: u64,
}

impl Operation for StopCompare {
    type Output = ();
}

/// How the file at each left path differs from the one at its right path,
/// as things are now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Recompare {
    pub pairs: Vec<(String, String)>,
    pub contents: bool,
}

impl Operation for Recompare {
    type Output = Vec<Difference>;
}

/// What copying to one of the folders would do.
#[derive(Facet, Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct SyncCount {
    /// How many files would be copied.
    pub copies: u32,
    /// How many of them would replace a file already there.
    pub replaces: u32,
    /// How many of the files replaced are newer than what replaces them.
    pub newer: u32,
}

/// The question asked before a copy that replaces files.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
pub struct SyncAsk {
    pub to_left: bool,
    /// The folder copied from and the one copied to, as the bar names them.
    pub from: String,
    pub into: String,
    pub count: SyncCount,
}

/// The two folders a window would compare, by the names that tell them
/// apart.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
pub struct ComparePair {
    pub left: String,
    pub right: String,
}

/// What the bar of a tab of compared folders shows besides the scan.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
pub struct CompareView {
    /// The folders' names: their own, or their whole paths when both are
    /// called the same.
    pub left: String,
    pub right: String,
    /// Files of one size are read to tell them apart.
    pub contents: bool,
    /// How many paths differ.
    pub differences: u32,
    /// What copying the selected rows, or every row when none is selected,
    /// to the left folder and to the right one would do.
    pub to_left: SyncCount,
    pub to_right: SyncCount,
    /// A copy would replace files and waits for `AnswerSync`.
    pub asking: Option<SyncAsk>,
}

/// What a tab of compared folders keeps besides the scan.
#[derive(Default)]
pub(super) struct Compare {
    contents: bool,
    /// The names that tell the two folders apart.
    names: (String, String),
    /// How each entry of the listing differs, as last looked at.
    kinds: Vec<Difference>,
    other_dirs: Arc<Vec<String>>,
    on_left: Arc<Vec<bool>>,
    alike: u64,
    /// How many rows each heading has, in the order of [`Difference::SHOWN`].
    shown: [u32; 5],
    /// The side a copy that replaces files waits to go to.
    asking: Option<bool>,
    /// What copying to the left and to the right would do, for the
    /// selection it was counted for.
    sync: Mutex<Option<(Stamp, [SyncCount; 2])>>,
}

/// The names that tell `left` and `right` apart: the folders' own, or
/// their whole paths when both are called the same.
fn names(left: &str, right: &str) -> (String, String) {
    let (a, b) = (
        Location::parse(left).title(),
        Location::parse(right).title(),
    );
    if a.to_lowercase() == b.to_lowercase() {
        (left.to_owned(), right.to_owned())
    } else {
        (a, b)
    }
}

/// Whether one of the folders is the other or inside it.
fn nested(a: &str, b: &str) -> bool {
    let (a, b) = (same_form(a), same_form(b));
    let inside = |inner: &str, outer: &str| {
        inner
            .strip_prefix(outer)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
    };
    inside(&a, &b) || inside(&b, &a)
}

const fn plural(count: u32) -> &'static str {
    if count == 1 { "" } else { "s" }
}

impl Results {
    /// What a tab shows while it compares `left` with `right`.
    fn comparing(left: String, right: String) -> Self {
        let compare = Compare {
            names: names(&left, &right),
            ..Compare::default()
        };
        Self::of(vec![left, right], Some(compare))
    }
}

impl Compare {
    /// "Compare a with b".
    pub(super) fn title(&self) -> String {
        format!("Compare {} with {}", self.names.0, self.names.1)
    }

    fn heading(&self, kind: Difference) -> String {
        let (left, right) = (&self.names.0, &self.names.1);
        match kind {
            Difference::Same => String::new(),
            Difference::OnlyLeft => format!("Only in {left}"),
            Difference::OnlyRight => format!("Only in {right}"),
            Difference::NewerLeft => format!("Newer in {left}"),
            Difference::NewerRight => format!("Newer in {right}"),
            Difference::Different => "Different, though changed at the same time".to_owned(),
        }
    }

    /// Puts `order` kind by kind, keeping the order within each; files that
    /// no longer differ drop out. Returns each kind's heading.
    pub(super) fn arrange(&mut self, listing: &Listing, order: &mut Vec<u32>) -> Vec<GroupSpan> {
        *self.sync.get_mut().unwrap_or_else(PoisonError::into_inner) = None;
        self.shown = [0; 5];
        // Until the scan's own listing comes nothing is known to differ.
        if self.kinds.len() != listing.len() {
            return Vec::new();
        }
        let kinds = &self.kinds;
        let place = |entry: u32| {
            kinds
                .get(entry as usize)
                .and_then(|kind| Difference::SHOWN.iter().position(|shown| shown == kind))
        };
        order.retain(|&entry| place(entry).is_some());
        let mut counts = [0_u32; 5];
        for &entry in order.iter() {
            if let Some(place) = place(entry) {
                counts[place] += 1;
            }
        }
        let mut starts = [0_u32; 5];
        let mut at = 0_u32;
        for (start, &count) in starts.iter_mut().zip(&counts) {
            *start = at;
            at += count;
        }
        let mut next = starts;
        let mut placed = vec![0_u32; order.len()];
        for &entry in order.iter() {
            if let Some(place) = place(entry) {
                placed[next[place] as usize] = entry;
                next[place] += 1;
            }
        }
        *order = placed;
        self.shown = counts;
        Difference::SHOWN
            .iter()
            .zip(starts)
            .zip(counts)
            .filter(|&(_, count)| count > 0)
            .map(|((&kind, first), len)| GroupSpan {
                label: self.heading(kind),
                first,
                len,
            })
            .collect()
    }

    /// How many paths differ.
    fn differences(&self) -> u32 {
        self.shown.iter().sum()
    }

    /// What the scan found, in a line.
    pub(super) fn found_text(&self) -> String {
        let differences = self.differences();
        if differences == 0 {
            return match self.alike {
                0 => "No differences".to_owned(),
                1 => "No differences: 1 file is the same in both".to_owned(),
                alike => format!("No differences: {alike} files are the same in both"),
            };
        }
        let mut text = format!("{differences} difference{}", plural(differences));
        let (left, right) = (&self.names.0, &self.names.1);
        let parts = [
            format!("only in {left}"),
            format!("only in {right}"),
            format!("newer in {left}"),
            format!("newer in {right}"),
            "different".to_owned(),
        ];
        let mut first = true;
        for (&count, part) in self.shown.iter().zip(parts) {
            if count == 0 {
                continue;
            }
            let _ = write!(text, "{} {count} {part}", if first { ":" } else { "," });
            first = false;
        }
        text
    }
}

impl Model {
    /// The two folders [`Event::CompareFolders`] would compare in `window`:
    /// the one its focused pane shows and the one the next pane shows, the
    /// first of the two panes on the left. Both are folders of this
    /// machine, and neither is inside the other.
    pub(super) fn compare_pair(&self, window: &Window) -> Option<(String, String)> {
        let shown = window.layout.shown();
        if shown.len() < 2 {
            return None;
        }
        let at = shown.iter().position(|&tab| tab == window.focused)?;
        let other = (at + 1) % shown.len();
        let folder = |index: usize| {
            let tab = self.tabs.get(shown.get(index)?)?;
            tab.path
                .clone()
                .filter(|path| !path.is_empty() && !tab.trash && tab.is_local())
        };
        let (left, right) = (folder(at.min(other))?, folder(at.max(other))?);
        (!nested(&left, &right)).then_some((left, right))
    }

    /// The pair as shells name it.
    pub(super) fn compare_pair_view(&self, window: &Window) -> Option<ComparePair> {
        let (left, right) = self.compare_pair(window)?;
        let (left, right) = names(&left, &right);
        Some(ComparePair { left, right })
    }
}

impl Tab {
    fn compare(&self) -> Option<&Compare> {
        self.results.as_ref()?.compare.as_ref()
    }

    /// Where the file of `entry` is, or would be, in the left folder and in
    /// the right one.
    fn sides(&self, entry: u32) -> Option<(String, String)> {
        let compare = self.compare()?;
        let index = entry as usize;
        if index >= self.listing.len() {
            return None;
        }
        let dir = self.listing.dir_index(index);
        let found = self.listing.entry(index);
        // As rows name their files.
        let (here, name) = found.get_splitted_path();
        let this = Location::parse(here).join(name).to_string();
        let other = Location::parse(compare.other_dirs.get(dir)?)
            .join(name)
            .to_string();
        Some(if *compare.on_left.get(dir)? {
            (this, other)
        } else {
            (other, this)
        })
    }

    /// The rows a copy takes: the selected ones, or all when none is.
    fn sync_rows(&self) -> Vec<usize> {
        if self.selection.is_empty() {
            (0..self.order.len()).collect()
        } else {
            self.selection.rows().collect()
        }
    }

    fn kind_at(&self, row: usize) -> Option<(u32, Difference)> {
        let entry = *self.order.get(row)?;
        let kind = *self.compare()?.kinds.get(entry as usize)?;
        Some((entry, kind))
    }

    /// What copying to the left folder and to the right one would do.
    /// Counted once for each selection.
    fn sync_counts(&self) -> [SyncCount; 2] {
        let Some(compare) = self.compare() else {
            return [SyncCount::default(); 2];
        };
        let stamp = self.selection_stamp();
        let mut memo = compare.sync.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((at, counts)) = *memo
            && at == stamp
        {
            return counts;
        }
        let mut counts = [SyncCount::default(); 2];
        for row in self.sync_rows() {
            let Some((_, kind)) = self.kind_at(row) else {
                continue;
            };
            for (count, to_left) in counts.iter_mut().zip([true, false]) {
                let (copies, replaces) = kind.copies(to_left);
                count.copies += u32::from(copies);
                count.replaces += u32::from(replaces);
                count.newer += u32::from(
                    kind == if to_left {
                        Difference::NewerLeft
                    } else {
                        Difference::NewerRight
                    },
                );
            }
        }
        *memo = Some((stamp, counts));
        counts
    }

    /// The files a copy to one side takes, each with where it goes;
    /// without the ones that would replace a file unless `replace`.
    fn sync_pairs(&self, to_left: bool, replace: bool) -> Vec<(PathBuf, PathBuf)> {
        self.sync_rows()
            .into_iter()
            .filter_map(|row| {
                let (entry, kind) = self.kind_at(row)?;
                let (copies, replaces) = kind.copies(to_left);
                if !copies || (replaces && !replace) {
                    return None;
                }
                let (left, right) = self.sides(entry)?;
                Some(if to_left {
                    (PathBuf::from(right), PathBuf::from(left))
                } else {
                    (PathBuf::from(left), PathBuf::from(right))
                })
            })
            .collect()
    }

    pub(super) fn compare_view(&self) -> Option<CompareView> {
        let results = self.results.as_ref()?;
        let compare = results.compare.as_ref()?;
        // Nothing is copied while the folders are still being compared.
        let [left_count, right_count] = if results.job.is_some() {
            [SyncCount::default(); 2]
        } else {
            self.sync_counts()
        };
        let (left, right) = compare.names.clone();
        Some(CompareView {
            contents: compare.contents,
            differences: compare.differences(),
            to_left: left_count,
            to_right: right_count,
            asking: compare.asking.map(|to_left| SyncAsk {
                to_left,
                from: if to_left { right.clone() } else { left.clone() },
                into: if to_left { left.clone() } else { right.clone() },
                count: if to_left { left_count } else { right_count },
            }),
            left,
            right,
        })
    }

    /// Asks how the tab's files in the folders `affected`, on either side,
    /// differ now, when it shows compared folders and a file operation
    /// ended.
    pub(super) fn check_compare(
        &self,
        number: u32,
        affected: &[String],
    ) -> Option<Command<Effect, Event>> {
        let results = self.results.as_ref()?;
        let compare = results.compare.as_ref()?;
        if results.job.is_some() || affected.is_empty() || self.listing.is_empty() {
            return None;
        }
        let affected: HashSet<String> = affected.iter().map(|dir| same_form(dir)).collect();
        let touched: Vec<bool> = self
            .listing
            .dirs()
            .iter()
            .zip(compare.other_dirs.iter())
            .map(|(dir, other)| {
                affected.contains(&same_form(dir)) || affected.contains(&same_form(other))
            })
            .collect();
        // Files that no longer differ are asked about too: Undo may have
        // made them differ again.
        let (entries, pairs): (Vec<u32>, Vec<(String, String)>) = (0..self.listing.len())
            .filter(|&entry| touched.get(self.listing.dir_index(entry)) == Some(&true))
            .filter_map(|entry| {
                let entry = entry as u32;
                Some((entry, self.sides(entry)?))
            })
            .unzip();
        if entries.is_empty() {
            return None;
        }
        let generation = self.generation;
        Some(
            Command::request_from_shell(Recompare {
                pairs,
                contents: compare.contents,
            })
            .then_send(move |kinds| Event::DifferencesChecked {
                tab: number,
                generation,
                entries,
                kinds,
            }),
        )
    }
}

impl DirFleet {
    /// The events of tabs that compare folders.
    pub(super) fn compare_event(model: &mut Model, event: Event) -> Command<Effect, Event> {
        match event {
            Event::CompareFolders { window } => Self::compare_folders(model, window),
            Event::SetCompareOptions { tab, contents } => {
                let Model {
                    tabs, generation, ..
                } = model;
                let Some(state) = tab_id(tab).and_then(|id| tabs.get_mut(&id)) else {
                    return Command::done();
                };
                let Some(compare) = state
                    .results
                    .as_mut()
                    .and_then(|results| results.compare.as_mut())
                    .filter(|compare| compare.contents != contents)
                else {
                    return Command::done();
                };
                compare.contents = contents;
                // The folders are compared again, the new way.
                render().and(Self::reread(state, tab, generation))
            }
            Event::SyncSelection { tab, to_left } => Self::sync_selection(model, tab, to_left),
            Event::AnswerSync { tab, choice } => {
                let asked = tab_id(tab)
                    .and_then(|id| model.tabs.get_mut(&id))
                    .and_then(|state| state.results.as_mut())
                    .and_then(|results| results.compare.as_mut())
                    .and_then(|compare| compare.asking.take());
                let Some(to_left) = asked else {
                    return Command::done();
                };
                let copy = match choice {
                    ConflictChoice::Replace => Self::sync(model, tab, to_left, true),
                    ConflictChoice::Skip => Self::sync(model, tab, to_left, false),
                    ConflictChoice::KeepBoth | ConflictChoice::Cancel => Command::done(),
                };
                render().and(copy)
            }
            Event::FoldersCompared {
                tab,
                generation,
                update,
            } => Self::compared(model, tab, generation, update),
            Event::DifferencesChecked {
                tab,
                generation,
                entries,
                kinds,
            } => {
                let Some(state) = Self::current(model, tab, generation) else {
                    return Command::done();
                };
                let Some(compare) = state
                    .results
                    .as_mut()
                    .and_then(|results| results.compare.as_mut())
                else {
                    return Command::done();
                };
                let mut changed = false;
                for (&entry, &kind) in entries.iter().zip(&kinds) {
                    if let Some(known) = compare.kinds.get_mut(entry as usize)
                        && *known != kind
                    {
                        *known = kind;
                        changed = true;
                    }
                }
                if !changed {
                    return Command::done();
                }
                // The whole order again: a file takes its new heading.
                Self::reorder(state, tab).and(render())
            }
            _ => Command::done(),
        }
    }

    /// Opens a tab beside the window's focused one that compares the
    /// folders its two panes show.
    fn compare_folders(model: &mut Model, window: u32) -> Command<Effect, Event> {
        let Some(index) = model.window_index(window) else {
            return Command::done();
        };
        let Some((left, right)) = model.compare_pair(&model.windows[index]) else {
            return Command::done();
        };
        let beside = model.windows[index].focused;
        let new = TabId::new(model.tab_ids.next_id());
        model.tabs.insert(new, Tab::default());
        let window = &mut model.windows[index];
        window.layout.insert_after(beside, new);
        window.focused = new;
        Self::show_scan(model, new, Results::comparing(left, right))
    }

    /// Starts comparing the tab's folders over, stopping the scan that runs.
    pub(super) fn compare_scan(tab: &mut Tab, number: u32) -> Command<Effect, Event> {
        let stop = tab.stop_scan();
        let generation = tab.generation;
        let view = tab.list_view();
        let Some(results) = tab.results.as_mut() else {
            return stop;
        };
        let ([left, right], Some(compare)) = (results.roots.as_slice(), results.compare.as_mut())
        else {
            return stop;
        };
        compare.asking = None;
        let scan = Command::stream_from_shell(CompareFolders {
            id: generation,
            left: left.clone(),
            right: right.clone(),
            contents: compare.contents,
            view,
        })
        .then_send(move |update| Event::FoldersCompared {
            tab: number,
            generation,
            update,
        });
        results.progress = ScanProgress::default();
        results.stopped = false;
        results.job = Some((generation, scan.abort_handle()));
        tab.loading = true;
        stop.and(scan)
    }

    /// How far the scan is, or what it found.
    fn compared(
        model: &mut Model,
        tab: u32,
        generation: u64,
        update: CompareUpdate,
    ) -> Command<Effect, Event> {
        let Some(results) =
            Self::current(model, tab, generation).and_then(|state| state.results.as_mut())
        else {
            return Command::done();
        };
        let Some(compare) = results.compare.as_mut() else {
            return Command::done();
        };
        match update {
            CompareUpdate::Progress(progress) => {
                results.progress = progress;
                render()
            }
            CompareUpdate::Found(found) => {
                compare.kinds = found.kinds;
                compare.other_dirs = found.other_dirs;
                compare.on_left = found.on_left;
                compare.alike = found.alike;
                compare.asking = None;
                results.job = None;
                results.done = true;
                results.unread = found.unread;
                Self::listed(model, tab, generation, found.output)
            }
        }
    }

    /// Copies the tab's chosen files to the left folder or the right one;
    /// asks first when that replaces files.
    fn sync_selection(model: &mut Model, tab: u32, to_left: bool) -> Command<Effect, Event> {
        let Some(state) = tab_id(tab).and_then(|id| model.tabs.get_mut(&id)) else {
            return Command::done();
        };
        // Not while the folders are still being compared.
        if state
            .results
            .as_ref()
            .is_none_or(|results| results.job.is_some() || results.compare.is_none())
        {
            return Command::done();
        }
        let [left, right] = state.sync_counts();
        let count = if to_left { left } else { right };
        if count.copies == 0 {
            return Command::done();
        }
        if count.replaces == 0 {
            return Self::sync(model, tab, to_left, true);
        }
        if let Some(compare) = state
            .results
            .as_mut()
            .and_then(|results| results.compare.as_mut())
        {
            compare.asking = Some(to_left);
        }
        render()
    }

    /// Copies the tab's chosen files to one side, as one operation.
    fn sync(model: &mut Model, tab: u32, to_left: bool, replace: bool) -> Command<Effect, Event> {
        let Some(state) = tab_id(tab).and_then(|id| model.tabs.get(&id)) else {
            return Command::done();
        };
        let pairs = state.sync_pairs(to_left, replace);
        let into = state
            .results
            .as_ref()
            .and_then(|results| results.roots.get(usize::from(!to_left)))
            .cloned();
        let Some(into) = into.filter(|_| !pairs.is_empty()) else {
            return Command::done();
        };
        // The results stay as they are: nothing of theirs is selected anew.
        Self::run(
            model,
            FileOp::CopyPairs {
                pairs,
                into: PathBuf::from(into),
            },
            Origin::User,
            None,
            false,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_are_told_apart_by_size_time_and_what_reading_them_said() {
        use Difference::{Different, NewerLeft, NewerRight, Same};
        // Unread: one size and changed within two seconds is the same file.
        assert_eq!(Difference::of((10, 100), (10, 100), None), Same);
        assert_eq!(Difference::of((10, 100), (10, 102), None), Same);
        assert_eq!(Difference::of((10, 100), (10, 103), None), NewerRight);
        assert_eq!(Difference::of((10, 200), (10, 100), None), NewerLeft);
        assert_eq!(Difference::of((10, 100), (11, 101), None), Different);
        assert_eq!(Difference::of((10, 300), (11, 100), None), NewerLeft);
        // Read: the same bytes are the same file whenever it was changed.
        assert_eq!(Difference::of((10, 100), (10, 900), Some(true)), Same);
        assert_eq!(
            Difference::of((10, 100), (10, 900), Some(false)),
            NewerRight
        );
        assert_eq!(Difference::of((10, 100), (10, 101), Some(false)), Different);
    }

    #[test]
    fn a_folder_is_not_compared_with_itself_or_one_inside_it() {
        assert!(nested("/a/b", "/a/b"));
        assert!(nested("/a/b", "/a/b/c"));
        assert!(nested("C:\\a\\b\\c", "c:/a/b/"));
        assert!(!nested("/a/b", "/a/bc"));
        assert!(!nested("/a/b", "/a/c"));
        // Folders called the same are named by their paths.
        assert_eq!(
            names("/x/src", "/y/Src"),
            ("/x/src".to_owned(), "/y/Src".to_owned())
        );
        assert_eq!(names("/x/a", "/y/b"), ("a".to_owned(), "b".to_owned()));
    }
}
