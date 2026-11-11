//! Archives opened as folders, and what a tab can do where it is.
//!
//! A tab inside an archive is a tab like any other: its path is the
//! archive's location (`archive:<file>!/folder`), listed by the effects
//! crate from the archive's own list of entries. Opening a file there
//! brings a copy out first. Nothing can be changed inside an archive.
use crux_core::Command;
use crux_core::capability::Operation;
use facet::Facet;
use serde::{Deserialize, Serialize};

use std::path::PathBuf;

use crux_core::render::render;

use super::{DirFleet, Effect, Event, Model, SystemOp, Tab, tab_id};
use crate::fileops::{ArchiveFormat, ConflictPolicy, FileOp, Origin, validate_name};
use crate::ids::TabId;
use crate::location::{Capabilities, Location, LocationKind};

/// Extensions of the archives that open as folders. Files that only happen
/// to be archives inside (documents, apps, installers) keep opening in
/// their apps, and so do single compressed files.
const EXTENSIONS: [&str; 12] = [
    "zip", "7z", "rar", "tar", "tgz", "tbz", "tbz2", "txz", "tzst", "tlz4", "cab", "cpio",
];

/// What a tar archive is called once it is compressed.
const COMPRESSED_TAR: [&str; 8] = [
    ".tar.gz",
    ".tar.bz2",
    ".tar.xz",
    ".tar.zst",
    ".tar.lz4",
    ".tar.lz",
    ".tar.lzma",
    ".tar.z",
];

/// Whether a file called `name` opens as a folder.
pub fn is_archive_name(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    COMPRESSED_TAR.iter().any(|end| name.ends_with(end))
        || name
            .rsplit_once('.')
            .is_some_and(|(stem, extension)| !stem.is_empty() && EXTENSIONS.contains(&extension))
}

/// Bring the item at `location` to this machine as a real file, to open it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Materialize {
    pub location: String,
}

/// Why an item can't be read where it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unreadable {
    /// The archive wants its password; `wrong` when the one given did not fit.
    Password {
        wrong: bool,
    },
    Failed(String),
}

/// Where the copy is, or why there is none. Opaque to facet: it never
/// leaves the core.
#[derive(Clone, Debug, PartialEq, Eq, Facet)]
#[facet(opaque)]
pub struct Materialized(pub Result<String, Unreadable>);

/// Text that must not be shown or kept: an archive's password on its way
/// from the shell to where archives are read. Its `Debug` says nothing.
#[derive(Facet, Serialize, Deserialize, Clone, PartialEq, Eq, Default)]
pub struct Secret {
    text: String,
}

impl Secret {
    pub fn new(text: impl Into<String>) -> Self {
        Self { text: text.into() }
    }

    /// The text itself, for whoever must use it.
    pub fn reveal(&self) -> &str {
        &self.text
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(…)")
    }
}

/// Use `password` for the archive that is the file at `archive` from now
/// on, until the app ends.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SetArchivePassword {
    pub archive: String,
    pub password: Secret,
}

impl Operation for SetArchivePassword {
    type Output = ();
}

/// An archive asks for its password.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct PasswordView {
    /// The archive's name.
    pub archive: String,
    /// The password given before did not fit.
    pub wrong: bool,
}

/// The archive about to be made of a tab's selection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Compress {
    /// The archive's name, without what its format adds.
    name: String,
    format: ArchiveFormat,
    /// What goes in.
    srcs: Vec<PathBuf>,
}

/// A format the archive can be made in.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct FormatChoice {
    pub format: ArchiveFormat,
    pub label: String,
}

/// The Compress dialog, while it is open.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct CompressView {
    /// The name as typed, without what the format adds.
    pub name: String,
    pub format: ArchiveFormat,
    pub formats: Vec<FormatChoice>,
    /// The archive's whole name ("Photos.tar.gz").
    pub file_name: String,
    /// How many items go in.
    pub count: u32,
    /// Why this name won't do; empty when it will.
    pub error: String,
}

impl Compress {
    fn file_name(&self) -> String {
        format!("{}.{}", self.name.trim(), self.format.extension())
    }
}

/// What waits for an archive's password, done again once it is given.
pub(super) enum Retry {
    /// List the tab again.
    List(TabId),
    /// Open the file at this location.
    Open(String),
    /// Run the extraction again, for the tab that asked for it.
    Extract { op: FileOp, tab: Option<TabId> },
}

struct Prompt {
    /// The archive's file, as a location.
    archive: String,
    wrong: bool,
    retry: Retry,
}

