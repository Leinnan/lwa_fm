//! Search state: the text being typed plus a list of chips (name patterns
//! and filters on type, extension, size, date and extra folders), the query
//! syntax that maps to the same chips.

pub mod filter;
pub mod query;
pub mod saved;

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Deserialize, Serialize, Default, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SearchTermType {
    #[default]
    Plain,
    Glob,
    Regex,
}

#[derive(Deserialize, Serialize, Default, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MatchMode {
    #[default]
    All,
    Any,
}

#[derive(Deserialize, Serialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FileKind {
    Folder,
    Image,
    Video,
    Audio,
    Document,
    Archive,
}

impl FileKind {
    pub const ALL: [Self; 6] = [
        Self::Folder,
        Self::Image,
        Self::Video,
        Self::Audio,
        Self::Document,
        Self::Archive,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Folder => "Folder",
            Self::Image => "Image",
            Self::Video => "Video",
            Self::Audio => "Audio",
            Self::Document => "Document",
            Self::Archive => "Archive",
        }
    }

    pub const fn keyword(self) -> &'static str {
        match self {
            Self::Folder => "folder",
            Self::Image => "image",
            Self::Video => "video",
            Self::Audio => "audio",
            Self::Document => "doc",
            Self::Archive => "archive",
        }
    }
}

#[derive(Deserialize, Serialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Cmp {
    Less,
    LessOrEqual,
    Equal,
    GreaterOrEqual,
    Greater,
}

impl Cmp {
    pub const ALL: [Self; 5] = [
        Self::Less,
        Self::LessOrEqual,
        Self::Equal,
        Self::GreaterOrEqual,
        Self::Greater,
    ];

    pub const fn symbol(self) -> &'static str {
        match self {
            Self::Less => "<",
            Self::LessOrEqual => "<=",
            Self::Equal => "=",
            Self::GreaterOrEqual => ">=",
            Self::Greater => ">",
        }
    }

    pub const fn holds(self, value: u64, reference: u64) -> bool {
        match self {
            Self::Less => value < reference,
            Self::LessOrEqual => value <= reference,
            Self::Equal => value == reference,
            Self::GreaterOrEqual => value >= reference,
            Self::Greater => value > reference,
        }
    }
}

/// How recently an item was modified.
#[derive(Deserialize, Serialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Age {
    Today,
    Week,
    Month,
    Year,
}

impl Age {
    pub const ALL: [Self; 4] = [Self::Today, Self::Week, Self::Month, Self::Year];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Today => "Today",
            Self::Week => "This week",
            Self::Month => "This month",
            Self::Year => "This year",
        }
    }

    pub const fn keyword(self) -> &'static str {
        match self {
            Self::Today => "today",
            Self::Week => "week",
            Self::Month => "month",
            Self::Year => "year",
        }
    }

    /// The longest age (in seconds) that still counts.
    pub const fn seconds(self) -> u64 {
        const DAY: u64 = 24 * 60 * 60;
        match self {
            Self::Today => DAY,
            Self::Week => 7 * DAY,
            Self::Month => 31 * DAY,
            Self::Year => 366 * DAY,
        }
    }
}

/// One part of a search, shown as a chip in the search bar.
#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, Eq, Hash)]
pub enum Chip {
    /// A pattern matched against the item name. `exclude` turns it into
    /// "name must not match".
    Name {
        pattern: String,
        kind: SearchTermType,
        exclude: bool,
    },
    /// File extensions, lower case, without the dot.
    Ext(Vec<String>),
    /// Any of these kinds. Older data stored a single kind.
    Kind(#[serde(deserialize_with = "one_or_many")] Vec<FileKind>),
    Size {
        cmp: Cmp,
        bytes: u64,
    },
    Modified(Age),
    /// Opened (in the app, or as the filesystem tracks it) this recently.
    Opened(Age),
    /// An extra folder to search in.
    In(PathBuf),
}

impl Chip {
    pub fn name(pattern: impl Into<String>) -> Self {
        Self::Name {
            pattern: pattern.into(),
            kind: SearchTermType::Plain,
            exclude: false,
        }
    }

