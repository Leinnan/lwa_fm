//! The path bar, as egui's: crumbs from an anchor (Home, a mounted volume or
//! the root), each one's subfolders on request, and Go to Folder with a list
//! of matching folders as you type ('~' is Home; a bare name matches the
//! favorites and recent folders). A folder that doesn't exist is reported
//! instead of opened.
use std::cmp::Reverse;
use std::path::{Component, MAIN_SEPARATOR, Path};

use crux_core::Command;
use crux_core::capability::Operation;
use crux_core::render::render;
use facet::Facet;
use fuzzy_matcher::FuzzyMatcher;
use fuzzy_matcher::skim::SkimMatcherV2;
use serde::{Deserialize, Serialize};

use super::{DirFleet, Effect, Event, Model, Tab, tab_id, title_of};

/// Matches shown by Go to Folder at most.
const MATCHES_SHOWN: usize = 50;

/// The subfolders of `path`, by name, sorted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChildDirs {
    pub path: String,
    pub show_hidden: bool,
}

/// Subfolder names, or why the folder can't be read. Opaque to facet: it
/// never leaves the core.
#[derive(Clone, Debug, PartialEq, Eq, Facet)]
#[facet(opaque)]
pub struct ChildDirsOutput(pub Result<Vec<String>, String>);

impl Operation for ChildDirs {
    type Output = ChildDirsOutput;
}

/// What the first shown crumb stands for.
#[derive(Facet, Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub enum Anchor {
    Root,
    Home,
    Volume,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct CrumbView {
    pub title: String,
    pub path: String,
    pub anchor: Option<Anchor>,
}

/// A crumb's subfolders, once read.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ChildrenView {
    pub path: String,
    pub names: Vec<String>,
}

/// Go to Folder, while open.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct GoToView {
    pub text: String,
    pub matches: Vec<GoToMatch>,
    pub error: Option<String>,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct GoToMatch {
    pub title: String,
    pub path: String,
}

/// Go to Folder's state in a tab.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct GoTo {
    text: String,
    /// The folder whose subfolders are listed, and their names.
    listed: Option<(String, Vec<String>)>,
    error: Option<String>,
}

/// The crumbs of `path`: the folders above the anchor (only the anchor's
/// menu shows them) and the crumbs from the anchor on.
pub fn crumbs(path: &str, home: &str) -> (Vec<CrumbView>, Vec<CrumbView>) {
    let mut all: Vec<CrumbView> = Vec::new();
    let mut so_far = std::path::PathBuf::new();
    for component in Path::new(path).components() {
        so_far.push(component.as_os_str());
        let title = match component {
            Component::RootDir => MAIN_SEPARATOR.to_string(),
            Component::Prefix(prefix) => prefix.as_os_str().to_string_lossy().into_owned(),
            other => other.as_os_str().to_string_lossy().into_owned(),
        };
        // "C:" and "\" are one crumb on Windows.
        if matches!(component, Component::RootDir)
            && let Some(last) = all.last_mut()
        {
            last.path = so_far.to_string_lossy().into_owned();
            continue;
        }
        all.push(CrumbView {
            title,
            path: so_far.to_string_lossy().into_owned(),
            anchor: None,
        });
    }
    if all.is_empty() {
        return (Vec::new(), all);
    }
    let trimmed = |text: &str| text.trim_end_matches(['/', '\\']).to_owned();
    let home_index = (!home.is_empty())
        .then(|| {
            all.iter()
                .position(|crumb| trimmed(&crumb.path) == trimmed(home))
        })
        .flatten();
    let (index, anchor) = home_index.map_or_else(
        || {
            let names: Vec<&str> = all.iter().map(|crumb| crumb.title.as_str()).collect();
            volume_anchor(&names)
        },
        |index| (index, Anchor::Home),
    );
    let mut shown = all.split_off(index);
    shown[0].anchor = Some(anchor);
    if anchor == Anchor::Home {
        "Home".clone_into(&mut shown[0].title);
    }
    (all, shown)
}

