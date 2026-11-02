//! What the path editor knows: the typed text, the folder it points into and
//! that folder's subfolders matched against the last path segment. Folders
//! are read only when the text moves to another parent, and matched only when
//! the text changes, so an idle editor does no work.
//!
//! Text that is not a path yet, such as a bare name, searches the favorite and
//! recent folders instead, so any of them is a few letters away.
use super::{breadcrumbs::home_dir, dir_handling::child_directories};
use fuzzy_matcher::FuzzyMatcher;
use fuzzy_matcher::skim::SkimMatcherV2;
use std::{
    borrow::Cow,
    cmp::Reverse,
    path::{MAIN_SEPARATOR, Path, PathBuf, is_separator},
};

/// Where a row of the dropdown comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    Subfolder,
    Favorite,
    Recent,
}

/// A folder that matches the typed text.
#[derive(Debug, Clone)]
pub struct PathMatch {
    /// Index into the folders searched.
    child: usize,
    /// The characters of the name that matched.
    pub indices: Vec<usize>,
}

#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Default)]
pub struct DirectoryPathInfo {
    pub text_input: String,
    /// Not edited since it opened: the text is the current folder, and its
    /// subfolders are what is useful to list.
    pristine: bool,
    show_hidden: bool,
    /// The text `matches` were made for.
    matched_text: Option<String>,
    /// The folder `children` came from.
    listed: Option<PathBuf>,
    found: bool,
    children: Vec<(String, PathBuf)>,
    /// Favorites first, then recent folders.
    shortcuts: Vec<(String, PathBuf)>,
    favorite_count: usize,
    /// The text is not a path, so `shortcuts` are searched, not `children`.
    searching_shortcuts: bool,
    /// The whole text is selected on opening, so typing replaces it.
    select_all_pending: bool,
    matches: Vec<PathMatch>,
    /// The row chosen with the keyboard. Hovering does not change it.
    pub selected: Option<usize>,
    /// How far the rows are scrolled.
    pub scroll_offset: f32,
}

impl DirectoryPathInfo {
    pub fn build(path: &Path) -> Self {
        Self {
            text_input: path.to_string_lossy().to_string(),
            pristine: true,
            select_all_pending: true,
            ..Default::default()
        }
    }

    /// Adds the folders a bare name searches. A recent folder that is also a
    /// favorite is listed once.
    #[must_use]
    pub fn with_shortcuts(mut self, favorites: Vec<(String, PathBuf)>, recent: &[PathBuf]) -> Self {
        self.favorite_count = favorites.len();
        self.shortcuts = favorites;
        for folder in recent {
            if self.shortcuts.iter().all(|(_, path)| path != folder) {
                let name = folder.file_name().map_or_else(
                    || folder.display().to_string(),
                    |name| name.to_string_lossy().into_owned(),
                );
                self.shortcuts.push((name, folder.clone()));
            }
        }
        self
    }

    /// Whether the text should be selected now. True only once.
    pub const fn take_select_all(&mut self) -> bool {
        let pending = self.select_all_pending;
        self.select_all_pending = false;
        pending
    }

    /// An editor whose folder is already read, for tests of paths that do not
    /// exist.
    #[cfg(test)]
    pub fn with_children(path: &Path, children: &[&str]) -> Self {
        Self {
            listed: Some(path.to_path_buf()),
            found: true,
            children: children
                .iter()
                .map(|name| ((*name).to_owned(), path.join(name)))
                .collect(),
            ..Self::build(path)
        }
    }

    pub const fn mark_edited(&mut self) {
        self.pristine = false;
        self.select_all_pending = false;
    }

    /// Brings the list up to date with the text. Returns whether it changed.
    pub fn refresh(&mut self, show_hidden: bool) -> bool {
        if self.matched_text.as_deref() == Some(self.text_input.as_str())
            && self.show_hidden == show_hidden
        {
            return false;
        }
        let expanded = expand_home(&self.text_input, home_dir()).into_owned();
        let query = split_query(&expanded, self.pristine);
        self.searching_shortcuts = query.is_none();
        if let Some((parent, leaf)) = query {
            let parent = Path::new(parent);
            if self.listed.as_deref() != Some(parent) || self.show_hidden != show_hidden {
                self.found = parent.is_dir();
                self.children = child_directories(parent, show_hidden);
                self.children
                    .sort_by_cached_key(|(name, _)| name.to_lowercase());
                self.listed = Some(parent.to_path_buf());
            }
            self.matches = rank(&self.children, leaf);
        } else {
            self.listed = None;
            self.children.clear();
            self.matches = rank(&self.shortcuts, &expanded);
        }
        self.show_hidden = show_hidden;
        self.matched_text = Some(self.text_input.clone());
        self.selected = None;
        true
    }

    pub fn matches(&self) -> &[PathMatch] {
        &self.matches
    }

