//! The address bar's breadcrumbs. They start at an anchor (Home, a mounted
//! volume or the root), collapse from the middle when space runs out, and
//! each chevron lists the subfolders of the crumb before it, as in Explorer
//! and Dolphin.
use super::{
    ActionToPerform,
    commands::TabAction,
    dock::TabData,
    top_bottom::TopDisplayPathPart,
    ui::{self, Palette},
};
use crate::{helper::DataHolder as _, locations::Locations};
use egui::{Response, Ui, text::LayoutJob};
use lucide_icons::Icon;
use std::{
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
};

/// Width of the chevron between two crumbs.
const CHEVRON_WIDTH: f32 = 24.0;

/// What the first visible crumb stands for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Anchor {
    Root,
    Home,
    Volume,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Crumb {
    pub name: String,
    pub path: String,
    pub anchor: Option<Anchor>,
}

fn trimmed(path: &str) -> &str {
    let trimmed = path.trim_end_matches(['/', '\\']);
    if trimmed.is_empty() { path } else { trimmed }
}

/// Splits the path into the folders above the anchor, which only the anchor's
/// menu shows, and the crumbs from the anchor on.
pub fn anchored(parts: &[TopDisplayPathPart], home: Option<&str>) -> (Vec<Crumb>, Vec<Crumb>) {
    let mut all: Vec<Crumb> = parts
        .iter()
        .map(|part| Crumb {
            name: if part.text.is_empty() {
                "/".to_owned()
            } else {
                part.text.clone()
            },
            path: part.path.clone(),
            anchor: None,
        })
        .collect();
    if all.is_empty() {
        return (Vec::new(), all);
    }
    let names: Vec<&str> = all.iter().map(|c| c.name.as_str()).collect();
    let home_index = home.and_then(|home| {
        all.iter()
            .position(|crumb| trimmed(&crumb.path) == trimmed(home))
    });
    let (index, anchor) = home_index.map_or_else(|| volume_anchor(&names), |i| (i, Anchor::Home));
    let mut shown = all.split_off(index);
    shown[0].anchor = Some(anchor);
    if anchor == Anchor::Home {
        "Home".clone_into(&mut shown[0].name);
    }
    (all, shown)
}

/// Where a mounted volume starts in `names`: `/Volumes/<name>` on macOS,
/// `/media/<user>/<name>` or `/run/media/<user>/<name>` on Linux. Otherwise
/// the root.
fn volume_anchor(names: &[&str]) -> (usize, Anchor) {
    match names {
        ["/", "Volumes", _, ..] => (2, Anchor::Volume),
        ["/", "media", _, _, ..] => (3, Anchor::Volume),
        ["/", "run", "media", _, _, ..] => (4, Anchor::Volume),
        _ => (0, Anchor::Root),
    }
}

/// Which crumbs to draw.
#[derive(Debug, PartialEq, Eq)]
pub struct Fit {
    /// Indices of the crumbs drawn, in order.
    pub shown: Vec<usize>,
    /// Indices of the crumbs behind "…".
    pub hidden: Vec<usize>,
    /// The anchor shows only its icon.
    pub compact_anchor: bool,
}

/// Fits crumbs of `widths` into `available`. The current folder is kept
/// longest, then its nearest parents, then the anchor (first by dropping its
/// name, then entirely). `compact_anchor` is the anchor's width as an icon,
/// `ellipsis` the width of "…" and `sep` the width of one chevron.
pub fn fit(widths: &[f32], compact_anchor: f32, ellipsis: f32, sep: f32, available: f32) -> Fit {
    let n = widths.len();
    let all = |compact| Fit {
        shown: (0..n).collect(),
        hidden: Vec::new(),
        compact_anchor: compact,
    };
    if n == 0 {
        return all(false);
    }
    let total = sep.mul_add((n - 1) as f32, widths.iter().sum::<f32>());
    if total <= available || n == 1 {
        return all(false);
    }
    let compact_total = total - widths[0] + compact_anchor.min(widths[0]);
    if compact_total <= available {
        return all(true);
    }
    let mut used = compact_anchor + sep + ellipsis + sep + widths[n - 1];
    if used > available {
        // Only the current folder: everything else goes behind "…".
        return Fit {
            shown: vec![n - 1],
            hidden: (0..n - 1).collect(),
            compact_anchor: false,
        };
    }
    let mut first = n - 1;
    while first > 1 && used + sep + widths[first - 1] <= available {
        first -= 1;
        used += sep + widths[first];
    }
    let mut shown = vec![0];
    shown.extend(first..n);
    Fit {
        shown,
        hidden: (1..first).collect(),
        compact_anchor: true,
    }
}