/// Where a mounted volume starts: `/Volumes/<name>` on macOS,
/// `/media/<user>/<name>` or `/run/media/<user>/<name>` on Linux.
fn volume_anchor(names: &[&str]) -> (usize, Anchor) {
    match names {
        ["/", "Volumes", _, ..] => (2, Anchor::Volume),
        ["/", "media", _, _, ..] => (3, Anchor::Volume),
        ["/", "run", "media", _, _, ..] => (4, Anchor::Volume),
        _ => (0, Anchor::Root),
    }
}

const fn is_separator(c: char) -> bool {
    c == '/' || c == MAIN_SEPARATOR
}

/// `~` and `~/rest` are under Home; `~name` is an ordinary name.
fn expand_home(text: &str, home: &str) -> String {
    match text.strip_prefix('~') {
        Some(rest) if !home.is_empty() && (rest.is_empty() || rest.starts_with(is_separator)) => {
            format!("{}{rest}", home.trim_end_matches(is_separator))
        }
        _ => text.to_owned(),
    }
}

/// The folder to list and the name typed so far; `None` for a bare name.
fn split_query(text: &str) -> Option<(&str, &str)> {
    if text.is_empty() {
        return None;
    }
    if text.ends_with(is_separator) {
        return Some((text, ""));
    }
    let index = text.rfind(is_separator)?;
    Some(text.split_at(index + 1))
}

/// `names` matching `leaf`: names starting with it first, then by score.
fn rank<'a>(names: impl Iterator<Item = (&'a str, &'a str)>, leaf: &str) -> Vec<GoToMatch> {
    let matcher = SkimMatcherV2::default().ignore_case();
    let prefix = leaf.to_lowercase();
    let mut scored: Vec<(bool, i64, usize, GoToMatch)> = names
        .enumerate()
        .filter_map(|(index, (title, path))| {
            let score = if leaf.is_empty() {
                0
            } else {
                matcher.fuzzy_match(title, leaf)?
            };
            let starts = title.to_lowercase().starts_with(&prefix);
            Some((
                starts,
                score,
                index,
                GoToMatch {
                    title: title.to_owned(),
                    path: path.to_owned(),
                },
            ))
        })
        .collect();
    scored.sort_by_key(|(starts, score, index, _)| (Reverse(*starts), Reverse(*score), *index));
    scored
        .into_iter()
        .take(MATCHES_SHOWN)
        .map(|(_, _, _, found)| found)
        .collect()
}

/// The subfolders of `parent` matching `leaf`, once `parent` is listed.
fn child_matches(goto: &GoTo, parent: &str, leaf: &str) -> Vec<GoToMatch> {
    goto.listed
        .as_ref()
        .filter(|(listed, _)| listed == parent)
        .map(|(listed, names)| {
            let paths: Vec<String> = names
                .iter()
                .map(|name| Path::new(listed).join(name).to_string_lossy().into_owned())
                .collect();
            rank(
                names
                    .iter()
                    .map(String::as_str)
                    .zip(paths.iter().map(String::as_str)),
                leaf,
            )
        })
        .unwrap_or_default()
}

impl Tab {
    pub(super) fn crumb_views(&self, home: &str) -> (Vec<CrumbView>, Vec<CrumbView>) {
        self.path
            .as_deref()
            .map_or_else(|| (Vec::new(), Vec::new()), |path| crumbs(path, home))
    }

    pub(super) fn children_view(&self) -> Option<ChildrenView> {
        self.children.as_ref().map(|(path, names)| ChildrenView {
            path: path.clone(),
            names: names.clone(),
        })
    }
}

