//! The folder's entries: the list, the icon grid, and what they are made of.
mod columns;
mod layout;
mod press;
mod rename;
mod row;
mod tiles;
mod view;

pub use columns::ColumnSet;
pub use view::{FileView, FileViewInput};

use windows_reactor::{Border, Image, Stretch, TextBlock, VerticalAlignment, View};

use crate::icons::{self, IconSize};

/// Files dropped on a tab from outside or from another pane.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dropped {
    pub tab: u32,
    pub paths: Vec<String>,
    /// The folder dropped on; the tab's own when `None`.
    pub target: Option<String>,
    /// The tab shows the Recycle Bin: what is dropped there is deleted.
    pub on_trash: bool,
}

/// A glyph of the system icon font, `size` pixels tall.
pub fn glyph(glyph: &str, size: f64) -> TextBlock {
    TextBlock::new()
        .text(glyph)
        .font_family("Segoe Fluent Icons,Segoe MDL2 Assets")
        .font_size(size)
        .vertical_alignment(VerticalAlignment::Center)
}

/// The shell's icon for an entry, `size` points on each side; a glyph when
/// the shell has none.
pub fn entry_icon(name: &str, is_dir: bool, size: f64) -> View {
    icons::for_entry(name, is_dir, IconSize::for_points(size)).map_or_else(
        || {
            Border::new()
                .width(size)
                .height(size)
                .content(glyph(
                    if is_dir { "\u{E8B7}" } else { "\u{E7C3}" },
                    size * 0.9,
                ))
                .into()
        },
        |icon| {
            Image::new()
                .source_data(icon)
                .stretch(Stretch::Uniform)
                .width(size)
                .height(size)
                .vertical_alignment(VerticalAlignment::Center)
                .into()
        },
    )
}
