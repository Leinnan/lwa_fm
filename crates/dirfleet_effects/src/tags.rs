//! Which item has which tag.
//!
//! Kept as JSON beside the app's state, by path as the core names items;
//! the tags themselves (names and colours) are the core's. A tag follows
//! an item renamed or moved in the app, with everything below it. Searches
//! for `tag:name` are answered from here: whoever orders a listing finds
//! the tags beside the open history.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, PoisonError, RwLock};

use dirfleet_core::app::{ListDirOutput, ListTagged, TagOp, Tagged, TaggedListing};
use dirfleet_core::listing::{DirEntryMetaData, EntryType, ListingBuilder};
use dirfleet_core::location::Location;

use crate::list::display_order_with;
use crate::open_history::OpenHistory;

#[derive(Default)]
pub struct TagStore {
    by_path: RwLock<BTreeMap<String, Vec<u32>>>,
    /// The tags' names in small letters, by id.
    names: RwLock<Vec<(u32, String)>>,
    file: Option<PathBuf>,
}

/// A path written one way, to tell whether two spellings name one item:
/// as listings write folders, and without regard to case where disks have
/// none.
fn one_form(path: &str) -> String {
    if Location::parse(path).as_local().is_none() {
        return path.to_owned();
    }
    // The long-path prefix is no part of the path.
    let path = path.strip_prefix(r"\\?\").unwrap_or(path);
    let form = crate::paths::canonical_string(Path::new(path));
    if cfg!(windows) {
        form.to_lowercase()
    } else {
        form
    }
}

impl TagStore {
    /// The tags kept in `file`, or none kept when there is no file.
    pub fn load(file: Option<PathBuf>) -> Self {
        let by_path = file
            .as_deref()
            .and_then(|file| std::fs::read(file).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Self {
            by_path: RwLock::new(by_path),
            names: RwLock::default(),
            file,
        }
    }

    /// Whether the item at `path` has a tag of each of `names` (in small
    /// letters). The path may be written another way than it was tagged
    /// under: with another drive letter's case, or the long-path prefix.
    pub fn has_all(&self, path: &str, names: &[String]) -> bool {
        // Copied out, so neither lock is held while names are compared.
        let tags: Option<Vec<u32>> = {
            let by_path = self.by_path.read().unwrap_or_else(PoisonError::into_inner);
            by_path.get(path).cloned().or_else(|| {
                let wanted = one_form(path);
                by_path
                    .iter()
                    .find(|(tagged, _)| one_form(tagged) == wanted)
                    .map(|(_, tags)| tags.clone())
            })
        };
        let Some(tags) = tags else {
            return false;
        };
        let known = self
            .names
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        names.iter().all(|name| {
            known
                .iter()
                .any(|(id, known)| known == name && tags.contains(id))
        })
    }

    /// Does `op`, and says which item has which tag now.
    pub fn apply(&self, op: &TagOp) -> Tagged {
        let mut by_path = self.by_path.write().unwrap_or_else(PoisonError::into_inner);
        let changed = match op {
            TagOp::Names(names) => {
                *self.names.write().unwrap_or_else(PoisonError::into_inner) = names
                    .iter()
                    .map(|(id, name)| (*id, name.to_lowercase()))
                    .collect();
                false
            }
            TagOp::Set { paths, tag, on } => {
                let mut changed = false;
                for path in paths {
                    let has = by_path.get(path).is_some_and(|tags| tags.contains(tag));
                    if has == *on {
                        continue;
                    }
                    changed = true;
                    if *on {
                        by_path.entry(path.clone()).or_default().push(*tag);
                    } else if let Some(tags) = by_path.get_mut(path) {
                        tags.retain(|other| other != tag);
                        if tags.is_empty() {
                            by_path.remove(path);
                        }
                    }
                }
                changed
            }
            TagOp::Clear { paths } => {
                let before = by_path.len();
                for path in paths {
                    by_path.remove(path);
                }
                by_path.len() != before
            }
            TagOp::Rename(pairs) => {
                let mut changed = false;
                for (from, to) in pairs {
                    let from_path = Path::new(from);
                    let moved: Vec<String> = by_path
                        .keys()
                        .filter(|path| Path::new(path).starts_with(from_path))
                        .cloned()
                        .collect();
                    for old in moved {
                        let Some(tags) = by_path.remove(&old) else {
                            continue;
                        };
                        let rest = Path::new(&old)
                            .strip_prefix(from_path)
                            .unwrap_or_else(|_| Path::new(""));
                        let new = if rest.as_os_str().is_empty() {
                            PathBuf::from(to)
                        } else {
                            Path::new(to).join(rest)
                        };
                        by_path.insert(new.to_string_lossy().into_owned(), tags);
                        changed = true;
                    }
                }
                changed
            }
            TagOp::Forget { tag } => {
                let mut changed = false;
                by_path.retain(|_, tags| {
                    let before = tags.len();
                    tags.retain(|other| other != tag);
                    changed |= tags.len() != before;
                    !tags.is_empty()
                });
                changed
            }
        };
        if changed && let Some(file) = &self.file {
            match serde_json::to_vec(&*by_path) {
                Ok(bytes) => {
                    if let Err(error) = crate::storage::write_atomically(file, &bytes) {
                        log::warn!("tags not saved: {error}");
                    }
                }
                Err(error) => log::warn!("tags not saved: {error}"),
            }
        }
        Tagged(
            by_path
                .iter()
                .map(|(path, tags)| (path.clone(), tags.clone()))
                .collect(),
        )
    }

    /// The items that have the tag, as a listing whose entries know their
    /// folders: on this machine the ones still there, on servers the ones
    /// tagged (no server is asked). The rest are named as missing.
    pub fn list(&self, request: &ListTagged, history: &OpenHistory) -> TaggedListing {
        let paths: Vec<String> = self
            .by_path
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter(|(_, tags)| tags.contains(&request.tag))
            .map(|(path, _)| path.clone())
            .collect();
        let mut builder = ListingBuilder::with_capacity(paths.len());
        let mut missing = Vec::new();
        for path in paths {
            let location = Location::parse(&path);
            let (Some(parent), Some(name)) = (location.parent(), location.file_name()) else {
                missing.push(path);
                continue;
            };
            let meta = if location.as_local().is_some() {
                let Ok(found) = Path::new(&path).symlink_metadata() else {
                    missing.push(path);
                    continue;
                };
                crate::listing_io::metadata_of(&found)
            } else {
                // Elsewhere: as it was tagged, a file as far as is known.
                DirEntryMetaData::from(EntryType::File)
            };
            let dir: Arc<str> = Arc::from(parent.to_string());
            builder.push(&dir, name, meta);
        }
        let listing = Arc::new(builder.finish());
        TaggedListing {
            output: ListDirOutput::Listed {
                order: display_order_with(&listing, &request.view, history),
                opened: Arc::new(history.keys_if_any(&listing)),
                listing,
                view: request.view.clone(),
            },
            missing,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dirfleet_core::app::ListView;

    fn tags_of(tagged: &Tagged, path: &str) -> Vec<u32> {
        tagged
            .0
            .iter()
            .find(|(tagged, _)| tagged == path)
            .map(|(_, tags)| tags.clone())
            .unwrap_or_default()
    }

    #[test]
    fn tags_are_given_taken_kept_and_follow_what_is_renamed() {
        let dir = tempfile::tempdir().expect("temp dir");
        let file = dir.path().join("tags.json");
        let store = TagStore::load(Some(file.clone()));
        let set = |paths: &[&str], tag, on| TagOp::Set {
            paths: paths.iter().map(|path| (*path).to_owned()).collect(),
            tag,
            on,
        };
        let _ = store.apply(&set(&["/p/a.txt", "/p/sub/b.txt"], 1, true));
        let tagged = store.apply(&set(&["/p/a.txt", "/q/c.txt"], 2, true));
        assert_eq!(tags_of(&tagged, "/p/a.txt"), [1, 2]);
        assert_eq!(tags_of(&tagged, "/p/sub/b.txt"), [1]);
        // Given twice is given once; taken from what lacks it changes nothing.
        let tagged = store.apply(&set(&["/p/a.txt"], 1, true));
        assert_eq!(tags_of(&tagged, "/p/a.txt"), [1, 2]);
        let tagged = store.apply(&set(&["/q/c.txt"], 1, false));
        assert_eq!(tags_of(&tagged, "/q/c.txt"), [2]);

        // Kept between launches.
        let again = TagStore::load(Some(file));
        let tagged = again.apply(&TagOp::Names(Vec::new()));
        assert_eq!(tags_of(&tagged, "/p/a.txt"), [1, 2]);

        // A renamed folder takes the tags of what is in it along.
        let tagged = store.apply(&TagOp::Rename(vec![("/p".to_owned(), "/moved".to_owned())]));
        assert_eq!(tags_of(&tagged, "/p/a.txt"), Vec::<u32>::new());
        let moved = Path::new("/moved");
        let at = |rest: &str| moved.join(rest).to_string_lossy().into_owned();
        assert_eq!(tags_of(&tagged, &at("a.txt")), [1, 2]);
        assert_eq!(tags_of(&tagged, &at("sub/b.txt")), [1]);

        // The last tag taken, the item is no longer kept; a deleted tag is
        // nobody's; cleared items have none.
        let tagged = store.apply(&TagOp::Forget { tag: 1 });
        assert_eq!(tags_of(&tagged, &at("a.txt")), [2]);
        assert_eq!(tagged.0.len(), 2, "{tagged:?}");
        let tagged = store.apply(&TagOp::Clear {
            paths: vec!["/q/c.txt".to_owned()],
        });
        assert_eq!(tagged.0.len(), 1);
    }

    #[test]
    fn a_tag_s_items_are_listed_where_they_are_and_the_gone_ones_named() {
        let dir = tempfile::tempdir().expect("temp dir");
        let root = dir.path();
        std::fs::create_dir(root.join("sub")).expect("folder");
        std::fs::write(root.join("here.txt"), b"12345").expect("write");
        std::fs::write(root.join("sub/deep.txt"), b"1").expect("write");
        let text = |name: &str| root.join(name).to_string_lossy().into_owned();
        let store = TagStore::default();
        let _ = store.apply(&TagOp::Set {
            paths: vec![
                text("here.txt"),
                text("sub"),
                text("sub/deep.txt"),
                text("gone.txt"),
                "sftp://office/files/remote.txt".to_owned(),
            ],
            tag: 3,
            on: true,
        });
        let _ = store.apply(&TagOp::Set {
            paths: vec![text("here.txt")],
            tag: 4,
            on: true,
        });
        let history = OpenHistory::load(None);
        let listed = store.list(
            &ListTagged {
                tag: 3,
                view: ListView::default(),
            },
            &history,
        );
        assert_eq!(listed.missing, [text("gone.txt")]);
        let ListDirOutput::Listed { listing, order, .. } = listed.output else {
            panic!("a listing");
        };
        let mut names: Vec<(String, u64)> = order
            .iter()
            .map(|&entry| {
                let entry = entry as usize;
                (listing.name(entry).to_owned(), listing.meta(entry).size)
            })
            .collect();
        names.sort();
        // A folder's size is whatever the system says: only the files' count.
        let size = |name: &str| names.iter().find(|(n, _)| n == name).map(|(_, size)| *size);
        assert_eq!(names.len(), 4, "{names:?}");
        assert_eq!(size("here.txt"), Some(5));
        assert_eq!(size("deep.txt"), Some(1));
        assert_eq!(
            size("remote.txt"),
            Some(0),
            "a server's item is not asked about"
        );
        assert!(size("sub").is_some());
        // Entries know their folders: a row's path is the item's.
        let here = (0..listing.len())
            .find(|&entry| listing.name(entry) == "here.txt")
            .expect("listed");
        assert_eq!(listing.entry(here).get_path(), root.join("here.txt"));

        // The other tag has one item.
        let other = store.list(
            &ListTagged {
                tag: 4,
                view: ListView::default(),
            },
            &history,
        );
        let ListDirOutput::Listed { order, .. } = other.output else {
            panic!("a listing");
        };
        assert_eq!(order.len(), 1);
    }

    #[test]
    fn items_are_found_by_their_tags_names() {
        let store = TagStore::default();
        let _ = store.apply(&TagOp::Names(vec![
            (1, "Red".to_owned()),
            (2, "Work".to_owned()),
        ]));
        let _ = store.apply(&TagOp::Set {
            paths: vec!["/p/a.txt".to_owned()],
            tag: 1,
            on: true,
        });
        let _ = store.apply(&TagOp::Set {
            paths: vec!["/p/a.txt".to_owned(), "/p/b.txt".to_owned()],
            tag: 2,
            on: true,
        });
        let has = |path: &str, names: &[&str]| {
            let names: Vec<String> = names.iter().map(|name| (*name).to_owned()).collect();
            store.has_all(path, &names)
        };
        assert!(has("/p/a.txt", &["red"]));
        assert!(has("/p/a.txt", &["red", "work"]));
        assert!(has("/p/b.txt", &["work"]));
        assert!(!has("/p/b.txt", &["red", "work"]));
        assert!(!has("/p/c.txt", &["work"]), "an item without tags");
        assert!(
            has("/p/sub/../a.txt", &["red"]),
            "the same item, written another way"
        );
        assert!(!has("/p/a.txt", &["nosuch"]), "a name no tag has");
    }
}
