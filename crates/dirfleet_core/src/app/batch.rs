//! Batch rename: several selected items renamed at once, as one undoable
//! step, with a live preview that flags names that can't be used.
//!
//! Every name goes through the same steps, in this order: it is built from
//! a template (`{name}`, `{ext}`, `{n}`, `{parent}`, `{modified}`,
//! `{created}`), text in it is found and replaced, characters are removed
//! from its ends and its spaces tidied, and its letters' case is changed.
//! A step left as it starts changes nothing.
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crux_core::Command;
use crux_core::capability::Operation;
use crux_core::render::render;
use facet::Facet;
use serde::{Deserialize, Serialize};

use super::{DirFleet, Effect, Event, Model, Tab, tab_id};
use crate::fileops::{FileOp, Origin, validate_name};
use crate::listing::EntryType;

/// How many items the dialog is sent; all of them are renamed.
pub const BATCH_PREVIEW: usize = 200;

/// The part of a name text is found and replaced in.
#[derive(Facet, Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[repr(C)]
pub enum BatchScope {
    /// The name with its extension.
    #[default]
    Whole,
    /// The name before its extension.
    Name,
    /// The extension, without its dot.
    Extension,
}

/// What is done to the case of a name's letters.
#[derive(Facet, Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[repr(C)]
pub enum BatchCase {
    #[default]
    Keep,
    /// Name and extension in small letters.
    Lower,
    /// Name and extension in capitals.
    Upper,
    /// Each word of the name with a capital, the extension as it is.
    Title,
    /// The name's first letter a capital, the extension as it is.
    Sentence,
}

/// The batch rename's fields, as the shell's dialog edits them.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "a dialog's switches, one flag each"
)]
pub struct BatchFields {
    /// What each name is built from: text with `{name}` (the name before
    /// its extension), `{ext}` (the extension with its dot), `{n}` (the
    /// counter), `{parent}` (the folder's name), `{modified}` and
    /// `{created}` (dates, as `2026-11-08`, or as the format after a
    /// colon says: `{modified:%Y%m%d-%H%M}`).
    pub template: String,
    pub find: String,
    pub replace: String,
    /// `find` is a regular expression, and `replace` may use `$1`.
    pub regex: bool,
    pub match_case: bool,
    pub scope: BatchScope,
    /// Characters taken off the start and the end of the name before its
    /// extension.
    pub remove_first: u32,
    pub remove_last: u32,
    /// Spaces at the name's ends go, and runs of them become one.
    pub trim: bool,
    pub case: BatchCase,
    /// The first `{n}`, how much it grows by, and how many digits it has
    /// at least (0: as many as the last number needs).
    pub start: u32,
    pub step: u32,
    pub padding: u32,
}

impl Default for BatchFields {
    // `{name}` and `{ext}` are the template's own placeholders.
    #[allow(clippy::literal_string_with_formatting_args)]
    fn default() -> Self {
        Self {
            template: "{name}{ext}".to_owned(),
            find: String::new(),
            replace: String::new(),
            regex: false,
            match_case: true,
            scope: BatchScope::Whole,
            remove_first: 0,
            remove_last: 0,
            trim: false,
            case: BatchCase::Keep,
            start: 1,
            step: 1,
            padding: 0,
        }
    }
}

/// One item's new name, and why it can't be used, if so.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct BatchItem {
    pub old: String,
    pub new: String,
    /// "" when the name can be used.
    pub problem: String,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct BatchRenameView {
    pub fields: BatchFields,
    /// The items, in their order; of more than [`BATCH_PREVIEW`] only that
    /// many, the ones whose name can't be used first.
    pub items: Vec<BatchItem>,
    /// How many items are being renamed.
    pub total: u32,
    /// Items whose name changes.
    pub changes: u32,
    /// Items whose new name can't be used: renaming waits until there are none.
    pub problems: u32,
    /// What is wrong with the regular expression, when it can't be read:
    /// renaming waits for that too.
    pub error: String,
}

/// How far local time is from UTC at each of `times` (Unix seconds), in
/// seconds: dates in names are the user's own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalOffsets {
    pub times: Vec<u64>,
}

impl Operation for LocalOffsets {
    type Output = Vec<i32>;
}

