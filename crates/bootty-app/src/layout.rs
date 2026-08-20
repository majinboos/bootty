//! Binary split-tree layout for native terminal panes.
//!
//! bootty's native engine renders several live PTYs at once, arranged by this tree. The tree is a
//! pure UI concern (the mux backend keeps only a flat pane list), so all geometry, split ratios, and
//! focus live here and stay egui-free except for the `Rect`/`Pos2` value types. A leaf is a pane id;
//! an internal node splits its area between two children at `ratio` (the fraction given to the
//! first/left/top child). "Split the current pane" replaces the focused leaf with a split whose
//! children are the old pane and the new one.

use bootty_mux::snapshot::{MuxPaneLayout, MuxPaneSplitDirection};
use eframe::egui::{Pos2, Rect, Vec2};

pub type PaneId = String;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplitDirection {
    /// New pane opens to the right; children sit side by side.
    Right,
    /// New pane opens below; children stack vertically.
    Down,
}

/// A focus-movement direction for keyboard pane navigation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Clone, Debug, PartialEq)]
enum Node {
    Leaf(PaneId),
    Split {
        direction: SplitDirection,
        /// Fraction of the splittable extent given to `first` (left/top), in (0, 1).
        ratio: f32,
        first: Box<Node>,
        second: Box<Node>,
    },
}

/// A draggable divider between a split node's two children.
#[derive(Clone, Debug, PartialEq)]
pub struct Divider {
    /// Path of `0` (first) / `1` (second) steps from the root to the split node this divider
    /// controls. Stable while the tree shape is unchanged, which holds for the duration of a drag.
    pub path: Vec<u8>,
    pub direction: SplitDirection,
    /// Screen rect of the gap strip the user grabs.
    pub rect: Rect,
    /// The full area this split divides, for converting a pointer position into a new ratio.
    pub area: Rect,
}