impl DirFleet {
    /// Go to Folder's matches for its text: the listed folder's subfolders,
    /// or, for a bare name, the favorites and recent folders.
    pub(super) fn goto_view(model: &Model, tab: &Tab) -> Option<GoToView> {
        let goto = tab.goto.as_ref()?;
        let expanded = expand_home(&goto.text, &model.home);
        let matches = split_query(&expanded).map_or_else(
            || Self::shortcut_matches(model, &expanded),
            |(parent, leaf)| child_matches(goto, parent, leaf),
        );
        Some(GoToView {
            text: goto.text.clone(),
            matches,
            error: goto.error.clone(),
        })
    }

    /// Opens Go to Folder at the tab's folder, its subfolders listed.
    fn open_goto(model: &mut Model, tab_number: u32) -> Command<Effect, Event> {
        let Some(state) = tab_id(tab_number).and_then(|id| model.tabs.get_mut(&id)) else {
            return Command::done();
        };
        let text = state.path.clone().map(|path| {
            let mut text = path;
            if !text.ends_with(is_separator) {
                text.push(MAIN_SEPARATOR);
            }
            text
        });
        state.goto = Some(GoTo::default());
        Self::pathbar_event(
            model,
            Event::GoToTyped {
                tab: tab_number,
                text: text.unwrap_or_default(),
            },
        )
    }

    /// Opens `path` (a chosen match), or what Go to Folder's text names, once
    /// it is known to exist. A bare name opens its best match.
    fn goto_submit(model: &Model, tab: u32, path: Option<String>) -> Command<Effect, Event> {
        let target = path.filter(|path| !path.is_empty()).or_else(|| {
            let state = model.tabs.get(&tab_id(tab)?)?;
            let expanded = expand_home(&state.goto.as_ref()?.text, &model.home);
            if split_query(&expanded).is_none() {
                return Self::goto_view(model, state)?
                    .matches
                    .first()
                    .map(|found| found.path.clone());
            }
            Some(expanded)
        });
        target.map_or_else(Command::done, |target| {
            Self::child_dirs(target.clone(), false, move |output| Event::GoToChecked {
                tab,
                target,
                output,
            })
        })
    }

    /// The favorites and recent folders matching a bare name.
    fn shortcut_matches(model: &Model, text: &str) -> Vec<GoToMatch> {
        let mut shortcuts: Vec<(String, String)> = model
            .sidebar
            .favorites
            .iter()
            .map(|favorite| (favorite.name.clone(), favorite.path.clone()))
            .collect();
        for path in &model.sidebar.recent {
            if shortcuts.iter().all(|(_, known)| known != path) {
                shortcuts.push((title_of(path), path.clone()));
            }
        }
        rank(
            shortcuts
                .iter()
                .map(|(title, path)| (title.as_str(), path.as_str())),
            text,
        )
    }

    fn child_dirs(
        path: String,
        show_hidden: bool,
        then: impl FnOnce(ChildDirsOutput) -> Event + Send + 'static,
    ) -> Command<Effect, Event> {
        Command::request_from_shell(ChildDirs { path, show_hidden }).then_send(then)
    }

