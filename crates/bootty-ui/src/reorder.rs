//! Where a drag would drop.
//!
//! Both chrome bars let the user drag an item to reorder it, one horizontally across wrapped rows
//! and one vertically down a list. The geometry differs; the decision does not. This is that
//! decision, with no notion of what is being reordered.
//!
//! A *block* is a run of adjacent cells that move together — every row a session owns, or every
//! cell a status segment paints. Blocks live in *lanes*: the status bar's wrapped rows, or a single
//! lane for a plain list. Positions are along the drag axis, in points.

/// One run of cells that moves as a unit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ReorderBlock<'a> {
    /// Identity of what is being moved. The caller's vocabulary; this only compares it.
    pub anchor: &'a str,
    /// Which lane the block sits in. Use 0 throughout for a single-lane list.
    pub lane: usize,
    /// Start and end along the drag axis.
    pub start: f32,
    pub end: f32,
}

/// Where the drop would land.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DropTarget<'a> {
    /// Insert before this block, or at the end when `None`.
    pub before: Option<&'a str>,
    /// Where to paint the insertion indicator, along the drag axis.
    pub indicator: f32,
}

/// The drop `pointer` would produce while dragging `source`, or `None` when the drop would change
/// nothing.
///
/// `lane` is the lane the pointer is in. `require_lane` decides what "no lane" means: a list drops
/// nothing when the pointer is off its rows, while a bar treats every lane as reachable so a drag
/// above or below the strip still lands.
#[must_use]
pub fn drop_target<'a>(
    // The anchors outlive the slice, so a caller may build the blocks locally.
    blocks: &[ReorderBlock<'a>],
    source: &str,
    pointer: f32,
    lane: Option<usize>,
    require_lane: bool,
) -> Option<DropTarget<'a>> {
    if require_lane && lane.is_none() {
        return None;
    }
    let source_index = blocks.iter().position(|block| block.anchor == source)?;
    let in_lane = |block: &ReorderBlock<'_>| lane.is_none_or(|lane| block.lane == lane);

    // Insert before the first block in the lane whose midpoint the pointer has not passed;
    // otherwise after the lane's last block.
    let target_index = blocks
        .iter()
        .position(|block| in_lane(block) && pointer < (block.start + block.end) * 0.5)
        .unwrap_or_else(|| {
            blocks
                .iter()
                .rposition(in_lane)
                .map_or(blocks.len(), |index| index + 1)
        });

    // Dropping a block onto itself, or immediately after itself, is the order it already has.
    if target_index == source_index || target_index == source_index + 1 {
        return None;
    }

    let target = blocks.get(target_index);
    Some(DropTarget {
        before: target.map(|block| block.anchor),
        indicator: match target {
            Some(block) => block.start,
            None => blocks.last().map_or(pointer, |block| block.end),
        },
    })
}

/// Fold consecutive cells that share an anchor into blocks.
///
/// `cells` yields `(anchor, lane, start, end)` in paint order. A cell with no anchor cannot be
/// dragged and breaks the run.
pub fn blocks_from<'a>(
    cells: impl IntoIterator<Item = (Option<&'a str>, usize, f32, f32)>,
) -> Vec<ReorderBlock<'a>> {
    let mut blocks: Vec<ReorderBlock<'a>> = Vec::new();
    for (anchor, lane, start, end) in cells {
        let Some(anchor) = anchor else {
            continue;
        };
        match blocks.last_mut() {
            Some(block) if block.lane == lane && block.anchor == anchor => block.end = end,
            _ => blocks.push(ReorderBlock {
                anchor,
                lane,
                start,
                end,
            }),
        }
    }
    blocks
}
