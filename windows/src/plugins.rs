//! What plugins show in the app: their part of a folder's status bar, their
//! inspector sections, the forms they ask to fill in, the results they list,
//! the jobs they run, and their page in Settings.
use std::rc::Rc;

use dirfleet_core::app::{
    Event, FormFieldKind, FormFieldView, FormPathMode, PluginBarView, PluginFormView,
    PluginJobView, PluginResultsView, PluginSectionView, PluginView, PluginsView, ResultsRowView,
    TabViewModel,
};
use windows_reactor::{
    Border, Button, ButtonStyle, Callback, Color, ComboBox, FontWeight, Grid, GridLength,
    HorizontalAlignment, Key, Menu, MenuExt as _, MenuItem, Orientation, ProgressBar, ProgressRing,
    ScrollBarVisibility, ScrollViewer, Slider, StackPanel, TextBlock, TextBox, TextTrimming,
    TextWrapping, Thickness, TooltipExt as _, VerticalAlignment, View, keyed,
};

use crate::core_link::CoreLink;
use crate::files::glyph;
use crate::theme;

/// How many result rows a dialog draws; the rest are counted in a note.
const RESULT_ROWS: usize = 300;

fn small(text: &str) -> TextBlock {
    TextBlock::new().text(text).font_size(12.0)
}

fn problem(text: &str) -> View {
    small(text)
        .foreground(Color::rgb(0xE0, 0x50, 0x50))
        .text_wrapping(TextWrapping::Wrap)
        .into()
}

fn spinner() -> View {
    ProgressRing::new()
        .is_indeterminate(true)
        .is_active(true)
        .width(14.0)
        .height(14.0)
        .into()
}

/// The color a segment's tone asks for; `None` keeps the text's own.
const fn tone(tone: &str) -> Option<Color> {
    match tone.as_bytes() {
        b"good" => Some(Color::rgb(0x4C, 0xB0, 0x60)),
        b"warn" => Some(Color::rgb(0xD8, 0xA0, 0x30)),
        b"bad" => Some(Color::rgb(0xE0, 0x50, 0x50)),
        _ => None,
    }
}

/// What plugins add to the bar under a folder; `None` when they add nothing.
pub fn bar(core: &CoreLink, tab: &TabViewModel) -> Option<View> {
    if tab.plugin_bar.is_empty() {
        return None;
    }
    let id = tab.id;
    let parts: Vec<View> = tab
        .plugin_bar
        .iter()
        .flat_map(|bar: &PluginBarView| {
            let mut parts: Vec<View> = Vec::new();
            if bar.loading {
                parts.push(spinner());
            }
            if !bar.error.is_empty() {
                parts.push(problem(&bar.error));
            }
            for segment in &bar.segments {
                let mut pieces: Vec<View> = Vec::new();
                if segment.spinner {
                    pieces.push(spinner());
                }
                if !segment.text.is_empty() {
                    let text = small(&segment.text)
                        .opacity(if segment.tone == "muted" { 0.6 } else { 0.9 })
                        .vertical_alignment(VerticalAlignment::Center);
                    pieces.push(match tone(&segment.tone) {
                        Some(color) => text.foreground(color).into(),
                        None => text.into(),
                    });
                }
                if !segment.badge.is_empty() {
                    pieces.push(
                        Border::new()
                            .background(theme::field())
                            .corner_radius(8.0)
                            .padding(Thickness::new(6.0, 1.0, 6.0, 1.0))
                            .vertical_alignment(VerticalAlignment::Center)
                            .content(small(&segment.badge))
                            .into(),
                    );
                }
                if !segment.button.is_empty() {
                    let core = core.clone();
                    let action = segment.action.clone();
                    pieces.push(
                        Button::new()
                            .content(small(&segment.button))
                            .on_click(move || {
                                core.send(Event::RunPluginAction {
                                    tab: id,
                                    action: action.clone(),
                                });
                            })
                            .into(),
                    );
                }
                let piece = StackPanel::new()
                    .orientation(Orientation::Horizontal)
                    .spacing(6.0)
                    .children(pieces);
                parts.push(if segment.tooltip.is_empty() {
                    piece.into()
                } else {
                    piece.tooltip(&segment.tooltip)
                });
            }
            parts
        })
        .collect();
    Some(
        Border::new()
            .border_brush(theme::stroke())
            .border_thickness(Thickness::new(0.0, 1.0, 0.0, 0.0))
            .padding(Thickness::new(12.0, 4.0, 12.0, 4.0))
            .content(
                StackPanel::new()
                    .orientation(Orientation::Horizontal)
                    .spacing(14.0)
                    .children(parts),
            )
            .into(),
    )
}