    /// The folders the text is matched against.
    fn searched(&self) -> &[(String, PathBuf)] {
        if self.searching_shortcuts {
            &self.shortcuts
        } else {
            &self.children
        }
    }

    pub fn name_of(&self, row: &PathMatch) -> &str {
        &self.searched()[row.child].0
    }

    pub fn path_of(&self, row: &PathMatch) -> &Path {
        &self.searched()[row.child].1
    }

    pub const fn kind_of(&self, row: &PathMatch) -> RowKind {
        if !self.searching_shortcuts {
            RowKind::Subfolder
        } else if row.child < self.favorite_count {
            RowKind::Favorite
        } else {
            RowKind::Recent
        }
    }

    /// Where a shortcut's folder is, since its name alone may not say. The
    /// rows of a listed folder all share the header's path.
    pub fn location_of(&self, row: &PathMatch) -> Option<String> {
        if !self.searching_shortcuts {
            return None;
        }
        let parent = self.path_of(row).parent()?;
        Some(abbreviate_home(parent, home_dir()))
    }

    /// The dropdown's title: the folder being listed, written with `~` when
    /// it is under Home. `None` when there is nothing to show.
    pub fn header_label(&self) -> Option<String> {
        if self.searching_shortcuts {
            return (!self.shortcuts.is_empty()).then(|| "Favorites and recent".to_owned());
        }
        self.listed
            .as_deref()
            .map(|path| abbreviate_home(path, home_dir()))
    }

    /// The header's count of folders, or `None` when the folder is missing.
    pub fn count_label(&self) -> Option<String> {
        (self.searching_shortcuts || self.found)
            .then(|| count_label(self.matches.len(), self.searched().len()))
    }

    /// What the dropdown says in place of rows.
    pub const fn empty_label(&self) -> &'static str {
        if self.searching_shortcuts {
            "No matching folders"
        } else if !self.found {
            "Folder not found"
        } else if self.children.is_empty() {
            "No folders"
        } else {
            "No matching folders"
        }
    }

    /// Moves the keyboard selection. Returns whether it moved.
    pub fn move_selection(&mut self, down: bool, up: bool) -> bool {
        let next = next_selection(self.selected, down, up, self.matches.len());
        let moved = next != self.selected;
        self.selected = next;
        moved
    }

    /// The text Tab completes to: the selected row or else the top one, with
    /// a separator so the next level is listed.
    pub fn completion(&self) -> Option<String> {
        let row = self
            .selected
            .and_then(|index| self.matches.get(index))
            .or_else(|| self.matches.first())?;
        let mut text = self.path_of(row).to_string_lossy().into_owned();
        text.push(MAIN_SEPARATOR);
        Some(text)
    }

    /// The folder Enter opens: the selected row, or else the typed path. A
    /// bare name is no path, so it opens the best match.
    pub fn target(&self) -> PathBuf {
        self.selected
            .and_then(|index| self.matches.get(index))
            .or_else(|| {
                self.searching_shortcuts
                    .then(|| self.matches.first())
                    .flatten()
            })
            .map_or_else(
                || PathBuf::from(expand_home(&self.text_input, home_dir()).as_ref()),
                |row| self.path_of(row).to_path_buf(),
            )
    }
}

/// Splits the text into the folder to list and the name typed so far. Text
/// that names no folder yet, such as a bare name, gives `None`.
fn split_query(text: &str, pristine: bool) -> Option<(&str, &str)> {
    if text.is_empty() {
        return None;
    }
    if pristine || text.ends_with(is_separator) {
        return Some((text, ""));
    }
    let index = text.rfind(is_separator)?;
    Some(text.split_at(index + 1))
}

/// `~` and `~/rest` are under Home. `~name` is an ordinary name.
fn expand_home<'a>(text: &'a str, home: Option<&str>) -> Cow<'a, str> {
    let Some(rest) = text.strip_prefix('~') else {
        return Cow::Borrowed(text);
    };
    match home {
        Some(home) if rest.is_empty() || rest.starts_with(is_separator) => {
            Cow::Owned(format!("{}{rest}", home.trim_end_matches(is_separator)))
        }
        _ => Cow::Borrowed(text),
    }
}

fn abbreviate_home(path: &Path, home: Option<&str>) -> String {
    let Some(rest) = home.and_then(|home| path.strip_prefix(home).ok()) else {
        return path.display().to_string();
    };
    if rest.as_os_str().is_empty() {
        "~".to_owned()
    } else {
        format!("~{MAIN_SEPARATOR}{}", rest.display())
    }
}

fn count_label(shown: usize, total: usize) -> String {
    let noun = if total == 1 { "folder" } else { "folders" };
    if shown == total {
        format!("{total} {noun}")
    } else {
        format!("{shown} of {total} {noun}")
    }
}