/// What the app knows about archives beyond the tabs inside them.
#[derive(Default)]
pub(super) struct Archives {
    prompt: Option<Prompt>,
}

impl Archives {
    pub(super) fn password_view(&self) -> Option<PasswordView> {
        self.prompt.as_ref().map(|prompt| PasswordView {
            archive: Location::parse(&prompt.archive).title(),
            wrong: prompt.wrong,
        })
    }
}

/// The file of the archive `location` is inside, as a location.
fn archive_of(location: &Location) -> Option<String> {
    match location {
        Location::Archive { outer, .. } => Some(outer.to_string()),
        _ => None,
    }
}

impl Operation for Materialize {
    type Output = Materialized;
}

/// What sort of place a tab shows and what can be done there, so a shell
/// can leave out what would do nothing.
#[derive(Facet, Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct LocationView {
    pub kind: LocationKind,
    pub caps: Capabilities,
}

/// What opening a row comes to.
pub(super) enum Opening {
    /// A folder, or an archive opened as one: the tab goes there.
    Browse(String),
    /// A file on this machine: its app opens it.
    Open(String),
    /// A file somewhere else: a copy is brought here, then opened.
    OpenElsewhere(String),
}

/// A folder on this machine, where everything can be done.
impl Default for LocationView {
    fn default() -> Self {
        let here = Location::Local(String::new());
        Self {
            kind: here.kind(),
            caps: here.default_caps(),
        }
    }
}

impl Tab {
    /// Whether the tab shows a folder on this machine (or the Trash).
    pub(super) fn is_local(&self) -> bool {
        self.location()
            .is_none_or(|location| location.as_local().is_some())
    }

    pub(super) fn location_view(&self) -> LocationView {
        let location = self
            .location()
            .unwrap_or_else(|| Location::Local(String::new()));
        LocationView {
            kind: location.kind(),
            caps: location.default_caps(),
        }
    }

    /// What opening the row at `row` does. `archives` says whether archives
    /// open as folders.
    pub(super) fn opening(&self, row: usize, archives: bool) -> Option<Opening> {
        let entry = *self.order.get(row)? as usize;
        let location = self.row_location(row)?;
        let meta = self.listing.meta(entry);
        if !meta.entry_type.is_leaf() {
            return Some(Opening::Browse(location.to_string()));
        }
        // A package is a folder shown as one item: its app opens it.
        let is_file = meta.entry_type == crate::listing::EntryType::File;
        if archives && is_file && is_archive_name(self.listing.name(entry)) {
            return Some(Opening::Browse(location.into_archive().to_string()));
        }
        Some(match location {
            Location::Local(path) => Opening::Open(path),
            elsewhere => Opening::OpenElsewhere(elsewhere.to_string()),
        })
    }
}

/// Why nothing can be changed at `location`; `None` where things can.
pub(super) const fn why_unchangeable(location: &Location) -> Option<&'static str> {
    if location.default_caps().writable {
        return None;
    }
    Some(match location.kind() {
        LocationKind::Archive => "What is inside an archive can't be changed",
        LocationKind::Local | LocationKind::Remote => "Nothing can be changed here",
    })
}

impl Tab {
    /// What Extract takes from the tab into `dst` (the folder the archive
    /// is in when `None`), one operation for each archive.
    ///
    /// Inside an archive: the selected entries, or the folder shown when
    /// nothing is selected. In a folder: each selected archive, whole.
    fn extractions(&self, dst: Option<&str>) -> Vec<FileOp> {
        let rows: Vec<usize> = self.selection.rows().collect();
        let policy = ConflictPolicy::KeepBoth;
        if let Some(Location::Archive { outer, inner }) = self.location() {
            let Some(dst) = dst
                .map(str::to_owned)
                .or_else(|| outer.parent()?.as_local().map(str::to_owned))
            else {
                return Vec::new();
            };
            let mut members: Vec<String> = rows
                .iter()
                .filter_map(|&row| match self.row_location(row)? {
                    Location::Archive { inner, .. } => Some(inner),
                    _ => None,
                })
                .collect();
            if members.is_empty() && !inner.is_empty() {
                members.push(inner);
            }
            return vec![FileOp::Extract {
                archive: PathBuf::from(outer.to_string()),
                // The whole archive keeps together; picked items land as they are.
                own_folder: members.is_empty(),
                members,
                dst: PathBuf::from(dst),
                policy,
            }];
        }
        let Some(here) = self.path.as_deref().filter(|_| self.is_local()) else {
            return Vec::new();
        };
        rows.into_iter()
            .filter_map(|row| {
                let entry = *self.order.get(row)? as usize;
                let is_file =
                    self.listing.meta(entry).entry_type == crate::listing::EntryType::File;
                let archive = self.row_path(row)?;
                (is_file && is_archive_name(self.listing.name(entry))).then(|| FileOp::Extract {
                    archive,
                    members: Vec::new(),
                    dst: PathBuf::from(dst.unwrap_or(here)),
                    policy,
                    own_folder: true,
                })
            })
            .collect()
    }

