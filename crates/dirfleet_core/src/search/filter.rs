//! Compiles a [`Search`] into a predicate over listing entries.
//!
//! Pure: the current time and, for `opened:` filters, when each entry was
//! last opened are passed in by the caller.

use super::{Chip, Cmp, FileKind, MatchMode, Search, SearchTermType};
use crate::listing::kinds::VIDEO_EXTS;
use crate::listing::{EntryType, FoldedNames, Listing, collator};
use std::cmp::Ordering;

const IMAGE_EXTS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "bmp", "webp", "tif", "tiff", "ico", "svg", "heic", "avif", "raw",
    "psd",
];
const AUDIO_EXTS: &[&str] = &[
    "mp3", "wav", "flac", "ogg", "m4a", "aac", "wma", "opus", "aiff",
];
const DOCUMENT_EXTS: &[&str] = &[
    "pdf", "doc", "docx", "odt", "rtf", "txt", "md", "xls", "xlsx", "ods", "csv", "ppt", "pptx",
    "odp", "epub",
];
const ARCHIVE_EXTS: &[&str] = &["zip", "rar", "7z", "tar", "gz", "bz2", "xz", "zst", "iso"];

fn extension(name: &str) -> Option<String> {
    let (stem, ext) = name.rsplit_once('.')?;
    (!stem.is_empty()).then(|| ext.to_ascii_lowercase())
}

/// The kind of a file, judged by its extension.
pub fn kind_of(name: &str, is_dir: bool) -> Option<FileKind> {
    if is_dir {
        return Some(FileKind::Folder);
    }
    let ext = extension(name)?;
    let ext = ext.as_str();
    if IMAGE_EXTS.contains(&ext) {
        Some(FileKind::Image)
    } else if VIDEO_EXTS.contains(&ext) {
        Some(FileKind::Video)
    } else if AUDIO_EXTS.contains(&ext) {
        Some(FileKind::Audio)
    } else if DOCUMENT_EXTS.contains(&ext) {
        Some(FileKind::Document)
    } else if ARCHIVE_EXTS.contains(&ext) {
        Some(FileKind::Archive)
    } else {
        None
    }
}

enum Pattern {
    Plain(Box<(String, memchr::memmem::Finder<'static>)>),
    Glob(glob::Pattern),
    Regex(regex::Regex),
}

fn compile_pattern(pattern: &str, kind: SearchTermType, case_sensitive: bool) -> Option<Pattern> {
    match kind {
        SearchTermType::Plain => {
            use unicode_normalization::UnicodeNormalization;
            // Composed like the folded names it is matched against.
            let pattern: String = if case_sensitive {
                pattern.nfc().collect()
            } else {
                pattern.nfc().flat_map(char::to_lowercase).collect()
            };
            let finder = memchr::memmem::Finder::new(pattern.as_bytes()).into_owned();
            Some(Pattern::Plain(Box::new((pattern, finder))))
        }
        SearchTermType::Glob => glob::Pattern::new(pattern).ok().map(Pattern::Glob),
        SearchTermType::Regex => regex::RegexBuilder::new(pattern)
            .case_insensitive(!case_sensitive)
            .build()
            .ok()
            .map(Pattern::Regex),
    }
}

/// The plain words of a search, for highlighting them in result names.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Highlights {
    words: Vec<String>,
    case_sensitive: bool,
}

impl Highlights {
    pub fn new(search: &Search) -> Option<Self> {
        let words: Vec<String> = search
            .effective_chips()
            .into_iter()
            .filter_map(|chip| match chip {
                Chip::Name {
                    pattern,
                    kind: SearchTermType::Plain,
                    exclude: false,
                } if !pattern.is_empty() => Some(if search.case_sensitive {
                    pattern
                } else {
                    pattern.to_lowercase()
                }),
                _ => None,
            })
            .collect();
        (!words.is_empty()).then_some(Self {
            words,
            case_sensitive: search.case_sensitive,
        })
    }