pub(super) fn home_dir() -> Option<&'static str> {
    static HOME: OnceLock<Option<String>> = OnceLock::new();
    HOME.get_or_init(|| {
        directories::UserDirs::new().map(|dirs| dirs.home_dir().to_string_lossy().into_owned())
    })
    .as_deref()
}

const fn anchor_icon(anchor: Anchor) -> Icon {
    match anchor {
        Anchor::Home => Icon::House,
        Anchor::Root | Anchor::Volume => Icon::HardDrive,
    }
}

/// The crumb's text as the button draws it, so it can be measured exactly.
fn crumb_job(ui: &Ui, crumb: &Crumb, current: bool, compact: bool) -> LayoutJob {
    let font = egui::TextStyle::Button.resolve(ui.style());
    let palette = Palette::of(ui);
    let color = if current {
        ui.visuals().strong_text_color()
    } else {
        palette.secondary
    };
    let mut job = LayoutJob::default();
    // The name sits 6 points after the anchor's icon.
    let leading = crumb.anchor.map_or(0.0, |anchor| {
        job.append(
            &char::from(anchor_icon(anchor)).to_string(),
            0.0,
            egui::TextFormat {
                font_id: egui::FontId::new(14.0, egui::FontFamily::Name("lucide".into())),
                color,
                valign: egui::Align::Center,
                ..Default::default()
            },
        );
        6.0
    });
    // The root's name is just "/", which the drive icon already says.
    let icon_only = compact || (crumb.anchor == Some(Anchor::Root) && crumb.name == "/");
    if !icon_only {
        job.append(
            &crumb.name,
            leading,
            egui::TextFormat {
                font_id: font,
                color,
                valign: egui::Align::Center,
                ..Default::default()
            },
        );
    }
    job
}

fn crumb_width(ui: &Ui, job: &LayoutJob) -> f32 {
    let galley = ui.fonts_mut(|fonts| fonts.layout_job(job.clone()));
    (2.0f32.mul_add(ui.spacing().button_padding.x, galley.size().x)).max(ui::control_height(ui))
}

/// The accessible name of a crumb.
fn crumb_name(crumb: &Crumb) -> String {
    if crumb.anchor == Some(Anchor::Root) && crumb.name == "/" {
        "Root folder".to_owned()
    } else {
        crumb.name.clone()
    }
}

/// Draws the breadcrumbs of `tab` into the space left in `ui`.
pub fn show(ui: &mut Ui, tab: &TabData) {
    // egui sizes a button's padding by its state's outline width, so a crumb
    // would grow on hover and push the ones after it along. With no outline
    // in any state the size stays put and hovering only fills.
    let widgets = &mut ui.visuals_mut().widgets;
    for state in [
        &mut widgets.inactive,
        &mut widgets.hovered,
        &mut widgets.active,
        &mut widgets.open,
    ] {
        state.bg_stroke = egui::Stroke::NONE;
    }
    let parts = &tab.top_display_path;
    if parts.is_empty() {
        ui.label(tab.current_path.get_name_from_path());
        return;
    }
    let (above, crumbs) = anchored(parts, home_dir());
    let last = crumbs.len() - 1;
    let jobs: Vec<LayoutJob> = crumbs
        .iter()
        .enumerate()
        .map(|(i, crumb)| crumb_job(ui, crumb, i == last, false))
        .collect();
    let widths: Vec<f32> = jobs.iter().map(|job| crumb_width(ui, job)).collect();
    let side = ui::control_height(ui);
    let sep = 2.0f32.mul_add(ui.spacing().item_spacing.x, CHEVRON_WIDTH);
    let compact_anchor = crumb_width(ui, &crumb_job(ui, &crumbs[0], last == 0, true));
    let layout = fit(&widths, compact_anchor, side, sep, ui.available_width());
    if layout.shown[0] != 0 {
        overflow_button(ui, &above, &crumbs, &layout.hidden);
        chevron(ui, tab, &crumbs[last - 1], Some(&crumbs[last]));
    }
    for (k, &i) in layout.shown.iter().enumerate() {
        if k > 0 {
            let before = layout.shown[k - 1];
            if before + 1 == i {
                chevron(ui, tab, &crumbs[before], Some(&crumbs[i]));
            } else {
                // The gap: "…" stands for the folders in between.
                chevron(ui, tab, &crumbs[before], crumbs.get(before + 1));
                overflow_button(ui, &[], &crumbs, &layout.hidden);
                chevron(ui, tab, &crumbs[i - 1], Some(&crumbs[i]));
            }
        }
        let job = if i == 0 && layout.compact_anchor {
            crumb_job(ui, &crumbs[i], i == last, true)
        } else {
            jobs[i].clone()
        };
        let response = crumb_button(ui, &crumbs[i], job);
        if i == 0 && !above.is_empty() {
            // The folders above Home or a volume stay one right-click away.
            response.context_menu(|ui| {
                ui.weak("Enclosing folders");
                folder_list(ui, &above);
            });
        }
    }
}

