//! What a press on an entry means: select, extend, open, or ask for its menu.
use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use dirfleet_core::app::Event;
use windows_reactor::{InputModifiers, PointerEventInfo};

use crate::core_link::CoreLink;

/// Two presses on one entry this close together open it.
const DOUBLE_CLICK: Duration = Duration::from_millis(500);

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
    pub fn press(&mut self, core: &CoreLink, tab: u32, row: u32, info: &PointerEventInfo) {
        if info.is_right_button_pressed {
            self.last = None;
            core.send(Event::ContextClick { tab, row });
            return;
        }
        if !info.is_left_button_pressed {
            return;
        }
        let now = Instant::now();
        let double = self
            .last
            .is_some_and(|(before, at)| before == row && now.duration_since(at) < DOUBLE_CLICK);
        if double {
            self.last = None;
            core.send(Event::OpenRow { tab, row });
        } else {
            self.last = Some((row, now));
            core.send(Event::Click {
                tab,
                row,
                command: info.modifiers.contains(InputModifiers::CONTROL),
                shift: info.modifiers.contains(InputModifiers::SHIFT),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
