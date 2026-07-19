use eframe::egui::{Pos2, Rect};

use crate::mux::controller::MuxScope;

#[derive(Clone, Debug, PartialEq)]
enum PresentationNode {
    Leaf(MuxScope),
    Split {
        ratio: f32,
        first: Box<PresentationNode>,
        second: Box<PresentationNode>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct SpacePresentationTree {
    root: Option<PresentationNode>,
    focused: Option<MuxScope>,
}

impl SpacePresentationTree {
    pub(super) fn from_scopes(scopes: impl IntoIterator<Item = MuxScope>) -> Self {
        let mut ordered = Vec::new();
        for scope in scopes {
            if !ordered.contains(&scope) {
                ordered.push(scope);
            }
        }
        Self {
            root: build_tree(&ordered),
            focused: ordered.first().copied(),
        }
    }

    pub(super) fn scopes(&self) -> Vec<MuxScope> {
        let mut scopes = Vec::new();
        if let Some(root) = &self.root {
            collect_scopes(root, &mut scopes);
        }
        scopes
    }

    pub(super) fn focused(&self) -> Option<MuxScope> {
        self.focused
    }

    pub(super) fn focus(&mut self, scope: MuxScope) -> bool {
        if self.focused == Some(scope) {
            return false;
        }
        if !self.scopes().contains(&scope) {
            return false;
        }
        self.focused = Some(scope);
        true
    }

    pub(super) fn add(&mut self, scope: MuxScope) -> bool {
        let mut scopes = self.scopes();
        if scopes.contains(&scope) {
            return false;
        }
        scopes.push(scope);
        self.root = build_tree(&scopes);
        if self.focused.is_none() {
            self.focused = Some(scope);
        }
        true
    }

    pub(super) fn reorder(&mut self, source: MuxScope, before: Option<MuxScope>) -> bool {
        if before == Some(source) {
            return false;
        }
        let mut scopes = self.scopes();
        let Some(source_index) = scopes.iter().position(|scope| *scope == source) else {
            return false;
        };
        let source = scopes.remove(source_index);
        let target_index = before
            .and_then(|target| scopes.iter().position(|scope| *scope == target))
            .unwrap_or(scopes.len());
        if target_index == source_index.min(scopes.len()) {
            return false;
        }
        scopes.insert(target_index, source);
        self.root = build_tree(&scopes);
        true
    }

    pub(super) fn layout(&self, rect: Rect, gap: f32) -> Vec<(MuxScope, Rect)> {
        let mut leaves = Vec::new();
        if let Some(root) = &self.root {
            layout_node(root, rect, gap.max(0.0), &mut leaves);
        }
        leaves
    }
}

fn build_tree(scopes: &[MuxScope]) -> Option<PresentationNode> {
    match scopes {
        [] => None,
        [scope] => Some(PresentationNode::Leaf(*scope)),
        scopes => {
            let split = scopes.len() / 2;
            let first = build_tree(&scopes[..split])?;
            let second = build_tree(&scopes[split..])?;
            Some(PresentationNode::Split {
                ratio: split as f32 / scopes.len() as f32,
                first: Box::new(first),
                second: Box::new(second),
            })
        }
    }
}

fn collect_scopes(node: &PresentationNode, scopes: &mut Vec<MuxScope>) {
    match node {
        PresentationNode::Leaf(scope) => scopes.push(*scope),
        PresentationNode::Split { first, second, .. } => {
            collect_scopes(first, scopes);
            collect_scopes(second, scopes);
        }
    }
}

fn layout_node(node: &PresentationNode, rect: Rect, gap: f32, leaves: &mut Vec<(MuxScope, Rect)>) {
    match node {
        PresentationNode::Leaf(scope) => leaves.push((*scope, rect)),
        PresentationNode::Split {
            ratio,
            first,
            second,
        } => {
            let usable_width = (rect.width() - gap).max(0.0);
            let split_x = rect.min.x + usable_width * ratio.clamp(0.0, 1.0);
            let first_rect = Rect::from_min_max(rect.min, Pos2::new(split_x, rect.max.y));
            let second_rect = Rect::from_min_max(
                Pos2::new((split_x + gap).min(rect.max.x), rect.min.y),
                rect.max,
            );
            layout_node(first, first_rect, gap, leaves);
            layout_node(second, second_rect, gap, leaves);
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::mux::controller::{BindingId, SpaceId};

    use super::*;

    fn scope(binding_id: i64) -> MuxScope {
        MuxScope::new(
            SpaceId::from_persistence(1),
            BindingId::from_persistence(binding_id),
        )
    }

    #[test]
    fn two_binding_leaves_fill_distinct_side_by_side_rects() {
        let first = scope(10);
        let second = scope(20);
        let tree = SpacePresentationTree::from_scopes([first, second]);

        let leaves = tree.layout(
            Rect::from_min_max(Pos2::ZERO, Pos2::new(1000.0, 600.0)),
            8.0,
        );

        assert_eq!(leaves.len(), 2);
        assert_eq!(leaves[0].0, first);
        assert_eq!(leaves[1].0, second);
        assert_eq!(leaves[0].1.width(), 496.0);
        assert_eq!(leaves[1].1.width(), 496.0);
        assert_eq!(leaves[1].1.min.x - leaves[0].1.max.x, 8.0);
    }

    #[test]
    fn focus_and_reorder_change_only_host_tree_state() {
        let first = scope(10);
        let second = scope(20);
        let third = scope(30);
        let mut tree = SpacePresentationTree::from_scopes([first, second]);

        assert!(tree.add(third));
        assert!(tree.focus(second));
        assert!(tree.reorder(third, Some(first)));
        assert!(!tree.reorder(first, Some(first)));

        assert_eq!(tree.focused(), Some(second));
        assert_eq!(tree.scopes(), vec![third, first, second]);
    }
}
