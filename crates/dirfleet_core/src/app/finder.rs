//! A tab's search (`docs/crux-migration/search.md`): typing filters the
//! listing the tab has, and the folders are read again only when what is read
//! changes (subfolders, `in:` folders, hidden items while walking). Typing is
//! recorded in history after a pause; chips and options at once.
use std::sync::Arc;

use crux_core::Command;
use crux_core::capability::Operation;
use crux_core::render::render;
use facet::Facet;
use serde::{Deserialize, Serialize};

use super::{DirFleet, Effect, Event, ListDirOutput, ListView, Model, Tab, tab_id};
use crate::CurrentPath;
use crate::history::HistoryEntry;
use crate::location::Location;
use crate::search::filter::{Highlights, pattern_error};
use crate::search::{Age, Chip, Cmp, FileKind, MatchMode, Search, query};

/// How long typing must pause before it is recorded in history.
const TYPING_COMMIT_MILLIS: u64 = 700;

/// Read `roots` and their subfolders `depth` levels deep, handing over what
/// was found so far, then all of it. Stopped by [`StopWalk`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchWalk {
    pub id: u64,
    pub roots: Vec<String>,
    pub depth: u32,
    pub view: ListView,
}

impl Operation for SearchWalk {
    type Output = WalkUpdate;
}

/// Part of a walk's results, or all of them once `done`. Opaque to facet:
/// it never leaves the core.
#[derive(Clone, Debug, PartialEq, Eq, Facet)]
#[facet(opaque)]
pub struct WalkUpdate {
    pub output: ListDirOutput,
    pub done: bool,
    /// The first root as the file system names it (symlinks resolved), which
    /// the entries' folders are below.
    pub root: String,
}

/// Stop the walk `id`, telling the handler too (crux #622).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StopWalk {
    pub id: u64,
}

impl Operation for StopWalk {
    type Output = ();
}

/// The time now in Unix seconds, then again every so often, for dates
/// shown as "3 days ago": the core never reads the clock.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Clock;

impl Operation for Clock {
    type Output = u64;
}

/// Answer after `millis` milliseconds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Delay {
    pub millis: u64,
}

impl Operation for Delay {
    type Output = ();
}

/// A tab's search, for its search bar.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "what a shell draws, flag by flag"
)]
pub struct SearchView {
    /// The text being typed.
    pub text: String,
    pub chips: Vec<ChipView>,
    pub case_sensitive: bool,
    /// 1 searches the folder itself; more also searches subfolders.
    pub depth: u32,
    /// Names match when any word does, not all.
    pub any: bool,
    /// Folders are still being read; `StopSearch` keeps what was found.
    pub searching: bool,
    /// The tokens of the preset filters in use, to check them in menus.
    pub filters_on: Vec<String>,
    /// What is searched: the tab's folder or the favorites.
    pub scope: SearchScope,
    /// Nothing matched in the folder alone: offer "Search subfolders"
    /// (`SetSearchDepth` to [`WIDER_DEPTH`]).
    pub offer_subfolders: bool,
    /// ... and "Search favorites" (`SetSearchFavorites`), when there are some.
    pub offer_favorites: bool,
}

/// How deep "Search subfolders" looks, as egui's.
pub const WIDER_DEPTH: u32 = 4;

/// One line of the search syntax help.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct SearchHelpRow {
    pub example: String,
    pub meaning: String,
}

pub(super) fn search_help() -> Vec<SearchHelpRow> {
    query::HELP
        .iter()
        .map(|(example, meaning)| SearchHelpRow {
            example: (*example).to_owned(),
            meaning: (*meaning).to_owned(),
        })
        .collect()
}