impl Divider {
    /// The ratio (fraction for the first/left/top child) implied by dragging this divider to
    /// `pointer`, before any min-size clamping.
    pub fn ratio_at(&self, pointer: Pos2, gap: f32) -> f32 {
        match self.direction {
            SplitDirection::Right => {
                let extent = (self.area.width() - gap).max(1.0);
                (pointer.x - self.area.min.x) / extent
            }
            SplitDirection::Down => {
                let extent = (self.area.height() - gap).max(1.0);
                (pointer.y - self.area.min.y) / extent
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PaneLayout {
    root: Node,
    focused: PaneId,
}

enum Removal {
    NotFound,
    RemovedLeaf,
    Replaced(Node),
}

impl PaneLayout {
    pub fn single(pane: PaneId) -> Self {
        Self {
            root: Node::Leaf(pane.clone()),
            focused: pane,
        }
    }

    pub fn from_mux_layout(layout: &MuxPaneLayout) -> Option<Self> {
        let root = Self::node_from_mux_layout(layout)?;
        let focused = Self::first_leaf(&root).to_owned();
        Some(Self { root, focused })
    }

    fn node_from_mux_layout(layout: &MuxPaneLayout) -> Option<Node> {
        match layout {
            MuxPaneLayout::Pane(pane) => Some(Node::Leaf(pane.clone())),
            MuxPaneLayout::Split {
                direction,
                ratio_millis,
                first,
                second,
            } => Some(Node::Split {
                direction: match direction {
                    MuxPaneSplitDirection::Right => SplitDirection::Right,
                    MuxPaneSplitDirection::Down => SplitDirection::Down,
                },
                ratio: (f32::from(*ratio_millis) / 1000.0).clamp(0.05, 0.95),
                first: Box::new(Self::node_from_mux_layout(first)?),
                second: Box::new(Self::node_from_mux_layout(second)?),
            }),
        }
    }

    pub fn focused(&self) -> &str {
        &self.focused
    }

    pub fn is_single(&self) -> bool {
        matches!(self.root, Node::Leaf(_))
    }

    pub fn contains(&self, pane: &str) -> bool {
        Self::node_contains(&self.root, pane)
    }

    /// All pane ids, left-to-right / top-to-bottom (in-order leaf traversal).
    pub fn panes(&self) -> Vec<PaneId> {
        let mut out = Vec::new();
        Self::collect_leaves(&self.root, &mut out);
        out
    }

    /// Move focus to `pane` if it is a leaf of this layout.
    pub fn set_focus(&mut self, pane: &str) -> bool {
        if self.contains(pane) {
            self.focused = pane.to_owned();
            true
        } else {
            false
        }
    }

    /// Replace the focused leaf with a split whose first child is the old pane and second child is
    /// `new_pane`, then focus the new pane.
    pub fn split_focused(&mut self, new_pane: PaneId, direction: SplitDirection) {
        let focused = self.focused.clone();
        Self::split_leaf(&mut self.root, &focused, &new_pane, direction);
        self.focused = new_pane;
    }

    /// Remove `pane`, collapsing its parent so the sibling takes the parent's slot. Refuses to
    /// remove the last pane. Returns whether a pane was removed; if the focused pane went away,
    /// focus moves to the surviving neighbor.
    pub fn remove(&mut self, pane: &str) -> bool {
        match Self::remove_node(&self.root, pane) {
            Removal::Replaced(node) => {
                self.root = node;
                if !self.contains(&self.focused) {
                    self.focused = Self::first_leaf(&self.root).to_owned();
                }
                true
            }
            // RemovedLeaf at the root means `pane` is the only pane; keep it.
            Removal::RemovedLeaf | Removal::NotFound => false,
        }
    }

    /// Bring the tree in line with the backend's pane set: drop leaves whose pane has gone away
    /// (closed or exited elsewhere) and adopt any pane that appeared outside the UI split path by
    /// splitting the focused leaf in the supplied direction. No-ops when already in sync.
    pub fn reconcile(&mut self, pane_ids: &[PaneId]) {
        self.reconcile_with_new_pane_direction(pane_ids, SplitDirection::Right);
    }

    pub fn reconcile_with_new_pane_direction(
        &mut self,
        pane_ids: &[PaneId],
        new_pane_direction: SplitDirection,
    ) {
        for existing in self.panes() {
            if !pane_ids.contains(&existing) {
                self.remove(&existing);
            }
        }
        for id in pane_ids {
            if !self.contains(id) {
                self.split_focused(id.clone(), new_pane_direction);
            }
        }
    }

    /// Pane id → screen rect for every leaf, dividing `area` by each split's ratio with a `gap`
    /// reserved between children for the divider.
    pub fn rects(&self, area: Rect, gap: f32) -> Vec<(PaneId, Rect)> {
        let mut out = Vec::new();
        Self::layout_node(&self.root, area, gap, &mut out);
        out
    }

    /// The draggable divider strips, one per split node.
    pub fn dividers(&self, area: Rect, gap: f32) -> Vec<Divider> {
        let mut out = Vec::new();
        Self::collect_dividers(&self.root, area, gap, &mut Vec::new(), &mut out);
        out
    }
    /// The rmux window size needed for this split tree when each leaf has the supplied terminal
    /// cell size. Rmux panes share a single window layout, so every internal split consumes one
    /// server-side separator cell even though Bootty paints its own divider chrome.
    pub fn terminal_window_size<F>(&self, mut leaf_size: F) -> Option<(u16, u16)>
    where
        F: FnMut(&str) -> Option<(u16, u16)>,
    {
        Self::node_terminal_window_size(&self.root, &mut leaf_size)
    }

    /// Set the ratio of the split node addressed by `path` (sequence of 0=first / 1=second steps),
    /// clamped to keep both children at least `min_first`/`min_second` fraction of the extent.
    pub fn set_ratio_at(&mut self, path: &[u8], ratio: f32, min_first: f32, min_second: f32) {
        let mut node = &mut self.root;
        for step in path {
            match node {
                Node::Split { first, second, .. } => {
                    node = if *step == 0 { first } else { second };
                }
                Node::Leaf(_) => return,
            }
        }
        if let Node::Split { ratio: r, .. } = node {
            *r = ratio.clamp(min_first, 1.0 - min_second);
        }
    }

    /// The pane geometrically adjacent to `from` in `direction`, using the laid-out rects. Returns
    /// `None` at the edge of the layout.
    pub fn neighbor(
        &self,
        from: &str,
        direction: Direction,
        area: Rect,
        gap: f32,
    ) -> Option<PaneId> {
        let rects = self.rects(area, gap);
        let origin = rects
            .iter()
            .find(|(pane, _)| pane == from)
            .map(|(_, rect)| *rect)?;
        let mut best: Option<(f32, PaneId)> = None;
        for (pane, rect) in &rects {
            if pane == from {
                continue;
            }
            let Some((primary, overlap)) = directional_gap(origin, *rect, direction) else {
                continue;
            };
            // Prefer the nearest candidate along the movement axis, breaking ties toward the one
            // with the most perpendicular overlap with the origin.
            let score = primary - overlap * 0.001;
            if best
                .as_ref()
                .is_none_or(|(best_score, _)| score < *best_score)
            {
                best = Some((score, pane.clone()));
            }
        }
        best.map(|(_, pane)| pane)
    }

    fn node_contains(node: &Node, pane: &str) -> bool {
        match node {
            Node::Leaf(id) => id == pane,
            Node::Split { first, second, .. } => {
                Self::node_contains(first, pane) || Self::node_contains(second, pane)
            }
        }
    }

    fn collect_leaves(node: &Node, out: &mut Vec<PaneId>) {
        match node {
            Node::Leaf(id) => out.push(id.clone()),
            Node::Split { first, second, .. } => {
                Self::collect_leaves(first, out);
                Self::collect_leaves(second, out);
            }
        }
    }

    fn first_leaf(node: &Node) -> &str {
        match node {
            Node::Leaf(id) => id,
            Node::Split { first, .. } => Self::first_leaf(first),
        }
    }

    fn split_leaf(
        node: &mut Node,
        target: &str,
        new_pane: &str,
        direction: SplitDirection,
    ) -> bool {
        match node {
            Node::Leaf(id) if id == target => {
                let old = std::mem::replace(node, Node::Leaf(new_pane.to_owned()));
                *node = Node::Split {
                    direction,
                    ratio: 0.5,
                    first: Box::new(old),
                    second: Box::new(Node::Leaf(new_pane.to_owned())),
                };
                true
            }
            Node::Leaf(_) => false,
            Node::Split { first, second, .. } => {
                Self::split_leaf(first, target, new_pane, direction)
                    || Self::split_leaf(second, target, new_pane, direction)
            }
        }
    }

    fn remove_node(node: &Node, pane: &str) -> Removal {
        match node {
            Node::Leaf(id) if id == pane => Removal::RemovedLeaf,
            Node::Leaf(_) => Removal::NotFound,
            Node::Split {
                direction,
                ratio,
                first,
                second,
            } => match Self::remove_node(first, pane) {
                Removal::RemovedLeaf => Removal::Replaced((**second).clone()),
                Removal::Replaced(node) => Removal::Replaced(Node::Split {
                    direction: *direction,
                    ratio: *ratio,
                    first: Box::new(node),
                    second: second.clone(),
                }),
                Removal::NotFound => match Self::remove_node(second, pane) {
                    Removal::RemovedLeaf => Removal::Replaced((**first).clone()),
                    Removal::Replaced(node) => Removal::Replaced(Node::Split {
                        direction: *direction,
                        ratio: *ratio,
                        first: first.clone(),
                        second: Box::new(node),
                    }),
                    Removal::NotFound => Removal::NotFound,
                },
            },
        }
    }

    fn layout_node(node: &Node, area: Rect, gap: f32, out: &mut Vec<(PaneId, Rect)>) {
        match node {
            Node::Leaf(id) => out.push((id.clone(), area)),
            Node::Split {
                direction,
                ratio,
                first,
                second,
            } => {
                let (first_area, second_area) = split_area(area, *direction, *ratio, gap);
                Self::layout_node(first, first_area, gap, out);
                Self::layout_node(second, second_area, gap, out);
            }
        }
    }

    fn collect_dividers(
        node: &Node,
        area: Rect,
        gap: f32,
        path: &mut Vec<u8>,
        out: &mut Vec<Divider>,
    ) {
        if let Node::Split {
            direction,
            ratio,
            first,
            second,
        } = node
        {
            out.push(Divider {
                path: path.clone(),
                direction: *direction,
                rect: divider_rect(area, *direction, *ratio, gap),
                area,
            });
            let (first_area, second_area) = split_area(area, *direction, *ratio, gap);
            path.push(0);
            Self::collect_dividers(first, first_area, gap, path, out);
            path.pop();
            path.push(1);
            Self::collect_dividers(second, second_area, gap, path, out);
            path.pop();
        }
    }
    fn node_terminal_window_size<F>(node: &Node, leaf_size: &mut F) -> Option<(u16, u16)>
    where
        F: FnMut(&str) -> Option<(u16, u16)>,
    {
        match node {
            Node::Leaf(id) => leaf_size(id),
            Node::Split {
                direction,
                first,
                second,
                ..
            } => {
                let (first_cols, first_rows) = Self::node_terminal_window_size(first, leaf_size)?;
                let (second_cols, second_rows) =
                    Self::node_terminal_window_size(second, leaf_size)?;
                Some(match direction {
                    SplitDirection::Right => (
                        first_cols.saturating_add(second_cols).saturating_add(1),
                        first_rows.max(second_rows),
                    ),
                    SplitDirection::Down => (
                        first_cols.max(second_cols),
                        first_rows.saturating_add(second_rows).saturating_add(1),
                    ),
                })
            }
        }
    }
}

/// Split `area` into (first, second) sub-rects, reserving `gap` between them for the divider.
fn split_area(area: Rect, direction: SplitDirection, ratio: f32, gap: f32) -> (Rect, Rect) {
    match direction {
        SplitDirection::Right => {
            let usable = (area.width() - gap).max(0.0);
            let first_w = usable * ratio;
            let first = Rect::from_min_size(area.min, Vec2::new(first_w, area.height()));
            let second =
                Rect::from_min_max(Pos2::new(area.min.x + first_w + gap, area.min.y), area.max);
            (first, second)
        }
        SplitDirection::Down => {
            let usable = (area.height() - gap).max(0.0);
            let first_h = usable * ratio;
            let first = Rect::from_min_size(area.min, Vec2::new(area.width(), first_h));
            let second =
                Rect::from_min_max(Pos2::new(area.min.x, area.min.y + first_h + gap), area.max);
            (first, second)
        }
    }
}

fn divider_rect(area: Rect, direction: SplitDirection, ratio: f32, gap: f32) -> Rect {
    match direction {
        SplitDirection::Right => {
            let first_w = (area.width() - gap).max(0.0) * ratio;
            Rect::from_min_size(
                Pos2::new(area.min.x + first_w, area.min.y),
                Vec2::new(gap, area.height()),
            )
        }
        SplitDirection::Down => {
            let first_h = (area.height() - gap).max(0.0) * ratio;
            Rect::from_min_size(
                Pos2::new(area.min.x, area.min.y + first_h),
                Vec2::new(area.width(), gap),
            )
        }
    }
}

/// For a candidate rect relative to `origin` in `direction`, return `(distance_along_axis,
/// perpendicular_overlap)` when the candidate lies on the correct side with overlap, else `None`.
fn directional_gap(origin: Rect, candidate: Rect, direction: Direction) -> Option<(f32, f32)> {
    let overlaps_x = origin.min.x < candidate.max.x && candidate.min.x < origin.max.x;
    let overlaps_y = origin.min.y < candidate.max.y && candidate.min.y < origin.max.y;
    match direction {
        Direction::Right if candidate.center().x > origin.center().x && overlaps_y => Some((
            candidate.min.x - origin.max.x,
            vertical_overlap(origin, candidate),
        )),
        Direction::Left if candidate.center().x < origin.center().x && overlaps_y => Some((
            origin.min.x - candidate.max.x,
            vertical_overlap(origin, candidate),
        )),
        Direction::Down if candidate.center().y > origin.center().y && overlaps_x => Some((
            candidate.min.y - origin.max.y,
            horizontal_overlap(origin, candidate),
        )),
        Direction::Up if candidate.center().y < origin.center().y && overlaps_x => Some((
            origin.min.y - candidate.max.y,
            horizontal_overlap(origin, candidate),
        )),
        _ => None,
    }
}

fn vertical_overlap(a: Rect, b: Rect) -> f32 {
    (a.max.y.min(b.max.y) - a.min.y.max(b.min.y)).max(0.0)
}

fn horizontal_overlap(a: Rect, b: Rect) -> f32 {
    (a.max.x.min(b.max.x) - a.min.x.max(b.min.x)).max(0.0)
}
