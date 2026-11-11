//! The sidebar: favorites, saved searches, recent folders, the user's own
//! folders, the Recycle Bin and the drives, each in a section that folds.
use dirfleet_core::app::{Event, SidebarIcon, SidebarItem, SidebarSection, SidebarSectionView};
use dirfleet_core::drives::DriveKind;
use windows_reactor::{
    Border, Brush, Button, ButtonStyle, Color, Grid, GridLength, HorizontalAlignment, Orientation,
    ProgressBar, ScrollBarVisibility, ScrollViewer, StackPanel, TextBlock, TextTrimming,
    ThemeBrush, Thickness, TooltipExt as _, VerticalAlignment, View, keyed,
};

use crate::core_link::CoreLink;
use crate::files::glyph;

/// The sidebar's width.
pub const WIDTH: f64 = 220.0;

const fn icon_glyph(icon: SidebarIcon) -> &'static str {
    match icon {
        SidebarIcon::Folder => "\u{E8B7}",
        SidebarIcon::Home => "\u{E80F}",
        SidebarIcon::Documents => "\u{E8A5}",
        SidebarIcon::Desktop => "\u{E7F4}",
        SidebarIcon::Downloads => "\u{E896}",
        SidebarIcon::Pictures => "\u{E91B}",
        SidebarIcon::Music => "\u{E8D6}",
        SidebarIcon::Recent => "\u{E823}",
        SidebarIcon::Trash => "\u{E74D}",
        SidebarIcon::SavedSearch => "\u{E721}",
        SidebarIcon::Drive(DriveKind::Removable) => "\u{E88E}",
        SidebarIcon::Drive(DriveKind::Network) => "\u{E968}",
        SidebarIcon::Drive(DriveKind::Optical) => "\u{E958}",
        SidebarIcon::Drive(DriveKind::Fixed | DriveKind::Virtual) => "\u{EDA2}",
    }
}

/// What choosing `item` tells the core, in tab `tab`.
fn chosen(item: &SidebarItem, tab: u32) -> Event {
    item.saved_search.map_or_else(
        || {
            if item.trash {
                Event::OpenTrash { tab }
            } else {
                Event::Navigate {
                    tab,
                    path: item.path.clone(),
                }
            }
        },
        |id| Event::RunSavedSearch { tab, id },
    )
}

/// The window's sidebar; rows act on tab `tab`, the one with the focus.
pub fn sidebar(core: &CoreLink, tab: u32, sections: &[SidebarSectionView]) -> View {
    let parts = sections.iter().zip(0u32..).map(|(section, position)| {
        let mut lines: Vec<View> = vec![heading(core, section)];
        if !section.collapsed {
            lines.extend(section.items.iter().map(|item| row(core, tab, item)));
        }
        keyed(
            position,
            StackPanel::new()
                .orientation(Orientation::Vertical)
                .spacing(1.0)
                .margin(Thickness::new(0.0, 0.0, 0.0, 10.0))
                .children(lines),
        )
    });
    ScrollViewer::new()
        .vertical_scroll_bar_visibility(ScrollBarVisibility::Auto)
        .horizontal_scroll_bar_visibility(ScrollBarVisibility::Disabled)
        .content(
            StackPanel::new()
                .orientation(Orientation::Vertical)
                .margin(Thickness::new(6.0, 6.0, 6.0, 6.0))
                .keyed_children(parts),
        )
        .into()
}

/// A section's title; pressed, it folds or unfolds the section.
fn heading(core: &CoreLink, section: &SidebarSectionView) -> View {
    let core = core.clone();
    let (which, collapsed): (SidebarSection, bool) = (section.section, section.collapsed);
    Button::new()
        .style(ButtonStyle::Subtle)
        .horizontal_alignment(HorizontalAlignment::Stretch)
        .horizontal_content_alignment(HorizontalAlignment::Stretch)
        .automation_name(format!(
            "{} {}",
            if collapsed { "Expand" } else { "Collapse" },
            section.title
        ))
        .content(
            Grid::new()
                .columns([GridLength::STAR, GridLength::Auto])
                .children((
                    TextBlock::new()
                        .text(section.title.clone())
                        .font_size(12.0)
                        .opacity(0.6),
                    glyph(if collapsed { "\u{E76C}" } else { "\u{E70D}" }, 8.0)
                        .opacity(0.6)
                        .grid_column(1),
                )),
        )
        .on_click(move || {
            core.send(Event::SetSectionCollapsed {
                section: which,
                collapsed: !collapsed,
            });
        })
        .into()
}

