//! Entries shown in groups: by kind, by how long ago they changed, by size
//! or by the first letter of their name.
//!
//! Grouping rearranges a display order that is already sorted: the entries
//! of a group come together, in the order the sort gave them, and the groups
//! are told apart by [`GroupSpan`]s beside the order. Rows stay positions in
//! that order, so everything that works on rows works on grouped ones.
use std::collections::{BTreeMap, HashMap};

use super::{EntryType, Listing, kind_label};
use crate::view_settings::{GroupBy, Sort};

/// Groups told apart at most; what would make more goes into a last group.
pub const MAX_GROUPS: usize = 256;

/// A run of rows under one heading.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupSpan {
    pub label: String,
    /// The position of the group's first row in the display order.
    pub first: u32,
    pub len: u32,
}

const DAY: u64 = 86_400;
/// How long ago, at most, each group but the last two reaches.
const AGES: [(u64, &str); 4] = [
    (DAY, "Last 24 hours"),
    (7 * DAY, "Last 7 days"),
    (30 * DAY, "Last 30 days"),
    (365 * DAY, "Last 12 months"),
];
const OLDER: &str = "Older";
const UNDATED: &str = "No date";

/// Sizes up to which each group but the last reaches, in bytes.
const SIZES: [(u64, &str); 5] = [
    (0, "Empty"),
    (100_000, "Tiny (up to 100 KB)"),
    (1_000_000, "Small (100 KB \u{2013} 1 MB)"),
    (100_000_000, "Medium (1 \u{2013} 100 MB)"),
    (1_000_000_000, "Large (100 MB \u{2013} 1 GB)"),
];
const HUGE: &str = "Huge (over 1 GB)";

const SYMBOLS: u32 = 0;
const DIGITS: u32 = 1;
/// Added to a letter, so letters come after symbols and digits.
const LETTERS: u32 = 2;

/// What tells the groups of one grouping apart: a key per entry, smaller for
/// the groups that come first, and each key's heading.
struct Grouping {
    /// One per position of the display order.
    keys: Vec<u32>,
    labels: HashMap<u32, String>,
    /// The key of the group that stays first whichever way the rest runs:
    /// folders, where folders are sorted first.
    pinned: Option<u32>,
}

fn by_kind(listing: &Listing, order: &[u32]) -> Grouping {
    // A label is made once for each spelling of an extension.
    let mut of_extension: HashMap<&str, usize> = HashMap::new();
    let mut labels: Vec<String> = vec!["Folder".to_owned()];
    let mut of_label: HashMap<String, usize> = HashMap::from([("Folder".to_owned(), 0)]);
    let mut label_id = |name: &str, entry_type: EntryType| {
        let label = kind_label(name, entry_type);
        *of_label.entry(label).or_insert_with_key(|label| {
            labels.push(label.clone());
            labels.len() - 1
        })
    };
    let ids: Vec<usize> = order
        .iter()
        .map(|&entry| {
            let name = listing.name(entry as usize);
            match listing.meta(entry as usize).entry_type {
                EntryType::Directory => 0,
                EntryType::Package => label_id(name, EntryType::Package),
                EntryType::File => {
                    let extension = std::path::Path::new(name)
                        .extension()
                        .and_then(std::ffi::OsStr::to_str)
                        .unwrap_or("");
                    if let Some(&id) = of_extension.get(extension) {
                        id
                    } else {
                        let id = label_id(name, EntryType::File);
                        of_extension.insert(extension, id);
                        id
                    }
                }
            }
        })
        .collect();
    // Folders first, then the kinds by name.
    let mut sorted: Vec<usize> = (1..labels.len()).collect();
    sorted.sort_by_cached_key(|&id| labels[id].to_lowercase());
    let mut key_of = vec![0_u32; labels.len()];
    for (rank, &id) in sorted.iter().enumerate() {
        key_of[id] = u32::try_from(rank + 1).unwrap_or(u32::MAX);
    }
    Grouping {
        keys: ids.iter().map(|&id| key_of[id]).collect(),
        labels: labels
            .into_iter()
            .enumerate()
            .map(|(id, label)| (key_of[id], label))
            .collect(),
        pinned: Some(0),
    }
}

