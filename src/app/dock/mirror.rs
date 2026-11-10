//! A window's `egui_dock` state as a mirror of the core's layout: built from
//! the core's flat layout, and read back into one after `egui_dock` changed
//! it. Only the main surface is mirrored; floating surfaces (a tab being
//! torn off) are taken out by the workspace before reading back.
//!
//! `egui_dock` keeps a binary tree in a vector: node `i` has children
//! `2i + 1` (first: left or top) and `2i + 2`. `Horizontal` nodes put their
//! children side by side, `Vertical` ones above each other, and `fraction`
//! is the first child's share.
use std::num::NonZeroU32;

use dirfleet_core::app::{FlatLayout, FlatPane, FlatSplit, NodeRef};
use egui_dock::{DockState, Node, NodeIndex, NodePath, Split, SurfaceIndex, Tree};

use super::TabId;

fn tab_id(number: u32) -> Option<TabId> {
    NonZeroU32::new(number).map(TabId::new)
}

/// The core's flat layout of `dock`'s main surface; `None` when it holds no tab.
pub fn flat_from_dock(dock: &DockState<TabId>) -> Option<FlatLayout> {
    let mut flat = FlatLayout {
        root: NodeRef::Pane(0),
        panes: Vec::new(),
        splits: Vec::new(),
    };
    flat.root = walk(dock.main_surface(), NodeIndex::root(), &mut flat)?;
    Some(flat)
}

fn walk(tree: &Tree<TabId>, at: NodeIndex, flat: &mut FlatLayout) -> Option<NodeRef> {
    if at.0 >= tree.len() {
        return None;
    }
    match &tree[at] {
        Node::Empty => None,
        Node::Leaf(leaf) => {
            if leaf.tabs().is_empty() {
                return None;
            }
            flat.panes.push(FlatPane {
                tabs: leaf.tabs().iter().map(|tab| tab.number()).collect(),
                active: u32::try_from(leaf.active.0.min(leaf.len() - 1)).unwrap_or(0),
            });
            Some(NodeRef::Pane(
                u32::try_from(flat.panes.len() - 1).unwrap_or(0),
            ))
        }
        Node::Horizontal(split) | Node::Vertical(split) => {
            let vertical = matches!(tree[at], Node::Vertical(_));
            let first = walk(tree, at.left(), flat);
            let second = walk(tree, at.right(), flat);
            match (first, second) {
                (Some(first), Some(second)) => {
                    flat.splits.push(FlatSplit {
                        vertical,
                        permille: (split.fraction * 1000.0).round().clamp(0.0, 1000.0) as u16,
                        first,
                        second,
                    });
                    Some(NodeRef::Split(
                        u32::try_from(flat.splits.len() - 1).unwrap_or(0),
                    ))
                }
                // An emptied half: the other half stands alone.
                (only, None) | (None, only) => only,
            }
        }
    }
}

/// A dock showing `flat`, with the pane of `focused` focused; `None` when
/// `flat` is malformed.
pub fn dock_from_flat(flat: &FlatLayout, focused: Option<TabId>) -> Option<DockState<TabId>> {
    let mut dock = DockState::new(Vec::new());
    build(
        dock.main_surface_mut(),
        NodeIndex::root(),
        flat.root,
        flat,
        0,
    )?;
    if let Some(focused) = focused
        && let Some((node, _)) = dock.main_surface().find_tab(&focused)
    {
        dock.set_focused_node_and_surface(NodePath::new(SurfaceIndex::main(), node));
    }
    Some(dock)
}

