//! What a press on an entry means: select, extend, open, or ask for its menu.
use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use dirfleet_core::app::Event;
use windows_reactor::{
    Border, Callback, DragDropAction, DragDropOperation, DragDropPolicy, DroppedData,
    InputModifiers, PointerEventInfo,
};

use crate::core_link::CoreLink;

/// Two presses on one entry this close together open it.
const DOUBLE_CLICK: Duration = Duration::from_millis(500);
/// How far a pressed entry is moved before it is being dragged.
const DRAG_DISTANCE: f64 = 5.0;

/// What an entry tells the view it is in.
#[derive(Clone, PartialEq)]
pub struct EntryHooks {
    /// Files were dropped on the folder at this position.
    pub on_drop: Callback<(u32, DroppedData)>,
    /// The entry at this position is being dragged.
    pub on_drag: Callback<u32>,
}

impl EntryHooks {
    /// `frame`, the area of the entry at `index`, taking the files dropped on
    /// it when the entry is a folder.
    pub fn taking_drops(&self, frame: Border, is_dir: bool, index: u32) -> Border {
        if !is_dir {
            return frame;
        }
        let on_drop = self.on_drop.clone();
        frame
            .drop_policy(
                DragDropPolicy::new().storage_items(DragDropAction::new(DragDropOperation::Move)),
            )
            .on_drop(move |data: DroppedData| on_drop.call((index, data)))
    }
}

/// Whether a pointer pressed at `from` and now at `to` has gone far enough
/// to be dragging what it pressed.
fn dragging(from: (f64, f64), to: (f64, f64)) -> bool {
    (to.0 - from.0).hypot(to.1 - from.1) > DRAG_DISTANCE
}

/// A press on an entry that may turn into a drag. The entry's handlers share
/// it, so the pointer's moves cost nothing until one of them is the drag.
#[derive(Clone, Default)]
pub struct DragWatch(Rc<Cell<Option<(u32, f64, f64)>>>);

impl DragWatch {
    /// The main button went down on `entry`.
    pub fn pressed(&self, entry: u32, info: &PointerEventInfo) {
        self.0.set(
            info.is_left_button_pressed
                .then_some((entry, info.window_x, info.window_y)),
        );
    }

    /// The pointer moved: the entry it now drags, told once per press.
    pub fn moved(&self, info: &PointerEventInfo) -> Option<u32> {
        let (entry, x, y) = self.0.get()?;
        if !info.is_left_button_pressed {
            self.0.set(None);
            return None;
        }
        dragging((x, y), (info.window_x, info.window_y)).then(|| {
            self.0.set(None);
            entry
        })
    }

    pub fn released(&self) {
        self.0.set(None);
    }
}

/// The pointer whose press an entry took, so the area behind the entries
/// knows the press was not on empty space. Both handlers see the same press:
/// the entry's first, then the area's.
#[derive(Clone, Default)]
pub struct PressClaim(Rc<Cell<Option<u32>>>);

impl PressClaim {
    /// Called by an entry's handler.
    pub fn claim(&self, info: &PointerEventInfo) {
        self.0.set(Some(info.pointer_id));
    }

    /// Called by the area's handler: whether an entry took this press.
    pub fn taken(&self, info: &PointerEventInfo) -> bool {
        self.0.take() == Some(info.pointer_id)
    }
}

impl PartialEq for PressClaim {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}

/// Tells a second press on the same entry from a first.
#[derive(Default)]
pub struct Presses {
    last: Option<(u32, Instant)>,
}

impl Presses {
    /// Forgets the last press, when what was under it has changed.
    pub const fn forget(&mut self) {
        self.last = None;
    }

    /// Tells the core what the press on entry `row` of `tab` asks for.
    ///
    /// A plain press on an entry that is `selected` already is not told yet:
    /// it would leave that entry alone selected, and it may be the start of
    /// dragging all of them. The click is returned instead, to send when the
    /// button goes up without a drag.
    pub fn press(
        &mut self,
        core: &CoreLink,
        tab: u32,
        row: u32,
        info: &PointerEventInfo,
        selected: bool,
    ) -> Option<Event> {
        if info.is_right_button_pressed {
            self.last = None;
            core.send(Event::ContextClick { tab, row });
            return None;
        }
        if !info.is_left_button_pressed {
            return None;
        }
        let now = Instant::now();
        let double = self
            .last
            .is_some_and(|(before, at)| before == row && now.duration_since(at) < DOUBLE_CLICK);
        if double {
            self.last = None;
            core.send(Event::OpenRow { tab, row });
            return None;
        }
        self.last = Some((row, now));
        let command = info.modifiers.contains(InputModifiers::CONTROL);
        let shift = info.modifiers.contains(InputModifiers::SHIFT);
        let click = Event::Click {
            tab,
            row,
            command,
            shift,
        };
        if selected && !command && !shift {
            return Some(click);
        }
        core.send(click);
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(x: f64, y: f64, left: bool) -> PointerEventInfo {
        PointerEventInfo {
            window_x: x,
            window_y: y,
            is_left_button_pressed: left,
            ..PointerEventInfo::default()
        }
    }

    #[test]
    fn a_pressed_entry_is_dragged_once_it_has_moved_far_enough() {
        let watch = DragWatch::default();
        assert_eq!(watch.moved(&at(90.0, 90.0, true)), None, "nothing pressed");
        watch.pressed(3, &at(10.0, 10.0, true));
        assert_eq!(watch.moved(&at(12.0, 13.0, true)), None, "a twitch");
        assert_eq!(watch.moved(&at(14.0, 14.0, true)), Some(3));
        assert_eq!(watch.moved(&at(40.0, 40.0, true)), None, "told once");
    }

    #[test]
    fn a_press_let_go_or_made_with_another_button_drags_nothing() {
        let watch = DragWatch::default();
        watch.pressed(3, &at(10.0, 10.0, true));
        watch.released();
        assert_eq!(watch.moved(&at(40.0, 40.0, true)), None);
        watch.pressed(3, &at(10.0, 10.0, true));
        assert_eq!(
            watch.moved(&at(40.0, 40.0, false)),
            None,
            "the button is up"
        );
        watch.pressed(3, &at(10.0, 10.0, false));
        assert_eq!(watch.moved(&at(40.0, 40.0, true)), None);
    }

    #[test]
    fn a_press_an_entry_took_is_not_a_press_on_empty_space() {
        let claim = PressClaim::default();
        let press = PointerEventInfo {
            pointer_id: 7,
            ..PointerEventInfo::default()
        };
        assert!(!claim.taken(&press), "nothing claimed it");
        claim.claim(&press);
        assert!(claim.taken(&press));
        assert!(!claim.taken(&press), "a claim is used once");
        claim.claim(&PointerEventInfo {
            pointer_id: 8,
            ..PointerEventInfo::default()
        });
        assert!(!claim.taken(&press), "another pointer's claim");
    }
}