fn navigate(path: &str, new_tab: bool) {
    if let Some(action) = ActionToPerform::path_from_str(path, new_tab) {
        action.schedule();
    }
}

fn crumb_button(ui: &mut Ui, crumb: &Crumb, job: LayoutJob) -> Response {
    let side = ui::control_height(ui);
    let response = ui.add(
        egui::Button::new(job)
            .frame_when_inactive(false)
            .min_size(egui::vec2(side, side))
            .truncate(),
    );
    let name = crumb_name(crumb);
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &name));
    let response = response.on_hover_text(&crumb.path);
    let new_tab =
        response.middle_clicked() || (response.clicked() && ui.input(|i| i.modifiers.command));
    if response.clicked() || new_tab {
        navigate(&crumb.path, new_tab);
    }
    super::file_ops::folder_drop_target(ui, &response, Path::new(&crumb.path));
    let part = TopDisplayPathPart {
        text: crumb_name(crumb),
        path: crumb.path.clone(),
    };
    response.context_menu(|ui| {
        super::top_bottom::breadcrumb_menu_entry(ui, &part);
        ui.separator();
        let favorites = ui.data_get_persisted::<Locations>().unwrap_or_default();
        super::menus::folder_actions(ui, Path::new(&part.path), &favorites, false);
    });
    response
}

/// The chevron after `parent`: a menu of its subfolders, with `next` marked.
fn chevron(ui: &mut Ui, tab: &TabData, parent: &Crumb, next: Option<&Crumb>) {
    let side = ui::control_height(ui);
    let response = ui
        .scope(|ui| {
            // No padding, so the chevron is exactly as wide as `fit` assumes.
            ui.spacing_mut().button_padding.x = 0.0;
            ui.add_sized(
                [CHEVRON_WIDTH, side],
                egui::Button::new(
                    ui::glyph(Icon::ChevronRight)
                        .size(12.0)
                        .color(Palette::of(ui).secondary),
                )
                .frame_when_inactive(false),
            )
        })
        .inner;
    let name = format!("Folders in {}", crumb_name(parent));
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &name));
    let children_id = egui::Id::new(("breadcrumb_children", &parent.path));
    let menu = egui::Popup::menu(&response).show(|ui| {
        // Read the folder once when the menu opens, not every frame.
        let children = ui.data_mut(|data| {
            data.get_temp_mut_or_insert_with(children_id, || {
                Arc::new(super::dir_handling::child_directories(
                    Path::new(&parent.path),
                    tab.show_hidden,
                ))
            })
            .clone()
        });
        if children.is_empty() {
            ui.weak("No folders");
            return;
        }
        egui::ScrollArea::vertical()
            .max_height(320.0)
            .show(ui, |ui| {
                for (name, path) in children.iter() {
                    let on = next.is_some_and(|next| Path::new(&next.path) == path.as_path());
                    if ui.add(egui::Button::selectable(on, name)).clicked() {
                        TabAction::ChangePaths(path.clone().into()).schedule_tab(tab.id);
                        ui.close();
                    }
                }
            });
    });
    if menu.is_none() {
        ui.data_mut(|data| data.remove::<Arc<Vec<(String, PathBuf)>>>(children_id));
    }
}

