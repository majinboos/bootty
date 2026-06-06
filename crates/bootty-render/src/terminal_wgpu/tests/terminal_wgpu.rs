use super::super::*;
use crate::{
    paint_plan::TextAttrs,
    terminal_image::{KittyImageLayer, KittyImagePlacement, KittyVirtualPlacement},
    terminal_render::{FillCommand, FillRole, LineCommand},
};
use std::sync::Arc;

#[test]
fn terminal_font_priority_matches_reference_default_before_system_fallback() {
    assert_eq!(GHOSTTY_FONT_FAMILY_PRIORITY[0], "JetBrains Mono");
    assert!(GHOSTTY_FONT_FAMILY_PRIORITY.contains(&"JetBrainsMono Nerd Font Mono"));
    assert!(GHOSTTY_FONT_FAMILY_PRIORITY.contains(&"Symbols Nerd Font Mono"));
}

#[test]
fn terminal_font_priority_uses_text_command_face_before_fallbacks() {
    let face = ResolvedFontFace {
        family: "Maple Mono NF".to_owned(),
        fallback_families: vec!["Symbols Nerd Font Mono".to_owned()],
        style: FontStyle::Regular,
    };

    let priority = terminal_font_family_priority(&face);

    assert_eq!(priority[0], "Maple Mono NF");
    assert_eq!(priority[1], "Symbols Nerd Font Mono");
    assert!(
        priority
            .iter()
            .position(|family| family == "JetBrains Mono")
            .expect("Ghostty fallback")
            > 1
    );
}

#[test]
fn terminal_text_cell_metrics_follow_ghostty_primary_face_rounding() {
    let config = crate::terminal_text::TerminalTextConfig {
        font_size: 30.0,
        cell_width: 1.0,
        cell_height: 1.0,
        ..Default::default()
    };
    let face = crate::terminal_text::FontResolver::new(config.clone()).resolve_face(
        &crate::paint_plan::TextAttrs {
            fg: PlanColor {
                r: 255,
                g: 255,
                b: 255,
                a: 255,
            },
            bold: false,
            italic: false,
            underline: libghostty_vt::style::Underline::None,
            strikethrough: false,
            overline: false,
        },
    );
    let font = terminal_font(&face).expect("system monospace font resolves");
    let expected = ghostty_cell_metrics_from_font(&font, config.font_size);
    let metrics = terminal_text_cell_metrics(&config);

    assert_eq!(metrics, expected);
    assert_ne!(
        metrics,
        CellMetrics::new(config.cell_width, config.cell_height)
    );
}

#[test]
fn background_command_vertices_include_cursor_commands_in_frame_order() {
    let surface = SurfaceRect::from_min_size(0.0, 0.0, 10.0, 10.0);
    let red = PlanColor {
        r: 255,
        g: 0,
        b: 0,
        a: 255,
    };
    let green = PlanColor {
        r: 0,
        g: 255,
        b: 0,
        a: 255,
    };
    let blue = PlanColor {
        r: 0,
        g: 0,
        b: 255,
        a: 255,
    };
    let commands = [
        TerminalRenderCommand::FillRect(FillCommand {
            rect: surface,
            color: red,
            role: FillRole::SurfaceBackground,
        }),
        TerminalRenderCommand::Cursor(CursorCommand {
            rect: surface,
            fill_rect: SurfaceRect::from_min_size(0.0, 8.0, 10.0, 2.0),
            color: green,
            shape: crate::paint_plan::CursorShape::Underline,
        }),
        TerminalRenderCommand::FillRect(FillCommand {
            rect: surface,
            color: blue,
            role: FillRole::CellBackground,
        }),
    ];

    let vertices = commands
        .iter()
        .flat_map(|command| background_command_vertices(surface, command))
        .collect::<Vec<_>>();

    assert_eq!(vertices.len(), 18);
    assert_eq!(vertices[0].color, color_to_float(red));
    assert_eq!(vertices[6].color, color_to_float(green));
    assert_eq!(vertices[12].color, color_to_float(blue));
}