fn by_age(listing: &Listing, order: &[u32], now: u64) -> Grouping {
    let modified = |entry: u32| u64::from(*listing.meta(entry as usize).modified_at);
    // Before the clock first ticks, the newest entry is as good as now.
    let now = if now == 0 {
        order
            .iter()
            .map(|&entry| modified(entry))
            .max()
            .unwrap_or(0)
    } else {
        now
    };
    let undated = u32::try_from(AGES.len() + 1).unwrap_or(u32::MAX);
    let keys = order
        .iter()
        .map(|&entry| match modified(entry) {
            0 => undated,
            time => {
                // What says it changed in the future changed just now.
                let age = now.saturating_sub(time);
                let group = AGES.iter().position(|&(limit, _)| age < limit);
                u32::try_from(group.unwrap_or(AGES.len())).unwrap_or(u32::MAX)
            }
        })
        .collect();
    let labels = AGES
        .iter()
        .map(|&(_, label)| label)
        .chain([OLDER, UNDATED])
        .zip(0..)
        .map(|(label, key)| (key, label.to_owned()))
        .collect();
    Grouping {
        keys,
        labels,
        pinned: None,
    }
}

fn by_size(listing: &Listing, order: &[u32]) -> Grouping {
    let keys = order
        .iter()
        .map(|&entry| {
            let meta = listing.meta(entry as usize);
            if meta.entry_type == EntryType::Directory {
                return 0;
            }
            let group = SIZES.iter().position(|&(limit, _)| meta.size <= limit);
            u32::try_from(group.unwrap_or(SIZES.len()) + 1).unwrap_or(u32::MAX)
        })
        .collect();
    let labels = std::iter::once("Folders")
        .chain(SIZES.iter().map(|&(_, label)| label))
        .chain([HUGE])
        .zip(0..)
        .map(|(label, key)| (key, label.to_owned()))
        .collect();
    Grouping {
        keys,
        labels,
        pinned: Some(0),
    }
}

fn by_name(listing: &Listing, order: &[u32]) -> Grouping {
    let mut labels = HashMap::from([
        (SYMBOLS, "Symbols".to_owned()),
        (DIGITS, "0 \u{2013} 9".to_owned()),
    ]);
    let keys = order
        .iter()
        .map(|&entry| {
            let Some(first) = listing.name(entry as usize).chars().next() else {
                return SYMBOLS;
            };
            if first.is_numeric() {
                DIGITS
            } else if first.is_alphabetic() {
                let letter = first.to_uppercase().next().unwrap_or(first);
                let key = LETTERS + u32::from(letter);
                labels.entry(key).or_insert_with(|| letter.to_string());
                key
            } else {
                SYMBOLS
            }
        })
        .collect();
    Grouping {
        keys,
        labels,
        pinned: None,
    }
}

/// Whether the display order, sorted by what it is grouped by, runs from
/// the last group to the first: the groups then run that way too.
fn runs_backwards(keys: &Grouping, by: GroupBy, sort: Sort) -> bool {
    let same = matches!(
        (by, sort),
        (GroupBy::Kind, Sort::Kind)
            | (GroupBy::Modified, Sort::Modified)
            | (GroupBy::Size, Sort::Size)
            | (GroupBy::Name, Sort::Name)
    );
    if !same {
        return false;
    }
    let mut free = keys.keys.iter().filter(|&&key| Some(key) != keys.pinned);
    match (free.next(), free.next_back()) {
        (Some(first), Some(last)) => first > last,
        _ => false,
    }
}

