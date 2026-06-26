use std::sync::Arc;
use std::time::Duration;

use eframe::egui::{
    Align, Direction, FontSelection, Galley, Pos2, Response, Sense, Stroke, Ui, Widget, WidgetInfo,
    WidgetText, WidgetType, epaint, pos2, text_selection::LabelSelectionState,
};

#[derive(Debug, Default, Clone, Copy, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub enum PauseAtEdges {
    NoPause,
    #[default]
    PauseAtStart,
    PauseAtEnd,
    PauseAtStartAndEnd,
}

/// A label that scrolls its text when it doesn't fit in the available space.
///
/// When the text is wider than the widget and scrolling is enabled (the default),
/// the text smoothly scrolls left so the full content is revealed over time.
/// A configurable gap separates the end of the text from the beginning when
/// it wraps around.
/// Padding (in px) so the end of text is never at the exact clip-rect boundary
/// where a pixel might be trimmed.
const END_PADDING: f32 = 4.0;

pub struct ScrollableLabel {
    text: WidgetText,
    sense: Option<Sense>,
    selectable: Option<bool>,
    halign: Option<Align>,
    edges_behaviour: PauseAtEdges,
    scroll: bool,
    scroll_speed: f32,
    scroll_pause_duration: f32,
    gap_width: f32,
}

impl ScrollableLabel {
    pub fn new(text: impl Into<WidgetText>) -> Self {
        Self {
            text: text.into(),
            sense: None,
            selectable: None,
            halign: None,
            edges_behaviour: PauseAtEdges::PauseAtStart,
            scroll: true,
            scroll_speed: 40.0,
            scroll_pause_duration: 1.5,
            gap_width: 50.0,
        }
    }

    pub fn text(&self) -> &str {
        self.text.text()
    }

    /// Sets the horizontal alignment of the Label to the given `Align` value.
    ///
    /// Ignored when scrolling is active (text always starts left-aligned).
    #[inline]
    pub fn halign(mut self, align: Align) -> Self {
        self.halign = Some(align);
        self
    }

    /// Can the user select the text with the mouse?
    ///
    /// Overrides [`crate::style::Interaction::selectable_labels`].
    /// Text selection is automatically disabled during scrolling.
    #[inline]
    pub fn selectable(mut self, selectable: bool) -> Self {
        self.selectable = Some(selectable);
        self
    }

    /// Make the label respond to clicks and/or drags.
    ///
    /// By default, a label is inert and does not respond to click or drags.
    /// By calling this you can turn the label into a button of sorts.
    /// This will also give the label the hover-effect of a button, but without the frame.
    ///
    /// ```
    /// # use egui::{Label, Sense};
    /// # egui::__run_test_ui(|ui| {
    /// if ui.add(Label::new("click me").sense(Sense::click())).clicked() {
    ///     /* … */
    /// }
    /// # });
    /// ```
    #[inline]
    pub fn sense(mut self, sense: Sense) -> Self {
        self.sense = Some(sense);
        self
    }

    /// Enable or disable automatic scrolling of text that doesn't fit.
    ///
    /// When disabled, long text is truncated with ellipsis and shown on hover.
    /// Enabled by default.
    #[inline]
    pub fn scroll(mut self, scroll: bool) -> Self {
        self.scroll = scroll;
        self
    }

    /// Set the scroll speed in pixels per second. Default is 40.0 px/s.
    #[inline]
    pub fn scroll_speed(mut self, speed: f32) -> Self {
        self.scroll_speed = speed;
        self
    }

    /// Set the duration in seconds to pause at the start and/or end of the scroll
    /// cycle (depending on [`PauseAtEdges`]). Default is 1.5s.
    #[inline]
    pub fn scroll_pause_duration(mut self, seconds: f32) -> Self {
        self.scroll_pause_duration = seconds;
        self
    }

    /// Configure pause behaviour when the scroll reaches the start or end of the text.
    #[inline]
    pub fn edges_behaviour(mut self, behaviour: PauseAtEdges) -> Self {
        self.edges_behaviour = behaviour;
        self
    }

    /// Set the visual gap (in px) between the end of the text and the
    /// beginning of the duplicate when scrolling wraps around.
    /// Default is 50.0 px.
    #[inline]
    pub fn gap_width(mut self, gap: f32) -> Self {
        self.gap_width = gap;
        self
    }
}