/// "…": the folders that did not fit, nearest first.
fn overflow_button(ui: &mut Ui, above: &[Crumb], crumbs: &[Crumb], hidden: &[usize]) {
    let mut folders: Vec<Crumb> = above.to_vec();
    folders.extend(hidden.iter().map(|&i| crumbs[i].clone()));
    let name = format!(
        "{} more folder{}",
        folders.len(),
        if folders.len() == 1 { "" } else { "s" }
    );
    ui::icon_menu_button(ui, Icon::Ellipsis, &name, |ui| folder_list(ui, &folders));
}

/// A menu of folders, nearest first.
fn folder_list(ui: &mut Ui, folders: &[Crumb]) {
    for crumb in folders.iter().rev() {
        let icon = crumb.anchor.map_or(Icon::Folder, anchor_icon);
        let response = ui.add(egui::Button::new((
            ui::glyph(icon).size(14.0),
            crumb_name(crumb),
        )));
        let new_tab =
            response.middle_clicked() || (response.clicked() && ui.input(|i| i.modifiers.command));
        if response.clicked() || new_tab {
            navigate(&crumb.path, new_tab);
            ui.close();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parts(path: &str) -> Vec<TopDisplayPathPart> {
        let mut display = super::super::top_bottom::TopDisplayPath::default();
        display.build(Path::new(path));
        display.to_vec()
    }

    #[test]
    #[cfg(not(windows))]
    fn anchors_at_home_then_volume_then_root() {
        let (above, shown) = anchored(&parts("/Users/ann/Documents"), Some("/Users/ann"));
        assert_eq!(above.len(), 2);
        assert_eq!(shown[0].name, "Home");
        assert_eq!(shown[0].anchor, Some(Anchor::Home));
        assert_eq!(shown[1].name, "Documents");

        let (above, shown) = anchored(&parts("/Volumes/Backup SSD/2026"), Some("/Users/ann"));
        assert_eq!(above.len(), 2);
        assert_eq!(shown[0].name, "Backup SSD");
        assert_eq!(shown[0].anchor, Some(Anchor::Volume));

        let (above, shown) = anchored(&parts("/media/ann/USB/photos"), None);
        assert_eq!((above.len(), shown[0].name.as_str()), (3, "USB"));

        let (above, shown) = anchored(&parts("/private/tmp"), Some("/Users/ann"));
        assert_eq!(above, []);
        assert_eq!(shown[0].anchor, Some(Anchor::Root));
        assert_eq!(crumb_name(&shown[0]), "Root folder");
    }

    #[test]
    fn keeps_everything_when_it_fits() {
        let fit = fit(&[40.0, 60.0, 60.0], 28.0, 28.0, 40.0, 240.0);
        assert_eq!(fit.shown, [0, 1, 2]);
        assert_eq!(fit.hidden, Vec::<usize>::new());
        assert!(!fit.compact_anchor);
    }

    #[test]
    fn drops_the_anchor_name_before_any_folder() {
        // 100 + 60 + 60 + 2 * 40 = 300; with a 28px anchor: 228.
        let fit = fit(&[100.0, 60.0, 60.0], 28.0, 28.0, 40.0, 240.0);
        assert_eq!(fit.shown, [0, 1, 2]);
        assert!(fit.compact_anchor);
    }

    #[test]
    fn collapses_from_the_middle_keeping_the_nearest_parents() {
        let widths = [80.0, 60.0, 60.0, 60.0, 60.0, 60.0];
        // anchor 28 + 40 + "…" 28 + 40 + last 60 = 196; each parent adds 100.
        let fit = fit(&widths, 28.0, 28.0, 40.0, 380.0);
        assert_eq!(fit.shown, [0, 4, 5]);
        assert_eq!(fit.hidden, [1, 2, 3]);
        assert!(fit.compact_anchor);
    }

    #[test]
    fn keeps_only_the_current_folder_when_very_narrow() {
        let fit = fit(&[80.0, 60.0, 120.0], 28.0, 28.0, 40.0, 200.0);
        assert_eq!(fit.shown, [2]);
        assert_eq!(fit.hidden, [0, 1]);
    }
}
