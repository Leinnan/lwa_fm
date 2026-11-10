//! Short messages, as egui's toasts: what finished, what failed, drives
//! coming and going. Each goes away after a while, or when dismissed.
use crux_core::Command;
use crux_core::render::render;
use facet::Facet;
use serde::{Deserialize, Serialize};

use super::{Delay, DirFleet, Effect, Event, Model};

/// How long a toast stays, in milliseconds; errors stay longer.
const SHOWN_MILLIS: u64 = 4000;
const ERROR_MILLIS: u64 = 8000;
/// Toasts shown at once; older ones go first.
const MAX_TOASTS: usize = 4;

#[derive(Facet, Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub enum ToastKind {
    Info,
    Success,
    Warning,
    Error,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ToastView {
    pub id: u64,
    pub text: String,
    pub kind: ToastKind,
}

#[derive(Default)]
pub(super) struct Toasts {
    next_id: u64,
    shown: Vec<ToastView>,
}

impl Toasts {
    pub(super) fn views(&self) -> Vec<ToastView> {
        self.shown.clone()
    }

    /// Whether this toast is on screen, so a repeated warning isn't stacked.
    pub(super) fn is_shown(&self, kind: ToastKind, text: &str) -> bool {
        self.shown
            .iter()
            .any(|toast| toast.kind == kind && toast.text == text)
    }
}

impl DirFleet {
    /// Shows `text` until it expires.
    pub(super) fn toast(
        model: &mut Model,
        kind: ToastKind,
        text: impl Into<String>,
    ) -> Command<Effect, Event> {
        let toasts = &mut model.toasts;
        toasts.next_id += 1;
        let id = toasts.next_id;
        toasts.shown.push(ToastView {
            id,
            text: text.into(),
            kind,
        });
        let excess = toasts.shown.len().saturating_sub(MAX_TOASTS);
        toasts.shown.drain(..excess);
        let millis = if kind == ToastKind::Error {
            ERROR_MILLIS
        } else {
            SHOWN_MILLIS
        };
        render().and(
            Command::request_from_shell(Delay { millis })
                .then_send(move |()| Event::DismissToast { id }),
        )
    }

    pub(super) fn dismiss_toast(model: &mut Model, id: u64) -> Command<Effect, Event> {
        let before = model.toasts.shown.len();
        model.toasts.shown.retain(|toast| toast.id != id);
        if model.toasts.shown.len() == before {
            Command::done()
        } else {
            render()
        }
    }
}