/// What plugins say about the selection, for the inspector.
pub fn sections(sections: &[PluginSectionView]) -> Vec<View> {
    sections
        .iter()
        .map(|section| {
            let mut lines: Vec<View> = vec![
                small(&section.title)
                    .font_weight(FontWeight::SEMI_BOLD)
                    .into(),
            ];
            if section.loading {
                lines.push(spinner());
            }
            if !section.error.is_empty() {
                lines.push(problem(&section.error));
            }
            lines.extend(section.rows.iter().map(|row| {
                Grid::new()
                    .columns([GridLength::Pixel(84.0), GridLength::STAR])
                    .children((
                        small(&row.label).opacity(0.6),
                        small(&row.value)
                            .text_wrapping(TextWrapping::Wrap)
                            .grid_column(1),
                    ))
                    .into()
            }));
            StackPanel::new()
                .orientation(Orientation::Vertical)
                .spacing(4.0)
                .children(lines)
                .into()
        })
        .collect()
}

/// One field of a plugin's form. `set` makes the event that tells the core
/// the field's new value, as text.
#[expect(clippy::too_many_lines, reason = "one arm per kind of field")]
fn field(
    core: &CoreLink,
    field: &FormFieldView,
    owner: isize,
    set: fn(String, String) -> Event,
) -> View {
    let change = || {
        let core = core.clone();
        let id = field.id.clone();
        move |value: String| core.send(set(id.clone(), value))
    };
    let typed = || {
        let send = change();
        Callback::new(move |text: Rc<str>| send(text.to_string()))
    };
    let labelled = |control: View| -> View {
        if field.label.is_empty() {
            return control;
        }
        Grid::new()
            .columns([GridLength::Pixel(150.0), GridLength::STAR])
            .column_spacing(10.0)
            .children((
                TextBlock::new()
                    .text(field.label.clone())
                    .text_wrapping(TextWrapping::Wrap)
                    .vertical_alignment(VerticalAlignment::Center),
                Border::new().grid_column(1).content(control),
            ))
            .into()
    };
    // What a screen reader calls the control: its label, which stands beside
    // it and not on it.
    let name = if field.label.is_empty() {
        field.placeholder.as_str()
    } else {
        field.label.as_str()
    };
    let text_box = || {
        TextBox::new()
            .automation_name(name)
            .text(field.value.clone())
            .placeholder_text(field.placeholder.clone())
            .is_enabled(field.enabled)
            .on_text_changed(typed())
    };
    let control: View = match field.kind {
        FormFieldKind::Info => TextBlock::new()
            .text(field.text.clone())
            .opacity(0.8)
            .text_wrapping(TextWrapping::Wrap)
            .is_text_selection_enabled(true)
            .into(),
        FormFieldKind::Paths => StackPanel::new()
            .orientation(Orientation::Vertical)
            .spacing(2.0)
            .children(
                field
                    .paths
                    .iter()
                    .map(|row| {
                        Grid::new()
                            .columns([GridLength::STAR, GridLength::Auto])
                            .column_spacing(10.0)
                            .children((
                                small(&row.path).text_trimming(TextTrimming::CharacterEllipsis),
                                small(&row.size).opacity(0.6).grid_column(1),
                            ))
                            .into()
                    })
                    .collect::<Vec<View>>(),
            )
            .into(),
        FormFieldKind::Separator => Border::new().height(1.0).background(theme::stroke()).into(),
        FormFieldKind::Group => TextBlock::new()
            .text(field.label.clone())
            .font_weight(FontWeight::SEMI_BOLD)
            .into(),
        FormFieldKind::Text | FormFieldKind::Time => labelled(text_box().into()),
        FormFieldKind::Multiline => labelled(
            text_box()
                .accepts_return(true)
                .text_wrapping(TextWrapping::Wrap)
                .min_height(64.0)
                .into(),
        ),
        FormFieldKind::Int | FormFieldKind::Float => {
            let bounds = field
                .min_milli
                .zip(field.max_milli)
                .filter(|(min, max)| field.slider && min < max);
            match bounds {
                Some((min, max)) => {
                    let (low, high) = (min as f64 / 1000.0, max as f64 / 1000.0);
                    let step = field.step_milli.map_or(
                        if field.kind == FormFieldKind::Int {
                            1.0
                        } else {
                            0.01
                        },
                        |step| step as f64 / 1000.0,
                    );
                    let send = change();
                    let whole = field.kind == FormFieldKind::Int;
                    labelled(
                        Grid::new()
                            .columns([GridLength::STAR, GridLength::Pixel(56.0)])
                            .column_spacing(8.0)
                            .children((
                                Slider::new()
                                    .automation_name(name)
                                    .minimum(low)
                                    .maximum(high)
                                    .step_frequency(step)
                                    .value(field.value.parse().unwrap_or(low))
                                    .is_enabled(field.enabled)
                                    .on_value_changed(move |value: f64| {
                                        send(if whole {
                                            format!("{value:.0}")
                                        } else {
                                            value.to_string()
                                        });
                                    }),
                                small(&field.value)
                                    .vertical_alignment(VerticalAlignment::Center)
                                    .grid_column(1),
                            ))
                            .into(),
                    )
                }
                None => labelled(text_box().into()),
            }
        }
        FormFieldKind::Bool => {
            let send = change();
            labelled(
                theme::switch(field.on)
                    .automation_name(name)
                    .is_enabled(field.enabled)
                    .on_toggled(move |on: bool| send(on.to_string()))
                    .into(),
            )
        }
        FormFieldKind::Choice | FormFieldKind::Radio => {
            let send = change();
            let values: Vec<String> = field
                .options
                .iter()
                .map(|option| option.value.clone())
                .collect();
            let chosen = values.iter().position(|value| *value == field.value);
            labelled(
                ComboBox::new()
                    .automation_name(name)
                    .items_source(field.options.iter().map(|option| option.label.clone()))
                    .selected_index(chosen)
                    .is_enabled(field.enabled)
                    .on_selection_changed(move |index: Option<usize>| {
                        let picked = index.filter(|index| Some(*index) != chosen);
                        if let Some(value) = picked.and_then(|index| values.get(index)) {
                            send(value.clone());
                        }
                    })
                    .into(),
            )
        }
        FormFieldKind::Path => {
            // Folders are picked in the system's dialog; a file's path is typed or pasted.
            let choose: View = if field.path_mode == FormPathMode::Folder {
                let send = change();
                Button::new()
                    .content("Choose…")
                    .is_enabled(field.enabled)
                    .on_click(move || {
                        let send = send.clone();
                        crate::ui_thread::defer(move || {
                            if let Some(folder) = dirfleet_winshell::pick_folder(owner, None) {
                                send(folder.to_string_lossy().into_owned());
                            }
                        });
                    })
                    .into()
            } else {
                Border::new().into()
            };
            labelled(
                Grid::new()
                    .columns([GridLength::STAR, GridLength::Auto])
                    .column_spacing(6.0)
                    .children((text_box(), Border::new().grid_column(1).content(choose)))
                    .into(),
            )
        }
    };
    let mut lines = vec![control];
    if !field.help.is_empty() {
        lines.push(
            small(&field.help)
                .opacity(0.6)
                .text_wrapping(TextWrapping::Wrap)
                .into(),
        );
    }
    if !field.error.is_empty() {
        lines.push(problem(&field.error));
    }
    StackPanel::new()
        .orientation(Orientation::Vertical)
        .spacing(2.0)
        .margin(Thickness::new(f64::from(field.depth) * 16.0, 0.0, 0.0, 0.0))
        .children(lines)
        .into()
}

