//! Looking inside files for a search's text.
//!
//! A file is read line by line, and reading stops at its first matching
//! line. Files that are not text (they hold a NUL byte) and very large ones
//! are left alone. Text in UTF-16 with a byte order mark is read as text.
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use dirfleet_core::app::MatchLine;
use dirfleet_core::listing::{EntryType, Listing, ListingBuilder};
use dirfleet_core::search::filter::CompiledQuery;
use dirfleet_core::search::{Chip, Search, SearchTermType};
use grep_matcher::Matcher as _;
use grep_regex::{RegexMatcher, RegexMatcherBuilder};
use grep_searcher::{BinaryDetection, Searcher, SearcherBuilder, sinks};

/// Files larger than this are not read.
const LARGEST: u64 = 64 * 1024 * 1024;

/// A matching line is shown up to about this many bytes.
const SHOWN: usize = 200;

/// How much of a long line is shown before what matched.
const LEAD: usize = 40;

/// What a search looks for inside files.
pub struct ContentScan {
    /// Every one of these must be found in a file.
    terms: Vec<RegexMatcher>,
    /// A term could not be read as something to look for: nothing matches.
    broken: bool,
    /// The search's other committed filters: a file they leave out is not
    /// opened.
    filter: Option<CompiledQuery>,
}

/// A reader of files, kept between files so its buffers are.
pub fn searcher() -> Searcher {
    SearcherBuilder::new()
        .binary_detection(BinaryDetection::quit(0))
        .line_number(true)
        .build()
}

/// The first byte at or after `at` that starts a character.
fn char_start(line: &[u8], mut at: usize) -> usize {
    while at < line.len() && line[at] & 0xC0 == 0x80 {
        at += 1;
    }
    at.min(line.len())
}

/// The line as it is shown: without the space around it, and with only the
/// part around bytes `start..end` (what matched) when it is long.
fn shown(number: u64, line: &[u8], start: usize, end: usize) -> MatchLine {
    let line = line.trim_ascii_end();
    let indent = line.len() - line.trim_ascii_start().len();
    let (start, end) = (
        start.clamp(indent, line.len()),
        end.clamp(indent, line.len()),
    );
    // A long line begins a little before what matched.
    let begin = if end.saturating_sub(indent) <= SHOWN {
        indent
    } else {
        char_start(line, start.saturating_sub(LEAD).max(indent))
    };
    let stop = char_start(line, (begin + SHOWN).max(end.min(begin + 2 * SHOWN)));
    let cut = begin > indent;
    let mut text = String::new();
    if cut {
        text.push('…');
    }
    let lead = text.len();
    text.push_str(&String::from_utf8_lossy(&line[begin..stop]));
    if stop < line.len() {
        text.push('…');
    }
    // Where it matched, unless bytes that are no text moved what follows.
    let (from, to) = (lead + start - begin, lead + end.min(stop) - begin);
    let marked = std::str::from_utf8(&line[begin..stop]).is_ok()
        && text.is_char_boundary(from)
        && text.is_char_boundary(to);
    MatchLine {
        line: u32::try_from(number).unwrap_or(u32::MAX),
        text,
        start: if marked { from as u32 } else { 0 },
        end: if marked { to as u32 } else { 0 },
    }
}

impl ContentScan {
    /// What `search`'s committed chips look for; `None` when none of them
    /// looks inside files. `now` is the Unix time its date filters count
    /// back from.
    pub fn new(search: &Search, now: u64) -> Option<Self> {
        let mut terms = Vec::new();
        let mut broken = false;
        for chip in &search.chips {
            let Chip::Content { pattern, kind } = chip else {
                continue;
            };
            if pattern.trim().is_empty() {
                continue;
            }
            let built = RegexMatcherBuilder::new()
                .case_insensitive(!search.case_sensitive)
                .fixed_strings(*kind != SearchTermType::Regex)
                .line_terminator(Some(b'\n'))
                .build(pattern);
            match built {
                Ok(term) => terms.push(term),
                Err(_) => broken = true,
            }
        }
        if terms.is_empty() && !broken {
            return None;
        }
        // Opening times are not known here; what is found is filtered by
        // them afterwards like any listing.
        let rest = Search {
            chips: search
                .chips
                .iter()
                .filter(|chip| !matches!(chip, Chip::Opened(_)))
                .cloned()
                .collect(),
            ..search.committed()
        };
        Some(Self {
            terms,
            broken,
            filter: CompiledQuery::new(Some(&rest), now),
        })
    }

