use bootty_app::layout::{Direction, PaneId, PaneLayout, SplitDirection};
use bootty_mux::snapshot::{MuxPaneLayout, MuxPaneSplitDirection};
use egui::{Pos2, Rect, Vec2};
use pretty_assertions::assert_eq;
use proptest::prelude::*;

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

#[derive(Clone, Copy, Debug, proptest_derive::Arbitrary)]
struct SplitLayoutInput {
    #[proptest(strategy = "10u16..2_000")]
    width: u16,
    #[proptest(strategy = "10u16..2_000")]
    height: u16,
    #[proptest(strategy = "0u8..=25")]
    gap_percent: u8,
    horizontal: bool,
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

proptest! {
    /// Property: a split partitions the available axis exactly once, accounting for its gap.
    #[test]
    fn split_rectangles_conserve_available_space(input in any::<SplitLayoutInput>()) {
        let SplitLayoutInput { width, height, gap_percent, horizontal } = input;
        let width = f32::from(width);
        let height = f32::from(height);
        let area = Rect::from_min_size(Pos2::ZERO, Vec2::new(width, height));
        let gap = f32::from(gap_percent) / 100.0 * if horizontal { width } else { height };
        let single = PaneLayout::single("a".to_owned());
        prop_assert_eq!(*rect_for(&single.rects(area, gap), "a"), area);
        prop_assert!(single.dividers(area, gap).is_empty() && single.is_single());
        let direction = if horizontal { SplitDirection::Right } else { SplitDirection::Down };
        let mut layout = PaneLayout::single("a".to_owned());
        layout.split_focused("b".to_owned(), direction);
        prop_assert_eq!(layout.focused(), "b");
        prop_assert_eq!(layout.panes(), vec!["a".to_owned(), "b".to_owned()]);
        let rects = layout.rects(area, gap);
        let divider = layout.dividers(area, gap).remove(0);
        prop_assert!((divider.ratio_at(area.center(), 0.0) - 0.5).abs() < 0.01);
        let first = rect_for(&rects, "a");
        let second = rect_for(&rects, "b");
        let occupied = if horizontal {
            first.width() + gap + second.width()
        } else {
            first.height() + gap + second.height()
        };

        prop_assert!((occupied - if horizontal { width } else { height }).abs() < 0.01,
            "first={first:?}, second={second:?}, gap={gap}, area={area:?}");
        let separated = if horizontal {
            first.max.x <= second.min.x
        } else {
            first.max.y <= second.min.y
        };
        prop_assert!(separated, "split panes overlap: {first:?}, {second:?}");
        if horizontal {
            prop_assert_eq!(first.height(), height);
            prop_assert_eq!(second.height(), height);
        } else {
            prop_assert_eq!(first.width(), width);
            prop_assert_eq!(second.width(), width);
        }

        let (forward, backward, outside) = if horizontal {
            (Direction::Right, Direction::Left, Direction::Up)
        } else {
            (Direction::Down, Direction::Up, Direction::Left)
        };
        prop_assert_eq!(layout.neighbor("a", forward, area, gap), Some("b".to_owned()));
        prop_assert_eq!(layout.neighbor("b", backward, area, gap), Some("a".to_owned()));
        prop_assert_eq!(layout.neighbor("a", outside, area, gap), None);

        let mut nested = layout.clone();
        nested.split_focused("c".to_owned(), if horizontal { SplitDirection::Down } else { SplitDirection::Right });
        prop_assert_eq!(nested.panes(), vec!["a".to_owned(), "b".to_owned(), "c".to_owned()]);
        let nested_rects = nested.rects(area, 0.0);
        let primary = |rect: &Rect| if horizontal { rect.width() / width } else { rect.height() / height };
        let secondary = |rect: &Rect| if horizontal { rect.height() / height } else { rect.width() / width };
        prop_assert!((primary(rect_for(&nested_rects, "a")) - 0.5).abs() < 0.01);
        prop_assert!((primary(rect_for(&nested_rects, "b")) - 0.5).abs() < 0.01);
        prop_assert!((secondary(rect_for(&nested_rects, "b")) - 0.5).abs() < 0.01);
        nested.reconcile(&["a".to_owned(), "b".to_owned()]);
        let reconciled = nested.rects(area, 0.0);
        prop_assert!((secondary(rect_for(&reconciled, "b")) - 1.0).abs() < 0.01);

        layout.set_ratio_at(&[], 0.99, 0.1, 0.2);
        let clamped = layout.rects(area, 0.0);
        let first_fraction = if horizontal {
            rect_for(&clamped, "a").width() / width
        } else {
            rect_for(&clamped, "a").height() / height
        };
        prop_assert!((first_fraction - 0.8).abs() < 0.01, "clamped ratio={first_fraction}");

        let mut collapsed = layout;
        prop_assert!(collapsed.remove("b"));
        prop_assert_eq!(collapsed.panes(), vec!["a".to_owned()]);
        prop_assert_eq!(collapsed.focused(), "a");
        prop_assert_eq!(*rect_for(&collapsed.rects(area, gap), "a"), area);
        prop_assert!(!collapsed.remove("a"));
    }
}