fn fields(
    core: &CoreLink,
    fields: &[FormFieldView],
    owner: isize,
    set: fn(String, String) -> Event,
) -> Vec<View> {
    fields
        .iter()
        .map(|each| field(core, each, owner, set))
        .collect()
}

/// The form a plugin asks to fill in before it runs.
pub fn form(core: &CoreLink, form: &PluginFormView, owner: isize) -> View {
    let mut lines: Vec<View> = Vec::new();
    if form.loading {
        lines.push(spinner());
    }
    if !form.error.is_empty() {
        lines.push(problem(&form.error));
    }
    lines.extend(fields(core, &form.fields, owner, |id, text| {
        Event::SetPluginFormField { id, text }
    }));
    if !form.message.is_empty() {
        lines.push(
            TextBlock::new()
                .text(form.message.clone())
                .text_wrapping(TextWrapping::Wrap)
                .into(),
        );
    }
    ScrollViewer::new()
        .max_height(440.0)
        .vertical_scroll_bar_visibility(ScrollBarVisibility::Auto)
        .content(
            StackPanel::new()
                .orientation(Orientation::Vertical)
                .spacing(10.0)
                .margin(Thickness::new(0.0, 0.0, 14.0, 0.0))
                .children(lines),
        )
        .into()
}

/// What a row's menu tells the core.
fn row_event(row: &ResultsRowView, results: &PluginResultsView, key: &Key) -> Option<Event> {
    let path = row.path.clone();
    if *key == Key::from("open") {
        Some(Event::OpenPluginResultsRow { path })
    } else if *key == Key::from("refresh") {
        Some(Event::RefreshPluginResultsRow { path })
    } else if *key == Key::from("copy") {
        Some(Event::CopyPath { path })
    } else {
        let _ = results;
        row.menu
            .iter()
            .find(|item| Key::from(format!("action-{}", item.action)) == *key)
            .map(|item| Event::PluginResultsRowAction {
                path,
                action: item.action.clone(),
            })
    }
}