    /// The Compress dialog as it stands, with why its name won't do.
    pub(super) fn compress_view(&self) -> Option<CompressView> {
        let compress = self.compress.as_ref()?;
        let file_name = compress.file_name();
        let taken = (0..self.listing.len())
            .any(|entry| self.listing.name(entry).eq_ignore_ascii_case(&file_name));
        let error = if compress.name.trim().is_empty() {
            "The archive needs a name.".to_owned()
        } else if let Err(reason) = validate_name(&file_name) {
            reason.to_owned()
        } else if taken {
            format!("{file_name} already exists.")
        } else {
            String::new()
        };
        Some(CompressView {
            name: compress.name.clone(),
            format: compress.format,
            formats: ArchiveFormat::ALL
                .into_iter()
                .map(|format| FormatChoice {
                    format,
                    label: format!("{} (.{})", format.label(), format.extension()),
                })
                .collect(),
            file_name,
            count: u32::try_from(compress.srcs.len()).unwrap_or(u32::MAX),
            error,
        })
    }

    /// What Compress would put in an archive, and what to call it: one
    /// item gives its own name, several the folder's.
    fn compress_draft(&self) -> Option<Compress> {
        let folder = self.path.as_deref().filter(|_| self.is_local())?;
        let srcs = self.targets();
        let name = match srcs.as_slice() {
            [] => return None,
            [only] => {
                let is_dir = self
                    .single_item()
                    .and_then(|row| self.order.get(row))
                    .is_some_and(|&entry| !self.listing.meta(entry as usize).entry_type.is_leaf());
                let name = only.file_name()?.to_string_lossy().into_owned();
                // A file's name without its ending; a folder's as it is.
                match name.rsplit_once('.') {
                    Some((stem, _)) if !is_dir && !stem.is_empty() => stem.to_owned(),
                    _ => name,
                }
            }
            _ => Location::parse(folder).title(),
        };
        Some(Compress {
            name,
            format: ArchiveFormat::default(),
            srcs,
        })
    }

    /// Whether Extract has something to take from the tab.
    pub(super) fn can_extract(&self) -> bool {
        !self.extractions(Some("")).is_empty()
    }
}

impl DirFleet {
    /// Brings the file at `location` to this machine and opens the copy.
    pub(super) fn open_elsewhere(location: String) -> Command<Effect, Event> {
        let asked = location.clone();
        Command::request_from_shell(Materialize { location }).then_send(move |output| {
            Event::Materialized {
                location: asked,
                output,
            }
        })
    }

    /// The copy asked for by [`Self::open_elsewhere`] is here, or isn't.
    pub(super) fn materialized(
        model: &mut Model,
        location: String,
        output: Materialized,
    ) -> Command<Effect, Event> {
        match output.0 {
            Ok(path) => Self::system(SystemOp::Open(vec![path])),
            Err(Unreadable::Password { wrong }) => {
                if Self::ask_archive_password(model, wrong, Retry::Open(location)) {
                    render()
                } else {
                    Command::done()
                }
            }
            Err(Unreadable::Failed(reason)) => Self::notify(model, reason),
        }
    }

    /// Asks for the password of the archive `retry` is about. Says whether
    /// it asked: there is nothing to ask for what is in no archive.
    pub(super) fn ask_archive_password(model: &mut Model, wrong: bool, retry: Retry) -> bool {
        let archive = match &retry {
            Retry::List(tab) => model
                .tabs
                .get(tab)
                .and_then(Tab::location)
                .as_ref()
                .and_then(archive_of),
            Retry::Open(location) => archive_of(&Location::parse(location)),
            Retry::Extract {
                op: FileOp::Extract { archive, .. },
                ..
            } => Some(archive.to_string_lossy().into_owned()),
            Retry::Extract { .. } => None,
        };
        let Some(archive) = archive else {
            return false;
        };
        model.archives.prompt = Some(Prompt {
            archive,
            wrong,
            retry,
        });
        true
    }