    /// The events of the path bar and Go to Folder.
    pub(super) fn pathbar_event(model: &mut Model, event: Event) -> Command<Effect, Event> {
        let home = model.home.clone();
        match event {
            Event::GoToSubmit { tab, path } => return Self::goto_submit(model, tab, path),
            Event::ListChildren { tab, path } => {
                let show_hidden = tab_id(tab)
                    .and_then(|id| model.tabs.get(&id))
                    .is_some_and(|state| state.view.show_hidden);
                return Self::child_dirs(path.clone(), show_hidden, move |output| {
                    Event::ChildrenListed { tab, path, output }
                });
            }
            _ => {}
        }
        let tab_number = match &event {
            Event::ChildrenListed { tab, .. }
            | Event::OpenGoTo { tab }
            | Event::GoToTyped { tab, .. }
            | Event::GoToListed { tab, .. }
            | Event::GoToChecked { tab, .. }
            | Event::CloseGoTo { tab } => *tab,
            _ => return Command::done(),
        };
        let Some(state) = tab_id(tab_number).and_then(|id| model.tabs.get_mut(&id)) else {
            return Command::done();
        };
        let show_hidden = state.view.show_hidden;
        match event {
            Event::ChildrenListed { path, output, .. } => {
                state.children = Some((path, output.0.unwrap_or_default()));
                render()
            }
            Event::OpenGoTo { .. } => Self::open_goto(model, tab_number),
            Event::GoToTyped { tab, text } => {
                let Some(goto) = state.goto.as_mut() else {
                    return Command::done();
                };
                goto.text.clone_from(&text);
                goto.error = None;
                let expanded = expand_home(&text, &home);
                let parent = split_query(&expanded).map(|(parent, _)| parent.to_owned());
                match parent {
                    Some(parent)
                        if goto
                            .listed
                            .as_ref()
                            .is_none_or(|(listed, _)| *listed != parent) =>
                    {
                        render().and(Self::child_dirs(
                            parent.clone(),
                            show_hidden,
                            move |output| Event::GoToListed {
                                tab,
                                parent,
                                output,
                            },
                        ))
                    }
                    _ => render(),
                }
            }
            Event::GoToListed { parent, output, .. } => {
                if let Some(goto) = state.goto.as_mut() {
                    goto.listed = Some((parent, output.0.unwrap_or_default()));
                }
                render()
            }
            Event::GoToChecked {
                tab,
                target,
                output,
            } => {
                if output.0.is_ok() {
                    state.goto = None;
                    return Command::event(Event::Navigate { tab, path: target });
                }
                if let Some(goto) = state.goto.as_mut() {
                    goto.error = Some(format!("{target} does not exist"));
                }
                render()
            }
            Event::CloseGoTo { .. } => {
                state.goto = None;
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
    #[cfg(not(windows))]
    fn anchors_at_home_then_volume_then_root() {
        let (above, shown) = crumbs("/Users/ann/Documents", "/Users/ann");
        assert_eq!(above.len(), 2);
        assert_eq!(shown[0].title, "Home");
        assert_eq!(shown[0].anchor, Some(Anchor::Home));
        assert_eq!(shown[1].title, "Documents");
        assert_eq!(shown[1].path, "/Users/ann/Documents");

        let (above, shown) = crumbs("/Volumes/Backup SSD/2026", "/Users/ann");
        assert_eq!(above.len(), 2);
        assert_eq!(shown[0].title, "Backup SSD");
        assert_eq!(shown[0].anchor, Some(Anchor::Volume));

        let (above, shown) = crumbs("/media/ann/USB/photos", "");
        assert_eq!((above.len(), shown[0].title.as_str()), (3, "USB"));

        let (above, shown) = crumbs("/private/tmp", "/Users/ann");
        assert_eq!(above, []);
        assert_eq!(shown[0].anchor, Some(Anchor::Root));
        assert_eq!(shown[0].path, "/");
    }

    #[test]
    fn home_expands_and_names_split_from_their_folder() {
        assert_eq!(expand_home("~/Music", "/Users/ann"), "/Users/ann/Music");
        assert_eq!(expand_home("~ann", "/Users/ann"), "~ann");
        assert_eq!(split_query("/a/b"), Some(("/a/", "b")));
        assert_eq!(split_query("/a/"), Some(("/a/", "")));
        assert_eq!(split_query("work"), None);
    }

    #[test]
    fn names_starting_with_the_text_come_first() {
        let names = [("Downloads", "/d"), ("Documents", "/D"), ("Desktop", "/k")];
        let ranked = rank(names.into_iter(), "do");
        let titles: Vec<_> = ranked.iter().map(|found| found.title.as_str()).collect();
        assert_eq!(&titles[..2], ["Downloads", "Documents"]);
        assert_eq!(rank(names.into_iter(), "zz"), Vec::new());
    }
}