/// One result: its cells, what is happening to it, and its menu.
fn result_row(core: &CoreLink, results: &PluginResultsView, row: &ResultsRowView) -> View {
    let mut cells: Vec<View> = row
        .cells
        .iter()
        .zip(&results.columns)
        .zip(0..)
        .map(|((cell, column), position)| {
            small(cell)
                .text_trimming(TextTrimming::CharacterEllipsis)
                .horizontal_alignment(if column.numeric {
                    HorizontalAlignment::Right
                } else {
                    HorizontalAlignment::Left
                })
                .vertical_alignment(VerticalAlignment::Center)
                .grid_column(position)
                .into()
        })
        .collect();
    let last = i32::try_from(results.columns.len()).unwrap_or(i32::MAX - 1);
    let status: View = if row.failure.is_empty() {
        small(&row.status).opacity(0.6).into()
    } else {
        problem(&row.status).tooltip(&row.failure)
    };
    cells.push(Border::new().grid_column(last).content(status).into());
    if !row.path.is_empty() {
        let mut items = vec![MenuItem::item("open", results.open_label.clone())];
        if !results.refresh_label.is_empty() {
            items.push(MenuItem::item("refresh", results.refresh_label.clone()));
        }
        items.extend(row.menu.iter().map(|item| {
            let key = format!("action-{}", item.action);
            if row.actions_enabled {
                MenuItem::item(key, item.label.clone())
            } else {
                MenuItem::disabled(key, item.label.clone())
            }
        }));
        items.push(MenuItem::separator("copy-separator"));
        items.push(MenuItem::item("copy", results.copy_label.clone()));
        let (core, chosen_row, all) = (core.clone(), row.clone(), results.clone());
        cells.push(
            Border::new()
                .grid_column(last + 1)
                .content(
                    Button::new()
                        .style(ButtonStyle::Subtle)
                        .is_enabled(!row.busy)
                        .automation_name(format!("Actions for {}", row.path))
                        .content(glyph("\u{E712}", 11.0))
                        .menu(Menu::new(
                            items,
                            Callback::new(move |key: Key| {
                                if let Some(event) = row_event(&chosen_row, &all, &key) {
                                    core.send(event);
                                }
                            }),
                        )),
                )
                .into(),
        );
    }
    let mut lengths: Vec<GridLength> = results.columns.iter().map(|_| GridLength::STAR).collect();
    lengths.extend([GridLength::Pixel(110.0), GridLength::Pixel(40.0)]);
    let line = Grid::new()
        .columns(lengths)
        .column_spacing(10.0)
        .children(cells);
    if row.tooltip.is_empty() {
        line.into()
    } else {
        line.tooltip(&row.tooltip)
    }
}

