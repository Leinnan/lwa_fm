//! The path bar: the folder's path as crumbs to go to, from an anchor (home,
//! a drive, the root) down to the folder shown.
use dirfleet_core::app::{Anchor, CrumbView, Event, TabViewModel};
use windows_reactor::{
    Border, Button, ButtonStyle, Callback, Flyout, FlyoutExt as _, FlyoutPlacement, Key, Menu,
    MenuExt as _, MenuItem, Orientation, ScrollBarVisibility, ScrollViewer, StackPanel, TextBlock,
    TextTrimming, VerticalAlignment, View,
};

use crate::core_link::CoreLink;
use crate::files::glyph;

/// What a crumb is reckoned to take, to tell how many fit: its text and its padding.
const CHARACTER_WIDTH: f64 = 7.2;
const CRUMB_PADDING: f64 = 24.0;
/// The arrow between two crumbs, which lists the subfolders of the one before.
const ARROW_WIDTH: f64 = 26.0;
/// The anchor's icon, or the "…" standing for the crumbs left out.
const SYMBOL_WIDTH: f64 = 34.0;

/// Which crumbs are shown when not all fit: the anchor always, then as many
/// of the last ones as there is room for; the ones between fold into a menu.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fit {
    /// How many crumbs after the anchor are folded away.
    pub folded: usize,
}

/// How many crumbs after the first must fold for the rest to fit in `width`.
pub fn fit(titles: &[&str], width: f64) -> Fit {
    let crumb =
        |title: &str| (title.chars().count() as f64).mul_add(CHARACTER_WIDTH, CRUMB_PADDING);
    let Some((anchor, rest)) = titles.split_first() else {
        return Fit { folded: 0 };
    };
    let anchor_width = SYMBOL_WIDTH + crumb(anchor);
    let widths: Vec<f64> = rest
        .iter()
        .map(|title| ARROW_WIDTH + crumb(title))
        .collect();
    let total: f64 = anchor_width + widths.iter().sum::<f64>();
    if total <= width {
        return Fit { folded: 0 };
    }
    // The folder shown always stays; fold from the anchor's side.
    let mut used = anchor_width + SYMBOL_WIDTH + ARROW_WIDTH;
    let mut kept = 0;
    for crumb_width in widths.iter().rev() {
        if kept > 0 && used + crumb_width > width {
            break;
        }
        used += crumb_width;
        kept += 1;
    }
    Fit {
        folded: rest.len() - kept,
    }
}

/// The icon of an anchor.
const fn anchor_glyph(anchor: Option<Anchor>) -> &'static str {
    match anchor {
        Some(Anchor::Home) => "\u{E80F}",
        Some(Anchor::Volume) => "\u{EDA2}",
        Some(Anchor::Root) | None => "\u{E7F4}",
    }
}

/// The path bar of `tab`, for a space `width` wide.
pub fn path_bar(core: &CoreLink, tab: &TabViewModel, width: f64) -> View {
    let id = tab.id;
    if tab.crumbs.is_empty() {
        // The Recycle Bin and search results have a title, not a path.
        return TextBlock::new()
            .text(tab.title.clone())
            .text_trimming(TextTrimming::CharacterEllipsis)
            .vertical_alignment(VerticalAlignment::Center)
            .into();
    }
    let titles: Vec<&str> = tab
        .crumbs
        .iter()
        .map(|crumb| crumb.title.as_str())
        .collect();
    let folded = fit(&titles, width).folded;
    let go = |path: &str| {
        let core = core.clone();
        let path = path.to_owned();
        move || {
            core.send(Event::Navigate {
                tab: id,
                path: path.clone(),
            });
        }
    };
    let navigate_menu = |crumbs: &[CrumbView]| {
        let core = core.clone();
        let paths: Vec<String> = crumbs.iter().map(|crumb| crumb.path.clone()).collect();
        Menu::new(
            crumbs
                .iter()
                .zip(0u32..)
                .map(|(crumb, position)| MenuItem::item(position, crumb.title.clone())),
            Callback::new(move |key: Key| {
                let chosen = paths
                    .iter()
                    .zip(0u32..)
                    .find(|(_, position)| Key::from(*position) == key);
                if let Some((path, _)) = chosen {
                    core.send(Event::Navigate {
                        tab: id,
                        path: path.clone(),
                    });
                }
            }),
        )
    };

    let mut parts: Vec<View> = Vec::new();
    let (anchor, rest) = (&tab.crumbs[0], &tab.crumbs[1..]);
    // The anchor's icon lists the folders above it.
    let symbol = Button::new()
        .style(ButtonStyle::Subtle)
        .automation_name("Enclosing folders")
        .is_enabled(!tab.crumbs_above.is_empty())
        .content(glyph(anchor_glyph(anchor.anchor), 14.0));
    parts.push(if tab.crumbs_above.is_empty() {
        symbol.into()
    } else {
        symbol.menu(navigate_menu(&tab.crumbs_above))
    });
    let crumb = |crumb: &CrumbView, last: bool| -> View {
        Button::new()
            .style(ButtonStyle::Subtle)
            .automation_name(crumb.title.clone())
            .content(TextBlock::new().text(crumb.title.clone()).opacity(if last {
                1.0
            } else {
                0.8
            }))
            .on_click(go(&crumb.path))
            .into()
    };
    parts.push(crumb(anchor, rest.is_empty()));
    if folded > 0 {
        parts.push(arrow(core, tab, anchor));
        parts.push(
            Button::new()
                .style(ButtonStyle::Subtle)
                .automation_name("More folders")
                .content(glyph("\u{E712}", 12.0))
                .menu(navigate_menu(&rest[..folded])),
        );
    }
    let mut before = if folded > 0 {
        &rest[folded - 1]
    } else {
        anchor
    };
    for (position, shown) in rest.iter().enumerate().skip(folded) {
        parts.push(arrow(core, tab, before));
        parts.push(crumb(shown, position + 1 == rest.len()));
        before = shown;
    }
    // The folder shown lists its subfolders too.
    parts.push(arrow(core, tab, before));
    StackPanel::new()
        .orientation(Orientation::Horizontal)
        .vertical_alignment(VerticalAlignment::Center)
        .children(parts)
        .into()
}