    /// Whether this chip filters by name (and so takes part in "match all /
    /// match any").
    pub const fn is_name(&self) -> bool {
        matches!(self, Self::Name { exclude: false, .. })
    }

    /// A chip with nothing filled in yet (a new one in its editor). It
    /// filters nothing and is dropped when its editor closes.
    pub fn is_blank(&self) -> bool {
        match self {
            Self::Name { pattern, .. } => pattern.trim().is_empty(),
            Self::Ext(exts) => exts.is_empty(),
            Self::Kind(kinds) => kinds.is_empty(),
            Self::In(path) => path.as_os_str().is_empty(),
            Self::Size { .. } | Self::Modified(_) | Self::Opened(_) => false,
        }
    }
}

/// Accepts a single kind (older saved data) or a list.
fn one_or_many<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<FileKind>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        One(FileKind),
        Many(Vec<FileKind>),
    }
    Ok(match OneOrMany::deserialize(deserializer)? {
        OneOrMany::One(kind) => vec![kind],
        OneOrMany::Many(kinds) => kinds,
    })
}

/// Whether two size filters bound the same side ("over" or "under"), so a
/// new one replaces the old instead of contradicting it.
const fn same_bound(a: Cmp, b: Cmp) -> bool {
    matches!(
        (a, b),
        (
            Cmp::Greater | Cmp::GreaterOrEqual,
            Cmp::Greater | Cmp::GreaterOrEqual
        ) | (Cmp::Less | Cmp::LessOrEqual, Cmp::Less | Cmp::LessOrEqual)
            | (Cmp::Equal, Cmp::Equal)
    )
}

#[derive(Serialize, Default, Debug, Clone, PartialEq, Eq)]
pub struct Search {
    /// The text being typed. It is parsed like committed text, so it
    /// filters live exactly as the chips it will become.
    pub value: String,
    pub case_sensitive: bool,
    /// 1 searches the folder itself; more also searches subfolders.
    pub depth: usize,
    pub chips: Vec<Chip>,
    /// How name chips (and the typed text) combine.
    pub match_mode: MatchMode,
    /// The saved search this one was loaded from, to offer "Update".
    #[serde(skip)]
    pub saved_from: Option<u64>,
}

impl Search {
    /// Whether anything restricts the results.
    pub const fn has_query(&self) -> bool {
        !self.value.is_empty() || !self.chips.is_empty()
    }

    /// Extra folders added with `in:` chips.
    pub fn extra_dirs(&self) -> impl Iterator<Item = &PathBuf> {
        self.chips.iter().filter_map(|chip| match chip {
            Chip::In(path) if !path.as_os_str().is_empty() => Some(path),
            _ => None,
        })
    }

    /// Adds a chip, merging it with one of the same sort instead of adding a
    /// filter that contradicts it: types and extensions join the existing
    /// chip, and a date or size bound replaces the old one. Returns the undo
    /// label, or `None` when the search already had it.
    pub fn add_chip(&mut self, chip: Chip) -> Option<String> {
        let token = query::chip_token(&chip);
        let existing = self.chips.iter_mut().find(|old| match (&**old, &chip) {
            (Chip::Kind(_), Chip::Kind(_))
            | (Chip::Ext(_), Chip::Ext(_))
            | (Chip::Modified(_), Chip::Modified(_))
            | (Chip::Opened(_), Chip::Opened(_)) => true,
            (Chip::Size { cmp: a, .. }, Chip::Size { cmp: b, .. }) => same_bound(*a, *b),
            (old, new) => old == new,
        });
        let Some(old) = existing else {
            self.chips.push(chip);
            return Some(format!("add filter {token}"));
        };
        let before = old.clone();
        match (&mut *old, chip) {
            (Chip::Kind(kinds), Chip::Kind(new)) => {
                for kind in new {
                    if !kinds.contains(&kind) {
                        kinds.push(kind);
                    }
                }
            }
            (Chip::Ext(exts), Chip::Ext(new)) => {
                for ext in new {
                    if !exts.contains(&ext) {
                        exts.push(ext);
                    }
                }
            }
            (old, new) => *old = new,
        }
        (*old != before).then(|| format!("change filter {token}"))
    }