/// The results a plugin lists: its options, how far it is, and the rows.
#[expect(clippy::too_many_lines, reason = "the dialog's lines, top to bottom")]
pub fn results(core: &CoreLink, results: &PluginResultsView, owner: isize) -> View {
    let mut lines: Vec<View> = fields(core, &results.fields, owner, |id, text| {
        Event::SetPluginResultsField { id, text }
    });
    for note in [&results.subtitle, &results.message, &results.note] {
        if !note.is_empty() {
            lines.push(
                small(note)
                    .opacity(0.8)
                    .text_wrapping(TextWrapping::Wrap)
                    .into(),
            );
        }
    }
    // Run or stop, and how far the run is.
    let run = {
        let core = core.clone();
        let running = results.running;
        Button::new()
            .content(if running { "Stop" } else { "Run" })
            .is_enabled(!results.busy)
            .on_click(move || {
                core.send(if running {
                    Event::StopPluginResults
                } else {
                    Event::RunPluginResults
                });
            })
    };
    let progress: View = if results.running {
        ProgressBar::new()
            .minimum(0.0)
            .maximum(1000.0)
            .value(f64::from(results.progress))
            .vertical_alignment(VerticalAlignment::Center)
            .into()
    } else {
        Border::new().into()
    };
    lines.push(
        Grid::new()
            .columns([GridLength::Auto, GridLength::Pixel(180.0), GridLength::STAR])
            .column_spacing(12.0)
            .children((
                run,
                Border::new().grid_column(1).content(progress),
                small(if results.running {
                    &results.progress_text
                } else {
                    &results.summary
                })
                .opacity(0.7)
                .vertical_alignment(VerticalAlignment::Center)
                .grid_column(2),
            ))
            .into(),
    );
    if results.has_sizes {
        let amount = {
            let core = core.clone();
            let gigabytes = results.filter_gigabytes;
            Callback::new(move |text: Rc<str>| {
                core.send(Event::FilterPluginResults {
                    amount: text.to_string(),
                    gigabytes,
                });
            })
        };
        let unit = {
            let core = core.clone();
            let amount = results.filter_amount.clone();
            Callback::new(move |index: Option<usize>| {
                core.send(Event::FilterPluginResults {
                    amount: amount.clone(),
                    gigabytes: index == Some(1),
                });
            })
        };
        lines.push(
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(8.0)
                .children((
                    small("Larger than").vertical_alignment(VerticalAlignment::Center),
                    TextBox::new()
                        .text(results.filter_amount.clone())
                        .width(90.0)
                        .on_text_changed(amount),
                    ComboBox::new()
                        .items_source(["MB", "GB"])
                        .selected_index(usize::from(results.filter_gigabytes))
                        .on_selection_changed(unit),
                    small(&results.filter_text)
                        .opacity(0.7)
                        .vertical_alignment(VerticalAlignment::Center),
                ))
                .into(),
        );
    }
    if results.has_table && !results.rows.is_empty() {
        let mut lengths: Vec<GridLength> =
            results.columns.iter().map(|_| GridLength::STAR).collect();
        lengths.extend([GridLength::Pixel(110.0), GridLength::Pixel(40.0)]);
        let titles: Vec<View> = results
            .columns
            .iter()
            .zip(0..)
            .map(|(column, position)| {
                let core = core.clone();
                let arrow = match (column.sorted, column.descending) {
                    (false, _) => "",
                    (true, true) => "  ↓",
                    (true, false) => "  ↑",
                };
                Button::new()
                    .style(ButtonStyle::Subtle)
                    .horizontal_alignment(HorizontalAlignment::Stretch)
                    .horizontal_content_alignment(if column.numeric {
                        HorizontalAlignment::Right
                    } else {
                        HorizontalAlignment::Left
                    })
                    .content(small(&format!("{}{arrow}", column.title)).opacity(0.8))
                    .on_click(move || {
                        core.send(Event::SortPluginResults {
                            column: position as u32,
                        });
                    })
                    .grid_column(position)
                    .into()
            })
            .collect();
        lines.push(
            Grid::new()
                .columns(lengths)
                .column_spacing(10.0)
                .children(titles)
                .into(),
        );
        let rows = results
            .rows
            .iter()
            .take(RESULT_ROWS)
            .zip(0u32..)
            .map(|(row, position)| keyed(position, result_row(core, results, row)));
        lines.push(
            ScrollViewer::new()
                .height(300.0)
                .vertical_scroll_bar_visibility(ScrollBarVisibility::Auto)
                .content(
                    StackPanel::new()
                        .orientation(Orientation::Vertical)
                        .spacing(2.0)
                        .margin(Thickness::new(0.0, 0.0, 14.0, 0.0))
                        .keyed_children(rows),
                )
                .into(),
        );
        if results.rows.len() > RESULT_ROWS {
            lines.push(
                small(&format!(
                    "The first {RESULT_ROWS} of {} are shown.",
                    results.rows.len()
                ))
                .opacity(0.6)
                .into(),
            );
        }
    } else if !results.empty_text.is_empty() {
        lines.push(
            TextBlock::new()
                .text(results.empty_text.clone())
                .opacity(0.7)
                .text_wrapping(TextWrapping::Wrap)
                .into(),
        );
    }
    StackPanel::new()
        .orientation(Orientation::Vertical)
        .spacing(10.0)
        .children(lines)
        .into()
}

