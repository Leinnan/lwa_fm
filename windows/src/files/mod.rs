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

use dirfleet_core::app::TextRange;
use windows_reactor::{
    Border, Image, RichText, RichTextBlock, RichTextInline, RichTextRun, Stretch, TextBlock,
    TextWrapping, VerticalAlignment, View,
};

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

/// `name` cut where what a search matched begins and ends: each piece, and
/// whether it matched. Ranges count bytes; one that does not fall between
/// characters, or runs past the name, is left out.
fn pieces<'a>(name: &'a str, highlights: &[TextRange]) -> Vec<(&'a str, bool)> {
    let mut pieces = Vec::new();
    let mut done = 0;
    for range in highlights {
        let (start, end) = (range.start as usize, range.end as usize);
        if start < done || end <= start || name.get(start..end).is_none() {
            continue;
        }
        if start > done {
            pieces.push((&name[done..start], false));
        }
        pieces.push((&name[start..end], true));
        done = end;
    }
    if done < name.len() {
        pieces.push((&name[done..], false));
    }
    pieces
}

/// A name on one line with what the search matched in bold.
pub fn highlighted(name: &str, highlights: &[TextRange]) -> RichTextBlock {
    let runs = pieces(name, highlights).into_iter().map(|(text, matched)| {
        RichTextInline::Run(RichTextRun {
            text: text.to_owned(),
            is_bold: matched,
            is_italic: false,
        })
    });
    RichTextBlock::new()
        .paragraphs(RichText::single_paragraph(runs))
        // The row takes the presses, not the text.
        .is_text_selection_enabled(false)
        .text_wrapping(TextWrapping::NoWrap)
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

#[cfg(test)]
mod tests {
    use super::*;

    const fn range(start: u32, end: u32) -> TextRange {
        TextRange { start, end }
    }

    #[test]
    fn a_name_is_cut_where_its_matches_are() {
        assert_eq!(
            pieces("report-2024.pdf", &[range(0, 6), range(12, 15)]),
            [("report", true), ("-2024.", false), ("pdf", true)]
        );
        assert_eq!(pieces("plain", &[]), [("plain", false)]);
    }

    #[test]
    fn a_range_that_cannot_be_cut_is_left_out() {
        // Inside a character, past the end, and overlapping the one before.
        assert_eq!(pieces("żółw", &[range(1, 2)]), [("żółw", false)]);
        assert_eq!(pieces("abc", &[range(1, 9)]), [("abc", false)]);
        assert_eq!(
            pieces("abcdef", &[range(0, 3), range(2, 4)]),
            [("abc", true), ("def", false)]
        );
    }
}