/// Puts the entries of `order` into groups by `by` and says where each is.
///
/// `order` is sorted by `sort`, and within a group the entries keep that
/// order. `now` is the time, in Unix seconds, that ages are counted from.
/// With [`GroupBy::None`] the order is left alone and there are no groups.
pub fn arrange(
    listing: &Listing,
    order: &mut Vec<u32>,
    by: GroupBy,
    sort: Sort,
    now: u64,
) -> Vec<GroupSpan> {
    let found = match by {
        GroupBy::None => return Vec::new(),
        _ if order.is_empty() => return Vec::new(),
        GroupBy::Kind => by_kind(listing, order),
        GroupBy::Modified => by_age(listing, order, now),
        GroupBy::Size => by_size(listing, order),
        GroupBy::Name => by_name(listing, order),
    };
    let mut counts: BTreeMap<u32, u32> = BTreeMap::new();
    for &key in &found.keys {
        *counts.entry(key).or_default() += 1;
    }
    let backwards = runs_backwards(&found, by, sort);
    let pinned = found.pinned.filter(|key| counts.contains_key(key));
    let mut free: Vec<u32> = counts
        .keys()
        .copied()
        .filter(|&key| Some(key) != pinned)
        .collect();
    if backwards {
        free.reverse();
    }
    let ranked: Vec<u32> = pinned.into_iter().chain(free).collect();
    // What would make too many groups shares the last one.
    let last = MAX_GROUPS - 1;
    let rank_of: HashMap<u32, usize> = ranked
        .iter()
        .enumerate()
        .map(|(rank, &key)| (key, rank.min(last)))
        .collect();
    let mut spans: Vec<GroupSpan> = ranked
        .iter()
        .take(MAX_GROUPS)
        .enumerate()
        .map(|(rank, key)| GroupSpan {
            label: if rank == last && ranked.len() > MAX_GROUPS {
                "Other".to_owned()
            } else {
                found.labels.get(key).cloned().unwrap_or_default()
            },
            first: 0,
            len: 0,
        })
        .collect();
    for (&key, &count) in &counts {
        spans[rank_of[&key]].len += count;
    }
    let mut next = 0;
    for span in &mut spans {
        span.first = next;
        next += span.len;
    }
    let mut place: Vec<u32> = spans.iter().map(|span| span.first).collect();
    let mut arranged = vec![0_u32; order.len()];
    for (&entry, key) in order.iter().zip(&found.keys) {
        let slot = &mut place[rank_of[key]];
        arranged[*slot as usize] = entry;
        *slot += 1;
    }
    *order = arranged;
    spans
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::listing::{DirEntry, TimestampSeconds};

    fn file(name: &str, size: u64, modified: u32) -> DirEntry {
        let mut entry = DirEntry::test_new(&format!("/x/{name}"));
        entry.meta.size = size;
        entry.meta.modified_at = TimestampSeconds::from_unix(modified);
        entry
    }

    fn folder(name: &str) -> DirEntry {
        let mut entry = DirEntry::test_new(&format!("/x/{name}"));
        entry.meta.entry_type = EntryType::Directory;
        entry
    }

    /// The groups of `entries`, in the order given, as `label: names`.
    fn grouped(entries: &[DirEntry], by: GroupBy, sort: Sort, now: u64) -> Vec<String> {
        let listing = Listing::from_entries(entries);
        let mut order: Vec<u32> = (0..u32::try_from(entries.len()).expect("few")).collect();
        let spans = arrange(&listing, &mut order, by, sort, now);
        let mut covered = 0;
        spans
            .iter()
            .map(|span| {
                assert_eq!(span.first, covered, "groups follow one another");
                covered += span.len;
                let names: Vec<&str> = order[span.first as usize..(span.first + span.len) as usize]
                    .iter()
                    .map(|&entry| listing.name(entry as usize))
                    .collect();
                format!("{}: {}", span.label, names.join(" "))
            })
            .collect()
    }

    #[test]
    fn kinds_come_by_name_after_the_folders_and_keep_the_order_inside() {
        let entries = [
            folder("src"),
            file("b.txt", 1, 1),
            file("z.PNG", 1, 1),
            file("a.txt", 1, 1),
            file("a.png", 1, 1),
            file("README", 1, 1),
        ];
        assert_eq!(
            grouped(&entries, GroupBy::Kind, Sort::Name, 0),
            [
                "Folder: src",
                "File: README",
                "PNG image: z.PNG a.png",
                "TXT file: b.txt a.txt"
            ]
        );
        // Not grouped: nothing moves and there is nothing to tell apart.
        assert_eq!(
            grouped(&entries, GroupBy::None, Sort::Name, 0),
            Vec::<String>::new()
        );
    }

    #[test]
    fn ages_are_counted_from_now_or_else_from_the_newest_entry() {
        let now = 1_000 * DAY;
        let ago = |days: u64| u32::try_from(now - days * DAY - 60).expect("fits");
        let entries = [
            file("old", 1, ago(400)),
            file("today", 1, ago(0)),
            file("week", 1, ago(3)),
            file("undated", 1, 0),
            file("month", 1, ago(20)),
            file("year", 1, ago(200)),
        ];
        let expected = [
            "Last 24 hours: today",
            "Last 7 days: week",
            "Last 30 days: month",
            "Last 12 months: year",
            "Older: old",
            "No date: undated",
        ];
        assert_eq!(
            grouped(&entries, GroupBy::Modified, Sort::Name, now),
            expected
        );
        // No clock yet: the newest entry stands for now.
        assert_eq!(
            grouped(&entries, GroupBy::Modified, Sort::Name, 0),
            expected
        );
    }

    #[test]
    fn groups_run_the_way_the_same_sort_runs_with_folders_still_first() {
        let entries = [
            folder("dir"),
            file("huge", 2_000_000_000, 1),
            file("small", 500_000, 1),
            file("empty", 0, 1),
        ];
        // Largest first, as sorted: the groups follow.
        assert_eq!(
            grouped(&entries, GroupBy::Size, Sort::Size, 0),
            [
                "Folders: dir",
                "Huge (over 1 GB): huge",
                "Small (100 KB \u{2013} 1 MB): small",
                "Empty: empty"
            ]
        );
        // Sorted by something else, sizes run from the smallest.
        assert_eq!(
            grouped(&entries, GroupBy::Size, Sort::Name, 0),
            [
                "Folders: dir",
                "Empty: empty",
                "Small (100 KB \u{2013} 1 MB): small",
                "Huge (over 1 GB): huge"
            ]
        );
    }

    #[test]
    fn names_go_by_their_first_letter_whatever_its_case() {
        let entries = [
            file("_draft", 1, 1),
            file("2024.txt", 1, 1),
            file("apple", 1, 1),
            file("Avocado", 1, 1),
            file("\u{17c}aba", 1, 1),
            file("banana", 1, 1),
        ];
        assert_eq!(
            grouped(&entries, GroupBy::Name, Sort::Modified, 0),
            [
                "Symbols: _draft",
                "0 \u{2013} 9: 2024.txt",
                "A: apple Avocado",
                "B: banana",
                "\u{17b}: \u{17c}aba"
            ]
        );
    }

    #[test]
    fn what_would_make_too_many_groups_shares_the_last_one() {
        let entries: Vec<DirEntry> = (0..300)
            .map(|index| file(&format!("f.x{index:03}"), 1, 1))
            .collect();
        let listing = Listing::from_entries(&entries);
        let mut order: Vec<u32> = (0..300).collect();
        let spans = arrange(&listing, &mut order, GroupBy::Kind, Sort::Name, 0);
        assert_eq!(spans.len(), MAX_GROUPS);
        let last = spans.last().expect("groups");
        assert_eq!((last.label.as_str(), last.len), ("Other", 300 - 255));
        assert_eq!(spans.iter().map(|span| span.len).sum::<u32>(), 300);
        let mut seen = order.clone();
        seen.sort_unstable();
        assert_eq!(seen, (0..300).collect::<Vec<u32>>(), "every entry once");
    }
}
