//! The bar of a tab that shows what a scan found: how far the scan is, or
//! what it found and what to select of it. What the bar says and offers is
//! worked out as plain data ([`Bar`]) and drawn from that.
use dirfleet_core::app::{CompareView, Event, Keep, ResultsView, SyncCount, TabViewModel};
use windows_reactor::{
    Border, Button, Grid, GridLength, Orientation, StackPanel, TextBlock, TextTrimming, Thickness,
    TooltipExt as _, VerticalAlignment, View,
};

use crate::core_link::CoreLink;
use crate::theme;

/// One of the bar's buttons: its label, what it says when pointed at, and
/// what it tells the core.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Offer {
    pub label: &'static str,
    pub hint: &'static str,
    pub event: Event,
}

/// A button that copies files to one of two folders compared.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Copying {
    pub label: String,
    pub hint: String,
    pub event: Event,
    /// There is something to copy that way.
    pub enabled: bool,
}

/// What a results tab's bar shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bar {
    /// How far the scan is, or what it found.
    pub text: String,
    /// The buttons that select all but one file of each set; none while
    /// there are no sets.
    pub keeps: Vec<Offer>,
    /// Among compared folders: whether files of one size are read to tell
    /// them apart.
    pub contents: Option<bool>,
    /// Among compared folders: copying to the left one and to the right
    /// one; none while they are being compared.
    pub copies: Vec<Copying>,
    /// Among a tag's items: dropping the tags of the ones that are gone,
    /// when some are.
    pub cleanup: Option<Offer>,
    /// Stop while the scan runs, Scan Again afterwards.
    pub scan: Offer,
}

/// `name` in quotes.
fn quoted(name: &str) -> String {
    format!("\u{201c}{name}\u{201d}")
}

/// The button that copies to `into`, the left folder or the right one.
/// Folders named by their whole paths are told apart by their side.
pub fn copying(tab: &TabViewModel, compare: &CompareView, to_left: bool) -> Copying {
    let (into, count): (&str, SyncCount) = if to_left {
        (&compare.left, compare.to_left)
    } else {
        (&compare.right, compare.to_right)
    };
    let by_path = [&compare.left, &compare.right]
        .iter()
        .any(|name| name.contains(['/', '\\']));
    let label = match (by_path, to_left) {
        (true, true) => "Copy Left".to_owned(),
        (true, false) => "Copy Right".to_owned(),
        (false, _) => format!("Copy to {into}"),
    };
    let what = if tab.selected_count == 0 {
        "every file that differs"
    } else {
        "the selected files"
    };
    let hint = match count.copies {
        0 if tab.selected_count == 0 => format!("Nothing to copy to {}", quoted(into)),
        0 => format!("Nothing selected to copy to {}", quoted(into)),
        1 => format!("Copy {what} to {}: 1 file", quoted(into)),
        files => format!("Copy {what} to {}: {files} files", quoted(into)),
    };
    Copying {
        label,
        hint,
        event: Event::SyncSelection {
            tab: tab.id,
            to_left,
        },
        enabled: count.copies > 0,
    }
}

