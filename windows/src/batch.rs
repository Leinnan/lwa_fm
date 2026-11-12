//! The batch rename dialog: the steps every name goes through, each with
//! its fields, and what the names come to.
use std::fmt::Write as _;
use std::rc::Rc;

use dirfleet_core::app::{BatchCase, BatchFields, BatchRenameView, BatchScope, Event};
use windows_reactor::{
    Button, ButtonStyle, Callback, Color, Orientation, ScrollBarVisibility, ScrollViewer,
    StackPanel, TextBlock, TextBox, TextWrapping, TooltipExt as _, VerticalAlignment, View,
};

use crate::core_link::CoreLink;

/// What the template's placeholders stand for.
const PLACEHOLDERS: &str = "{name} the name, {ext} its extension, {n} a counter, {parent} the \
                            folder, {modified} and {created} a date (with a format: \
                            {modified:%Y%m%d-%H%M})";

/// What the dialog says above its preview: what stops the rename, or how
/// many names it changes; and how much of a long selection is shown.
pub fn summary(batch: &BatchRenameView) -> String {
    let mut text = if !batch.error.is_empty() {
        format!("The expression can't be read: {}", batch.error)
    } else if batch.problems == 1 {
        "1 name can't be used".to_owned()
    } else if batch.problems > 0 {
        format!("{} names can't be used", batch.problems)
    } else {
        format!("{} of {} names change", batch.changes, batch.total)
    };
    let shown = batch.items.len();
    if (shown as u64) < u64::from(batch.total) {
        let _ = write!(text, ". The first {shown} are shown");
    }
    text
}

