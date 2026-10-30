//! Fitting file names into a width: a middle ellipsis that keeps the end of
//! the name (where the extension is), two-line names for grid tiles, and the
//! caches that keep both off the per-frame path.
use egui::text::{LayoutJob, TextWrapping};
use egui::{Color32, FontId, Galley, TextFormat, Ui};
use lru::LruCache;
use std::cell::RefCell;
use std::hash::{BuildHasher, Hash};
use std::num::NonZeroUsize;
use std::sync::Arc;

const ELLIPSIS: &str = "…";
/// Widths are rounded down to this step, so resizing a pane reuses galleys.
const WIDTH_STEP: f32 = 4.0;
/// Extensions longer than this are treated as part of the name.
const MAX_EXTENSION_BYTES: usize = 16;

thread_local! {
    /// Fitted galleys keyed by a hash of (text, variant, width step); see [`fit_key`].
    static FIT_POOL: RefCell<LruCache<u64, Arc<Galley>>> =
        RefCell::new(LruCache::new(NonZeroUsize::new(2048).expect("capacity must be > 0")));
}

#[cfg(test)]
pub static FIT_LAYOUT_COUNT: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// Drops every fitted galley; called when the text style changes.
pub fn clear_pool() {
    FIT_POOL.with_borrow_mut(LruCache::clear);
}

pub fn fit_key(text: &str, variant: u64, width: u32) -> u64 {
    foldhash::fast::FixedState::default().hash_one((text, variant, width))
}

/// `width` rounded down to [`WIDTH_STEP`], in steps.
fn width_steps(width: f32) -> u32 {
    (width.max(0.0) / WIDTH_STEP).floor() as u32
}

/// A cached galley for `key`, built by `make` on a miss.
pub fn cached(key: u64, make: impl FnOnce() -> Arc<Galley>) -> Arc<Galley> {
    if let Some(galley) = FIT_POOL.with_borrow_mut(|pool| pool.get(&key).cloned()) {
        return galley;
    }
    #[cfg(test)]
    FIT_LAYOUT_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let galley = make();
    FIT_POOL.with_borrow_mut(|pool| pool.put(key, galley.clone()));
    galley
}

/// Variant for [`fit_key`] that tells highlighted galleys of the same name apart.
pub fn variant_of(value: &impl Hash) -> u64 {
    foldhash::fast::FixedState::default().hash_one(value)
}

/// `galley` fitted to `width` on one line, cut in the middle so the end of
/// the name stays visible. Returns `galley` itself when it fits. `variant`
/// must change whenever the galley's formatting does (e.g. highlights).
pub fn fit_single_line(ui: &Ui, galley: Arc<Galley>, width: f32, variant: u64) -> Arc<Galley> {
    if galley.size().x <= width + 0.5 {
        return galley;
    }
    let steps = width_steps(width);
    // The color is baked into the galley: the same text in another color is
    // another galley.
    let color = galley
        .job
        .sections
        .first()
        .map(|section| section.format.color);
    let key = fit_key(&galley.job.text, variant ^ variant_of(&color), steps);
    cached(key, || {
        let width = steps as f32 * WIDTH_STEP;
        let ellipsis = ellipsis_width(ui, &galley);
        middle_cut(&galley, width, ellipsis).map_or_else(
            || end_truncated(ui, &galley.job, width),
            |(head, tail)| {
                let mut job = cut_job(&galley.job, head, tail);
                job.wrap = TextWrapping::no_max_width();
                ui.fonts_mut(|fonts| fonts.layout_job(job))
            },
        )
    })
}

/// `name` on at most two centered lines of `width`: wrapped once, then the
/// rest cut in the middle. For grid tiles.
pub fn fit_two_lines(
    ui: &Ui,
    name: &str,
    font: &FontId,
    color: Color32,
    width: f32,
) -> Arc<Galley> {
    let steps = width_steps(width);
    let key = fit_key(name, variant_of(&("two_lines", color)), steps);
    cached(key, || {
        let width = (steps as f32 * WIDTH_STEP).max(1.0);
        let format = TextFormat::simple(font.clone(), color);
        let mut job = LayoutJob::single_section(name.to_owned(), format.clone());
        job.wrap = TextWrapping {
            max_width: width,
            break_anywhere: false,
            ..TextWrapping::default()
        };
        job.halign = egui::Align::Center;
        let wrapped = ui.fonts_mut(|fonts| fonts.layout_job(job));
        if wrapped.rows.len() <= 2 {
            return wrapped;
        }
        // Keep the first row as wrapped; fit everything after it on one line.
        let first_chars = wrapped.rows[0].row.glyphs.len();
        let split = name
            .char_indices()
            .nth(first_chars)
            .map_or(name.len(), |(byte, _)| byte);
        let (first, rest) = name.split_at(split);
        let rest_job = LayoutJob::single_section(rest.trim_start().to_owned(), format.clone());
        let rest_galley = ui.fonts_mut(|fonts| fonts.layout_job(rest_job));
        let ellipsis = ellipsis_width(ui, &rest_galley);
        let rest_text = middle_cut(&rest_galley, width, ellipsis).map_or_else(
            || rest_galley.job.text.clone(),
            |(head, tail)| {
                let text = &rest_galley.job.text;
                format!("{}{ELLIPSIS}{}", text[..head].trim_end(), &text[tail..])
            },
        );
        let mut job =
            LayoutJob::single_section(format!("{}\n{rest_text}", first.trim_end()), format);
        job.wrap = TextWrapping {
            max_width: width,
            max_rows: 2,
            break_anywhere: true,
            ..TextWrapping::default()
        };
        job.halign = egui::Align::Center;
        ui.fonts_mut(|fonts| fonts.layout_job(job))
    })
}