impl Bar {
    /// The bar of `tab`, when it shows what a scan found.
    pub fn of(tab: &TabViewModel) -> Option<Self> {
        let results = tab.results.as_ref()?;
        let id = tab.id;
        if let Some(compare) = &results.compare {
            return Some(Self::comparing(tab, results, compare));
        }
        if let Some(place) = &results.tag {
            return Some(Self {
                text: results.summary.clone(),
                keeps: Vec::new(),
                contents: None,
                copies: Vec::new(),
                cleanup: (place.missing > 0).then_some(Offer {
                    label: "Forget Missing",
                    hint: "Take the tag from the items that are no longer where they were",
                    event: Event::RemoveMissingTagged { tab: id },
                }),
                scan: Offer {
                    label: "Refresh",
                    hint: "List the tag's items again",
                    event: Event::Refresh { tab: id },
                },
            });
        }
        if results.running {
            return Some(Self {
                text: if results.progress.is_empty() {
                    "Looking for duplicates…".to_owned()
                } else {
                    results.progress.clone()
                },
                keeps: Vec::new(),
                contents: None,
                copies: Vec::new(),
                cleanup: None,
                scan: Offer {
                    label: "Stop",
                    hint: "Stop looking",
                    event: Event::StopResults { tab: id },
                },
            });
        }
        let keep = |label, hint, keep| Offer {
            label,
            hint,
            event: Event::SelectDuplicates { tab: id, keep },
        };
        Some(Self {
            text: results.summary.clone(),
            keeps: if results.sets == 0 {
                Vec::new()
            } else {
                vec![
                    keep(
                        "Newest",
                        "Select every file of each set but the one changed last",
                        Keep::Newest,
                    ),
                    keep(
                        "Oldest",
                        "Select every file of each set but the one changed first",
                        Keep::Oldest,
                    ),
                    keep(
                        "First",
                        "Select every file of each set but the one listed first",
                        Keep::First,
                    ),
                ]
            },
            contents: None,
            copies: Vec::new(),
            cleanup: None,
            scan: Offer {
                label: "Scan Again",
                hint: "Look through the folders again",
                event: Event::Refresh { tab: id },
            },
        })
    }

    /// The bar of a tab that compares two folders.
    fn comparing(tab: &TabViewModel, results: &ResultsView, compare: &CompareView) -> Self {
        let id = tab.id;
        if results.running {
            return Self {
                text: if results.progress.is_empty() {
                    "Comparing the folders…".to_owned()
                } else {
                    results.progress.clone()
                },
                keeps: Vec::new(),
                contents: Some(compare.contents),
                copies: Vec::new(),
                cleanup: None,
                scan: Offer {
                    label: "Stop",
                    hint: "Stop comparing",
                    event: Event::StopResults { tab: id },
                },
            };
        }
        Self {
            text: results.summary.clone(),
            keeps: Vec::new(),
            contents: Some(compare.contents),
            copies: vec![copying(tab, compare, true), copying(tab, compare, false)],
            cleanup: None,
            scan: Offer {
                label: "Compare Again",
                hint: "Compare the folders again",
                event: Event::Refresh { tab: id },
            },
        }
    }
}

/// What the switch on the bar of compared folders does.
const READING_HINT: &str =
    "Read files of the same size to tell them apart, whenever they were changed";