#[test]
fn background_command_vertices_keep_decorations_before_cursor() {
    let surface = SurfaceRect::from_min_size(0.0, 0.0, 10.0, 10.0);
    let red = PlanColor {
        r: 255,
        g: 0,
        b: 0,
        a: 255,
    };
    let green = PlanColor {
        r: 0,
        g: 255,
        b: 0,
        a: 255,
    };
    let commands = [
        TerminalRenderCommand::Decoration(LineCommand {
            start_x: 0.0,
            start_y: 8.0,
            end_x: 10.0,
            end_y: 8.0,
            color: red,
            style: crate::paint_plan::DecorationStyle::Single,
        }),
        TerminalRenderCommand::Cursor(CursorCommand {
            rect: surface,
            fill_rect: surface,
            color: green,
            shape: crate::paint_plan::CursorShape::Block,
        }),
    ];

    let vertices = commands
        .iter()
        .flat_map(|command| background_command_vertices(surface, command))
        .collect::<Vec<_>>();

    assert_eq!(vertices.len(), 12);
    assert_eq!(vertices[0].color, color_to_float(red));
    assert_eq!(vertices[6].color, color_to_float(green));
}

#[test]
fn terminal_callback_key_distinguishes_terminal_viewports() {
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let left = terminal_callback_key(SurfaceRect::from_min_size(0.0, 0.0, 10.0, 10.0), format);
    let right = terminal_callback_key(SurfaceRect::from_min_size(10.0, 0.0, 10.0, 10.0), format);

    assert_ne!(left, right);
}

#[test]
fn virtual_placements_without_drawable_layers_do_not_schedule_wgpu_callback() {
    let frame = TerminalRenderFrame {
        surface: SurfaceRect::from_min_size(0.0, 0.0, 10.0, 10.0),
        commands: vec![TerminalRenderCommand::KittyVirtualPlacement(
            KittyVirtualPlacement {
                image_id: 1,
                placement_id: 1,
                columns: 2,
                rows: 1,
                z: 0,
            },
        )],
    };

    assert!(terminal_render_callback(&frame, wgpu::TextureFormat::Rgba8Unorm).is_none());
}

#[test]
fn zero_width_text_does_not_advance_following_glyphs() {
    let command = TextCommand {
        rect: SurfaceRect::from_min_size(0.0, 0.0, 20.0, 10.0),
        text: "a\u{0301}b".to_owned(),
        attrs: TextAttrs {
            fg: PlanColor {
                r: 255,
                g: 255,
                b: 255,
                a: 255,
            },
            bold: false,
            italic: false,
            underline: libghostty_vt::style::Underline::None,
            strikethrough: false,
            overline: false,
        },
        face: ResolvedFontFace {
            family: "monospace".to_owned(),
            fallback_families: Vec::new(),
            style: FontStyle::Regular,
        },
        font_size: 10.0,
    };

    let b_min_x = text_draws(&command, 1.0)
        .into_iter()
        .filter(|draw| draw.ch == 'b')
        .map(|draw| draw.rect.min_x)
        .reduce(f32::min)
        .expect("trailing printable glyph renders");

    assert!(
        b_min_x < 20.0,
        "b rendered outside command rect at {b_min_x}"
    );
}

#[test]
fn image_vertices_use_destination_and_source_rect_uvs() {
    let surface = SurfaceRect::from_min_size(0.0, 0.0, 100.0, 100.0);
    let image = image_placement(
        SurfaceRect::from_min_size(10.0, 20.0, 30.0, 40.0),
        libghostty_vt::kitty::graphics::SourceRect {
            x: 1,
            y: 2,
            width: 3,
            height: 4,
        },
        10,
        20,
        vec![0; 10 * 20 * 4],
    );

    let vertices = image_vertices(surface, &image);

    assert_eq!(vertices.len(), 6);
    assert_float_pair(vertices[0].position, [-0.8, 0.6]);
    assert_float_pair(vertices[2].position, [-0.2, -0.2]);
    assert_float_pair(vertices[0].uv, [0.15, 0.125]);
    assert_float_pair(vertices[2].uv, [0.35, 0.275]);
}

