//! Matching lives in `dirfleet_core::search::filter`; the open-history test
//! below checks it against the app's own open history.

pub use dirfleet_core::search::filter::*;

#[cfg(test)]
mod tests {
    use crate::app::search::{Search, query};
    use crate::data::files::{DirEntry, Listing};

    #[test]
    fn opened_filter_uses_the_open_history() {
        let entries: Vec<DirEntry> = ["seen.txt", "unseen.txt"]
            .iter()
            .map(|name| DirEntry::test_new(&format!("/opened_filter/{name}")))
            .collect();
        let listing = Listing::from_entries(&entries);
        crate::app::open_history::record(std::path::Path::new("/opened_filter/seen.txt"));
        let search = Search {
            chips: query::parse("opened:today").chips,
            ..Default::default()
        };
        let order: Vec<u32> = (0..2).collect();
        let shown = crate::app::dir_handling::filter_visible(&listing, &order, true, Some(&search));
        assert_eq!(shown, vec![0]);
    }
}