/// The arrow after `crumb`: pressed, it lists the crumb's subfolders to go to.
fn arrow(core: &CoreLink, tab: &TabViewModel, crumb: &CrumbView) -> View {
    let id = tab.id;
    let listed = tab
        .children
        .as_ref()
        .filter(|children| children.path == crumb.path);
    let content: View = match listed {
        None => TextBlock::new().text("Loading…").opacity(0.7).into(),
        Some(children) if children.names.is_empty() => {
            TextBlock::new().text("No folders").opacity(0.7).into()
        }
        Some(children) => {
            let folders: Vec<View> = children
                .names
                .iter()
                .map(|name| {
                    let core = core.clone();
                    let path = std::path::Path::new(&crumb.path)
                        .join(name)
                        .to_string_lossy()
                        .into_owned();
                    Button::new()
                        .style(ButtonStyle::Subtle)
                        .horizontal_content_alignment(windows_reactor::HorizontalAlignment::Left)
                        .horizontal_alignment(windows_reactor::HorizontalAlignment::Stretch)
                        .content(name.clone())
                        .on_click(move || {
                            core.send(Event::Navigate {
                                tab: id,
                                path: path.clone(),
                            });
                        })
                        .into()
                })
                .collect();
            ScrollViewer::new()
                .max_height(360.0)
                .min_width(180.0)
                .vertical_scroll_bar_visibility(ScrollBarVisibility::Auto)
                .content(
                    StackPanel::new()
                        .orientation(Orientation::Vertical)
                        .children(folders),
                )
                .into()
        }
    };
    let core = core.clone();
    let path = crumb.path.clone();
    Border::new()
        .content(
            Button::new()
                .style(ButtonStyle::Subtle)
                .automation_name(format!("Folders in {}", crumb.title))
                .content(glyph("\u{E76C}", 9.0).opacity(0.7))
                .on_click(move || {
                    core.send(Event::ListChildren {
                        tab: id,
                        path: path.clone(),
                    });
                })
                .flyout_with(
                    // The flyout's own root must stay one kind of element: reactor
                    // cannot swap it, so what changes sits inside a border.
                    Flyout::rich(Border::new().content(content))
                        .placement(FlyoutPlacement::BottomEdgeAlignedLeft),
                ),
        )
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crumbs_that_fit_all_show() {
        assert_eq!(fit(&["C:", "Users", "me"], 600.0).folded, 0);
        assert_eq!(fit(&[], 10.0).folded, 0);
        assert_eq!(fit(&["C:"], 10.0).folded, 0);
    }

    #[test]
    fn the_middle_folds_first_and_the_folder_shown_stays() {
        let titles = [
            "C:",
            "Users",
            "someone",
            "Documents",
            "Projects",
            "dirfleet",
        ];
        let roomy = fit(&titles, 420.0).folded;
        let tight = fit(&titles, 260.0).folded;
        assert!(roomy > 0 && roomy < tight, "{roomy} then {tight}");
        // Even with no room the last crumb is kept.
        assert_eq!(fit(&titles, 0.0).folded, titles.len() - 2);
    }
}
