use bootty_surface::selection::{SelectionPoint, TerminalSelection, TerminalSelectionState};

#[test]
fn selection_orders_anchor_and_focus() {
    let selection = TerminalSelection::new(SelectionPoint::new(4, 3), SelectionPoint::new(1, 2));

    assert_eq!(
        selection.ordered(),
        (SelectionPoint::new(1, 2), SelectionPoint::new(4, 3))
    );
}

#[test]
fn row_ranges_span_partial_and_full_rows() {
    let selection = TerminalSelection::new(SelectionPoint::new(3, 1), SelectionPoint::new(2, 3));
    let ranges = selection.row_ranges(5, 8).collect::<Vec<_>>();

    assert_eq!(ranges, vec![(1, 3..8), (2, 0..8), (3, 0..3)]);
}

#[test]
fn click_without_drag_clears_selection() {
    let mut state = TerminalSelectionState::default();

    state.begin(SelectionPoint::new(2, 1));
    state.finish(SelectionPoint::new(2, 1));

    assert_eq!(state.selection(), None);
    assert!(!state.is_dragging());
}