    /// Turns one type on or off in the Type chip. Returns the undo label.
    pub fn toggle_kind(&mut self, kind: FileKind) -> String {
        let position = self.chips.iter().position(|c| matches!(c, Chip::Kind(_)));
        match position.map(|index| (index, &mut self.chips[index])) {
            Some((index, Chip::Kind(kinds))) if kinds.contains(&kind) => {
                kinds.retain(|k| *k != kind);
                if kinds.is_empty() {
                    self.chips.remove(index);
                }
                format!("remove filter kind:{}", kind.keyword())
            }
            _ => self.add_chip(Chip::Kind(vec![kind])).unwrap_or_default(),
        }
    }

    /// The query without UI-only state, for comparing with a saved search.
    pub fn same_query(&self, other: &Self) -> bool {
        Self {
            saved_from: None,
            ..self.clone()
        } == Self {
            saved_from: None,
            ..other.clone()
        }
    }

    /// The typed text becomes chips: `key:value` tokens and words.
    pub fn commit_text(&mut self) -> bool {
        if self.value.trim().is_empty() {
            return false;
        }
        let parsed = query::parse(&self.value);
        if parsed.any {
            self.match_mode = MatchMode::Any;
        }
        for chip in parsed.chips {
            self.add_chip(chip);
        }
        self.value.clear();
        true
    }

    /// The chips plus what the typed text will become.
    pub fn effective_chips(&self) -> Vec<Chip> {
        let mut chips = self.chips.clone();
        if !self.value.trim().is_empty() {
            chips.extend(query::parse(&self.value).chips);
        }
        chips
    }

    /// How names combine, counting an `OR` in the typed text.
    pub fn effective_match_mode(&self) -> MatchMode {
        if !self.value.is_empty() && query::parse(&self.value).any {
            MatchMode::Any
        } else {
            self.match_mode
        }
    }

    /// Typed text that is one plain word and no chips: the common typing
    /// case, where each extra letter only narrows the previous results.
    pub fn simple_text(&self) -> Option<&str> {
        let text = self.value.trim();
        let plain = self.chips.is_empty()
            && !text.is_empty()
            && !text
                .contains(|c: char| c.is_whitespace() || matches!(c, ':' | '*' | '?' | '"' | '/'))
            && !text.starts_with('-');
        plain.then_some(text)
    }

    /// Number of name patterns, typed or committed.
    pub fn name_count(&self) -> usize {
        self.effective_chips()
            .iter()
            .filter(|c| c.is_name())
            .count()
    }
}

/// The search format before chips existed, still found in saved data.
#[derive(Deserialize)]
#[serde(default)]
struct SearchCompat {
    value: String,
    term_type: SearchTermType,
    case_sensitive: bool,
    depth: usize,
    chips: Vec<Chip>,
    match_mode: MatchMode,
    /// Old: separate terms, now name chips.
    terms: Vec<LegacyTerm>,
    /// Old: extra folders, now `in:` chips.
    extra_dirs: Vec<PathBuf>,
}

impl Default for SearchCompat {
    fn default() -> Self {
        Self {
            value: String::new(),
            term_type: SearchTermType::Plain,
            case_sensitive: false,
            depth: 1,
            chips: Vec::new(),
            match_mode: MatchMode::All,
            terms: Vec::new(),
            extra_dirs: Vec::new(),
        }
    }
}

#[derive(Deserialize)]
struct LegacyTerm {
    pattern: String,
    term_type: SearchTermType,
}