impl ScrollableLabel {
    /// Do layout and position the galley in the ui, without painting it or adding widget info.
    pub fn layout_in_ui(self, ui: &mut Ui) -> (Pos2, Arc<Galley>, Response) {
        let selectable = self
            .selectable
            .unwrap_or_else(|| ui.style().interaction.selectable_labels);

        let mut sense = self.sense.unwrap_or_else(|| {
            if ui.memory(|mem| mem.options.screen_reader) {
                Sense::focusable_noninteractive()
            } else {
                Sense::hover()
            }
        });

        if selectable {
            let allow_drag_to_select = ui.input(|i| !i.has_touch_screen());

            let mut select_sense = if allow_drag_to_select {
                Sense::click_and_drag()
            } else {
                Sense::click()
            };
            select_sense -= Sense::FOCUSABLE;

            sense |= select_sense;
        }

        if let WidgetText::Galley(galley) = self.text {
            let (rect, response) = ui.allocate_exact_size(galley.size(), sense);
            let pos = match galley.job.halign {
                Align::LEFT => rect.left_top(),
                Align::Center => rect.center_top(),
                Align::RIGHT => rect.right_top(),
            };
            return (pos, galley, response);
        }

        let valign = ui.text_valign();
        let mut layout_job = Arc::unwrap_or_clone(self.text.into_layout_job(
            ui.style(),
            FontSelection::Default,
            valign,
        ));

        let available_width = ui.available_width();

        if ui.layout().main_dir() == Direction::LeftToRight
            && ui.layout().main_wrap()
            && available_width.is_finite()
        {
            let cursor = ui.cursor();
            let first_row_indentation = available_width - ui.available_size_before_wrap().x;
            debug_assert!(
                first_row_indentation.is_finite(),
                "first row indentation is not finite: {first_row_indentation}"
            );

            layout_job.wrap.max_width = available_width;
            layout_job.first_row_min_height = cursor.height();
            layout_job.halign = Align::Min;
            layout_job.justify = false;
            if let Some(first_section) = layout_job.sections.first_mut() {
                first_section.leading_space = first_row_indentation;
            }
            let galley = ui.fonts_mut(|fonts| fonts.layout_job(layout_job));

            let pos = pos2(ui.max_rect().left(), ui.cursor().top());
            assert!(!galley.rows.is_empty(), "Galleys are never empty");
            let rect = galley.rows[0]
                .rect_without_leading_space()
                .translate(pos.to_vec2());
            let mut response = ui.allocate_rect(rect, sense);
            response.set_intrinsic_size(galley.intrinsic_size());
            for placed_row in galley.rows.iter().skip(1) {
                let rect = placed_row.rect().translate(pos.to_vec2());
                response |= ui.allocate_rect(rect, sense);
            }
            (pos, galley, response)
        } else {
            if self.scroll {
                layout_job.wrap.max_width = f32::INFINITY;
                layout_job.wrap.max_rows = 1;
                layout_job.halign = Align::LEFT;
                layout_job.justify = false;
            } else {
                layout_job.wrap.max_width = available_width;
                layout_job.wrap.max_rows = 1;
                layout_job.wrap.break_anywhere = true;
                layout_job.halign = self
                    .halign
                    .unwrap_or_else(|| ui.layout().horizontal_placement());
                layout_job.justify = ui.layout().horizontal_justify();
            }

            let galley = ui.fonts_mut(|fonts| fonts.layout_job(layout_job));

            if self.scroll {
                let alloc_size = epaint::vec2(available_width, galley.size().y);
                let (rect, mut response) = ui.allocate_exact_size(alloc_size, sense);
                response.set_intrinsic_size(galley.intrinsic_size());
                (rect.left_top(), galley, response)
            } else {
                let (rect, mut response) = ui.allocate_exact_size(galley.size(), sense);
                response.set_intrinsic_size(galley.intrinsic_size());
                let galley_pos = match galley.job.halign {
                    Align::LEFT => rect.left_top(),
                    Align::Center => rect.center_top(),
                    Align::RIGHT => rect.right_top(),
                };
                (galley_pos, galley, response)
            }
        }
    }
}

