use bootty_surface::selection::{SelectionPoint, TerminalSelection, TerminalSelectionState};
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use rstest::rstest;

fn point(x: u16, y: u16) -> SelectionPoint {
    SelectionPoint::new(x, y)
}

#[rstest]
#[case(point(3, 1), point(2, 3), 5, 8, vec![(1, 3..8), (2, 0..8), (3, 0..3)])]
#[case(point(9, 1), point(12, 2), 3, 8, vec![(2, 0..8)])]
fn row_ranges_cover_the_selected_cells_once(
    #[case] anchor: SelectionPoint,
    #[case] focus: SelectionPoint,
    #[case] rows: u16,
    #[case] cols: u16,
    #[case] expected: Vec<(u16, std::ops::Range<u16>)>,
) {
    let actual = TerminalSelection::new(anchor, focus)
        .row_ranges(rows, cols)
        .collect::<Vec<_>>();
    assert_eq!(actual, expected);
}

#[test]
fn click_without_drag_clears_selection() {
    let mut state = TerminalSelectionState::default();

    state.begin(point(2, 1));
    state.finish(point(2, 1));

    assert_eq!(state, TerminalSelectionState::default());
}

proptest! {
    /// Property: endpoints are ordered lexicographically and reversing them never changes the
    /// selected cells.
    #[test]
    fn reversing_endpoints_preserves_row_ranges(
        anchor_x in any::<u16>(), anchor_y in any::<u16>(),
        focus_x in any::<u16>(), focus_y in any::<u16>(),
        rows in 1_u16..512, cols in 1_u16..512,
    ) {
        let anchor = point(anchor_x, anchor_y);
        let focus = point(focus_x, focus_y);
        let expected_order = if (anchor.y, anchor.x) <= (focus.y, focus.x) {
            (anchor, focus)
        } else {
            (focus, anchor)
        };
        prop_assert_eq!(TerminalSelection::new(anchor, focus).ordered(), expected_order);
        let forward = TerminalSelection::new(anchor, focus)
            .row_ranges(rows, cols).collect::<Vec<_>>();
        let reverse = TerminalSelection::new(focus, anchor)
            .row_ranges(rows, cols).collect::<Vec<_>>();

        prop_assert_eq!(forward, reverse);
    }

}
