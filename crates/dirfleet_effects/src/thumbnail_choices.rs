//! Video frames chosen as thumbnails, as egui's `thumbnail_choice`.
//!
//! They are kept as JSON beside the app's state, by path as the core names
//! it. A choice holds
//! for the version of the file (revision and size) it was made for, so a
//! changed file gets its default thumbnail back.
//!
//! The media service draws posters from the choices installed last.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Once, PoisonError, RwLock};
use std::time::Duration;

use dirfleet_core::app::{ChosenFrame, ThumbnailChoiceOp, ThumbnailChoices as Listed};

#[derive(Default)]
pub struct ThumbnailChoices {
    by_path: RwLock<BTreeMap<String, ChosenFrame>>,
    file: Option<PathBuf>,
}

/// The choices the media service draws from.
static INSTALLED: RwLock<Option<Arc<ThumbnailChoices>>> = RwLock::new(None);

impl ThumbnailChoices {
    /// The choices kept in `file`, or none kept when there is no file.
    pub fn load(file: Option<PathBuf>) -> Self {
        let by_path = file
            .as_deref()
            .and_then(|file| std::fs::read(file).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Self {
            by_path: RwLock::new(by_path),
            file,
        }
    }

    /// Makes these the choices posters are drawn from.
    pub fn install(self: &Arc<Self>) {
        static HOOK: Once = Once::new();
        HOOK.call_once(|| {
            crate::media::set_chosen_frames(|path, revision, size| {
                INSTALLED
                    .read()
                    .unwrap_or_else(PoisonError::into_inner)
                    .as_ref()?
                    .get(path, revision, size)
            });
        });
        *INSTALLED.write().unwrap_or_else(PoisonError::into_inner) = Some(Arc::clone(self));
    }

    fn get(&self, path: &Path, revision: u128, size: u64) -> Option<Duration> {
        let by_path = self.by_path.read().unwrap_or_else(PoisonError::into_inner);
        by_path
            .get(path.to_string_lossy().as_ref())
            .filter(|chosen| (chosen.revision, chosen.size) == (revision, size))
            .map(|chosen| Duration::from_millis(chosen.at_ms))
    }

    /// Does `op`, and says what the choices are now.
    pub fn apply(&self, op: &ThumbnailChoiceOp) -> Listed {
        let mut by_path = self.by_path.write().unwrap_or_else(PoisonError::into_inner);
        let changed = match op {
            ThumbnailChoiceOp::Load => false,
            ThumbnailChoiceOp::Set { path, frame } => match frame {
                Some(frame) => by_path.insert(path.clone(), *frame) != Some(*frame),
                None => by_path.remove(path).is_some(),
            },
            ThumbnailChoiceOp::Rename(pairs) => {
                let mut changed = false;
                for (from, to) in pairs {
                    let from_path = Path::new(from);
                    let moved: Vec<String> = by_path
                        .keys()
                        .filter(|path| Path::new(path).starts_with(from_path))
                        .cloned()
                        .collect();
                    for old in moved {
                        let Some(chosen) = by_path.remove(&old) else {
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
                        by_path.insert(new.to_string_lossy().into_owned(), chosen);
                        changed = true;
                    }
                }
                changed
            }
            ThumbnailChoiceOp::Clear => {
                let had = !by_path.is_empty();
                by_path.clear();
                had
            }
        };
        if changed && let Some(file) = &self.file {
            match serde_json::to_vec(&*by_path) {
                Ok(bytes) => {
                    if let Err(error) = crate::storage::write_atomically(file, &bytes) {
                        log::warn!("thumbnail choices not saved: {error}");
                    }
                }
                Err(error) => log::warn!("thumbnail choices not saved: {error}"),
            }
        }
        Listed(
            by_path
                .iter()
                .map(|(path, chosen)| (path.clone(), *chosen))
                .collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(at_ms: u64) -> ChosenFrame {
        ChosenFrame {
            at_ms,
            revision: 7,
            size: 100,
        }
    }

    #[test]
    fn a_choice_holds_for_its_version_survives_a_reload_and_follows_renames() {
        let dir = tempfile::tempdir().expect("temp dir");
        let file = dir.path().join("thumbnail_choices.json");
        let choices = ThumbnailChoices::load(Some(file.clone()));
        let set = |path: &str, at| ThumbnailChoiceOp::Set {
            path: path.into(),
            frame: Some(frame(at)),
        };
        let _ = choices.apply(&set("/v/sub/a.mp4", 4_500));
        let _ = choices.apply(&set("/v/b.mp4", 1_000));
        let at = |choices: &ThumbnailChoices, path: &str, revision| {
            choices.get(Path::new(path), revision, 100)
        };
        assert_eq!(
            at(&choices, "/v/sub/a.mp4", 7),
            Some(Duration::from_millis(4_500))
        );
        assert_eq!(at(&choices, "/v/sub/a.mp4", 8), None, "the file changed");

        let reloaded = ThumbnailChoices::load(Some(file.clone()));
        let listed = reloaded.apply(&ThumbnailChoiceOp::Rename(vec![(
            "/v/sub".into(),
            "/w/moved".into(),
        )]));
        assert_eq!(
            listed
                .0
                .iter()
                .map(|(path, _)| path.as_str())
                .collect::<Vec<_>>(),
            ["/v/b.mp4", "/w/moved/a.mp4"]
        );
        let _ = reloaded.apply(&ThumbnailChoiceOp::Set {
            path: "/v/b.mp4".into(),
            frame: None,
        });
        let again = ThumbnailChoices::load(Some(file));
        assert_eq!(again.apply(&ThumbnailChoiceOp::Load).0.len(), 1);
        assert_eq!(again.apply(&ThumbnailChoiceOp::Clear).0, Vec::new());
    }
}