/// The folders matching `leaf`: names that start with it first, then by score.
fn rank(children: &[(String, PathBuf)], leaf: &str) -> Vec<PathMatch> {
    if leaf.is_empty() {
        return (0..children.len())
            .map(|child| PathMatch {
                child,
                indices: Vec::new(),
            })
            .collect();
    }
    let matcher = SkimMatcherV2::default().ignore_case();
    let prefix = leaf.to_lowercase();
    let mut scored: Vec<(bool, i64, PathMatch)> = children
        .iter()
        .enumerate()
        .filter_map(|(child, (name, _))| {
            let (score, indices) = matcher.fuzzy_indices(name, leaf)?;
            let starts = name.to_lowercase().starts_with(&prefix);
            Some((starts, score, PathMatch { child, indices }))
        })
        .collect();
    scored.sort_by_key(|(starts, score, row)| (Reverse(*starts), Reverse(*score), row.child));
    scored.into_iter().map(|(_, _, row)| row).collect()
}

/// Up from the first row leaves the list, back to the typed text.
const fn next_selection(
    selected: Option<usize>,
    down: bool,
    up: bool,
    count: usize,
) -> Option<usize> {
    match selected {
        Some(index) if index >= count => None,
        Some(index) if down => {
            if index + 1 < count {
                Some(index + 1)
            } else {
                Some(index)
            }
        }
        Some(0) if up => None,
        Some(index) if up => Some(index - 1),
        None if down && count > 0 => Some(0),
        other => other,
    }
}

