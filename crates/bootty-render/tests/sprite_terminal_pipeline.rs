use bootty_render::{
    paint_plan::PlanColor,
    terminal_sprite::{SpriteCommand, SpriteRegistry, SpriteShape, WgpuSpriteBackend},
};
use bootty_surface::geometry::SurfaceRect;

#[test]
fn wgpu_sprite_backend_builds_primitive_buffers_for_task_shapes() {
    let registry = SpriteRegistry::prompt_graphics();
    let rect = SurfaceRect::from_min_size(0.0, 0.0, 8.0, 24.0);
    let color = PlanColor {
        r: 10,
        g: 20,
        b: 30,
        a: 255,
    };
    let mut all_commands = Vec::new();

    for ch in ['┃', '\u{E0B8}', '\u{E0B1}', '\u{E0B4}'] {
        let glyph = registry.glyph_for(ch).expect("task sprite glyph");
        all_commands.extend(registry.commands_for(glyph, rect));
    }

    assert!(
        all_commands
            .iter()
            .any(|command| matches!(command, SpriteCommand::FillRect { .. }))
    );
    assert!(all_commands.iter().any(|command| matches!(
        command,
        SpriteCommand::FillPolygon {
            shape: SpriteShape::Triangle,
            ..
        }
    )));
    assert!(
        all_commands
            .iter()
            .any(|command| matches!(command, SpriteCommand::StrokePolyline { .. }))
    );

    let primitives = WgpuSpriteBackend::build_primitives(&all_commands, color);

    assert!(primitives.vertices.len() >= 18);
    assert!(primitives.indices.len() >= 24);
    assert!(primitives.indices.len().is_multiple_of(3));
}