/// An item being renamed.
#[derive(Clone, Debug)]
struct Item {
    path: PathBuf,
    /// Folders have no extension.
    folder: bool,
    /// When it was changed and made, in Unix seconds, and how far local
    /// time was from UTC then (0 until that is known).
    modified: u64,
    created: u64,
    offsets: (i32, i32),
}

/// What the fields make of the items.
#[derive(Clone, Debug, Default)]
struct Renamed {
    names: Vec<String>,
    /// Why each name can't be used; "" when it can.
    problems: Vec<&'static str>,
    changes: u32,
    refused: u32,
    error: String,
}

/// The dialog's state, while it is open.
#[derive(Clone, Debug)]
pub(super) struct BatchRename {
    items: Vec<Item>,
    fields: BatchFields,
    /// The names in the items' folders, as the listing has them: each with
    /// its folder, written as names are told apart.
    taken: HashSet<(PathBuf, String)>,
    /// Local time was asked about, for dates in names.
    offsets_asked: bool,
    renamed: Renamed,
}

fn name_of(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// A name as disks that take one spelling for another tell names apart.
fn folded(name: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    name.nfc().flat_map(char::to_lowercase).collect()
}

/// A name before its extension, and the extension with its dot ("" for a
/// folder, and for a name that has none).
fn split(name: &str, folder: bool) -> (&str, &str) {
    match name.rfind('.') {
        Some(dot) if dot > 0 && !folder => name.split_at(dot),
        _ => (name, ""),
    }
}

/// A time as `format` says: `%Y %y %m %d %H %M %S`, `%%` for a percent
/// sign, anything else as written.
fn date(seconds: i64, format: &str) -> String {
    let days = seconds.div_euclid(86_400);
    let in_day = seconds.rem_euclid(86_400);
    let (year, month, day) = super::format::civil_from_days(days);
    let mut text = String::new();
    let mut chars = format.chars();
    while let Some(character) = chars.next() {
        if character != '%' {
            text.push(character);
            continue;
        }
        let _ = match chars.next() {
            Some('Y') => write!(text, "{year:04}"),
            Some('y') => write!(text, "{:02}", year.rem_euclid(100)),
            Some('m') => write!(text, "{month:02}"),
            Some('d') => write!(text, "{day:02}"),
            Some('H') => write!(text, "{:02}", in_day / 3600),
            Some('M') => write!(text, "{:02}", in_day % 3600 / 60),
            Some('S') => write!(text, "{:02}", in_day % 60),
            Some('%') | None => write!(text, "%"),
            Some(other) => write!(text, "%{other}"),
        };
    }
    text
}

/// A name with each word's first letter a capital and the rest small.
fn title_case(name: &str) -> String {
    let mut text = String::with_capacity(name.len());
    let mut in_word = false;
    for character in name.chars() {
        if !character.is_alphanumeric() {
            in_word = false;
            text.push(character);
        } else if in_word {
            text.extend(character.to_lowercase());
        } else {
            in_word = true;
            text.extend(character.to_uppercase());
        }
    }
    text
}

/// A name with its first letter a capital and the rest small.
fn sentence_case(name: &str) -> String {
    let mut text = String::with_capacity(name.len());
    let mut started = false;
    for character in name.chars() {
        if started || !character.is_alphanumeric() {
            text.extend(character.to_lowercase());
        } else {
            started = true;
            text.extend(character.to_uppercase());
        }
    }
    text
}

/// What finds the text to replace.
enum Finder {
    /// Nothing is looked for.
    Nothing,
    Found(regex::Regex),
    /// The expression can't be read.
    Broken(String),
}

impl Finder {
    fn new(fields: &BatchFields) -> Self {
        if fields.find.is_empty() {
            return Self::Nothing;
        }
        let pattern = if fields.regex {
            fields.find.clone()
        } else {
            regex::escape(&fields.find)
        };
        match regex::RegexBuilder::new(&pattern)
            .case_insensitive(!fields.match_case)
            .build()
        {
            Ok(regex) => Self::Found(regex),
            Err(error) => Self::Broken(
                error
                    .to_string()
                    .lines()
                    .last()
                    .unwrap_or_default()
                    .trim()
                    .to_owned(),
            ),
        }
    }

    fn replace(&self, text: &str, fields: &BatchFields) -> String {
        match self {
            Self::Found(regex) if fields.regex => regex
                .replace_all(text, fields.replace.as_str())
                .into_owned(),
            Self::Found(regex) => regex
                .replace_all(text, regex::NoExpand(&fields.replace))
                .into_owned(),
            Self::Nothing | Self::Broken(_) => text.to_owned(),
        }
    }
}

impl Item {
    /// The template with its placeholders filled in for this item, whose
    /// counter reads `number`.
    // `{name}` and friends are the template's own placeholders.
    #[allow(clippy::literal_string_with_formatting_args)]
    fn templated(&self, template: &str, number: &str) -> String {
        let name = name_of(&self.path);
        let (stem, ext) = split(&name, self.folder);
        let mut text = String::new();
        let mut rest = template;
        while let Some(open) = rest.find('{') {
            text.push_str(&rest[..open]);
            rest = &rest[open..];
            // A brace never closed is part of the name.
            let Some(close) = rest.find('}') else {
                break;
            };
            let key = &rest[1..close];
            let (key, format) = key.split_once(':').unwrap_or((key, "%Y-%m-%d"));
            let local = |time: u64, offset: i32| {
                i64::try_from(time).unwrap_or(i64::MAX) + i64::from(offset)
            };
            match key {
                "name" => text.push_str(stem),
                "ext" => text.push_str(ext),
                "n" => text.push_str(number),
                "parent" => text.push_str(&self.path.parent().map(name_of).unwrap_or_default()),
                "modified" => text.push_str(&date(local(self.modified, self.offsets.0), format)),
                "created" => text.push_str(&date(local(self.created, self.offsets.1), format)),
                // Not a placeholder: braces are part of the name.
                _ => text.push_str(&rest[..=close]),
            }
            rest = &rest[close + 1..];
        }
        text.push_str(rest);
        text
    }

    /// The item's new name: the template filled in, then each step of
    /// `fields` in turn.
    fn renamed(&self, fields: &BatchFields, finder: &Finder, number: &str) -> String {
        let name = self.templated(&fields.template, number);
        let name = match fields.scope {
            BatchScope::Whole => finder.replace(&name, fields),
            BatchScope::Name => {
                let (stem, ext) = split(&name, self.folder);
                format!("{}{ext}", finder.replace(stem, fields))
            }
            BatchScope::Extension => {
                let (stem, ext) = split(&name, self.folder);
                let ext = finder.replace(ext.trim_start_matches('.'), fields);
                if ext.is_empty() {
                    stem.to_owned()
                } else {
                    format!("{stem}.{ext}")
                }
            }
        };
        let (stem, ext) = split(&name, self.folder);
        let length = stem.chars().count();
        let kept = length
            .saturating_sub(fields.remove_first as usize)
            .saturating_sub(fields.remove_last as usize);
        let mut stem: String = stem
            .chars()
            .skip(fields.remove_first as usize)
            .take(kept)
            .collect();
        if fields.trim {
            stem = stem.split_whitespace().collect::<Vec<_>>().join(" ");
        }
        match fields.case {
            BatchCase::Keep => format!("{stem}{ext}"),
            BatchCase::Lower => format!("{stem}{ext}").to_lowercase(),
            BatchCase::Upper => format!("{stem}{ext}").to_uppercase(),
            BatchCase::Title => format!("{}{ext}", title_case(&stem)),
            BatchCase::Sentence => format!("{}{ext}", sentence_case(&stem)),
        }
    }

    /// The item's folder and `name` in it, as names are told apart.
    fn place(&self, name: &str) -> (PathBuf, String) {
        (
            self.path
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_default(),
            folded(name),
        )
    }
}

impl BatchRename {
    /// Whether names take dates, which are told in local time.
    fn takes_dates(&self) -> bool {
        self.fields.template.contains("{modified") || self.fields.template.contains("{created")
    }

    /// Works the names out anew: after the fields changed, or local time
    /// became known.
    fn rename(&mut self) {
        let fields = &self.fields;
        let finder = Finder::new(fields);
        let number_at =
            |index: usize| u64::from(fields.start) + index as u64 * u64::from(fields.step);
        let width = if fields.padding > 0 {
            fields.padding as usize
        } else {
            number_at(self.items.len().saturating_sub(1))
                .to_string()
                .len()
        };
        let names: Vec<String> = self
            .items
            .iter()
            .enumerate()
            .map(|(index, item)| {
                let number = format!("{:0width$}", number_at(index));
                item.renamed(fields, &finder, &number)
            })
            .collect();
        // How many items take each name, and the names the items leave.
        let mut wanted: HashMap<(PathBuf, String), u32> = HashMap::new();
        for (item, name) in self.items.iter().zip(&names) {
            *wanted.entry(item.place(name)).or_default() += 1;
        }
        let left: HashSet<(PathBuf, String)> = self
            .items
            .iter()
            .map(|item| item.place(&name_of(&item.path)))
            .collect();
        let mut changes = 0;
        let problems: Vec<&'static str> = self
            .items
            .iter()
            .zip(&names)
            .map(|(item, name)| {
                let renamed = *name != name_of(&item.path);
                changes += u32::from(renamed);
                if let Err(problem) = validate_name(name) {
                    return problem;
                }
                let place = item.place(name);
                if wanted.get(&place).copied().unwrap_or_default() > 1 {
                    "Two items would get this name."
                } else if renamed && self.taken.contains(&place) && !left.contains(&place) {
                    // A name one of the items leaves is free to take.
                    "An item with this name already exists."
                } else {
                    ""
                }
            })
            .collect();
        self.renamed = Renamed {
            refused: u32::try_from(
                problems
                    .iter()
                    .filter(|problem| !problem.is_empty())
                    .count(),
            )
            .unwrap_or(u32::MAX),
            names,
            problems,
            changes,
            error: match finder {
                Finder::Broken(error) => error,
                Finder::Nothing | Finder::Found(_) => String::new(),
            },
        };
    }

    pub(super) fn view(&self) -> BatchRenameView {
        let renamed = &self.renamed;
        let item = |index: usize| {
            Some(BatchItem {
                old: name_of(&self.items.get(index)?.path),
                new: renamed.names.get(index)?.clone(),
                problem: (*renamed.problems.get(index)?).to_owned(),
            })
        };
        // Few enough to be sent whole: in their own order.
        let long = self.items.len() > BATCH_PREVIEW;
        let refused = |index: &usize| {
            long && renamed
                .problems
                .get(*index)
                .is_some_and(|problem| !problem.is_empty())
        };
        // A long selection is shown by its beginning, after the names that
        // can't be used: those must be seen to be put right.
        let all = 0..self.items.len();
        let items = all
            .clone()
            .filter(refused)
            .chain(all.filter(|index| !refused(index)))
            .take(BATCH_PREVIEW)
            .filter_map(item)
            .collect();
        BatchRenameView {
            fields: self.fields.clone(),
            items,
            total: u32::try_from(self.items.len()).unwrap_or(u32::MAX),
            changes: renamed.changes,
            problems: renamed.refused,
            error: renamed.error.clone(),
        }
    }

    /// Asks how far local time was from UTC at the items' dates, the first
    /// time names take dates.
    fn ask_offsets(&mut self, tab: u32) -> Command<Effect, Event> {
        if self.offsets_asked || !self.takes_dates() {
            return Command::done();
        }
        self.offsets_asked = true;
        let times: Vec<u64> = self
            .items
            .iter()
            .flat_map(|item| [item.modified, item.created])
            .collect();
        let asked = times.clone();
        Command::request_from_shell(LocalOffsets { times }).then_send(move |offsets| {
            Event::BatchOffsets {
                tab,
                times: asked,
                offsets,
            }
        })
    }

    /// Takes the answer, when it is about these items still.
    fn take_offsets(&mut self, times: &[u64], offsets: &[i32]) {
        let same = times.len() == self.items.len() * 2
            && offsets.len() == times.len()
            && self
                .items
                .iter()
                .zip(times.chunks(2))
                .all(|(item, times)| times == [item.modified, item.created]);
        if !same {
            return;
        }
        for (item, offsets) in self.items.iter_mut().zip(offsets.chunks(2)) {
            if let [modified, created] = offsets {
                item.offsets = (*modified, *created);
            }
        }
        self.rename();
    }

    /// Each item whose name changes, with its new path.
    fn pairs(&self) -> Vec<(PathBuf, PathBuf)> {
        self.items
            .iter()
            .zip(&self.renamed.names)
            .filter(|(item, name)| **name != name_of(&item.path))
            .map(|(item, name)| (item.path.clone(), item.path.with_file_name(name)))
            .collect()
    }
}

impl Tab {
    /// Batch rename for the selection, when several items are selected
    /// where names can be changed: in folders of this machine.
    pub(super) fn start_batch(&self) -> Option<BatchRename> {
        let rows: Vec<usize> = self.selection.rows().collect();
        if rows.len() < 2
            || self.trash
            || self.results.is_some()
            || !self.is_local()
            || !self.location_view().caps.rename
        {
            return None;
        }
        let items = rows
            .into_iter()
            .filter_map(|row| {
                let entry = *self.order.get(row)? as usize;
                let meta = self.listing.meta(entry);
                Some(Item {
                    path: self.row_path(row)?,
                    folder: meta.entry_type == EntryType::Directory,
                    modified: u64::from(*meta.modified_at),
                    created: u64::from(*meta.created_at),
                    offsets: (0, 0),
                })
            })
            .collect();
        let taken = (0..self.order.len())
            .filter_map(|row| self.row_path(row))
            .map(|path| {
                (
                    path.parent().map(Path::to_path_buf).unwrap_or_default(),
                    folded(&name_of(&path)),
                )
            })
            .collect();
        let mut batch = BatchRename {
            items,
            fields: BatchFields::default(),
            taken,
            offsets_asked: false,
            renamed: Renamed::default(),
        };
        batch.rename();
        Some(batch)
    }
}

impl DirFleet {
    /// The events of the batch rename dialog.
    pub(super) fn batch_event(model: &mut Model, event: Event) -> Command<Effect, Event> {
        let tab = match &event {
            Event::StartBatchRename { tab }
            | Event::SetBatchRename { tab, .. }
            | Event::ApplyBatchRename { tab }
            | Event::CancelBatchRename { tab }
            | Event::BatchOffsets { tab, .. } => *tab,
            _ => return Command::done(),
        };
        let Some(id) = tab_id(tab).filter(|id| model.tabs.contains_key(id)) else {
            return Command::done();
        };
        let state = model.tabs.get_mut(&id).expect("checked above");
        match event {
            Event::StartBatchRename { .. } => state.batch = state.start_batch(),
            Event::SetBatchRename { fields, .. } => {
                let Some(batch) = state.batch.as_mut() else {
                    return Command::done();
                };
                batch.fields = fields;
                batch.rename();
                return render().and(batch.ask_offsets(tab));
            }
            Event::BatchOffsets { times, offsets, .. } => {
                if let Some(batch) = state.batch.as_mut() {
                    batch.take_offsets(&times, &offsets);
                }
            }
            Event::CancelBatchRename { .. } => state.batch = None,
            Event::ApplyBatchRename { .. } => {
                let Some(batch) = state.batch.as_ref() else {
                    return Command::done();
                };
                let renamed = &batch.renamed;
                if renamed.refused > 0 || renamed.changes == 0 || !renamed.error.is_empty() {
                    return Command::done();
                }
                let pairs = batch.pairs();
                state.select_after = pairs.iter().map(|(_, to)| to.clone()).collect();
                state.batch = None;
                return Self::run(
                    model,
                    FileOp::RenameMany(pairs),
                    Origin::User,
                    Some(id),
                    false,
                );
            }
            _ => {}
        }
        render()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(path: &str, folder: bool) -> Item {
        Item {
            path: PathBuf::from(path),
            folder,
            // 2025-11-08 14:03:00 and 2024-02-29 23:59:59, UTC.
            modified: 1_762_610_580,
            created: 1_709_251_199,
            offsets: (0, 0),
        }
    }

    fn renamed(item: &Item, fields: &BatchFields) -> String {
        item.renamed(fields, &Finder::new(fields), "007")
    }

    // `{name}` and friends are the template's own placeholders.
    #[allow(clippy::literal_string_with_formatting_args)]
    #[test]
    fn a_name_is_built_from_a_template_of_placeholders() {
        let photo = item("/trips/Rome/IMG_1.JPG", false);
        let with = |template: &str| {
            renamed(
                &photo,
                &BatchFields {
                    template: template.to_owned(),
                    ..BatchFields::default()
                },
            )
        };
        assert_eq!(with("{name}{ext}"), "IMG_1.JPG", "as it starts: no change");
        assert_eq!(with("{parent} {n}{ext}"), "Rome 007.JPG");
        assert_eq!(with("{modified} {name}{ext}"), "2025-11-08 IMG_1.JPG");
        assert_eq!(with("{created:%y%m%d-%H%M%S}{ext}"), "240229-235959.JPG");
        assert_eq!(with("{modified:%d.%m.%Y 100%%}"), "08.11.2025 100%");
        // What is no placeholder stays as written, braces and all.
        assert_eq!(with("{nam} {name"), "{nam} {name");
        // A folder has no extension, whatever dots its name has.
        let folder = item("/trips/Rome/v1.2", true);
        let fields = BatchFields {
            template: "{name}|{ext}".to_owned(),
            ..BatchFields::default()
        };
        assert_eq!(renamed(&folder, &fields), "v1.2|");
        // Local time moves a date: an hour before midnight, two hours east.
        let mut late = item("/x/a.txt", false);
        late.modified = 1_762_642_800; // 2025-11-08 23:00:00 UTC
        late.offsets = (7200, 0);
        let fields = BatchFields {
            template: "{modified:%Y-%m-%d %H}".to_owned(),
            ..BatchFields::default()
        };
        assert_eq!(renamed(&late, &fields), "2025-11-09 01");
    }

    #[test]
    fn text_is_found_and_replaced_plainly_or_by_expression() {
        let file = item("/x/Report.final.TXT", false);
        let replacing = |fields: BatchFields| renamed(&file, &fields);
        let plain = |find: &str, replace: &str| BatchFields {
            find: find.to_owned(),
            replace: replace.to_owned(),
            ..BatchFields::default()
        };
        assert_eq!(replacing(plain(".", "_")), "Report_final_TXT");
        assert_eq!(replacing(plain("report", "Notes")), "Report.final.TXT");
        assert_eq!(
            replacing(BatchFields {
                match_case: false,
                ..plain("report", "Notes")
            }),
            "Notes.final.TXT"
        );
        // Plain text is taken as written, signs and all.
        assert_eq!(replacing(plain("final", "$1")), "Report.$1.TXT");
        // In the name alone, or the extension alone.
        assert_eq!(
            replacing(BatchFields {
                scope: BatchScope::Name,
                ..plain(".", " ")
            }),
            "Report final.TXT"
        );
        assert_eq!(
            replacing(BatchFields {
                scope: BatchScope::Extension,
                match_case: false,
                ..plain("txt", "md")
            }),
            "Report.final.md"
        );
        assert_eq!(
            replacing(BatchFields {
                scope: BatchScope::Extension,
                ..plain("TXT", "")
            }),
            "Report.final",
            "an extension replaced by nothing takes its dot with it"
        );
        // An expression, with what it caught put back.
        let regex = |find: &str, replace: &str| BatchFields {
            regex: true,
            ..plain(find, replace)
        };
        assert_eq!(
            replacing(regex(r"^(\w+)\.(\w+)", "$2 $1")),
            "final Report.TXT"
        );
        assert_eq!(replacing(regex(r"[aeiou]", "")), "Rprt.fnl.TXT");
        // One that can't be read replaces nothing, and says so.
        let broken = regex("(", "x");
        assert_eq!(replacing(broken.clone()), "Report.final.TXT");
        assert!(matches!(Finder::new(&broken), Finder::Broken(error) if !error.is_empty()));
    }

    #[test]
    fn ends_are_removed_spaces_tidied_and_case_changed() {
        let file = item("/x/  IMG_2031 the   big day .Jpg", false);
        let with = |fields: BatchFields| renamed(&file, &fields);
        assert_eq!(
            with(BatchFields {
                trim: true,
                ..BatchFields::default()
            }),
            "IMG_2031 the big day.Jpg"
        );
        assert_eq!(
            with(BatchFields {
                remove_first: 11,
                remove_last: 1,
                ..BatchFields::default()
            }),
            "the   big day.Jpg"
        );
        assert_eq!(
            with(BatchFields {
                remove_first: 500,
                ..BatchFields::default()
            }),
            ".Jpg",
            "more than there is leaves nothing, which is then refused"
        );
        let cased = |case| {
            with(BatchFields {
                trim: true,
                case,
                ..BatchFields::default()
            })
        };
        assert_eq!(cased(BatchCase::Lower), "img_2031 the big day.jpg");
        assert_eq!(cased(BatchCase::Upper), "IMG_2031 THE BIG DAY.JPG");
        assert_eq!(cased(BatchCase::Title), "Img_2031 The Big Day.Jpg");
        assert_eq!(cased(BatchCase::Sentence), "Img_2031 the big day.Jpg");
    }
}
