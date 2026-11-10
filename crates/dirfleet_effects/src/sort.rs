//! A listing's display order, computed off the UI thread (in parallel for
//! large folders): the one sort every shell uses.
use std::cmp::Reverse;

use dirfleet_core::Sort;
use dirfleet_core::listing::{Listing, kind_label};
use rayon::slice::ParallelSliceMut;

/// Where "Date opened" and "Times opened" come from.
pub trait OpeningHistory: Sync {
    /// `(times opened, last opened)` for each entry of `listing`, in order.
    fn keys(&self, listing: &Listing) -> Vec<(u32, u32)>;
}

/// No open history: an entry was last opened when the file system last
/// accessed it, and never counted.
pub struct AccessTimes;

impl OpeningHistory for AccessTimes {
    fn keys(&self, listing: &Listing) -> Vec<(u32, u32)> {
        (0..listing.len())
            .map(|index| (0, *listing.meta(index).accessed_at))
            .collect()
    }
}

/// Folders first in both directions, like the other sorts.
fn sort_by_opening(
    order: &mut [u32],
    keys: &[(u32, u32)],
    is_file: &(impl Fn(u32) -> bool + Sync),
    invert: bool,
) {
    if invert {
        order.par_sort_unstable_by_key(|&i| (is_file(i), Reverse(keys[i as usize])));
    } else {
        order.par_sort_unstable_by_key(|&i| (is_file(i), keys[i as usize]));
    }
}

/// The display order (indices into `listing`) for `sort`, reversed when
/// `invert`. Folders stay first except in random order.
pub fn sort_order(
    listing: &Listing,
    sort: Sort,
    invert: bool,
    history: &dyn OpeningHistory,
) -> Vec<u32> {
    let mut order: Vec<u32> = (0..listing.len() as u32).collect();
    let is_file = |index: u32| listing.is_file(index as usize);
    let meta = |index: u32| listing.meta(index as usize);
    match (sort, invert) {
        (Sort::Name, invert) => {
            let keys = listing.sort_keys();
            // Folders stay first in both directions; only names reverse.
            order.par_sort_unstable_by(|a, b| {
                let names = keys.get(*a as usize).cmp(keys.get(*b as usize));
                is_file(*a)
                    .cmp(&is_file(*b))
                    .then(if invert { names.reverse() } else { names })
            });
        }
        (Sort::Modified, false) => {
            order.par_sort_unstable_by_key(|&i| (is_file(i), meta(i).modified_at));
        }
        (Sort::Modified, true) => {
            order.par_sort_unstable_by_key(|&i| (is_file(i), Reverse(meta(i).modified_at)));
        }
        (Sort::Created, false) => {
            order.par_sort_unstable_by_key(|&i| (is_file(i), meta(i).created_at));
        }
        (Sort::Created, true) => {
            order.par_sort_unstable_by_key(|&i| (is_file(i), Reverse(meta(i).created_at)));
        }
        (Sort::Accessed, invert) => {
            let opened: Vec<_> = history
                .keys(listing)
                .into_iter()
                .map(|(_, last)| (0, last))
                .collect();
            sort_by_opening(&mut order, &opened, &is_file, invert);
        }
        (Sort::MostOpened, invert) => {
            // Ties (most files: never opened here) fall back to recency.
            sort_by_opening(&mut order, &history.keys(listing), &is_file, invert);
        }
        (Sort::Size, false) => {
            order.par_sort_unstable_by_key(|&i| (is_file(i), meta(i).size));
        }
        (Sort::Size, true) => {
            order.par_sort_unstable_by_key(|&i| (is_file(i), Reverse(meta(i).size)));
        }
        (Sort::Kind, invert) => {
            // By extension, then name; folders first in both directions.
            let keys = listing.sort_keys();
            let kinds: Vec<String> = (0..listing.len())
                .map(|index| kind_label(listing.name(index), listing.meta(index).entry_type))
                .collect();
            order.par_sort_unstable_by(|a, b| {
                let (a, b) = (*a as usize, *b as usize);
                let kind = kinds[a].cmp(&kinds[b]);
                listing.is_file(a).cmp(&listing.is_file(b)).then(
                    (if invert { kind.reverse() } else { kind })
                        .then_with(|| keys.get(a).cmp(keys.get(b))),
                )
            });
        }
        (Sort::Random, _) => {
            use rand::seq::SliceRandom;
            order.shuffle(&mut rand::thread_rng());
        }
    }
    order
}