#[derive(Facet, Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[repr(C)]
pub enum SearchScope {
    #[default]
    Folder,
    Favorites,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
pub struct ChipView {
    /// The chip as it is typed (`ext:pdf`, `"two words"`).
    pub token: String,
    /// Why the chip's pattern is broken, if it is.
    pub error: Option<String>,
}

/// A submenu of preset filters ("Type", "Modified", "Size").
#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
pub struct FilterGroup {
    pub label: String,
    pub choices: Vec<FilterChoice>,
}

/// A preset filter: `token` is what `ToggleSearchFilter` takes.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
pub struct FilterChoice {
    pub label: String,
    pub token: String,
}

/// The preset filters, as egui's "+ Filter" menu offers them.
pub(super) fn filter_groups() -> Vec<FilterGroup> {
    let choice = |label: &str, chip: Chip| FilterChoice {
        label: label.to_owned(),
        token: query::chip_token(&chip),
    };
    let ages = |chip: fn(Age) -> Chip| {
        Age::ALL
            .into_iter()
            .map(|age| choice(age.label(), chip(age)))
            .collect()
    };
    vec![
        FilterGroup {
            label: "Type".into(),
            choices: FileKind::ALL
                .into_iter()
                .map(|kind| choice(kind.label(), Chip::Kind(vec![kind])))
                .collect(),
        },
        FilterGroup {
            label: "Modified".into(),
            choices: ages(Chip::Modified),
        },
        FilterGroup {
            label: "Opened".into(),
            choices: ages(Chip::Opened),
        },
        FilterGroup {
            label: "Size".into(),
            choices: [
                ("Under 1 MB", Cmp::Less, 1 << 20),
                ("Over 10 MB", Cmp::Greater, 10 << 20),
                ("Over 100 MB", Cmp::Greater, 100 << 20),
                ("Over 1 GB", Cmp::Greater, 1 << 30),
            ]
            .into_iter()
            .map(|(label, cmp, bytes)| choice(label, Chip::Size { cmp, bytes }))
            .collect(),
        },
    ]
}

/// Whether the preset `chip` is in use: one of the Type chip's kinds, or a
/// chip of its own.
fn preset_on(search: &Search, chip: &Chip) -> bool {
    match chip {
        Chip::Kind(kinds) => search.chips.iter().any(|old| match old {
            Chip::Kind(used) => kinds.iter().all(|kind| used.contains(kind)),
            _ => false,
        }),
        chip => search.chips.contains(chip),
    }
}

/// Bytes `start..end` of a name.
#[derive(Facet, Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct TextRange {
    pub start: u32,
    pub end: u32,
}

/// What a tab's listing is read from: re-read when it changes, re-ordered
/// otherwise.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ReadKey {
    roots: Vec<String>,
    /// Folders other than the tab's own (the favorites).
    scoped: bool,
    depth: usize,
    /// Only a walk reads hidden folders differently.
    show_hidden: bool,
}

impl ReadKey {
    /// One folder, read by itself.
    const fn is_flat(&self) -> bool {
        self.roots.len() == 1 && self.depth <= 1 && !self.scoped
    }
}

impl Tab {
    /// How the tab's listing is shown: its folder's view and its search.
    pub(super) fn list_view(&self) -> ListView {
        ListView {
            filter: self.filter.clone(),
            ..self.view.list_view()
        }
    }

    /// Replaces the search, and the filter listings are made with.
    pub(super) fn set_search(&mut self, search: Option<Search>) {
        self.filter = search
            .as_ref()
            .filter(|search| search.has_query())
            .map(|search| Arc::new(search.clone()));
        self.search = search;
    }

    /// What the tab reads now; `None` for the Trash and an empty tab.
    pub(super) fn read_key(&self) -> Option<ReadKey> {
        let path = self.path.clone()?;
        // Inside an archive only the folder shown is searched.
        if !self.is_local() {
            return Some(ReadKey {
                roots: vec![path],
                scoped: false,
                depth: 1,
                show_hidden: false,
            });
        }
        // Subfolders and `in:` folders are read only while there is a query,
        // so an empty search shows the folder itself.
        let search = self.filter.as_deref();
        let scoped = self.scope.is_some();
        let mut roots = self.scope.clone().unwrap_or_else(|| vec![path]);
        roots.extend(
            search
                .into_iter()
                .flat_map(Search::extra_dirs)
                .map(|dir| dir.to_string_lossy().into_owned()),
        );
        let depth = search.map_or(1, |search| search.depth.max(1));
        let mut key = ReadKey {
            roots,
            scoped,
            depth,
            show_hidden: false,
        };
        key.show_hidden = !key.is_flat() && self.view.show_hidden;
        Some(key)
    }

    /// Results come from more than one folder, so rows say which.
    pub(super) fn shows_folders(&self) -> bool {
        self.read.as_ref().is_some_and(|key| !key.is_flat())
    }

