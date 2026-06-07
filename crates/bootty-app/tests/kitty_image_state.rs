use bootty_app::{
    geometry::{CellMetrics, TerminalPadding, TerminalSurface},
    terminal_image::{KittyImageLayer, placement_destination},
};
use libghostty_vt::kitty::graphics::PlacementRenderInfo;

#[test]
fn kitty_image_layers_match_terminal_render_order() {
    assert_eq!(
        KittyImageLayer::ordered(),
        [
            KittyImageLayer::BelowBackground,
            KittyImageLayer::BelowText,
            KittyImageLayer::AboveText
        ]
    );
}

#[test]
fn kitty_placement_destination_uses_viewport_cell_position_and_offsets() {
    let surface = TerminalSurface::for_logical_size(
        120.0,
        80.0,
        CellMetrics::new(10.0, 20.0),
        TerminalPadding::uniform(2.0),
    );
    let rect = placement_destination(
        surface,
        PlacementRenderInfo {
            size: std::mem::size_of::<PlacementRenderInfo>(),
            pixel_width: 30,
            pixel_height: 40,
            grid_cols: 3,
            grid_rows: 2,
            viewport_col: 4,
            viewport_row: 1,
            viewport_visible: true,
            source_x: 0,
            source_y: 0,
            source_width: 30,
            source_height: 40,
        },
        3,
        5,
    );

    assert_eq!(rect.min_x, 45.0);
    assert_eq!(rect.min_y, 27.0);
    assert_eq!(rect.max_x, 75.0);
    assert_eq!(rect.max_y, 67.0);
}
