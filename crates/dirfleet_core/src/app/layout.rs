//! A window's panes and the tabs in each.
//!
//! The arrangement is `egui_dock`'s: a tree of splits whose leaves are
//! panes of tabs. Shells get it, and egui sends it back after a drag, in a
//! flat form ([`FlatLayout`]) that needs no recursive types.
use facet::Facet;
use serde::{Deserialize, Serialize};

use crate::ids::TabId;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Node {
    Pane(Pane),
    Split {
        /// The two halves are side by side (left, right) when false, one
        /// above the other when true.
        vertical: bool,
        /// The first half's share, in thousandths.
        permille: u16,
        first: Box<Self>,
        second: Box<Self>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pane {
    pub tabs: Vec<TabId>,
    /// The position of the tab shown, in `tabs`.
    pub active: usize,
}

impl Pane {
    pub fn active_tab(&self) -> Option<TabId> {
        self.tabs.get(self.active).copied()
    }
}

/// Where a tab goes when it is split off from its pane.
#[derive(Facet, Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub enum Side {
    Left,
    Right,
    Above,
    Below,
}

/// A node of a [`FlatLayout`].
#[derive(Facet, Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub enum NodeRef {
    /// `panes[index]`.
    Pane(u32),
    /// `splits[index]`.
    Split(u32),
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct FlatPane {
    /// The pane's tabs: tab numbers for shells, positions in the saved tab
    /// list on disk.
    pub tabs: Vec<u32>,
    pub active: u32,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct FlatSplit {
    pub vertical: bool,
    pub permille: u16,
    pub first: NodeRef,
    pub second: NodeRef,
}

/// A layout tree as lists: panes and splits referring to each other.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct FlatLayout {
    pub root: NodeRef,
    pub panes: Vec<FlatPane>,
    pub splits: Vec<FlatSplit>,
}

impl Node {
    pub fn single(tab: TabId) -> Self {
        Self::Pane(Pane {
            tabs: vec![tab],
            active: 0,
        })
    }

    /// Every pane, first half before second.
    pub fn panes(&self) -> Vec<&Pane> {
        match self {
            Self::Pane(pane) => vec![pane],
            Self::Split { first, second, .. } => {
                let mut panes = first.panes();
                panes.extend(second.panes());
                panes
            }
        }
    }

    fn panes_mut(&mut self) -> Vec<&mut Pane> {
        match self {
            Self::Pane(pane) => vec![pane],
            Self::Split { first, second, .. } => {
                let mut panes = first.panes_mut();
                panes.extend(second.panes_mut());
                panes
            }
        }
    }

    /// Every tab, pane by pane.
    pub fn tabs(&self) -> Vec<TabId> {
        self.panes()
            .into_iter()
            .flat_map(|pane| pane.tabs.iter().copied())
            .collect()
    }

    /// The tab each pane shows.
    pub fn shown(&self) -> Vec<TabId> {
        self.panes()
            .iter()
            .filter_map(|pane| pane.active_tab())
            .collect()
    }

    pub fn contains(&self, tab: TabId) -> bool {
        self.panes().iter().any(|pane| pane.tabs.contains(&tab))
    }

    pub fn is_empty(&self) -> bool {
        self.panes().iter().all(|pane| pane.tabs.is_empty())
    }

    fn pane_of_mut(&mut self, tab: TabId) -> Option<&mut Pane> {
        self.panes_mut()
            .into_iter()
            .find(|pane| pane.tabs.contains(&tab))
    }

    /// Shows `tab` in its pane.
    pub fn activate(&mut self, tab: TabId) -> bool {
        let Some(pane) = self.pane_of_mut(tab) else {
            return false;
        };
        pane.active = pane.tabs.iter().position(|&t| t == tab).unwrap_or(0);
        true
    }

    /// Adds `tab` to the pane of `beside`, at `index` (clamped), and shows it.
    pub fn insert(&mut self, beside: TabId, tab: TabId, index: usize) -> bool {
        let Some(pane) = self.pane_of_mut(beside) else {
            return false;
        };
        let index = index.min(pane.tabs.len());
        pane.tabs.insert(index, tab);
        pane.active = index;
        true
    }

    /// Adds `tab` right after `beside` and shows it.
    pub fn insert_after(&mut self, beside: TabId, tab: TabId) -> bool {
        let index = self
            .panes()
            .into_iter()
            .find_map(|pane| pane.tabs.iter().position(|&t| t == beside))
            .map_or(0, |index| index + 1);
        self.insert(beside, tab, index)
    }

    /// Takes `tab` out. A pane left empty goes, and its split with it.
    pub fn remove(&mut self, tab: TabId) -> bool {
        let removed = match self {
            Self::Pane(pane) => {
                let Some(index) = pane.tabs.iter().position(|&t| t == tab) else {
                    return false;
                };
                pane.tabs.remove(index);
                // egui_dock's rule: closing at or before the shown tab
                // shows the tab to its left.
                if index <= pane.active {
                    pane.active = pane.active.saturating_sub(1);
                }
                true
            }
            Self::Split { first, second, .. } => first.remove(tab) || second.remove(tab),
        };
        if removed {
            self.collapse();
        }
        removed
    }

    /// Replaces each split with an empty half by its other half.
    fn collapse(&mut self) {
        if let Self::Split { first, second, .. } = self {
            first.collapse();
            second.collapse();
            if first.is_empty() {
                *self = std::mem::replace(second.as_mut(), Self::single_empty());
            } else if second.is_empty() {
                *self = std::mem::replace(first.as_mut(), Self::single_empty());
            }
        }
    }

    const fn single_empty() -> Self {
        Self::Pane(Pane {
            tabs: Vec::new(),
            active: 0,
        })
    }

    /// Moves `tab` out of its pane into a new pane on `side` of it. A pane's
    /// only tab stays: splitting it would leave the pane empty.
    pub fn split(&mut self, tab: TabId, side: Side) -> bool {
        match self {
            Self::Pane(pane) => {
                if !pane.tabs.contains(&tab) || pane.tabs.len() < 2 {
                    return false;
                }
                let index = pane.tabs.iter().position(|&t| t == tab).unwrap_or(0);
                pane.tabs.remove(index);
                pane.active = pane.active.min(pane.tabs.len() - 1);
                let old = Box::new(std::mem::replace(self, Self::single_empty()));
                let new = Box::new(Self::single(tab));
                let vertical = matches!(side, Side::Above | Side::Below);
                let (first, second) = match side {
                    Side::Left | Side::Above => (new, old),
                    Side::Right | Side::Below => (old, new),
                };
                *self = Self::Split {
                    vertical,
                    permille: 500,
                    first,
                    second,
                };
                true
            }
            Self::Split { first, second, .. } => first.split(tab, side) || second.split(tab, side),
        }
    }

    /// The layout as lists, with each tab written by `number`.
    pub fn flatten(&self, number: impl Fn(TabId) -> u32 + Copy) -> FlatLayout {
        let mut flat = FlatLayout {
            root: NodeRef::Pane(0),
            panes: Vec::new(),
            splits: Vec::new(),
        };
        flat.root = self.flatten_into(&mut flat, number);
        flat
    }

    fn flatten_into(&self, flat: &mut FlatLayout, number: impl Fn(TabId) -> u32 + Copy) -> NodeRef {
        match self {
            Self::Pane(pane) => {
                flat.panes.push(FlatPane {
                    tabs: pane.tabs.iter().map(|&tab| number(tab)).collect(),
                    active: u32::try_from(pane.active).unwrap_or(0),
                });
                NodeRef::Pane(u32::try_from(flat.panes.len() - 1).unwrap_or(0))
            }
            Self::Split {
                vertical,
                permille,
                first,
                second,
            } => {
                let first = first.flatten_into(flat, number);
                let second = second.flatten_into(flat, number);
                flat.splits.push(FlatSplit {
                    vertical: *vertical,
                    permille: *permille,
                    first,
                    second,
                });
                NodeRef::Split(u32::try_from(flat.splits.len() - 1).unwrap_or(0))
            }
        }
    }

    /// The tree a [`FlatLayout`] describes, with tabs found by `tab`; `None`
    /// when it is malformed (a reference out of range, a loop) or names a tab
    /// that `tab` doesn't know or twice.
    pub fn unflatten(flat: &FlatLayout, tab: impl Fn(u32) -> Option<TabId> + Copy) -> Option<Self> {
        let mut seen = Vec::new();
        let node = Self::unflatten_ref(flat, flat.root, tab, &mut seen, 0)?;
        (!node.is_empty()).then_some(node)
    }

    fn unflatten_ref(
        flat: &FlatLayout,
        node: NodeRef,
        tab: impl Fn(u32) -> Option<TabId> + Copy,
        seen: &mut Vec<TabId>,
        depth: usize,
    ) -> Option<Self> {
        // Deeper than any real layout: a loop.
        if depth > 64 {
            return None;
        }
        match node {
            NodeRef::Pane(index) => {
                let pane = flat.panes.get(index as usize)?;
                let tabs = pane
                    .tabs
                    .iter()
                    .map(|&number| {
                        let id = tab(number)?;
                        (!seen.contains(&id)).then(|| {
                            seen.push(id);
                            id
                        })
                    })
                    .collect::<Option<Vec<_>>>()?;
                let active = (pane.active as usize).min(tabs.len().saturating_sub(1));
                Some(Self::Pane(Pane { tabs, active }))
            }
            NodeRef::Split(index) => {
                let split = flat.splits.get(index as usize)?;
                let first = Self::unflatten_ref(flat, split.first, tab, seen, depth + 1)?;
                let second = Self::unflatten_ref(flat, split.second, tab, seen, depth + 1)?;
                let mut node = Self::Split {
                    vertical: split.vertical,
                    permille: split.permille.clamp(50, 950),
                    first: Box::new(first),
                    second: Box::new(second),
                };
                node.collapse();
                Some(node)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use super::*;

    fn t(number: u32) -> TabId {
        TabId::new(NonZeroU32::new(number).expect("non-zero"))
    }

    fn pane(tabs: &[u32], active: usize) -> Node {
        Node::Pane(Pane {
            tabs: tabs.iter().map(|&n| t(n)).collect(),
            active,
        })
    }

    #[test]
    fn inserting_after_a_tab_shows_the_new_one() {
        let mut layout = pane(&[1, 2], 0);
        assert!(layout.insert_after(t(1), t(3)));
        assert_eq!(layout, pane(&[1, 3, 2], 1));
    }

    #[test]
    fn removing_the_shown_tab_shows_its_left_neighbour() {
        let mut layout = pane(&[1, 2, 3], 1);
        assert!(layout.remove(t(2)));
        assert_eq!(layout, pane(&[1, 3], 0));
        let mut layout = pane(&[1, 2, 3], 0);
        layout.remove(t(1));
        assert_eq!(layout, pane(&[2, 3], 0));
        let mut layout = pane(&[1, 2, 3], 2);
        layout.remove(t(1));
        assert_eq!(layout, pane(&[2, 3], 1), "the shown tab stays shown");
    }

    #[test]
    fn splitting_moves_the_tab_to_a_new_pane_and_closing_it_merges_back() {
        let mut layout = pane(&[1, 2], 1);
        assert!(layout.split(t(2), Side::Right));
        assert_eq!(
            layout,
            Node::Split {
                vertical: false,
                permille: 500,
                first: Box::new(pane(&[1], 0)),
                second: Box::new(pane(&[2], 0)),
            }
        );
        assert_eq!(layout.panes().len(), 2);
        assert!(!layout.split(t(2), Side::Below), "a pane's only tab stays");
        layout.remove(t(2));
        assert_eq!(layout, pane(&[1], 0));
    }

    #[test]
    fn layouts_round_trip_through_their_flat_form() {
        let mut layout = pane(&[1, 2, 3], 0);
        layout.split(t(3), Side::Below);
        layout.split(t(2), Side::Left);
        let flat = layout.flatten(TabId::number);
        assert_eq!(flat.panes.len(), 3);
        let back = Node::unflatten(&flat, |n| Some(t(n))).expect("valid");
        assert_eq!(back, layout);
    }

    #[test]
    fn a_malformed_flat_layout_is_refused() {
        let flat = pane(&[1, 2], 0).flatten(TabId::number);
        assert!(
            Node::unflatten(&flat, |n| (n == 1).then(|| t(1))).is_none(),
            "unknown tab"
        );
        let twice = FlatLayout {
            root: NodeRef::Split(0),
            panes: vec![FlatPane {
                tabs: vec![1],
                active: 0,
            }],
            splits: vec![FlatSplit {
                vertical: false,
                permille: 500,
                first: NodeRef::Pane(0),
                second: NodeRef::Pane(0),
            }],
        };
        assert!(
            Node::unflatten(&twice, |n| Some(t(n))).is_none(),
            "a tab twice"
        );
        let looping = FlatLayout {
            root: NodeRef::Split(0),
            panes: vec![],
            splits: vec![FlatSplit {
                vertical: false,
                permille: 500,
                first: NodeRef::Split(0),
                second: NodeRef::Split(0),
            }],
        };
        assert!(
            Node::unflatten(&looping, |n| Some(t(n))).is_none(),
            "a loop"
        );
    }
}