/// One place: its icon and name; a drive also shows how full it is and can be ejected.
fn row(core: &CoreLink, tab: u32, item: &SidebarItem) -> View {
    let mut label: Vec<View> = vec![
        TextBlock::new()
            .text(item.title.clone())
            .text_trimming(TextTrimming::CharacterEllipsis)
            .into(),
    ];
    if let Some(used) = item.used_permille {
        label.push(
            ProgressBar::new()
                .minimum(0.0)
                .maximum(1000.0)
                .value(f64::from(used))
                // Nearly full drives are marked.
                .show_error(used >= 900)
                .into(),
        );
    }
    let go = {
        let core = core.clone();
        let event = chosen(item, tab);
        move || core.send(event.clone())
    };
    let place = Button::new()
        .style(ButtonStyle::Subtle)
        .horizontal_alignment(HorizontalAlignment::Stretch)
        .horizontal_content_alignment(HorizontalAlignment::Stretch)
        .automation_name(item.title.clone())
        .content(
            Grid::new()
                .columns([GridLength::Pixel(20.0), GridLength::STAR])
                .column_spacing(8.0)
                .children((
                    glyph(icon_glyph(item.icon), 14.0),
                    StackPanel::new()
                        .grid_column(1)
                        .orientation(Orientation::Vertical)
                        .spacing(3.0)
                        .vertical_alignment(VerticalAlignment::Center)
                        .children(label),
                )),
        )
        .on_click(go);
    let place: View = if item.detail.is_empty() {
        place.into()
    } else {
        place.tooltip(&item.detail)
    };
    let eject: View = item.eject.as_ref().map_or_else(
        || Border::new().into(),
        |label| {
            let core = core.clone();
            let mount = item.path.clone();
            Button::new()
                .style(ButtonStyle::Subtle)
                .automation_name(format!("{label} {}", item.title))
                .content(glyph("\u{F847}", 12.0))
                .on_click(move || {
                    core.send(Event::EjectDrive {
                        mount: mount.clone(),
                    });
                })
                .tooltip(label)
        },
    );
    // The place the focused tab shows is marked, as a selected row is.
    let (fill, mark): (Brush, Brush) = if item.active {
        (
            Color::argb(0x28, 0x80, 0x80, 0x80).into(),
            ThemeBrush::Accent.into(),
        )
    } else {
        (Color::transparent().into(), Color::transparent().into())
    };
    Border::new()
        .corner_radius(4.0)
        .background(fill)
        .content(
            Grid::new()
                .columns([GridLength::Pixel(3.0), GridLength::STAR, GridLength::Auto])
                .children((
                    Border::new()
                        .width(3.0)
                        .height(16.0)
                        .corner_radius(1.5)
                        .background(mark)
                        .vertical_alignment(VerticalAlignment::Center),
                    Border::new().grid_column(1).content(place),
                    Border::new().grid_column(2).content(eject),
                )),
        )
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item() -> SidebarItem {
        SidebarItem {
            title: "Place".to_owned(),
            path: "C:\\Place".to_owned(),
            trash: false,
            icon: SidebarIcon::Folder,
            active: false,
            favorite: false,
            detail: String::new(),
            used_permille: None,
            eject: None,
            saved_search: None,
        }
    }

    #[test]
    fn a_folder_is_gone_to() {
        assert_eq!(
            chosen(&item(), 3),
            Event::Navigate {
                tab: 3,
                path: "C:\\Place".to_owned()
            }
        );
    }

    #[test]
    fn the_recycle_bin_and_saved_searches_are_not_paths() {
        let bin = SidebarItem {
            trash: true,
            path: String::new(),
            ..item()
        };
        assert_eq!(chosen(&bin, 3), Event::OpenTrash { tab: 3 });
        let search = SidebarItem {
            saved_search: Some(9),
            ..item()
        };
        assert_eq!(chosen(&search, 3), Event::RunSavedSearch { tab: 3, id: 9 });
    }
}