#[test]
fn image_pixels_are_expanded_to_rgba() {
    let rgb = image_placement(
        SurfaceRect::from_min_size(0.0, 0.0, 1.0, 1.0),
        libghostty_vt::kitty::graphics::SourceRect {
            x: 0,
            y: 0,
            width: 1,
            height: 1,
        },
        1,
        1,
        vec![1, 2, 3],
    );
    let rgb = KittyImagePlacement {
        image_format: libghostty_vt::kitty::graphics::ImageFormat::Rgb,
        ..rgb
    };

    assert_eq!(
        rgba_image_pixels(&rgb).as_deref(),
        Some(&[1, 2, 3, 255][..])
    );
}

#[test]
fn png_image_pixels_are_decoded_to_rgba() {
    let png = png_rgba_pixel([9, 8, 7, 6]);
    let image = KittyImagePlacement {
        image_format: libghostty_vt::kitty::graphics::ImageFormat::Png,
        ..image_placement(
            SurfaceRect::from_min_size(0.0, 0.0, 1.0, 1.0),
            libghostty_vt::kitty::graphics::SourceRect {
                x: 0,
                y: 0,
                width: 1,
                height: 1,
            },
            1,
            1,
            png,
        )
    };

    assert_eq!(
        rgba_image_pixels(&image).as_deref(),
        Some(&[9, 8, 7, 6][..])
    );
}

#[test]
fn png_image_pixels_strip_16_bit_rgb_for_texture_upload() {
    let png = png_rgb16_pixel([0x1234, 0xABCD, 0xFFFF]);
    let image = KittyImagePlacement {
        image_format: libghostty_vt::kitty::graphics::ImageFormat::Png,
        ..image_placement(
            SurfaceRect::from_min_size(0.0, 0.0, 1.0, 1.0),
            libghostty_vt::kitty::graphics::SourceRect {
                x: 0,
                y: 0,
                width: 1,
                height: 1,
            },
            1,
            1,
            png,
        )
    };

    assert_eq!(
        rgba_image_pixels(&image).as_deref(),
        Some(&[0x12, 0xAB, 0xFF, 255][..])
    );
}

#[test]
fn image_vertices_reject_out_of_bounds_source_rects() {
    let image = image_placement(
        SurfaceRect::from_min_size(0.0, 0.0, 1.0, 1.0),
        libghostty_vt::kitty::graphics::SourceRect {
            x: 1,
            y: 0,
            width: u32::MAX,
            height: 1,
        },
        2,
        2,
        vec![0; 16],
    );

    assert!(image_vertices(SurfaceRect::from_min_size(0.0, 0.0, 1.0, 1.0), &image).is_empty());
}

fn assert_float_pair(actual: [f32; 2], expected: [f32; 2]) {
    assert!((actual[0] - expected[0]).abs() < 0.0001, "{actual:?}");
    assert!((actual[1] - expected[1]).abs() < 0.0001, "{actual:?}");
}

fn image_placement(
    destination: SurfaceRect,
    source: libghostty_vt::kitty::graphics::SourceRect,
    image_width: u32,
    image_height: u32,
    data: Vec<u8>,
) -> KittyImagePlacement {
    KittyImagePlacement {
        image_id: 1,
        placement_id: 1,
        layer: KittyImageLayer::BelowText,
        image_width,
        image_height,
        image_format: libghostty_vt::kitty::graphics::ImageFormat::Rgba,
        source,
        destination,
        data: Arc::new(data),
    }
}

fn png_rgba_pixel(pixel: [u8; 4]) -> Vec<u8> {
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, 1, 1);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().expect("png header");
        writer.write_image_data(&pixel).expect("png data");
    }
    bytes
}

fn png_rgb16_pixel(pixel: [u16; 3]) -> Vec<u8> {
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, 1, 1);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Sixteen);
        let mut writer = encoder.write_header().expect("png header");
        let data = pixel
            .into_iter()
            .flat_map(u16::to_be_bytes)
            .collect::<Vec<_>>();
        writer.write_image_data(&data).expect("png data");
    }
    bytes
}