/// Renaming several items: how the new names are made, and what they come to.
#[expect(clippy::too_many_lines, reason = "the dialog's lines, top to bottom")]
pub fn dialog(core: &CoreLink, tab: u32, batch: &BatchRenameView) -> View {
    let fields = &batch.fields;
    // Every edit sends all the fields, changed in one place.
    let edit = |change: fn(&mut BatchFields, String)| {
        let core = core.clone();
        let fields = fields.clone();
        Callback::new(move |text: Rc<str>| {
            let mut fields = fields.clone();
            change(&mut fields, text.to_string());
            core.send(Event::SetBatchRename { tab, fields });
        })
    };
    // A button that stays lit while its choice holds; pressing it sends
    // the fields as they are with that choice made.
    let pick = |label: &str, hint: &str, on: bool, change: &dyn Fn(&mut BatchFields)| {
        let core = core.clone();
        let mut after = fields.clone();
        change(&mut after);
        Button::new()
            .content(label)
            .automation_name(hint)
            .style(if on {
                ButtonStyle::Accent
            } else {
                ButtonStyle::Default
            })
            .on_click(move || {
                core.send(Event::SetBatchRename {
                    tab,
                    fields: after.clone(),
                });
            })
            .tooltip(hint)
    };
    let number = |name: &str, value: u32, change: fn(&mut BatchFields, u32)| {
        let core = core.clone();
        let fields = fields.clone();
        TextBox::new()
            .automation_name(name)
            .text(value.to_string())
            .min_width(64.0)
            .on_text_changed(Callback::new(move |text: Rc<str>| {
                // What is no number yet is left to be finished.
                if let Ok(value) = text.trim().parse() {
                    let mut fields = fields.clone();
                    change(&mut fields, value);
                    core.send(Event::SetBatchRename { tab, fields });
                }
            }))
    };
    let label = |text: &str| -> View {
        TextBlock::new()
            .text(text)
            .font_size(12.0)
            .opacity(0.7)
            .vertical_alignment(VerticalAlignment::Center)
            .into()
    };
    let heading = |text: &str| -> View {
        TextBlock::new()
            .text(text)
            .font_size(12.0)
            .opacity(0.9)
            .into()
    };
    let row = |children: Vec<View>| -> View {
        StackPanel::new()
            .orientation(Orientation::Horizontal)
            .spacing(6.0)
            .children(children)
            .into()
    };
    let scope = |label: &str, hint: &str, scope: BatchScope| {
        pick(label, hint, fields.scope == scope, &|fields| {
            fields.scope = scope;
        })
    };
    let case = |label: &str, hint: &str, case: BatchCase| {
        pick(label, hint, fields.case == case, &|fields| {
            fields.case = case;
        })
    };
    let mut lines: Vec<View> = vec![
        heading("Build each name from"),
        TextBox::new()
            .automation_name("Template")
            .text(fields.template.clone())
            .placeholder_text("{name}{ext}")
            .on_text_changed(edit(|fields, text| fields.template = text))
            .tooltip(PLACEHOLDERS),
        row(vec![
            label("Counter from"),
            number("First number", fields.start, |fields, value| {
                fields.start = value;
            })
            .into(),
            label("by"),
            number("Step", fields.step, |fields, value| fields.step = value).into(),
            label("digits"),
            number("Digits at least", fields.padding, |fields, value| {
                fields.padding = value;
            })
            .into(),
        ]),
        heading("Then find and replace"),
        TextBox::new()
            .text(fields.find.clone())
            .placeholder_text("Find")
            .on_text_changed(edit(|fields, text| fields.find = text))
            .into(),
        TextBox::new()
            .text(fields.replace.clone())
            .placeholder_text(if fields.regex {
                "Replace with ($1 is what the first brackets caught)"
            } else {
                "Replace with"
            })
            .on_text_changed(edit(|fields, text| fields.replace = text))
            .into(),
        row(vec![
            pick(
                "Expression",
                "Find by regular expression",
                fields.regex,
                &|fields| fields.regex = !fields.regex,
            ),
            pick(
                "Match case",
                "Capitals and small letters differ",
                fields.match_case,
                &|fields| fields.match_case = !fields.match_case,
            ),
            label("in"),
            scope("All", "The name with its extension", BatchScope::Whole),
            scope("Name", "The name before its extension", BatchScope::Name),
            scope("Extension", "The extension alone", BatchScope::Extension),
        ]),
        heading("Then tidy"),
        row(vec![
            label("Remove first"),
            number(
                "Characters removed from the start",
                fields.remove_first,
                |fields, value| fields.remove_first = value,
            )
            .into(),
            label("last"),
            number(
                "Characters removed from the end",
                fields.remove_last,
                |fields, value| fields.remove_last = value,
            )
            .into(),
            pick(
                "Trim spaces",
                "Spaces at the ends go, and runs of them become one",
                fields.trim,
                &|fields| fields.trim = !fields.trim,
            ),
        ]),
        row(vec![
            case("As is", "Letters stay as they are", BatchCase::Keep),
            case("lower", "Everything in small letters", BatchCase::Lower),
            case("UPPER", "Everything in capitals", BatchCase::Upper),
            case("Title", "Every word with a capital", BatchCase::Title),
            case(
                "Sentence",
                "The first letter a capital",
                BatchCase::Sentence,
            ),
        ]),
    ];
    let stopped = batch.problems > 0 || !batch.error.is_empty();
    let said = TextBlock::new()
        .text(summary(batch))
        .font_size(12.0)
        .text_wrapping(TextWrapping::Wrap);
    lines.push(if stopped {
        said.foreground(Color::rgb(0xE0, 0x50, 0x50)).into()
    } else {
        said.opacity(0.7).into()
    });
    let preview: Vec<View> = batch
        .items
        .iter()
        .map(|item| {
            let refused = !item.problem.is_empty();
            let new = TextBlock::new()
                .text(if refused {
                    format!("{} ({})", item.new, item.problem)
                } else {
                    item.new.clone()
                })
                .font_size(12.0);
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(8.0)
                .children((
                    TextBlock::new()
                        .text(item.old.clone())
                        .font_size(12.0)
                        .opacity(0.6),
                    TextBlock::new().text("→").font_size(12.0).opacity(0.6),
                    if refused {
                        new.foreground(Color::rgb(0xE0, 0x50, 0x50))
                    } else {
                        new
                    },
                ))
                .into()
        })
        .collect();
    lines.push(
        ScrollViewer::new()
            .max_height(200.0)
            .vertical_scroll_bar_visibility(ScrollBarVisibility::Auto)
            .content(
                StackPanel::new()
                    .orientation(Orientation::Vertical)
                    .spacing(2.0)
                    .children(preview),
            )
            .into(),
    );
    StackPanel::new()
        .orientation(Orientation::Vertical)
        .spacing(8.0)
        .children(lines)
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use dirfleet_core::app::BatchItem;

    fn batch(
        total: u32,
        shown: usize,
        changes: u32,
        problems: u32,
        error: &str,
    ) -> BatchRenameView {
        BatchRenameView {
            fields: BatchFields::default(),
            items: vec![
                BatchItem {
                    old: "a".to_owned(),
                    new: "b".to_owned(),
                    problem: String::new(),
                };
                shown
            ],
            total,
            changes,
            problems,
            error: error.to_owned(),
        }
    }

    #[test]
    fn the_rename_dialog_says_what_changes_or_what_stops_it() {
        assert_eq!(summary(&batch(3, 3, 2, 0, "")), "2 of 3 names change");
        assert_eq!(summary(&batch(3, 3, 3, 1, "")), "1 name can't be used");
        assert_eq!(summary(&batch(3, 3, 3, 2, "")), "2 names can't be used");
        assert_eq!(
            summary(&batch(3, 3, 3, 2, "unclosed group")),
            "The expression can't be read: unclosed group"
        );
        // A long selection is renamed whole, though not all of it is shown.
        assert_eq!(
            summary(&batch(950, 200, 950, 0, "")),
            "950 of 950 names change. The first 200 are shown"
        );
    }
}