fn build(
    tree: &mut Tree<TabId>,
    at: NodeIndex,
    node: NodeRef,
    flat: &FlatLayout,
    depth: usize,
) -> Option<()> {
    if depth > 64 {
        return None;
    }
    match node {
        NodeRef::Pane(index) => {
            let pane = flat.panes.get(index as usize)?;
            let leaf = tree.leaf_mut(at).ok()?;
            while !leaf.is_empty() {
                leaf.remove_tab(0);
            }
            for &tab in &pane.tabs {
                leaf.append_tab(tab_id(tab)?);
            }
            if !leaf.is_empty() {
                let _ = leaf.set_active_tab((pane.active as usize).min(leaf.len() - 1));
            }
            Some(())
        }
        NodeRef::Split(index) => {
            let split = flat.splits.get(index as usize)?;
            // `split` needs the new half to hold a tab; it is replaced below.
            let placeholder = first_tab(flat, split.second, 0)?;
            let side = if split.vertical {
                Split::Below
            } else {
                Split::Right
            };
            let fraction = f32::from(split.permille.min(1000)) / 1000.0;
            let [first, second] = tree.split(at, side, fraction, Node::leaf(placeholder));
            build(tree, first, split.first, flat, depth + 1)?;
            build(tree, second, split.second, flat, depth + 1)
        }
    }
}

fn first_tab(flat: &FlatLayout, node: NodeRef, depth: usize) -> Option<TabId> {
    if depth > 64 {
        return None;
    }
    match node {
        NodeRef::Pane(index) => tab_id(*flat.panes.get(index as usize)?.tabs.first()?),
        NodeRef::Split(index) => first_tab(flat, flat.splits.get(index as usize)?.first, depth + 1),
    }
}

/// The tab shown in `dock`'s focused pane of the main surface.
pub fn focused_tab(dock: &DockState<TabId>) -> Option<TabId> {
    let path = dock.focused_leaf()?;
    if !path.surface.is_main() {
        return None;
    }
    let leaf = dock.leaf(path).ok()?;
    leaf.tabs().get(leaf.active.0).copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(number: u32) -> TabId {
        tab_id(number).expect("non-zero")
    }

    #[test]
    fn a_split_dock_reads_back_as_the_core_s_layout() {
        let mut dock = DockState::new(vec![t(1), t(2)]);
        let [_, right] = dock.main_surface_mut().split(
            NodeIndex::root(),
            Split::Right,
            0.3,
            Node::leaf_with(vec![t(3)]),
        );
        dock.main_surface_mut()
            .split(right, Split::Below, 0.5, Node::leaf(t(4)));
        let flat = flat_from_dock(&dock).expect("tabs");
        assert_eq!(flat.panes.len(), 3);
        assert_eq!(flat.splits.len(), 2);
        let root = flat.splits.last().expect("root split");
        assert!(!root.vertical);
        assert_eq!(root.permille, 300);
    }

    #[test]
    fn layouts_round_trip_between_dock_and_core() {
        let mut dock = DockState::new(vec![t(1), t(2)]);
        let [_, right] = dock.main_surface_mut().split(
            NodeIndex::root(),
            Split::Right,
            0.4,
            Node::leaf_with(vec![t(3), t(5)]),
        );
        dock.main_surface_mut()
            .split(right, Split::Below, 0.6, Node::leaf(t(4)));
        let flat = flat_from_dock(&dock).expect("tabs");
        let rebuilt = dock_from_flat(&flat, Some(t(4))).expect("valid");
        assert_eq!(flat_from_dock(&rebuilt), Some(flat));
        assert_eq!(focused_tab(&rebuilt), Some(t(4)));
    }

    #[test]
    fn an_emptied_half_leaves_the_other_alone() {
        let mut dock = DockState::new(vec![t(1)]);
        let [left, _] =
            dock.main_surface_mut()
                .split(NodeIndex::root(), Split::Right, 0.5, Node::leaf(t(2)));
        dock.main_surface_mut()
            .leaf_mut(left)
            .expect("leaf")
            .remove_tab(0);
        let flat = flat_from_dock(&dock).expect("one tab left");
        assert_eq!(flat.root, NodeRef::Pane(0));
        assert_eq!(flat.panes[0].tabs, vec![2]);
    }
}