impl Widget for ScrollableLabel {
    fn ui(self, ui: &mut Ui) -> Response {
        let interactive = self.sense.is_some_and(|sense| sense != Sense::hover());
        let selectable = self.selectable;
        let scroll = self.scroll;
        let scroll_speed = self.scroll_speed;
        let scroll_pause_duration = self.scroll_pause_duration;
        let edges_behaviour = self.edges_behaviour;
        let gap_width = self.gap_width;
        let text_str = self.text.text().to_owned();
        let (galley_pos, galley, mut response) = self.layout_in_ui(ui);
        response
            .widget_info(|| WidgetInfo::labeled(WidgetType::Label, ui.is_enabled(), galley.text()));

        if ui.is_rect_visible(response.rect) {
            let text_width = galley.size().x;
            let widget_width = response.rect.width();
            let needs_scroll = scroll && text_width > widget_width;

            if needs_scroll {
                let time = ui.input(|i| i.time as f32);
                let state_id = response.id.with("marquee_scroll").with(&text_str);

                let start_time = match ui.ctx().data_mut(|d| d.get_persisted::<f32>(state_id)) {
                    Some(t) => t,
                    None => {
                        ui.ctx().data_mut(|d| d.insert_persisted(state_id, time));
                        time
                    }
                };

                let offset = if text_width > 0.0 && widget_width > 0.0 && scroll_speed > 0.0 {
                    let has_start_pause = matches!(
                        edges_behaviour,
                        PauseAtEdges::PauseAtStart | PauseAtEdges::PauseAtStartAndEnd
                    );
                    let has_end_pause = matches!(
                        edges_behaviour,
                        PauseAtEdges::PauseAtEnd | PauseAtEdges::PauseAtStartAndEnd
                    );

                    let ep = END_PADDING;
                    let gap = gap_width;

                    let scroll_to_end_dist = (text_width - widget_width).max(0.0) + ep;
                    let gap_dist = gap;
                    let wrap_dist = (widget_width - ep).max(0.0);

                    let scroll_to_end_dur = if scroll_speed > 0.0 {
                        scroll_to_end_dist / scroll_speed
                    } else {
                        0.0
                    };
                    let gap_dur = if scroll_speed > 0.0 {
                        gap_dist / scroll_speed
                    } else {
                        0.0
                    };
                    let wrap_dur = if scroll_speed > 0.0 {
                        wrap_dist / scroll_speed
                    } else {
                        0.0
                    };

                    let total_cycle = scroll_to_end_dur
                        + gap_dur
                        + wrap_dur
                        + if has_start_pause {
                            scroll_pause_duration
                        } else {
                            0.0
                        }
                        + if has_end_pause {
                            scroll_pause_duration
                        } else {
                            0.0
                        };

                    if total_cycle <= 0.0 {
                        0.0
                    } else {
                        let elapsed = (time - start_time) % total_cycle;

                        let phase1_end = if has_start_pause {
                            scroll_pause_duration
                        } else {
                            0.0
                        };
                        let phase2_end = phase1_end + scroll_to_end_dur;
                        let phase3_end = phase2_end
                            + if has_end_pause {
                                scroll_pause_duration
                            } else {
                                0.0
                            };
                        let phase4_end = phase3_end + gap_dur;

                        if elapsed < phase1_end {
                            // Pause at start – beginning of text
                            0.0
                        } else if elapsed < phase2_end {
                            // Scroll to end: beginning → end of text
                            let t = (elapsed - phase1_end) / scroll_to_end_dur;
                            -t * scroll_to_end_dist
                        } else if elapsed < phase3_end {
                            // Pause at end – end of text visible
                            -scroll_to_end_dist
                        } else if elapsed < phase4_end {
                            // Gap – end of text shifts left, gap appears before
                            // the duplicate enters from the right
                            let t = (elapsed - phase3_end) / gap_dur;
                            -scroll_to_end_dist - t * gap_dist
                        } else {
                            // Wrap – duplicate start enters from right,
                            // end of original exits left
                            let t = (elapsed - phase4_end) / wrap_dur;
                            -scroll_to_end_dist - gap_dist - t * wrap_dist
                        }
                    }
                } else {
                    0.0
                };

                let response_color = if interactive {
                    ui.style().interact(&response).text_color()
                } else {
                    ui.style().visuals.text_color()
                };

                // Paint two copies so the text wraps seamlessly.
                // offset goes 0 → -(text_width + gap_width) then resets to 0.
                // Copy 2 is offset by text_width + gap_width so that at the
                // reset point copy 2 sits at galley_pos.x — same as copy 1 at offset=0.
                let painter = ui.painter_at(response.rect);
                painter.add(
                    epaint::TextShape::new(
                        pos2(galley_pos.x + offset, galley_pos.y),
                        galley.clone(),
                        response_color,
                    )
                    .with_underline(Stroke::NONE),
                );
                painter.add(
                    epaint::TextShape::new(
                        pos2(galley_pos.x + offset + text_width + gap_width, galley_pos.y),
                        galley,
                        response_color,
                    )
                    .with_underline(Stroke::NONE),
                );

                ui.ctx().request_repaint_after(Duration::from_millis(16));
            } else {
                if galley.elided {
                    let job = eframe::egui::text::LayoutJob {
                        sections: galley.job.sections.clone(),
                        text: galley.job.text.clone(),
                        ..eframe::egui::text::LayoutJob::default()
                    };
                    response = response.on_hover_text(job);
                }

                let response_color = if interactive {
                    ui.style().interact(&response).text_color()
                } else {
                    ui.style().visuals.text_color()
                };

                let underline = if response.has_focus() || response.highlighted() {
                    Stroke::new(1.0, response_color)
                } else {
                    Stroke::NONE
                };

                let selectable =
                    selectable.unwrap_or_else(|| ui.style().interaction.selectable_labels);
                if selectable {
                    LabelSelectionState::label_text_selection(
                        ui,
                        &response,
                        galley_pos,
                        galley,
                        response_color,
                        underline,
                    );
                } else {
                    ui.painter().add(
                        epaint::TextShape::new(galley_pos, galley, response_color)
                            .with_underline(underline),
                    );
                }
            }
        }

        response
    }
}