    /// Byte ranges of `name` to highlight, sorted and merged.
    pub fn ranges(&self, name: &str) -> Vec<std::ops::Range<usize>> {
        let folded;
        let haystack = if self.case_sensitive {
            name
        } else {
            folded = name.to_lowercase();
            // Lower-casing can change byte lengths; then offsets don't line up.
            if folded.len() != name.len() {
                return Vec::new();
            }
            &folded
        };
        let mut ranges: Vec<std::ops::Range<usize>> = self
            .words
            .iter()
            .flat_map(|word| {
                haystack
                    .match_indices(word.as_str())
                    .map(|(start, found)| start..start + found.len())
            })
            .filter(|range| name.is_char_boundary(range.start) && name.is_char_boundary(range.end))
            .collect();
        ranges.sort_by_key(|range| range.start);
        let mut merged: Vec<std::ops::Range<usize>> = Vec::new();
        for range in ranges {
            match merged.last_mut() {
                Some(last) if range.start <= last.end => last.end = last.end.max(range.end),
                _ => merged.push(range),
            }
        }
        merged
    }
}

/// Whether a pattern is valid, for highlighting broken chips.
pub fn pattern_error(pattern: &str, kind: SearchTermType) -> Option<String> {
    match kind {
        SearchTermType::Plain => None,
        SearchTermType::Glob => glob::Pattern::new(pattern).err().map(|e| e.to_string()),
        SearchTermType::Regex => regex::Regex::new(pattern).err().map(|e| e.to_string()),
    }
}

/// Locale-aware (primary strength) substring match, the slow fallback for
/// non-ASCII names whose lower-cased form did not contain the pattern.
fn collated_contains(name: &str, pattern: &str) -> bool {
    let search_len = pattern.len();
    if search_len > name.len() {
        return false;
    }
    let bytes = name.as_bytes();
    name.char_indices().any(|(j, _)| {
        j + search_len <= name.len()
            && name.is_char_boundary(j + search_len)
            && collator().compare_utf8(pattern.as_bytes(), &bytes[j..j + search_len])
                == Ordering::Equal
    })
}

impl Pattern {
    fn matches(
        &self,
        name: &str,
        folded: Option<&FoldedNames>,
        index: usize,
        case_sensitive: bool,
    ) -> bool {
        match self {
            Self::Plain(plain) => {
                let (pattern, finder) = plain.as_ref();
                if pattern.is_empty() {
                    return true;
                }
                folded.map_or_else(
                    || finder.find(name.as_bytes()).is_some(),
                    |folded| {
                        // The collator compares byte windows of the
                        // pattern's length, which for an ASCII pattern can
                        // only line up with ASCII text: covered above.
                        finder.find(folded.get(index).as_bytes()).is_some()
                            || (!folded.is_ascii(index)
                                && !pattern.is_ascii()
                                && collated_contains(name, pattern))
                    },
                )
            }
            Self::Glob(glob) => glob.matches_with(
                name,
                glob::MatchOptions {
                    case_sensitive,
                    ..glob::MatchOptions::new()
                },
            ),
            Self::Regex(re) => re.is_match(name),
        }
    }
}

enum Predicate {
    Ext(Vec<String>),
    /// Any of these kinds.
    Kind(Vec<FileKind>),
    Size(Cmp, u64),
    /// Modified at or after this Unix time.
    ModifiedSince(u64),
    /// Opened at or after this Unix time.
    OpenedSince(u64),
}

/// A search ready to test entries: name patterns combined by
/// [`MatchMode`], exclusions and metadata filters that always apply.
pub struct CompiledQuery {
    names: Vec<Pattern>,
    excluded: Vec<Pattern>,
    predicates: Vec<Predicate>,
    mode: MatchMode,
    case_sensitive: bool,
    /// Plain patterns are present, so folded names are needed when the
    /// search ignores case.
    pub needs_folded: bool,
    /// An `opened:` filter is present, so `matches` needs open times.
    pub needs_opened: bool,
}

impl CompiledQuery {
    /// `None` when the search filters nothing. `now` is the Unix time that
    /// "today" and "this week" count back from.
    pub fn new(search: Option<&Search>, now: u64) -> Option<Self> {
        let search = search?;
        let case_sensitive = search.case_sensitive;
        let mut names = Vec::new();
        let mut excluded = Vec::new();
        let mut predicates = Vec::new();
        // A chip still being filled in (blank) filters nothing yet.
        for chip in search
            .effective_chips()
            .iter()
            .filter(|chip| !chip.is_blank())
        {
            match chip {
                Chip::Name {
                    pattern,
                    kind,
                    exclude,
                } => {
                    if let Some(compiled) = compile_pattern(pattern, *kind, case_sensitive) {
                        if *exclude {
                            excluded.push(compiled);
                        } else {
                            names.push(compiled);
                        }
                    }
                }
                Chip::Ext(exts) => predicates.push(Predicate::Ext(exts.clone())),
                Chip::Kind(kinds) => predicates.push(Predicate::Kind(kinds.clone())),
                Chip::Size { cmp, bytes } => predicates.push(Predicate::Size(*cmp, *bytes)),
                Chip::Modified(age) => {
                    predicates.push(Predicate::ModifiedSince(now.saturating_sub(age.seconds())));
                }
                Chip::Opened(age) => {
                    predicates.push(Predicate::OpenedSince(now.saturating_sub(age.seconds())));
                }
                // Extra folders change what is scanned, not what matches.
                Chip::In(_) => {}
            }
        }
        if names.is_empty() && excluded.is_empty() && predicates.is_empty() {
            return None;
        }
        let needs_folded = !case_sensitive
            && names
                .iter()
                .chain(&excluded)
                .any(|pattern| matches!(pattern, Pattern::Plain(..)));
        let needs_opened = predicates
            .iter()
            .any(|predicate| matches!(predicate, Predicate::OpenedSince(_)));
        Some(Self {
            names,
            excluded,
            predicates,
            mode: search.effective_match_mode(),
            case_sensitive,
            needs_folded,
            needs_opened,
        })
    }