    /// Where the tab is; `None` for the Trash and an empty tab.
    pub(super) fn location(&self) -> Option<Location> {
        self.path.as_deref().map(Location::parse)
    }

    /// Where the row at `row` is: below the tab's folder, or in its own
    /// folder when results come from several.
    pub(super) fn row_location(&self, row: usize) -> Option<Location> {
        let entry = *self.order.get(row)? as usize;
        if self.shows_folders() {
            let found = self.listing.entry(entry);
            let (dir, name) = found.get_splitted_path();
            return Some(Location::parse(dir).join(name));
        }
        Some(self.location()?.join(self.listing.name(entry)))
    }

    /// The path of the row at `row`, when it is a file or folder on this
    /// machine. A row inside an archive or on a server has none, so whatever
    /// works on paths leaves such rows alone.
    pub(super) fn row_path(&self, row: usize) -> Option<std::path::PathBuf> {
        match self.row_location(row)? {
            Location::Local(path) => Some(std::path::PathBuf::from(path)),
            _ => None,
        }
    }

    /// Stops the running walk, telling the handler too (crux #622).
    pub(super) fn stop_walk(&mut self) -> Command<Effect, Event> {
        match self.walk.take() {
            Some((id, handle)) => {
                handle.abort();
                Command::notify_shell(StopWalk { id }).into()
            }
            None => Command::done(),
        }
    }

    /// The history entry of what the tab shows, with its search.
    fn history_entry(&self) -> Option<HistoryEntry> {
        let path = if self.trash {
            CurrentPath::Trash
        } else {
            CurrentPath::One(self.path.clone()?.into())
        };
        Some(HistoryEntry::new(path, self.search.clone()))
    }

    /// Records the search in history, unless it is what was recorded last,
    /// and returns how it changed (before, after) for the undo history.
    #[must_use]
    pub(super) fn commit_search(&mut self) -> Option<(Option<Search>, Option<Search>)> {
        let unchanged = match (&self.committed, &self.search) {
            (None, None) => true,
            (Some(a), Some(b)) => a.same_query(b),
            (Some(search), None) | (None, Some(search)) => !search.has_query(),
        };
        let before = std::mem::replace(&mut self.committed, self.search.clone());
        if unchanged {
            return None;
        }
        if let Some(entry) = self.history_entry() {
            self.history.visit(entry);
        }
        Some((before, self.search.clone()))
    }

    /// Puts back a search from the undo history, as already recorded.
    pub(super) fn restore_search(&mut self, search: Option<Search>) {
        if search.is_none() {
            self.scope = None;
        }
        self.committed.clone_from(&search);
        self.set_search(search);
    }

    /// The search bar; `has_favorites` decides whether nothing found offers
    /// to search the favorites.
    pub(super) fn search_view(&self, has_favorites: bool) -> Option<SearchView> {
        let search = self.search.as_ref()?;
        // As egui's: nothing matched in this folder alone, so offer more.
        let nothing_here = search.has_query()
            && self.walk.is_none()
            && !self.loading
            && self.order.is_empty()
            && search.depth <= 1;
        Some(SearchView {
            offer_subfolders: nothing_here,
            offer_favorites: nothing_here && has_favorites && self.scope.is_none(),
            text: search.value.clone(),
            chips: search
                .chips
                .iter()
                .map(|chip| ChipView {
                    token: query::chip_token(chip),
                    error: match chip {
                        Chip::Name { pattern, kind, .. } => pattern_error(pattern, *kind),
                        _ => None,
                    },
                })
                .collect(),
            case_sensitive: search.case_sensitive,
            depth: u32::try_from(search.depth.max(1)).unwrap_or(u32::MAX),
            any: search.match_mode == MatchMode::Any,
            searching: self.walk.is_some(),
            filters_on: filter_groups()
                .into_iter()
                .flat_map(|group| group.choices)
                .filter(|choice| {
                    query::parse_filter(&choice.token).is_some_and(|chip| preset_on(search, &chip))
                })
                .map(|choice| choice.token)
                .collect(),
            scope: if self.scope.is_some() {
                SearchScope::Favorites
            } else {
                SearchScope::Folder
            },
        })
    }

