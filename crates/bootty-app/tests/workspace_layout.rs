use bootty_app::layout::{Direction, PaneId, PaneLayout, SplitDirection};
use bootty_mux::snapshot::{MuxPaneLayout, MuxPaneSplitDirection};
use egui::{Pos2, Rect, Vec2};

fn area() -> Rect {
    Rect::from_min_size(Pos2::ZERO, Vec2::new(100.0, 80.0))
}

fn rect_for<'a>(rects: &'a [(PaneId, Rect)], pane: &str) -> &'a Rect {
    &rects
        .iter()
        .find(|(id, _)| id == pane)
        .expect("pane present")
        .1
}

fn approx(actual: f32, expected: f32) {
    assert!((actual - expected).abs() < 0.01, "{actual} != {expected}");
}

#[test]
fn one_pane_fills_the_terminal_area_without_a_divider() {
    let layout = PaneLayout::single("a".to_owned());
    let rects = layout.rects(area(), 4.0);

    assert_eq!(rects.len(), 1);
    assert_eq!(*rect_for(&rects, "a"), area());
    assert!(layout.dividers(area(), 4.0).is_empty());
    assert!(layout.is_single());
}

#[test]
fn splitting_right_places_and_focuses_the_new_pane() {
    let mut layout = PaneLayout::single("a".to_owned());
    layout.split_focused("b".to_owned(), SplitDirection::Right);

    assert_eq!(layout.focused(), "b");
    assert_eq!(layout.panes(), vec!["a".to_owned(), "b".to_owned()]);

    let rects = layout.rects(area(), 4.0);
    let left = rect_for(&rects, "a");
    let right = rect_for(&rects, "b");
    approx(left.width(), 48.0);
    approx(right.width(), 48.0);
    approx(left.min.x, 0.0);
    approx(right.min.x, 52.0);
    approx(left.height(), 80.0);
}

#[test]
fn splitting_down_reserves_the_configured_gap() {
    let mut layout = PaneLayout::single("a".to_owned());
    layout.split_focused("b".to_owned(), SplitDirection::Down);

    let rects = layout.rects(area(), 6.0);
    let top = rect_for(&rects, "a");
    let bottom = rect_for(&rects, "b");
    approx(top.height(), 37.0);
    approx(bottom.height(), 37.0);
    approx(bottom.min.y, 43.0);
    approx(top.width(), 100.0);
}

#[test]
fn a_nested_split_only_subdivides_the_focused_pane() {
    let mut layout = PaneLayout::single("a".to_owned());
    layout.split_focused("b".to_owned(), SplitDirection::Right);
    layout.split_focused("c".to_owned(), SplitDirection::Down);

    assert_eq!(
        layout.panes(),
        vec!["a".to_owned(), "b".to_owned(), "c".to_owned()]
    );

    let rects = layout.rects(area(), 0.0);
    approx(rect_for(&rects, "a").width(), 50.0);
    approx(rect_for(&rects, "b").width(), 50.0);
    approx(rect_for(&rects, "b").height(), 40.0);
    approx(rect_for(&rects, "c").min.y, 40.0);
}

#[test]
fn removing_a_pane_collapses_its_parent_and_refocuses_the_survivor() {
    let mut layout = PaneLayout::single("a".to_owned());
    layout.split_focused("b".to_owned(), SplitDirection::Right);

    assert!(layout.remove("b"));
    assert_eq!(layout.panes(), vec!["a".to_owned()]);
    assert_eq!(layout.focused(), "a");
    assert_eq!(*rect_for(&layout.rects(area(), 4.0), "a"), area());
}

#[test]
fn the_last_pane_cannot_be_removed() {
    let mut layout = PaneLayout::single("a".to_owned());

    assert!(!layout.remove("a"));
    assert_eq!(layout.panes(), vec!["a".to_owned()]);
}

#[test]
fn dividers_keep_their_split_direction_and_tree_path() {
    let mut layout = PaneLayout::single("a".to_owned());
    layout.split_focused("b".to_owned(), SplitDirection::Right);
    layout.split_focused("c".to_owned(), SplitDirection::Down);

    let dividers = layout.dividers(area(), 4.0);
    assert_eq!(dividers.len(), 2);
    assert_eq!(dividers[0].path, Vec::<u8>::new());
    assert_eq!(dividers[0].direction, SplitDirection::Right);
    assert_eq!(dividers[1].path, vec![1]);
    assert_eq!(dividers[1].direction, SplitDirection::Down);
}

#[test]
fn a_divider_maps_pointer_position_to_split_ratio() {
    let mut right = PaneLayout::single("a".to_owned());
    right.split_focused("b".to_owned(), SplitDirection::Right);
    let divider = right.dividers(area(), 0.0).remove(0);
    approx(divider.ratio_at(Pos2::new(75.0, 40.0), 0.0), 0.75);

    let mut down = PaneLayout::single("a".to_owned());
    down.split_focused("b".to_owned(), SplitDirection::Down);
    let divider = down.dividers(area(), 0.0).remove(0);
    approx(divider.ratio_at(Pos2::new(50.0, 20.0), 0.0), 0.25);
}