/// The jobs plugins run: what each does, how far it is, and a way to stop it.
pub fn jobs(core: &CoreLink, jobs: &[PluginJobView]) -> View {
    let lines = jobs.iter().map(|job| {
        let (stop, show) = (core.clone(), core.clone());
        let id = job.id;
        let results = job.results.clone();
        let title = if job.waiting {
            format!("{} (waiting)", job.title)
        } else {
            job.title.clone()
        };
        let reopen: View = if results.is_empty() {
            Border::new().into()
        } else {
            Button::new()
                .style(ButtonStyle::Subtle)
                .content(small("Show"))
                .on_click(move || {
                    show.send(Event::ShowPluginResults {
                        key: results.clone(),
                    });
                })
                .into()
        };
        let line = Grid::new()
            .height(crate::notices::OPERATION_HEIGHT)
            .columns([
                GridLength::Auto,
                GridLength::STAR,
                GridLength::Pixel(160.0),
                GridLength::Auto,
                GridLength::Auto,
            ])
            .column_spacing(12.0)
            .margin(Thickness::new(14.0, 0.0, 8.0, 0.0))
            .children((
                small(&title).vertical_alignment(VerticalAlignment::Center),
                small(&job.label)
                    .opacity(0.6)
                    .text_trimming(TextTrimming::CharacterEllipsis)
                    .vertical_alignment(VerticalAlignment::Center)
                    .grid_column(1),
                ProgressBar::new()
                    .minimum(0.0)
                    .maximum(1000.0)
                    .value(f64::from(job.progress))
                    .vertical_alignment(VerticalAlignment::Center)
                    .grid_column(2),
                Border::new().grid_column(3).content(reopen),
                Border::new().grid_column(4).content(
                    Button::new()
                        .style(ButtonStyle::Subtle)
                        .automation_name(format!("Stop: {}", job.title))
                        .content(glyph("\u{E711}", 10.0))
                        .on_click(move || stop.send(Event::CancelPluginJob { id }))
                        .tooltip("Stop"),
                ),
            ));
        keyed(id, line)
    });
    StackPanel::new()
        .orientation(Orientation::Vertical)
        .keyed_children(lines)
        .into()
}