    /// What to highlight in shown names: the search's plain words.
    pub(super) fn highlights(&self) -> Option<Highlights> {
        self.filter.as_deref().and_then(Highlights::new)
    }
}

/// `name`'s bytes matching `highlights`.
pub(super) fn ranges(highlights: Option<&Highlights>, name: &str) -> Vec<TextRange> {
    highlights.map_or_else(Vec::new, |highlights| {
        highlights
            .ranges(name)
            .into_iter()
            .map(|range| TextRange {
                start: u32::try_from(range.start).unwrap_or(u32::MAX),
                end: u32::try_from(range.end).unwrap_or(u32::MAX),
            })
            .collect()
    })
}

/// `dir` as shown in the Folder column: below `root` when it is inside it.
pub(super) fn relative_dir(dir: &str, root: Option<&str>) -> String {
    let Some(root) = root else {
        return dir.to_owned();
    };
    // Compare with one separator style; Windows accepts both.
    let normalize = |path: &str| path.replace('\\', "/");
    let dir_norm = normalize(dir);
    let root_norm = normalize(root);
    let root_norm = root_norm.trim_end_matches('/');
    dir_norm.strip_prefix(root_norm).map_or_else(
        || dir.to_owned(),
        |rest| {
            rest.trim_start_matches('/')
                .replace('/', std::path::MAIN_SEPARATOR_STR)
        },
    )
}

impl DirFleet {
    /// Reads what the tab shows: the Trash, its folder, or a walk through
    /// its search's folders. Results carry the tab's generation.
    pub(super) fn load(tab: &mut Tab, number: u32) -> Command<Effect, Event> {
        let stop = tab.stop_walk();
        let generation = tab.generation;
        if tab.trash {
            return stop.and(Self::list_trash(number, tab.list_view(), generation));
        }
        let Some(key) = tab.read_key() else {
            return stop;
        };
        tab.read = Some(key.clone());
        if key.is_flat() {
            let path = key.roots.into_iter().next().unwrap_or_default();
            tab.reading = true;
            return stop.and(Self::list(number, path, tab.list_view(), generation));
        }
        let walk = Command::stream_from_shell(SearchWalk {
            id: generation,
            roots: key.roots,
            depth: u32::try_from(key.depth).unwrap_or(u32::MAX),
            view: tab.list_view(),
        })
        .then_send(move |update| Event::Walked {
            tab: number,
            generation,
            update,
        });
        tab.walk = Some((generation, walk.abort_handle()));
        stop.and(walk)
    }

    /// Reads the tab again with a new generation, dropping late results of
    /// the old read but keeping its rows until the new ones come.
    pub(super) fn reread(
        tab: &mut Tab,
        number: u32,
        generation: &mut u64,
    ) -> Command<Effect, Event> {
        *generation += 1;
        tab.generation = *generation;
        tab.loading = true;
        tab.error = None;
        Self::load(tab, number)
    }

    /// Shows the tab's listing for its new search or view: read again when
    /// what is read changed, ordered again otherwise.
    pub(super) fn refilter(
        tab: &mut Tab,
        number: u32,
        generation: &mut u64,
    ) -> Command<Effect, Event> {
        if !tab.trash && tab.read_key() != tab.read {
            return Self::reread(tab, number, generation);
        }
        // A listing still being read is ordered when it arrives.
        if tab.loading {
            return Command::done();
        }
        Self::reorder(tab, number)
    }

    /// Part or all of a walk's results.
    pub(super) fn walked(
        model: &mut Model,
        tab: u32,
        generation: u64,
        update: WalkUpdate,
    ) -> Command<Effect, Event> {
        if let Some(state) = Self::current(model, tab, generation) {
            state.walk_root = Some(update.root);
            if update.done {
                state.walk = None;
            }
        }
        Self::listed(model, tab, generation, update.output)
    }