#[test]
fn resizing_a_split_clamps_both_children_to_their_minimums() {
    let mut layout = PaneLayout::single("a".to_owned());
    layout.split_focused("b".to_owned(), SplitDirection::Right);

    layout.set_ratio_at(&[], 0.75, 0.1, 0.1);
    let rects = layout.rects(area(), 0.0);
    approx(rect_for(&rects, "a").width(), 75.0);
    approx(rect_for(&rects, "b").width(), 25.0);

    layout.set_ratio_at(&[], 0.99, 0.1, 0.2);
    let rects = layout.rects(area(), 0.0);
    approx(rect_for(&rects, "a").width(), 80.0);
}

#[test]
fn reconciliation_removes_closed_panes_and_adopts_new_panes() {
    let mut layout = PaneLayout::single("a".to_owned());
    layout.split_focused("b".to_owned(), SplitDirection::Right);
    layout.split_focused("c".to_owned(), SplitDirection::Down);

    layout.reconcile(&["a".to_owned(), "c".to_owned(), "d".to_owned()]);

    let mut panes = layout.panes();
    panes.sort();
    assert_eq!(panes, vec!["a".to_owned(), "c".to_owned(), "d".to_owned()]);
    assert!(layout.contains("d"));
    assert!(!layout.contains("b"));

    let before = layout.clone();
    layout.reconcile(&["a".to_owned(), "c".to_owned(), "d".to_owned()]);
    assert_eq!(layout, before);
}

#[test]
fn closing_the_bottom_right_pane_preserves_the_left_split() {
    let mut layout = PaneLayout::single("left".to_owned());
    layout.split_focused("top-right".to_owned(), SplitDirection::Right);
    layout.split_focused("bottom-right".to_owned(), SplitDirection::Down);

    layout.reconcile(&["left".to_owned(), "top-right".to_owned()]);

    let rects = layout.rects(area(), 0.0);
    assert_eq!(
        layout.panes(),
        vec!["left".to_owned(), "top-right".to_owned()]
    );
    approx(rect_for(&rects, "left").height(), 80.0);
    approx(rect_for(&rects, "left").width(), 50.0);
    approx(rect_for(&rects, "top-right").height(), 80.0);
    approx(rect_for(&rects, "top-right").width(), 50.0);
}

#[test]
fn reconciliation_uses_the_requested_direction_for_an_async_pane() {
    let mut layout = PaneLayout::single("a".to_owned());

    layout
        .reconcile_with_new_pane_direction(&["a".to_owned(), "b".to_owned()], SplitDirection::Down);

    let dividers = layout.dividers(area(), 4.0);
    assert_eq!(dividers.len(), 1);
    assert_eq!(dividers[0].direction, SplitDirection::Down);
    assert_eq!(layout.focused(), "b");
}

#[test]
fn backend_layout_restores_split_orientation_and_ratio() {
    let layout = PaneLayout::from_mux_layout(&MuxPaneLayout::Split {
        direction: MuxPaneSplitDirection::Down,
        ratio_millis: 250,
        first: Box::new(MuxPaneLayout::Pane("a".to_owned())),
        second: Box::new(MuxPaneLayout::Pane("b".to_owned())),
    })
    .expect("mux layout should convert");

    let dividers = layout.dividers(area(), 0.0);
    assert_eq!(dividers.len(), 1);
    assert_eq!(dividers[0].direction, SplitDirection::Down);
    let rects = layout.rects(area(), 0.0);
    approx(rect_for(&rects, "a").height(), 20.0);
    approx(rect_for(&rects, "b").height(), 60.0);
}

#[test]
fn terminal_window_size_includes_internal_split_borders() {
    let mut right = PaneLayout::single("a".to_owned());
    right.split_focused("b".to_owned(), SplitDirection::Right);

    assert_eq!(
        right.terminal_window_size(|pane| match pane {
            "a" | "b" => Some((58, 40)),
            _ => None,
        }),
        Some((117, 40))
    );

    let mut nested = PaneLayout::single("a".to_owned());
    nested.split_focused("b".to_owned(), SplitDirection::Right);
    nested.split_focused("c".to_owned(), SplitDirection::Down);

    assert_eq!(
        nested.terminal_window_size(|pane| match pane {
            "a" => Some((58, 39)),
            "b" | "c" => Some((58, 19)),
            _ => None,
        }),
        Some((117, 39))
    );
}

#[test]
fn directional_navigation_finds_the_geometric_neighbor() {
    let mut layout = PaneLayout::single("a".to_owned());
    layout.split_focused("b".to_owned(), SplitDirection::Right);

    assert_eq!(
        layout.neighbor("a", Direction::Right, area(), 0.0),
        Some("b".to_owned())
    );
    assert_eq!(
        layout.neighbor("b", Direction::Left, area(), 0.0),
        Some("a".to_owned())
    );
    assert_eq!(layout.neighbor("a", Direction::Up, area(), 0.0), None);
    assert_eq!(layout.neighbor("a", Direction::Left, area(), 0.0), None);
}
