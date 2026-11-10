use std::num::NonZeroU32;

/// Names one tab for its whole life, whichever pane or window it is in.
/// Ids are never reused, so a stale one finds nothing instead of another tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TabId(NonZeroU32);

impl TabId {
    pub const fn new(id: NonZeroU32) -> Self {
        Self(id)
    }

    /// The id as a number, for plugins.
    pub const fn number(self) -> u32 {
        self.0.get()
    }
}

impl std::fmt::Display for TabId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// Names one OS window. [`WindowId::MAIN`] is the window the app starts with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WindowId(NonZeroU32);

impl WindowId {
    pub const MAIN: Self = Self(NonZeroU32::MIN);

    pub const fn new(id: NonZeroU32) -> Self {
        Self(id)
    }

    pub const fn number(self) -> u32 {
        self.0.get()
    }
}

/// Hands out ids that are never reused.
#[derive(Debug, Clone)]
pub struct IdAllocator {
    next: NonZeroU32,
}

impl IdAllocator {
    pub const fn starting_at(first: NonZeroU32) -> Self {
        Self { next: first }
    }

    pub fn next_id(&mut self) -> NonZeroU32 {
        let id = self.next;
        self.next = id.checked_add(1).unwrap_or(NonZeroU32::MAX);
        id
    }

    /// Makes sure `taken`, chosen elsewhere, is never handed out.
    pub fn reserve_past(&mut self, taken: NonZeroU32) {
        if taken >= self.next {
            self.next = taken.checked_add(1).unwrap_or(NonZeroU32::MAX);
        }
    }
}

impl Default for IdAllocator {
    fn default() -> Self {
        Self::starting_at(NonZeroU32::MIN)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_handed_out_in_order_and_never_repeat() {
        let mut ids = IdAllocator::default();
        let a = ids.next_id();
        let b = ids.next_id();
        assert_eq!((a.get(), b.get()), (1, 2));
    }
}