/// A cached one-line galley of the text `make` builds, keyed by `key`
/// (which must identify the text) and the color.
pub fn label(
    ui: &Ui,
    key: &impl Hash,
    make: impl FnOnce() -> String,
    font: &FontId,
    color: Color32,
) -> Arc<Galley> {
    let key = fit_key("", variant_of(&(key, color, font)), u32::MAX);
    cached(key, || {
        let job = LayoutJob::single_section(make(), TextFormat::simple(font.clone(), color));
        ui.fonts_mut(|fonts| fonts.layout_job(job))
    })
}

/// One line of plain `text` fitted to `width` by cutting at the start, so the
/// deepest part of a path stays visible: `…/deeper/folder`.
pub fn fit_path_start(
    ui: &Ui,
    text: &str,
    font: &FontId,
    color: Color32,
    width: f32,
) -> Arc<Galley> {
    let steps = width_steps(width);
    let key = fit_key(text, variant_of(&("path_start", color)), steps);
    cached(key, || {
        let width = steps as f32 * WIDTH_STEP;
        let format = TextFormat::simple(font.clone(), color);
        let job = LayoutJob::single_section(text.to_owned(), format.clone());
        let galley = ui.fonts_mut(|fonts| fonts.layout_job(job));
        if galley.size().x <= width + 0.5 {
            return galley;
        }
        let ellipsis = ellipsis_width(ui, &galley);
        let Some(glyphs) = single_row_glyphs(&galley) else {
            return galley;
        };
        let total = galley.size().x;
        let budget = (width - ellipsis).max(0.0);
        let mut start = glyphs.len();
        while start > 0 && total - glyphs[start - 1].pos.x <= budget {
            start -= 1;
        }
        let byte = text
            .char_indices()
            .nth(start)
            .map_or(text.len(), |(byte, _)| byte);
        let job = LayoutJob::single_section(format!("{ELLIPSIS}{}", &text[byte..]), format);
        ui.fonts_mut(|fonts| fonts.layout_job(job))
    })
}

fn ellipsis_width(ui: &Ui, galley: &Galley) -> f32 {
    let font = galley
        .job
        .sections
        .first()
        .map_or_else(FontId::default, |section| section.format.font_id.clone());
    ui.fonts_mut(|fonts| fonts.glyph_width(&font, '…'))
}

/// The glyphs of a one-row galley, one per char of its text.
fn single_row_glyphs(galley: &Galley) -> Option<&[egui::epaint::text::Glyph]> {
    let [row] = galley.rows.as_slice() else {
        return None;
    };
    let glyphs = row.row.glyphs.as_slice();
    (glyphs.len() == galley.job.text.chars().count()).then_some(glyphs)
}

/// Byte offsets `(head_end, tail_start)` to keep `text[..head_end]` and
/// `text[tail_start..]` with an ellipsis between them within `width`.
/// `None` when the galley isn't a plain single row.
fn middle_cut(galley: &Galley, width: f32, ellipsis: f32) -> Option<(usize, usize)> {
    let glyphs = single_row_glyphs(galley)?;
    let text = galley.job.text.as_str();
    let offsets: Vec<usize> = text.char_indices().map(|(byte, _)| byte).collect();
    let byte_at = |char_index: usize| offsets.get(char_index).copied().unwrap_or(text.len());
    let char_at = |byte: usize| offsets.partition_point(|&offset| offset < byte);
    let total = galley.size().x;
    let budget = (width - ellipsis).max(0.0);

    // Keep the extension whole when it takes at most 60% of the space.
    let extension_width = text
        .rfind('.')
        .filter(|&dot| dot > 0 && text.len() - dot <= MAX_EXTENSION_BYTES)
        .map_or(0.0, |dot| total - glyphs[char_at(dot)].pos.x);
    let tail_budget = if extension_width > 0.0 && extension_width <= budget * 0.6 {
        (budget * 0.4).max(extension_width)
    } else {
        budget * 0.4
    };
    let mut tail = glyphs.len();
    while tail > 0 && total - glyphs[tail - 1].pos.x <= tail_budget {
        tail -= 1;
    }
    let tail_width = glyphs.get(tail).map_or(0.0, |glyph| total - glyph.pos.x);
    let head_budget = budget - tail_width;
    let mut head = 0;
    while head < tail && glyphs[head].pos.x + glyphs[head].advance_width <= head_budget {
        head += 1;
    }
    let head_end = text[..byte_at(head)].trim_end().len();
    Some((head_end, byte_at(tail)))
}