    /// A tab's archive lists nothing without its password: the tab says
    /// so, and the password is asked for.
    pub(super) fn listing_needs_password(
        model: &mut Model,
        tab: u32,
        wrong: bool,
    ) -> Command<Effect, Event> {
        let Some(id) = tab_id(tab) else {
            return Command::done();
        };
        if let Some(state) = model.tabs.get_mut(&id) {
            state.error = Some("This archive is password protected.".to_owned());
            state.denied = false;
        }
        Self::ask_archive_password(model, wrong, Retry::List(id));
        render()
    }

    /// The events of the Compress dialog.
    fn compress_event(model: &mut Model, tab: u32, event: Event) -> Command<Effect, Event> {
        let Some(id) = tab_id(tab) else {
            return Command::done();
        };
        let Some(state) = model.tabs.get_mut(&id) else {
            return Command::done();
        };
        match event {
            Event::StartCompress { .. } => state.compress = state.compress_draft(),
            Event::SetCompress { name, format, .. } => {
                if let Some(compress) = &mut state.compress {
                    compress.name = name;
                    compress.format = format;
                }
            }
            Event::CancelCompress { .. } => state.compress = None,
            Event::ApplyCompress { .. } => {
                // A name that won't do keeps the dialog open, saying why.
                let fits = state
                    .compress_view()
                    .is_some_and(|view| view.error.is_empty());
                let folder = state.path.clone().filter(|_| fits);
                if let Some(folder) = folder
                    && let Some(compress) = state.compress.take()
                {
                    let op = FileOp::Compress {
                        dst: PathBuf::from(folder).join(compress.file_name()),
                        srcs: compress.srcs,
                        format: compress.format,
                    };
                    return Self::run(model, op, Origin::User, Some(id), false);
                }
            }
            _ => {}
        }
        render()
    }

    /// The events of extracting and of archive passwords.
    pub(super) fn archive_event(model: &mut Model, event: Event) -> Command<Effect, Event> {
        match event {
            Event::ExtractHere { tab } | Event::ExtractTo { tab, .. } => {
                let dst = match &event {
                    Event::ExtractTo { folder, .. } => Some(folder.as_str()),
                    _ => None,
                };
                let Some(id) = tab_id(tab) else {
                    return Command::done();
                };
                let ops = model
                    .tabs
                    .get(&id)
                    .map(|state| state.extractions(dst))
                    .unwrap_or_default();
                let started: Vec<_> = ops
                    .into_iter()
                    .map(|op| Self::run(model, op, Origin::User, Some(id), false))
                    .collect();
                Command::all(started)
            }
            Event::SubmitArchivePassword { password } => {
                let Some(prompt) = model.archives.prompt.take() else {
                    return Command::done();
                };
                let given: Command<Effect, Event> = Command::notify_shell(SetArchivePassword {
                    archive: prompt.archive,
                    password,
                })
                .into();
                let again = match prompt.retry {
                    Retry::List(tab) => Self::refresh(model, tab.number()),
                    Retry::Open(location) => Self::open_elsewhere(location),
                    Retry::Extract { op, tab } => Self::run(model, op, Origin::User, tab, false),
                };
                // In this order: the password is there before it is needed.
                given.then(again).and(render())
            }
            Event::CancelArchivePassword => {
                model.archives.prompt = None;
                render()
            }
            Event::StartCompress { tab }
            | Event::SetCompress { tab, .. }
            | Event::ApplyCompress { tab }
            | Event::CancelCompress { tab } => Self::compress_event(model, tab, event),
            _ => Command::done(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archives_are_told_from_other_files_by_name() {
        for name in [
            "a.zip",
            "A.ZIP",
            "b.7z",
            "c.rar",
            "d.tar",
            "e.tar.gz",
            "f.TGZ",
            "g.tar.zst",
            "h.cab",
        ] {
            assert!(is_archive_name(name), "{name}");
        }
        // Compressed single files, documents and installers open in their apps.
        for name in [
            "notes.txt",
            "a.gz",
            "report.docx",
            "app.jar",
            "setup.deb",
            "zip",
            ".zip",
            "tar.gz.txt",
        ] {
            assert!(!is_archive_name(name), "{name}");
        }
    }

    #[test]
    fn only_places_on_this_machine_can_be_changed() {
        assert_eq!(why_unchangeable(&Location::parse("/a/b")), None);
        assert!(why_unchangeable(&Location::parse("archive:/a/x.zip!/docs")).is_some());
    }
}