/// The scroll offset that brings `row` into view, moving as little as possible.
pub fn offset_to_reveal(row: usize, row_height: f32, viewport: f32, offset: f32) -> f32 {
    let top = row as f32 * row_height;
    if top < offset {
        top
    } else if top + row_height > offset + viewport {
        top + row_height - viewport
    } else {
        offset
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn children(names: &[&str]) -> Vec<(String, PathBuf)> {
        names
            .iter()
            .map(|name| ((*name).to_owned(), Path::new("/p").join(name)))
            .collect()
    }

    fn ranked(names: &[&str], leaf: &str) -> Vec<String> {
        let children = children(names);
        rank(&children, leaf)
            .iter()
            .map(|row| children[row.child].0.clone())
            .collect()
    }

    #[test]
    #[cfg(not(windows))]
    fn splits_the_text_into_folder_and_name() {
        assert_eq!(split_query("", false), None);
        assert_eq!(split_query("notes", false), None);
        assert_eq!(split_query("/", false), Some(("/", "")));
        assert_eq!(split_query("/Us", false), Some(("/", "Us")));
        assert_eq!(split_query("/Users/ann/", false), Some(("/Users/ann/", "")));
        assert_eq!(
            split_query("/Users/ann/Do", false),
            Some(("/Users/ann/", "Do"))
        );
    }

    #[test]
    #[cfg(windows)]
    fn splits_at_either_windows_separator() {
        assert_eq!(split_query(r"C:\Us", false), Some((r"C:\", "Us")));
        assert_eq!(split_query("C:/Us", false), Some(("C:/", "Us")));
        assert_eq!(split_query(r"C:\Users\", false), Some((r"C:\Users\", "")));
    }

    #[test]
    fn a_pristine_editor_lists_the_folder_it_opened_on() {
        assert_eq!(
            split_query("/Users/ann/Downloads", true),
            Some(("/Users/ann/Downloads", ""))
        );
    }

    #[test]
    #[cfg(not(windows))]
    fn expands_and_abbreviates_home() {
        let home = Some("/Users/ann");
        assert_eq!(expand_home("~", home), "/Users/ann");
        assert_eq!(expand_home("~/Docs", home), "/Users/ann/Docs");
        assert_eq!(expand_home("~bob/Docs", home), "~bob/Docs");
        assert_eq!(expand_home("~/Docs", None), "~/Docs");
        assert_eq!(expand_home("/tmp/~", home), "/tmp/~");

        assert_eq!(abbreviate_home(Path::new("/Users/ann"), home), "~");
        assert_eq!(
            abbreviate_home(Path::new("/Users/ann/Docs/2026"), home),
            "~/Docs/2026"
        );
        assert_eq!(
            abbreviate_home(Path::new("/Users/annie"), home),
            "/Users/annie"
        );
    }

    #[test]
    fn names_starting_with_the_text_rank_first() {
        let names = ["my-bevy", "Bevy-main", "other", "beta-v"];
        let found = ranked(&names, "bev");
        assert_eq!(found[0], "Bevy-main");
        assert!(found.contains(&"my-bevy".to_owned()));
        assert!(!found.contains(&"other".to_owned()));
    }

    #[test]
    fn an_empty_name_keeps_every_folder_in_order() {
        assert_eq!(ranked(&["b", "a"], ""), ["b", "a"]);
    }

    #[test]
    fn counts_read_naturally() {
        assert_eq!(count_label(1, 1), "1 folder");
        assert_eq!(count_label(142, 142), "142 folders");
        assert_eq!(count_label(3, 142), "3 of 142 folders");
        assert_eq!(count_label(0, 0), "0 folders");
    }

    #[test]
    fn selection_stops_at_the_last_row_and_leaves_upward() {
        assert_eq!(next_selection(None, false, false, 3), None);
        assert_eq!(next_selection(None, true, false, 3), Some(0));
        assert_eq!(next_selection(None, true, false, 0), None);
        assert_eq!(next_selection(Some(0), true, false, 2), Some(1));
        assert_eq!(next_selection(Some(1), true, false, 2), Some(1));
        assert_eq!(next_selection(Some(1), false, true, 2), Some(0));
        assert_eq!(next_selection(Some(0), false, true, 2), None);
        assert_eq!(next_selection(Some(5), false, false, 2), None);
    }

    #[test]
    fn reveals_a_row_by_the_smallest_scroll() {
        // Rows of 10 in a viewport of 30, scrolled to 20: rows 2-4 are visible.
        assert!((offset_to_reveal(3, 10.0, 30.0, 20.0) - 20.0).abs() < f32::EPSILON);
        assert!((offset_to_reveal(1, 10.0, 30.0, 20.0) - 10.0).abs() < f32::EPSILON);
        assert!((offset_to_reveal(5, 10.0, 30.0, 20.0) - 30.0).abs() < f32::EPSILON);
    }

    #[test]
    #[cfg(not(windows))]
    fn tab_completes_the_selected_row_or_the_top_one() {
        let mut info = DirectoryPathInfo::with_children(Path::new("/p"), &["Alpha", "Beta"]);
        assert!(info.refresh(false));
        assert_eq!(info.completion().as_deref(), Some("/p/Alpha/"));
        info.selected = Some(1);
        assert_eq!(info.completion().as_deref(), Some("/p/Beta/"));
        assert_eq!(info.target(), Path::new("/p/Beta"));
    }

    #[test]
    #[cfg(not(windows))]
    fn a_bare_name_searches_favorites_then_recent_folders() {
        let favorites = vec![("Work".to_owned(), PathBuf::from("/data/Work"))];
        let recent = [PathBuf::from("/data/Work"), PathBuf::from("/tmp/Workshop")];
        let mut info = DirectoryPathInfo::with_children(Path::new("/p"), &["Alpha"])
            .with_shortcuts(favorites, &recent);
        info.mark_edited();
        info.text_input.clear();
        assert!(info.refresh(false));
        // The recent copy of the favorite is left out.
        assert_eq!(info.matches().len(), 2);
        assert_eq!(info.header_label().as_deref(), Some("Favorites and recent"));
        let kinds: Vec<RowKind> = info.matches().iter().map(|r| info.kind_of(r)).collect();
        assert_eq!(kinds, [RowKind::Favorite, RowKind::Recent]);
        assert_eq!(
            info.location_of(&info.matches()[1]).as_deref(),
            Some("/tmp")
        );

        "shop".clone_into(&mut info.text_input);
        info.refresh(false);
        assert_eq!(info.count_label().as_deref(), Some("1 of 2 folders"));
        assert_eq!(info.target(), Path::new("/tmp/Workshop"));
        assert_eq!(info.completion().as_deref(), Some("/tmp/Workshop/"));

        "zzz".clone_into(&mut info.text_input);
        info.refresh(false);
        assert_eq!(info.empty_label(), "No matching folders");
    }

    #[test]
    fn without_shortcuts_a_bare_name_shows_no_dropdown() {
        let mut info = DirectoryPathInfo::with_children(Path::new("/p"), &["Alpha"]);
        info.mark_edited();
        "notes".clone_into(&mut info.text_input);
        info.refresh(false);
        assert_eq!(info.header_label(), None);
    }

    #[test]
    fn selects_the_text_only_on_opening() {
        let mut info = DirectoryPathInfo::build(Path::new("/p"));
        assert!(info.take_select_all());
        assert!(!info.take_select_all());
    }

    #[test]
    #[cfg(not(windows))]
    fn typing_filters_without_rereading_the_folder() {
        let mut info = DirectoryPathInfo::with_children(Path::new("/p"), &["Alpha", "Beta"]);
        info.refresh(false);
        assert_eq!(info.matches().len(), 2);
        info.selected = Some(1);
        info.mark_edited();
        "/p/be".clone_into(&mut info.text_input);
        assert!(info.refresh(false));
        // The missing /p would have emptied the list had it been read again.
        assert_eq!(info.matches().len(), 1);
        assert_eq!(info.selected, None);
        assert!(!info.refresh(false));
        assert_eq!(info.target(), Path::new("/p/be"));
    }
}
