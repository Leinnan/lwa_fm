use egui::{CornerRadius, Sense, UiBuilder};

use crate::consts::VERTICAL_SPACING;

pub mod autocomplete_text;
pub mod time_label;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonGroupElement {
    First,
    Last,
    InTheMiddle,
}

impl ButtonGroupElement {
    pub const fn from_index(index: usize, total: usize) -> Self {
        match index {
            0 => Self::First,
            n if n == total - 1 => Self::Last,
            _ => Self::InTheMiddle,
        }
    }

    pub const fn to_corner_radius(self, corner_size: u8) -> CornerRadius {
        match self {
            Self::First => CornerRadius {
                nw: corner_size,
                ne: 0,
                sw: corner_size,
                se: 0,
            },
            Self::Last => CornerRadius {
                nw: 0,
                ne: corner_size,
                sw: 0,
                se: corner_size,
            },
            Self::InTheMiddle => CornerRadius {
                nw: 0,
                ne: 0,
                sw: 0,
                se: 0,
            },
        }
    }
}
impl From<ButtonGroupElement> for CornerRadius {
    fn from(element: ButtonGroupElement) -> Self {
        element.to_corner_radius(5)
    }
}

pub trait UiBuilderExt {
    fn btn_frame(&self) -> egui::Frame;
    fn btn_frame_ui(&self) -> UiBuilder;
}
impl UiBuilderExt for egui::Ui {
    fn btn_frame(&self) -> egui::Frame {
        let bg_fill = self.style().visuals.widgets.active.bg_fill;
        let stroke = self.style().visuals.widgets.inactive.bg_stroke;
        egui::Frame::new()
            .corner_radius(5.0)
            .inner_margin(2)
            .outer_margin(VERTICAL_SPACING)
            .fill(bg_fill)
            .stroke(stroke)
    }

    fn btn_frame_ui(&self) -> UiBuilder {
        UiBuilder::new().sense(Sense::CLICK)
    }
}