    /// The events that change a tab's search.
    pub(super) fn search_event(model: &mut Model, event: Event) -> Command<Effect, Event> {
        if let Event::Walked {
            tab,
            generation,
            update,
        } = event
        {
            return Self::walked(model, tab, generation, update);
        }
        let Some(tab) = search_tab(&event) else {
            return Command::done();
        };
        let Model {
            tabs,
            generation,
            sidebar,
            files,
            ..
        } = model;
        let Some(id) = tab_id(tab) else {
            return Command::done();
        };
        let Some(state) = tabs.get_mut(&id) else {
            return Command::done();
        };
        // Typing still waiting for its pause becomes its own step first, so
        // Undo takes back the later edit alone.
        if !matches!(
            event,
            Event::SetSearchText { .. } | Event::SearchPaused { .. } | Event::StopSearch { .. }
        ) {
            Self::record_search(files, id, state.commit_search());
        }
        let mut search = state.search.clone().unwrap_or_default();
        let mut typing = Command::done();
        // Escape clears the text, then the filters, then closes, as egui's.
        let closed = match &event {
            Event::CloseSearch { .. } => true,
            Event::SearchEscape { .. } => search.value.is_empty() && search.chips.is_empty(),
            _ => false,
        };
        let typed_text = matches!(event, Event::SetSearchText { .. });
        match event {
            Event::SetSearchText { text, .. } => {
                if text == search.value && state.search.is_some() {
                    return Command::done();
                }
                search.value = text;
                state.typed += 1;
                let typed = state.typed;
                typing = Command::request_from_shell(Delay {
                    millis: TYPING_COMMIT_MILLIS,
                })
                .then_send(move |()| Event::SearchPaused { tab, typed });
            }
            Event::CommitSearchText { .. } => {
                search.commit_text();
            }
            Event::SearchEscape { .. } if !search.value.is_empty() => search.value.clear(),
            Event::SearchEscape { .. } => search.chips.clear(),
            Event::ToggleSearchFilter { token, .. } => {
                if !toggle_filter(&mut search, &token) {
                    return Command::done();
                }
            }
            Event::RemoveSearchChip { index, .. } => {
                if (index as usize) < search.chips.len() {
                    search.chips.remove(index as usize);
                }
            }
            Event::SetSearchCase { on, .. } => search.case_sensitive = on,
            Event::SetSearchDepth { depth, .. } => search.depth = depth.max(1) as usize,
            Event::SetSearchAny { any, .. } => {
                search.match_mode = if any { MatchMode::Any } else { MatchMode::All };
            }
            Event::StopSearch { .. } => return state.stop_walk().and(render()),
            Event::SetSearchFavorites { on, .. } => {
                let favorites: Vec<String> =
                    sidebar.favorites.iter().map(|f| f.path.clone()).collect();
                state.scope = (on && !favorites.is_empty()).then_some(favorites);
            }
            Event::SearchPaused { typed, .. } => {
                // Typing went on: a later pause records it.
                if typed == state.typed {
                    Self::record_search(files, id, state.commit_search());
                }
                return Command::done();
            }
            _ => {}
        }
        if closed {
            state.scope = None;
        }
        state.set_search((!closed).then_some(search));
        if !typed_text {
            Self::record_search(files, id, state.commit_search());
        }
        render()
            .and(Self::refilter(state, tab, generation))
            .and(typing)
    }
}

/// Turns the filter `token` on or off in `search`, as the "+ Filter" menu
/// does; `false` when the token isn't a filter.
fn toggle_filter(search: &mut Search, token: &str) -> bool {
    let Some(chip) = query::parse_filter(token) else {
        return false;
    };
    match chip {
        Chip::Kind(kinds) if kinds.len() == 1 => {
            search.toggle_kind(kinds[0]);
        }
        chip if search.chips.contains(&chip) => search.chips.retain(|old| *old != chip),
        chip => {
            search.add_chip(chip);
        }
    }
    true
}

/// The tab a search event is for.
const fn search_tab(event: &Event) -> Option<u32> {
    match event {
        Event::SetSearchText { tab, .. }
        | Event::CommitSearchText { tab }
        | Event::ToggleSearchFilter { tab, .. }
        | Event::RemoveSearchChip { tab, .. }
        | Event::SetSearchCase { tab, .. }
        | Event::SetSearchDepth { tab, .. }
        | Event::SetSearchAny { tab, .. }
        | Event::CloseSearch { tab }
        | Event::StopSearch { tab }
        | Event::SetSearchFavorites { tab, .. }
        | Event::SearchPaused { tab, .. }
        | Event::SearchEscape { tab } => Some(*tab),
        _ => None,
    }
}