/// One plugin in Settings: what it is, whether it runs, and its shortcuts.
#[expect(clippy::too_many_lines, reason = "the plugin's lines, top to bottom")]
fn plugin(core: &CoreLink, plugin: &PluginView) -> View {
    let enable = {
        let core = core.clone();
        let id = plugin.id.clone();
        theme::switch(plugin.enabled)
            .automation_name(format!("Run {}", plugin.name))
            .on_toggled(move |enabled: bool| {
                core.send(Event::SetPluginEnabled {
                    id: id.clone(),
                    enabled,
                });
            })
    };
    let mut lines: Vec<View> = vec![
        Grid::new()
            .columns([GridLength::STAR, GridLength::Auto])
            .children((
                StackPanel::new()
                    .orientation(Orientation::Vertical)
                    .children((
                        TextBlock::new().text(plugin.name.clone()),
                        small(&format!(
                            "{}{}{}  ·  {}",
                            plugin.version,
                            if plugin.author.is_empty() {
                                ""
                            } else {
                                "  ·  "
                            },
                            plugin.author,
                            if plugin.bundled { "built in" } else { "added" }
                        ))
                        .opacity(0.6),
                    )),
                Border::new().grid_column(1).content(enable),
            ))
            .into(),
    ];
    if !plugin.description.is_empty() {
        lines.push(
            small(&plugin.description)
                .opacity(0.8)
                .text_wrapping(TextWrapping::Wrap)
                .into(),
        );
    }
    lines.push(small(&plugin.status).opacity(0.8).into());
    if !plugin.permissions.is_empty() {
        lines.push(
            small(&format!("May: {}", plugin.permissions.join("; ")))
                .opacity(0.6)
                .text_wrapping(TextWrapping::Wrap)
                .into(),
        );
    }
    if plugin.needs_approval {
        let core = core.clone();
        let id = plugin.id.clone();
        lines.push(
            Button::new()
                .content("Allow")
                .on_click(move || core.send(Event::ApprovePlugin { id: id.clone() }))
                .into(),
        );
    }
    if !plugin.ffmpeg.is_empty() {
        lines.push(
            small(&format!("FFmpeg: {}", plugin.ffmpeg))
                .opacity(0.6)
                .into(),
        );
    }
    for shortcut in &plugin.shortcuts {
        let core = core.clone();
        let (id, action) = (plugin.id.clone(), shortcut.action.clone());
        let mut row: Vec<View> = vec![
            Grid::new()
                .columns([GridLength::STAR, GridLength::Pixel(150.0)])
                .column_spacing(10.0)
                .children((
                    small(&shortcut.title).vertical_alignment(VerticalAlignment::Center),
                    TextBox::new()
                        .automation_name(format!("Shortcut for {}", shortcut.title))
                        .text(shortcut.text.clone())
                        .placeholder_text("Shortcut")
                        .grid_column(1)
                        .on_text_changed(move |text: Rc<str>| {
                            core.send(Event::SetPluginShortcut {
                                plugin: id.clone(),
                                action: action.clone(),
                                text: text.to_string(),
                            });
                        }),
                ))
                .into(),
        ];
        if !shortcut.note.is_empty() {
            row.push(small(&shortcut.note).opacity(0.6).into());
        }
        lines.extend(row);
    }
    lines.extend(plugin.errors.iter().map(|error| problem(error)));
    Border::new()
        .background(theme::field())
        .corner_radius(6.0)
        .padding(Thickness::uniform(10.0))
        .content(
            StackPanel::new()
                .orientation(Orientation::Vertical)
                .spacing(6.0)
                .children(lines),
        )
        .into()
}

/// The plugins' part of Settings.
pub fn settings(core: &CoreLink, plugins: &PluginsView) -> Vec<View> {
    let mut lines: Vec<View> = plugins
        .plugins
        .iter()
        .map(|each| plugin(core, each))
        .collect();
    lines.extend(
        plugins
            .broken
            .iter()
            .map(|broken| problem(&format!("{}: {}", broken.folder, broken.message))),
    );
    let core = core.clone();
    lines.push(
        Button::new()
            .content("Reload Plugins")
            .horizontal_alignment(HorizontalAlignment::Left)
            .on_click(move || core.send(Event::ReloadPlugins))
            .into(),
    );
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tones_have_colors_and_the_rest_keep_the_texts_own() {
        assert!(tone("good").is_some() && tone("warn").is_some() && tone("bad").is_some());
        assert!(tone("normal").is_none() && tone("muted").is_none() && tone("").is_none());
    }
}