/// The bar of `tab`, when it shows what a scan found.
pub fn bar(core: &CoreLink, tab: &TabViewModel) -> Option<View> {
    let bar = Bar::of(tab)?;
    let button = |offer: &Offer| {
        let core = core.clone();
        let event = offer.event.clone();
        Button::new()
            .content(offer.label)
            .automation_name(offer.hint)
            .on_click(move || core.send(event.clone()))
            .tooltip(offer.hint)
    };
    let small = |text: &str| {
        TextBlock::new()
            .text(text)
            .font_size(12.0)
            .opacity(0.7)
            .text_trimming(TextTrimming::CharacterEllipsis)
            .vertical_alignment(VerticalAlignment::Center)
    };
    let mut buttons: Vec<View> = Vec::new();
    if !bar.keeps.is_empty() {
        buttons.push(small("Select all but the").into());
        buttons.extend(bar.keeps.iter().map(button));
    }
    if let Some(reading) = bar.contents {
        let (core, id) = (core.clone(), tab.id);
        buttons.push(small("Read files").tooltip(READING_HINT));
        buttons.push(
            theme::switch(reading)
                .automation_name(READING_HINT)
                .vertical_alignment(VerticalAlignment::Center)
                .on_toggled(move |contents: bool| {
                    core.send(Event::SetCompareOptions { tab: id, contents });
                })
                .into(),
        );
    }
    buttons.extend(bar.copies.iter().map(|copying| {
        let core = core.clone();
        let event = copying.event.clone();
        Button::new()
            .content(copying.label.clone())
            .is_enabled(copying.enabled)
            .automation_name(copying.hint.clone())
            .on_click(move || core.send(event.clone()))
            .tooltip(copying.hint.clone())
    }));
    if let Some(cleanup) = &bar.cleanup {
        buttons.push(button(cleanup));
    }
    buttons.push(button(&bar.scan));
    Some(
        Border::new()
            .border_brush(theme::stroke())
            .border_thickness(Thickness::new(0.0, 0.0, 0.0, 1.0))
            .padding(Thickness::new(12.0, 5.0, 8.0, 5.0))
            .content(
                Grid::new()
                    .columns([GridLength::STAR, GridLength::Auto])
                    .column_spacing(12.0)
                    .children((
                        small(&bar.text).tooltip(&bar.text),
                        StackPanel::new()
                            .grid_column(1)
                            .orientation(Orientation::Horizontal)
                            .spacing(6.0)
                            .children(buttons),
                    )),
            )
            .into(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tab(results: ResultsView) -> TabViewModel {
        TabViewModel {
            id: 7,
            results: Some(results),
            ..TabViewModel::default()
        }
    }

    #[test]
    fn a_folder_has_no_results_bar() {
        assert_eq!(Bar::of(&TabViewModel::default()), None);
    }

    #[test]
    fn a_running_scan_says_how_far_it_is_and_can_only_be_stopped() {
        let bar = Bar::of(&tab(ResultsView {
            running: true,
            progress: "Comparing 40 files…".to_owned(),
            ..ResultsView::default()
        }))
        .expect("a bar");
        assert_eq!(bar.text, "Comparing 40 files…");
        assert_eq!(bar.keeps, Vec::new());
        assert_eq!(
            (bar.scan.label, bar.scan.event),
            ("Stop", Event::StopResults { tab: 7 })
        );
        // Before the scan has said anything, the bar still says what goes on.
        let bar = Bar::of(&tab(ResultsView {
            running: true,
            ..ResultsView::default()
        }))
        .expect("a bar");
        assert_eq!(bar.text, "Looking for duplicates…");
    }

    #[test]
    fn what_was_found_can_be_selected_all_but_one_and_scanned_again() {
        let bar = Bar::of(&tab(ResultsView {
            summary: "2 sets of duplicates, 5 files".to_owned(),
            sets: 2,
            ..ResultsView::default()
        }))
        .expect("a bar");
        assert_eq!(bar.text, "2 sets of duplicates, 5 files");
        let keeps: Vec<(&str, Event)> = bar
            .keeps
            .iter()
            .map(|offer| (offer.label, offer.event.clone()))
            .collect();
        let select = |keep| Event::SelectDuplicates { tab: 7, keep };
        assert_eq!(
            keeps,
            [
                ("Newest", select(Keep::Newest)),
                ("Oldest", select(Keep::Oldest)),
                ("First", select(Keep::First)),
            ]
        );
        assert_eq!(
            (bar.scan.label, bar.scan.event),
            ("Scan Again", Event::Refresh { tab: 7 })
        );
        // Nothing found: nothing to select.
        let bar = Bar::of(&tab(ResultsView {
            summary: "No duplicates among 12 files".to_owned(),
            ..ResultsView::default()
        }))
        .expect("a bar");
        assert_eq!(bar.keeps, Vec::new());
        assert_eq!((bar.contents, bar.copies.len()), (None, 0));
    }

    fn compared(running: bool, to_left: u32, to_right: u32) -> ResultsView {
        let count = |copies| SyncCount {
            copies,
            ..SyncCount::default()
        };
        ResultsView {
            running,
            summary: "3 differences".to_owned(),
            compare: Some(CompareView {
                left: "a".to_owned(),
                right: "b".to_owned(),
                contents: true,
                to_left: count(to_left),
                to_right: count(to_right),
                ..CompareView::default()
            }),
            ..ResultsView::default()
        }
    }

    #[test]
    fn compared_folders_are_copied_either_way_and_compared_again() {
        let bar = Bar::of(&tab(compared(false, 0, 3))).expect("a bar");
        assert_eq!(bar.text, "3 differences");
        assert_eq!(bar.keeps, Vec::new(), "there are no sets to keep one of");
        assert_eq!(bar.contents, Some(true));
        let copies: Vec<(&str, bool, &Event)> = bar
            .copies
            .iter()
            .map(|copying| (copying.label.as_str(), copying.enabled, &copying.event))
            .collect();
        let sync = |to_left| Event::SyncSelection { tab: 7, to_left };
        assert_eq!(
            copies,
            [
                ("Copy to a", false, &sync(true)),
                ("Copy to b", true, &sync(false))
            ]
        );
        assert_eq!(bar.copies[0].hint, "Nothing to copy to \u{201c}a\u{201d}");
        assert_eq!(
            bar.copies[1].hint,
            "Copy every file that differs to \u{201c}b\u{201d}: 3 files"
        );
        assert_eq!(
            (bar.scan.label, bar.scan.event),
            ("Compare Again", Event::Refresh { tab: 7 })
        );

        // With a selection, it is the selected files that go.
        let mut selecting = tab(compared(false, 1, 0));
        selecting.selected_count = 2;
        let bar = Bar::of(&selecting).expect("a bar");
        assert_eq!(
            bar.copies[0].hint,
            "Copy the selected files to \u{201c}a\u{201d}: 1 file"
        );
        assert_eq!(
            bar.copies[1].hint,
            "Nothing selected to copy to \u{201c}b\u{201d}"
        );

        // While the folders are compared nothing is copied.
        let bar = Bar::of(&tab(compared(true, 2, 2))).expect("a bar");
        assert_eq!(bar.text, "Comparing the folders…");
        assert_eq!(bar.copies, Vec::new());
        assert_eq!(
            (bar.scan.label, bar.scan.event),
            ("Stop", Event::StopResults { tab: 7 })
        );
    }

    #[test]
    fn a_tag_s_items_are_refreshed_and_the_gone_ones_forgotten() {
        use dirfleet_core::app::TagPlaceView;
        let showing = |missing| {
            tab(ResultsView {
                summary: "3 items".to_owned(),
                tag: Some(TagPlaceView {
                    id: 2,
                    name: "Work".to_owned(),
                    missing,
                    ..TagPlaceView::default()
                }),
                ..ResultsView::default()
            })
        };
        let bar = Bar::of(&showing(0)).expect("a bar");
        assert_eq!(bar.text, "3 items");
        assert_eq!(bar.cleanup, None);
        assert_eq!(
            (bar.scan.label, bar.scan.event),
            ("Refresh", Event::Refresh { tab: 7 })
        );
        let bar = Bar::of(&showing(2)).expect("a bar");
        assert_eq!(
            bar.cleanup.map(|offer| (offer.label, offer.event)),
            Some(("Forget Missing", Event::RemoveMissingTagged { tab: 7 }))
        );
    }

    #[test]
    fn folders_named_by_their_paths_are_copied_to_by_side() {
        let mut results = compared(false, 1, 1);
        if let Some(compare) = &mut results.compare {
            compare.left = "C:\\x\\src".to_owned();
            compare.right = "D:\\y\\src".to_owned();
        }
        let bar = Bar::of(&tab(results)).expect("a bar");
        let labels: Vec<&str> = bar.copies.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(labels, ["Copy Left", "Copy Right"]);
        assert_eq!(
            bar.copies[1].hint,
            "Copy every file that differs to \u{201c}D:\\y\\src\u{201d}: 1 file"
        );
    }
}