    /// Whether entry `index` matches. `folded` is the listing's folded names
    /// when [`Self::needs_folded`]; `opened` holds, per entry, the Unix time
    /// it was last opened when [`Self::needs_opened`] (missing counts as never).
    pub fn matches(
        &self,
        listing: &Listing,
        folded: Option<&FoldedNames>,
        opened: &[u32],
        index: usize,
    ) -> bool {
        let name = listing.name(index);
        let matches_name =
            |pattern: &Pattern| pattern.matches(name, folded, index, self.case_sensitive);
        let names_ok = self.names.is_empty()
            || match self.mode {
                MatchMode::All => self.names.iter().all(matches_name),
                MatchMode::Any => self.names.iter().any(matches_name),
            };
        if !names_ok || self.excluded.iter().any(matches_name) {
            return false;
        }
        if self.predicates.is_empty() {
            return true;
        }
        let meta = listing.meta(index);
        let is_dir = meta.entry_type == EntryType::Directory;
        self.predicates.iter().all(|predicate| match predicate {
            Predicate::Ext(exts) => {
                !is_dir && extension(name).is_some_and(|ext| exts.contains(&ext))
            }
            Predicate::Kind(kinds) => {
                kind_of(name, is_dir).is_some_and(|kind| kinds.contains(&kind))
            }
            Predicate::Size(cmp, bytes) => {
                meta.entry_type == EntryType::File && cmp.holds(meta.size, *bytes)
            }
            Predicate::ModifiedSince(since) => u64::from(*meta.modified_at) >= *since,
            Predicate::OpenedSince(since) => {
                opened.get(index).is_some_and(|&at| u64::from(at) >= *since)
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{CompiledQuery, kind_of};
    use crate::listing::{DirEntry, Listing};
    use crate::search::{Chip, Cmp, FileKind, MatchMode, Search, query};

    const NOW: u64 = 1_800_000_000;

    fn listing(names: &[&str]) -> Listing {
        let entries: Vec<DirEntry> = names
            .iter()
            .map(|name| DirEntry::test_new(&format!("/virtual/{name}")))
            .collect();
        Listing::from_entries(&entries)
    }

    fn matching_opened(listing: &Listing, search: &Search, opened: &[u32]) -> Vec<String> {
        let query = CompiledQuery::new(Some(search), NOW);
        let folded = query
            .as_ref()
            .filter(|q| q.needs_folded)
            .map(|_| listing.folded_names());
        (0..listing.len())
            .filter(|&i| {
                query
                    .as_ref()
                    .is_none_or(|q| q.matches(listing, folded, opened, i))
            })
            .map(|i| listing.name(i).to_owned())
            .collect()
    }

    fn matching(listing: &Listing, search: &Search) -> Vec<String> {
        matching_opened(listing, search, &[])
    }

    #[test]
    fn combines_names_exclusions_and_filters() {
        let listing = listing(&[
            "Report 2024.pdf",
            "report draft.pdf",
            "photo.JPG",
            "notes.txt",
        ]);
        let search = |text: &str, mode| Search {
            chips: query::parse(text).chips,
            match_mode: mode,
            ..Default::default()
        };
        assert_eq!(
            matching(&listing, &search("report -draft", MatchMode::All)),
            vec!["Report 2024.pdf"]
        );
        assert_eq!(
            matching(&listing, &search("notes photo", MatchMode::Any)),
            vec!["photo.JPG", "notes.txt"]
        );
        assert_eq!(
            matching(&listing, &search("ext:pdf", MatchMode::All)).len(),
            2
        );
        assert_eq!(
            matching(&listing, &search("kind:image", MatchMode::All)),
            vec!["photo.JPG"]
        );
        let none = Search {
            chips: vec![Chip::Size {
                cmp: Cmp::Greater,
                bytes: 1,
            }],
            ..Default::default()
        };
        // Test entries have no size.
        assert_eq!(matching(&listing, &none), Vec::<String>::new());
        assert!(CompiledQuery::new(Some(&Search::default()), NOW).is_none());
    }

    #[test]
    fn opened_filter_uses_the_open_times_given() {
        let listing = listing(&["seen.txt", "unseen.txt"]);
        let search = Search {
            chips: query::parse("opened:today").chips,
            ..Default::default()
        };
        assert!(
            CompiledQuery::new(Some(&search), NOW)
                .expect("filters")
                .needs_opened
        );
        let opened = [u32::try_from(NOW - 60).expect("fits"), 0];
        assert_eq!(
            matching_opened(&listing, &search, &opened),
            vec!["seen.txt"]
        );
        assert_eq!(matching(&listing, &search), Vec::<String>::new());
    }

    #[test]
    fn non_ascii_names_match_ignoring_accents_and_case() {
        let listing = listing(&["Żołw.txt", "ŻÓŁW.md", "zolw.txt"]);
        let search = Search {
            value: "żółw".into(),
            ..Default::default()
        };
        // Lower-cased, "ŻÓŁW" contains it; "Żołw" only by the collator,
        // which compares non-ASCII text alone.
        assert_eq!(matching(&listing, &search), vec!["Żołw.txt", "ŻÓŁW.md"]);
    }

    #[test]
    fn highlights_merge_overlapping_words() {
        let search = Search {
            value: "rep port".into(),
            ..Default::default()
        };
        let highlights = super::Highlights::new(&search).expect("words");
        assert_eq!(highlights.ranges("My Report.txt"), vec![3..9]);
        assert_eq!(
            highlights.ranges("nothing"),
            Vec::<std::ops::Range<usize>>::new()
        );
    }

    #[test]
    fn kinds_follow_extensions() {
        assert_eq!(kind_of("a.MP4", false), Some(FileKind::Video));
        assert_eq!(kind_of("a.tar", false), Some(FileKind::Archive));
        assert_eq!(kind_of(".bashrc", false), None);
        assert_eq!(kind_of("dir.png", true), Some(FileKind::Folder));
    }
}