    /// The first line of the file at `path` holding the first term, when
    /// the file holds every term.
    pub fn scan(&self, searcher: &mut Searcher, path: &Path) -> Option<MatchLine> {
        if self.broken {
            return None;
        }
        let mut first = None;
        for (index, term) in self.terms.iter().enumerate() {
            let mut found = None;
            let sink = sinks::Bytes(|number, line| {
                let (start, end) = term
                    .find(line)
                    .ok()
                    .flatten()
                    .map_or((0, 0), |at| (at.start(), at.end()));
                found = Some(shown(number, line, start, end));
                Ok(false)
            });
            searcher.search_path(term, path, sink).ok()?;
            let found = found?;
            if index == 0 {
                first = Some(found);
            }
        }
        first
    }

    /// The files among `listing`, the entries of the folder `dir`, that the
    /// search matches, each with its matching line. `scanned` counts the
    /// files opened.
    pub fn keep(
        &self,
        dir: &Path,
        dir_name: &Arc<str>,
        listing: &Listing,
        cancel: &AtomicBool,
        scanned: &AtomicU64,
    ) -> (Listing, Vec<MatchLine>) {
        let folded = self
            .filter
            .as_ref()
            .filter(|filter| filter.needs_folded)
            .map(|_| listing.folded_names());
        let mut searcher = searcher();
        let mut builder = ListingBuilder::with_capacity(8);
        let mut lines = Vec::new();
        for index in 0..listing.len() {
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            let meta = listing.meta(index);
            if meta.entry_type != EntryType::File || meta.size == 0 || meta.size > LARGEST {
                continue;
            }
            if self
                .filter
                .as_ref()
                .is_some_and(|filter| !filter.matches(listing, folded, &[], index))
            {
                continue;
            }
            let name = listing.name(index);
            scanned.fetch_add(1, Ordering::Relaxed);
            if let Some(line) = self.scan(&mut searcher, &dir.join(name)) {
                builder.push(dir_name, name, *meta);
                lines.push(line);
            }
        }
        (builder.finish(), lines)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn search(text: &str) -> Search {
        let mut search = Search {
            value: text.to_owned(),
            ..Search::default()
        };
        search.commit_text();
        search
    }

    fn scan(text: &str, path: &Path) -> Option<MatchLine> {
        ContentScan::new(&search(text), 0)
            .expect("looks inside files")
            .scan(&mut searcher(), path)
    }

    #[test]
    fn the_first_matching_line_is_found_with_its_number_and_where_it_matched() {
        let dir = tempfile::tempdir().expect("temp dir");
        let file = dir.path().join("notes.txt");
        std::fs::write(&file, "one\r\n  two TODO later\r\nthree todo\r\n").expect("write");

        let found = scan("content:todo", &file).expect("found");
        assert_eq!((found.line, found.text.as_str()), (2, "two TODO later"));
        assert_eq!(
            &found.text[found.start as usize..found.end as usize],
            "TODO"
        );
        // An expression, and several terms: all must be there, the first shown.
        let found = scan("content:/t\\w+ee/", &file).expect("found");
        assert_eq!((found.line, found.text.as_str()), (3, "three todo"));
        assert_eq!(
            scan("content:one content:three", &file).map(|found| found.line),
            Some(1)
        );
        assert_eq!(scan("content:one content:four", &file), None);
        assert_eq!(
            scan("content:\"two TODO\"", &file).map(|found| found.line),
            Some(2)
        );
        // A dot in plain text is a dot.
        assert_eq!(scan("content:t.o", &file), None);
        // Letter case counts when the search says so.
        let mut exact = search("content:todo");
        exact.case_sensitive = true;
        let found = ContentScan::new(&exact, 0)
            .expect("looks inside files")
            .scan(&mut searcher(), &file)
            .expect("found");
        assert_eq!(found.line, 3);
    }

    #[test]
    fn what_is_not_text_is_left_alone_and_other_encodings_are_read() {
        let dir = tempfile::tempdir().expect("temp dir");
        let binary = dir.path().join("image.bin");
        std::fs::write(&binary, b"\x00\x01TODO\x00").expect("write");
        assert_eq!(scan("content:TODO", &binary), None);

        // UTF-16 with a byte order mark, as Windows programs write it.
        let wide = dir.path().join("wide.txt");
        let mut bytes = vec![0xFF, 0xFE];
        for unit in "first\r\nzażółć TODO\r\n".encode_utf16() {
            bytes.extend(unit.to_le_bytes());
        }
        std::fs::write(&wide, bytes).expect("write");
        let found = scan("content:todo", &wide).expect("found");
        assert_eq!((found.line, found.text.as_str()), (2, "zażółć TODO"));
        assert_eq!(
            &found.text[found.start as usize..found.end as usize],
            "TODO"
        );

        // A search that reads no contents, and one that can't be read.
        assert!(ContentScan::new(&search("todo ext:txt"), 0).is_none());
        assert_eq!(scan("content:/(/", &wide), None);
    }

    #[test]
    fn a_long_line_is_shown_around_what_matched() {
        let dir = tempfile::tempdir().expect("temp dir");
        let file = dir.path().join("long.txt");
        let line = format!("{}NEEDLE{}", "a".repeat(1000), "b".repeat(1000));
        std::fs::write(&file, &line).expect("write");
        let found = scan("content:needle", &file).expect("found");
        assert!(found.text.len() < 300, "{}", found.text.len());
        assert!(found.text.starts_with('…') && found.text.ends_with('…'));
        assert_eq!(
            &found.text[found.start as usize..found.end as usize],
            "NEEDLE"
        );
    }

    #[test]
    fn only_files_the_other_filters_keep_are_opened() {
        let dir = tempfile::tempdir().expect("temp dir");
        for (name, text) in [
            ("a.rs", "// TODO here"),
            ("b.rs", "nothing"),
            ("c.md", "TODO too"),
            ("draft.rs", "TODO as well"),
        ] {
            std::fs::write(dir.path().join(name), text).expect("write");
        }
        std::fs::create_dir(dir.path().join("TODO")).expect("folder");
        let listing = crate::listing_io::read_listing(dir.path()).expect("lists");
        let dir_name: Arc<str> = Arc::from(dir.path().to_string_lossy().as_ref());
        let kept = |text: &str| {
            let scan = ContentScan::new(&search(text), 0).expect("looks inside files");
            let (cancel, scanned) = (AtomicBool::new(false), AtomicU64::new(0));
            let (found, lines) = scan.keep(dir.path(), &dir_name, &listing, &cancel, &scanned);
            assert_eq!(found.len(), lines.len());
            let mut names: Vec<String> = found
                .iter()
                .map(|e| e.get_splitted_path().1.to_owned())
                .collect();
            names.sort();
            (names, scanned.into_inner())
        };
        assert_eq!(
            kept("content:todo"),
            (vec!["a.rs".into(), "c.md".into(), "draft.rs".into()], 4)
        );
        // Two of the four files are never opened; the folder never is.
        assert_eq!(kept("content:todo ext:rs -draft"), (vec!["a.rs".into()], 2));
    }
}
