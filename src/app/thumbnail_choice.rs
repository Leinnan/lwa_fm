//! Video frames the user picked as a file's thumbnail instead of the default
//! first overview moment.
//!
//! A choice is tied to the file's revision and size: once the file changes,
//! its frames may show something else, so the default thumbnail returns.
use crate::helper::canonical_form;
use bincode::{Decode, Encode};
use foldhash::{HashMap, HashMapExt};
use std::{
    path::{Path, PathBuf},
    sync::{LazyLock, RwLock},
    time::Duration,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
struct ThumbnailChoice {
    timestamp_ms: u64,
    revision: u128,
    size: u64,
}

struct Choices {
    tree: Option<sled::Tree>,
    by_path: HashMap<PathBuf, ThumbnailChoice>,
}

impl Choices {
    fn load(tree: Option<sled::Tree>) -> Self {
        let mut by_path = HashMap::new();
        for (key, value) in tree
            .iter()
            .flat_map(sled::Tree::iter)
            .filter_map(Result::ok)
        {
            let Ok(path) = std::str::from_utf8(&key) else {
                continue;
            };
            if let Ok((choice, _)) = bincode::decode_from_slice(&value, bincode::config::standard())
            {
                by_path.insert(PathBuf::from(path), choice);
            }
        }
        Self { tree, by_path }
    }

    fn key(path: &Path) -> Vec<u8> {
        path.to_string_lossy().into_owned().into_bytes()
    }

    fn get(&self, path: &Path, revision: u128, size: u64) -> Option<Duration> {
        let choice = self.by_path.get(&canonical_form(path))?;
        (choice.revision == revision && choice.size == size)
            .then(|| Duration::from_millis(choice.timestamp_ms))
    }

    fn put(&mut self, path: PathBuf, choice: ThumbnailChoice) {
        if let Some(tree) = &self.tree
            && let Ok(bytes) = bincode::encode_to_vec(choice, bincode::config::standard())
        {
            _ = tree.insert(Self::key(&path), bytes);
        }
        self.by_path.insert(path, choice);
    }

    fn remove(&mut self, path: &Path) -> Option<ThumbnailChoice> {
        if let Some(tree) = &self.tree {
            _ = tree.remove(Self::key(path));
        }
        self.by_path.remove(path)
    }

    fn rename(&mut self, from: &Path, to: &Path) {
        let from = canonical_form(from);
        let to = canonical_form(to);
        let moved: Vec<PathBuf> = self
            .by_path
            .keys()
            .filter(|path| path.starts_with(&from))
            .cloned()
            .collect();
        for old in moved {
            let Ok(rest) = old.strip_prefix(&from) else {
                continue;
            };
            let new = if rest.as_os_str().is_empty() {
                to.clone()
            } else {
                to.join(rest)
            };
            if let Some(choice) = self.remove(&old) {
                self.put(new, choice);
            }
        }
    }

    fn clear(&mut self) {
        if let Some(tree) = &self.tree {
            _ = tree.clear();
        }
        self.by_path.clear();
    }
}

static CHOICES: LazyLock<RwLock<Choices>> = LazyLock::new(|| {
    #[cfg(not(test))]
    let tree = crate::app::database::SLED_DIRS
        .open_tree("thumbnail_choice")
        .ok();
    // Tests never touch the real choices.
    #[cfg(test)]
    let tree = sled::Config::new()
        .temporary(true)
        .open()
        .ok()
        .and_then(|db| db.open_tree("thumbnail_choice").ok());
    RwLock::new(Choices::load(tree))
});

/// The source position of the frame chosen for `path`, while the file still
/// has the `revision` and `size` it had when the frame was chosen.
#[must_use]
pub fn get(path: &Path, revision: u128, size: u64) -> Option<Duration> {
    let choices = CHOICES.read().ok()?;
    if choices.by_path.is_empty() {
        return None;
    }
    choices.get(path, revision, size)
}

/// Whether `path` has a choice, current or not.
#[must_use]
pub fn has(path: &Path) -> bool {
    CHOICES
        .read()
        .is_ok_and(|choices| choices.by_path.contains_key(&canonical_form(path)))
}

pub fn set(path: &Path, revision: u128, size: u64, timestamp: Duration) {
    if let Ok(mut choices) = CHOICES.write() {
        choices.put(
            canonical_form(path),
            ThumbnailChoice {
                timestamp_ms: u64::try_from(timestamp.as_millis()).unwrap_or(u64::MAX),
                revision,
                size,
            },
        );
    }
}

/// Forgets the choice for `path`; returns whether there was one.
pub fn clear(path: &Path) -> bool {
    CHOICES
        .write()
        .is_ok_and(|mut choices| choices.remove(&canonical_form(path)).is_some())
}

/// Moves the choice of `from` (a file, or every file below a folder) to `to`.
pub fn rename(from: &Path, to: &Path) {
    if let Ok(mut choices) = CHOICES.write() {
        choices.rename(from, to);
    }
}

pub fn clear_all() {
    if let Ok(mut choices) = CHOICES.write() {
        choices.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_db() -> sled::Db {
        sled::Config::new().temporary(true).open().expect("temp db")
    }

    #[test]
    fn a_choice_applies_only_to_the_revision_it_was_made_for() {
        let mut choices = Choices::load(temp_db().open_tree("t").ok());
        let path = canonical_form(Path::new("/videos/clip.mp4"));
        choices.put(
            path.clone(),
            ThumbnailChoice {
                timestamp_ms: 4_500,
                revision: 7,
                size: 100,
            },
        );
        assert_eq!(
            choices.get(&path, 7, 100),
            Some(Duration::from_millis(4_500))
        );
        assert_eq!(choices.get(&path, 8, 100), None);
        assert_eq!(choices.get(&path, 7, 101), None);
    }

    #[test]
    fn choices_survive_a_reload_and_follow_renames() {
        let db = temp_db();
        let from = canonical_form(Path::new("/choice_src/sub/a.mp4"));
        {
            let mut choices = Choices::load(db.open_tree("t").ok());
            choices.put(
                from.clone(),
                ThumbnailChoice {
                    timestamp_ms: 1_000,
                    revision: 1,
                    size: 2,
                },
            );
        }
        let mut choices = Choices::load(db.open_tree("t").ok());
        assert!(choices.get(&from, 1, 2).is_some());
        choices.rename(Path::new("/choice_src"), Path::new("/choice_dst"));
        let to = canonical_form(Path::new("/choice_dst/sub/a.mp4"));
        assert_eq!(choices.get(&from, 1, 2), None);
        assert_eq!(choices.get(&to, 1, 2), Some(Duration::from_secs(1)));
        let reloaded = Choices::load(db.open_tree("t").ok());
        assert!(reloaded.get(&to, 1, 2).is_some());
        assert!(reloaded.get(&from, 1, 2).is_none());
    }

    #[test]
    fn public_api_sets_and_clears() {
        let path = Path::new("/choice_api/clip.mp4");
        set(path, 3, 4, Duration::from_millis(250));
        assert!(has(path));
        assert_eq!(get(path, 3, 4), Some(Duration::from_millis(250)));
        assert!(clear(path));
        assert!(!clear(path));
        assert_eq!(get(path, 3, 4), None);
    }
}
