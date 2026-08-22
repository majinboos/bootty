use bootty_item::{ModuleCoord, ModuleCornerRadius};
use bootty_ui::item_paint::{RUN_END_RADIUS, SWEEP_PERIOD, corner_radius, rect_radius, sweep_x};

fn coord(frac: f32) -> ModuleCoord {
    ModuleCoord { frac, px: 0.0 }
}

#[test]
fn a_sweeping_rect_travels_the_space_its_width_leaves_free_and_returns() {
    let width = coord(0.25);
    let travel = 1.0 - 0.25;
    // At the start of the period the fill sits at the left edge, at the midpoint it has reached
    // the far edge, and by the end of the period it is back.
    assert!(sweep_x(coord(0.0), width, true, 0.0).frac.abs() < 1e-6);
    assert!((sweep_x(coord(0.0), width, true, SWEEP_PERIOD / 2.0).frac - travel).abs() < 1e-6);
    assert!(sweep_x(coord(0.0), width, true, SWEEP_PERIOD).frac.abs() < 1e-6);
}

#[test]
fn a_rect_that_does_not_sweep_keeps_its_declared_position() {
    let x = ModuleCoord { frac: 0.4, px: 3.0 };
    assert_eq!(sweep_x(x, coord(0.25), false, 12.345), x);
}

#[test]
fn a_run_end_rounds_the_trailing_corners_only() {
    let square = ModuleCornerRadius::default();
    assert_eq!(rect_radius(square, false), corner_radius(square));
    let rounded = rect_radius(square, true);
    assert_eq!((rounded.nw, rounded.sw), (0, 0));
    assert_eq!((rounded.ne, rounded.se), (RUN_END_RADIUS, RUN_END_RADIUS));
}