impl From<SearchCompat> for Search {
    fn from(old: SearchCompat) -> Self {
        let mut chips = old.chips;
        chips.extend(old.terms.into_iter().map(|term| Chip::Name {
            pattern: term.pattern,
            kind: term.term_type,
            exclude: false,
        }));
        chips.extend(old.extra_dirs.into_iter().map(Chip::In));
        // A typed glob or regex used to be a mode of the box; keep it as a chip.
        let mut value = old.value;
        if old.term_type != SearchTermType::Plain && !value.is_empty() {
            chips.insert(
                0,
                Chip::Name {
                    pattern: std::mem::take(&mut value),
                    kind: old.term_type,
                    exclude: false,
                },
            );
        }
        Self {
            value,
            case_sensitive: old.case_sensitive,
            depth: old.depth,
            chips,
            match_mode: old.match_mode,
            saved_from: None,
        }
    }
}

impl<'de> Deserialize<'de> for Search {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        SearchCompat::deserialize(deserializer).map(Self::from)
    }
}

#[cfg(test)]
mod tests {
    use super::{Chip, Search, SearchTermType};
    use std::path::PathBuf;

    #[test]
    fn old_searches_become_chips() {
        let old = r#"{"value":"rep","depth":3,"case_sensitive":true,"term_type":"Plain",
            "extra_dirs":["D:/work"],"terms":[{"pattern":"*.pdf","term_type":"Glob"}],
            "match_mode":"Any"}"#;
        let search: Search = serde_json::from_str(old).expect("parse");
        assert_eq!(search.value, "rep");
        assert_eq!(search.depth, 3);
        assert_eq!(
            search.chips,
            vec![
                Chip::Name {
                    pattern: "*.pdf".into(),
                    kind: SearchTermType::Glob,
                    exclude: false
                },
                Chip::In(PathBuf::from("D:/work")),
            ]
        );
        let round: Search =
            serde_json::from_str(&serde_json::to_string(&search).expect("save")).expect("load");
        assert_eq!(round, search);
    }

    #[test]
    fn same_kind_filters_merge_or_replace() {
        use super::{Age, Cmp, FileKind};
        let mut search = Search::default();
        assert!(search.add_chip(Chip::Kind(vec![FileKind::Image])).is_some());
        assert!(search.add_chip(Chip::Kind(vec![FileKind::Video])).is_some());
        assert!(search.add_chip(Chip::Kind(vec![FileKind::Image])).is_none());
        search.add_chip(Chip::Modified(Age::Week));
        search.add_chip(Chip::Modified(Age::Year));
        let over = |bytes| Chip::Size {
            cmp: Cmp::Greater,
            bytes,
        };
        search.add_chip(over(1));
        search.add_chip(over(2));
        search.add_chip(Chip::Size {
            cmp: Cmp::Less,
            bytes: 9,
        });
        assert_eq!(
            search.chips,
            vec![
                Chip::Kind(vec![FileKind::Image, FileKind::Video]),
                Chip::Modified(Age::Year),
                over(2),
                Chip::Size {
                    cmp: Cmp::Less,
                    bytes: 9
                },
            ]
        );
        search.toggle_kind(FileKind::Image);
        search.toggle_kind(FileKind::Video);
        assert!(!search.chips.iter().any(|c| matches!(c, Chip::Kind(_))));
    }

    #[test]
    fn single_kind_from_older_data_still_loads() {
        let chip: Chip = serde_json::from_str(r#"{"Kind":"Image"}"#).expect("old");
        assert_eq!(chip, Chip::Kind(vec![super::FileKind::Image]));
    }

    #[test]
    fn committing_text_turns_tokens_into_chips() {
        let mut search = Search {
            value: "report ext:pdf".into(),
            ..Default::default()
        };
        assert!(search.commit_text());
        assert_eq!(search.value, "");
        assert_eq!(
            search.chips,
            vec![Chip::name("report"), Chip::Ext(vec!["pdf".into()])]
        );
    }
}