/// `job` with `text[head_end..tail_start]` replaced by an ellipsis, keeping
/// each kept piece's formatting.
fn cut_job(job: &LayoutJob, head_end: usize, tail_start: usize) -> LayoutJob {
    let mut out = LayoutJob {
        halign: job.halign,
        justify: job.justify,
        first_row_min_height: job.first_row_min_height,
        break_on_newline: job.break_on_newline,
        round_output_to_gui: job.round_output_to_gui,
        ..LayoutJob::default()
    };
    let append_part = |out: &mut LayoutJob, range: std::ops::Range<usize>| {
        for section in &job.sections {
            let start = section.byte_range.start.0.max(range.start);
            let end = section.byte_range.end.0.min(range.end);
            if start < end {
                out.append(&job.text[start..end], 0.0, section.format.clone());
            }
        }
    };
    append_part(&mut out, 0..head_end);
    if let Some(section) = job.sections.first() {
        let format = TextFormat {
            background: Color32::TRANSPARENT,
            ..section.format.clone()
        };
        out.append(ELLIPSIS, 0.0, format);
    }
    append_part(&mut out, tail_start..job.text.len());
    out
}

/// Fallback for names that aren't one plain row: egui's end ellipsis.
fn end_truncated(ui: &Ui, job: &LayoutJob, width: f32) -> Arc<Galley> {
    let mut job = job.clone();
    job.wrap = TextWrapping::truncate_at_width(width);
    ui.fonts_mut(|fonts| fonts.layout_job(job))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_ui(test: impl FnOnce(&Ui)) {
        let mut test = Some(test);
        let mut harness = egui_kittest::Harness::new_ui(|ui| {
            if let Some(test) = test.take() {
                test(ui);
            }
        });
        harness.run_steps(1);
    }

    fn plain(ui: &Ui, text: &str) -> Arc<Galley> {
        let job = LayoutJob::single_section(
            text.to_owned(),
            TextFormat::simple(FontId::default(), Color32::WHITE),
        );
        ui.fonts_mut(|fonts| fonts.layout_job(job))
    }

    #[test]
    fn middle_ellipsis_keeps_the_extension() {
        with_ui(|ui| {
            let name = "Zrzut ekranu 2026-03-16 o 12.45.04.png";
            let fitted = fit_single_line(ui, plain(ui, name), 140.0, 0);
            let text = &fitted.job.text;
            assert!(fitted.size().x <= 140.0, "{text} is {}", fitted.size().x);
            assert!(text.contains('…'), "{text}");
            assert!(text.ends_with("png"), "{text}");
            assert!(text.starts_with("Zrzut"), "{text}");
        });
    }

    #[test]
    fn names_that_fit_are_unchanged() {
        with_ui(|ui| {
            let galley = plain(ui, "short.txt");
            let fitted = fit_single_line(ui, galley.clone(), 400.0, 0);
            assert!(Arc::ptr_eq(&galley, &fitted));
        });
    }

    #[test]
    fn two_line_names_wrap_then_cut() {
        with_ui(|ui| {
            let name = "super-extra-long filename that should truncate gracefully in the icon grid view.txt";
            let galley = fit_two_lines(ui, name, &FontId::default(), Color32::WHITE, 150.0);
            assert_eq!(galley.rows.len(), 2, "{}", galley.job.text);
            assert!(galley.job.text.ends_with("view.txt"), "{}", galley.job.text);
            assert!(galley.size().x <= 150.0 + 0.5);
        });
    }

    #[test]
    fn paths_keep_their_deepest_folder() {
        with_ui(|ui| {
            let path = "very/long/path/to/some/deeply/nested/folder";
            let galley = fit_path_start(ui, path, &FontId::default(), Color32::WHITE, 120.0);
            assert!(galley.job.text.starts_with('…'), "{}", galley.job.text);
            assert!(galley.job.text.ends_with("folder"), "{}", galley.job.text);
            assert!(galley.size().x <= 120.0 + 0.5);
        });
    }
}
