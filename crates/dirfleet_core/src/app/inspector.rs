//! The inspector, as egui's: one selected item's name, path, type, size and
//! dates; for several, the counts, the files' total size and a breakdown by
//! kind. A folder's size is added up on request (`sizes`), since walking a
//! tree is slow, with hardlinked files counted once.
use std::collections::BTreeMap;

use facet::Facet;
use serde::{Deserialize, Serialize};

use super::checksums::ChecksumsView;
use super::format;
use super::thumbnails::{PREVIEW_EDGE, PreviewState, plays};
use super::{Model, Tab};
use crate::listing::{EntryType, TimestampSeconds, kind_label};
use crate::search::FileKind;
use crate::search::filter::kind_of;

#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct InspectorView {
    /// The item selected (or under the cursor), when there is one.
    pub item: Option<ItemInfo>,
    /// Several selected items, summed up.
    pub summary: Option<SelectionSummary>,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ItemInfo {
    pub name: String,
    pub path: String,
    pub kind: String,
    pub is_dir: bool,
    pub size: SizeInfo,
    pub created: String,
    pub modified: String,
    pub opened: String,
    /// Times opened from the app.
    pub opens: u32,
    pub preview: PreviewState,
    /// A video's or GIF's frames as an animated GIF to play, or "".
    pub animation: String,
    /// A file's checksums, worked out or not; folders have none.
    pub checksums: Option<ChecksumsView>,
    /// A picture's or video's size in pixels ("1920 × 1080"), or "".
    pub dimensions: String,
    /// How long a video or a sound plays ("3:07"), or "".
    pub duration: String,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[repr(C)]
pub enum SizeInfo {
    Known(String),
    /// A folder whose size hasn't been asked for: `CalculateSize` adds it up.
    NotCalculated,
    Calculating,
    Unavailable,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct SelectionSummary {
    pub count: u32,
    pub folders: u32,
    pub files: u32,
    /// The selected files' total size; folders are not added up.
    pub total: String,
    pub kinds: Vec<KindCount>,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct KindCount {
    pub kind: String,
    pub count: u32,
}

fn date(time: crate::listing::TimestampSeconds) -> String {
    if *time == 0 {
        "Unavailable".to_owned()
    } else {
        format::date(time)
    }
}

fn bytes(size: u64) -> String {
    format::bytes(size)
}

impl Tab {
    /// What the inspector shows for the tab's selection.
    /// When the entry was last opened: from the open history, or else the
    /// file system's access time.
    pub(super) fn last_opened(&self, entry: usize) -> TimestampSeconds {
        self.opened.get(entry).map_or_else(
            || self.listing.meta(entry).accessed_at,
            |&(_, last)| TimestampSeconds::from_unix(last),
        )
    }

    /// The entry's known dates, exactly, one per line, as egui's row
    /// tooltip shows them.
    pub(super) fn exact_dates(&self, entry: usize) -> String {
        let meta = self.listing.meta(entry);
        let changed = if self.trash { "Deleted" } else { "Modified" };
        [
            (changed, meta.modified_at),
            ("Created", meta.created_at),
            ("Opened", self.last_opened(entry)),
        ]
        .into_iter()
        .filter(|(_, time)| **time != 0)
        .map(|(label, time)| format!("{label}: {}", format::date(time)))
        .collect::<Vec<_>>()
        .join("\n")
    }

    pub(super) fn inspector_view(&self, model: &Model) -> InspectorView {
        // One item (or the cursor's), nothing, or a summary of several.
        let Some(row) = self.single_item() else {
            return InspectorView {
                item: None,
                summary: (self.selection.len() > 1).then(|| self.selection_summary()),
            };
        };
        InspectorView {
            item: self.item_info(row, model),
            summary: None,
        }
    }

    /// What several selected items are, added up.
    pub(super) fn selection_summary_now(&self) -> SelectionSummary {
        let mut summary = SelectionSummary {
            count: u32::try_from(self.selection.len()).unwrap_or(u32::MAX),
            folders: 0,
            files: 0,
            total: String::new(),
            kinds: Vec::new(),
        };
        let mut total = 0_u64;
        let mut kinds = BTreeMap::<&'static str, u32>::new();
        for row in self.selection.rows() {
            let Some(&entry) = self.order.get(row) else {
                continue;
            };
            let entry = entry as usize;
            let meta = self.listing.meta(entry);
            let is_dir = meta.entry_type == EntryType::Directory;
            if is_dir {
                summary.folders += 1;
            } else {
                summary.files += 1;
                total += meta.size;
            }
            let kind = kind_of(self.listing.name(entry), is_dir).map_or("Other", FileKind::label);
            *kinds.entry(kind).or_default() += 1;
        }
        summary.total = bytes(total);
        summary.kinds = kinds
            .into_iter()
            .map(|(kind, count)| KindCount {
                kind: kind.to_owned(),
                count,
            })
            .collect();
        summary
    }

    fn item_info(&self, row: usize, model: &Model) -> Option<ItemInfo> {
        let (thumbnails, checksums) = (&model.thumbnails, &model.checksums);
        let entry = *self.order.get(row)? as usize;
        let meta = self.listing.meta(entry);
        let name = self.listing.name(entry);
        let path = self.row_path(row)?.to_string_lossy().into_owned();
        let is_dir = meta.entry_type == EntryType::Directory;
        let size = if is_dir {
            model.sizes.info(&path)
        } else {
            SizeInfo::Known(bytes(meta.size))
        };
        Some(ItemInfo {
            name: name.to_owned(),
            preview: thumbnails.preview(&path, meta),
            animation: if plays(&path, meta) {
                thumbnails.animation(&path, PREVIEW_EDGE, meta)
            } else {
                String::new()
            },
            // The Trash's rows are not files to read where they are shown.
            checksums: (meta.entry_type.is_leaf() && !self.trash)
                .then(|| self.checksums_view(checksums, entry, &path)),
            dimensions: model.media.dimensions(&path, meta),
            duration: model.media.duration(&path, meta),
            path,
            kind: kind_label(name, meta.entry_type),
            is_dir,
            size,
            created: date(meta.created_at),
            modified: date(meta.modified_at),
            opened: date(self.last_opened(entry)),
            opens: self.opened.get(entry).map_or(0, |&(count, _)| count),
        })
    }
}
