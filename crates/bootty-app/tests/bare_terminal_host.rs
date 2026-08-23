use pretty_assertions::assert_eq;
use rstest::rstest;

use std::sync::{Arc, Mutex};

use bootty_render::{
    geometry::{CellMetrics, SurfaceRect, TerminalGeometry, TerminalPadding},
    paint_plan::PlanColor,
    terminal_render::{FillRole, TerminalRenderCommand},
    terminal_sprite::SpriteFamily,
    terminal_text::TerminalTextConfig,
};
use bootty_terminal::terminal_image::{
    KittyImageFrame, KittyImageLayer, KittyImagePlacement, KittyVirtualPlacement,
};
use bootty_terminal::{
    terminal_engine::{TerminalColorConfig, TerminalEngine},
    terminal_frame::{CellStyle, CursorSnapshot, FrameColors, FrameStats, RenderCell, RenderFrame},
    terminal_input_model::{KeyMods, MouseAction, MouseButton, MouseEncoderSize, TerminalKey},
};
use bootty_winit::bare_host::{
    BareRendererSurfaceConfig, BareTerminalInput, BareTerminalViewport, bare_terminal_key_input,
    bare_terminal_key_input_with_remaps, bare_terminal_key_input_with_sides,
    bare_terminal_mouse_input, bare_terminal_paste_shortcut, terminal_render_frame_for_bare_host,
};
use bootty_winit::{
    direct_input::ModifierSideState,
    input_binding::{BindingKey, BindingMods, BindingTrigger},
    modifier_remap::ModifierRemapSet,
};
use libghostty_vt::{
    kitty::graphics::SourceRect,
    render::{CursorVisualStyle, Dirty},
    style::{RgbColor, Underline},
};
use winit::event::MouseScrollDelta;
use winit::keyboard::{KeyCode, ModifiersState};

fn bare_viewport(
    width: u32,
    height: u32,
    cell_width: f32,
    cell_height: f32,
) -> BareTerminalViewport {
    BareTerminalViewport::new(
        width,
        height,
        CellMetrics::new(cell_width, cell_height),
        TerminalPadding::default(),
    )
}

fn kitty_viewport() -> BareTerminalViewport {
    bare_viewport(120, 40, 10.0, 20.0)
}

fn terminal_engine(cols: u16, rows: u16, cell_width: u32, cell_height: u32) -> TerminalEngine {
    TerminalEngine::new(TerminalGeometry {
        cols,
        rows,
        cell_width,
        cell_height,
    })
    .expect("terminal engine")
}

fn kitty_terminal_engine() -> TerminalEngine {
    terminal_engine(10, 4, 10, 20)
}

fn extracted_render_frame(
    engine: &mut TerminalEngine,
    viewport: BareTerminalViewport,
) -> bootty_render::terminal_render::TerminalRenderFrame {
    let frame = engine.extract_frame().expect("terminal frame");
    terminal_render_frame_for_bare_host(frame, viewport, &TerminalTextConfig::default())
}

#[test]
fn bare_viewport_resize_updates_terminal_geometry_from_renderer_metrics() {
    let mut viewport = BareTerminalViewport::new(
        1200,
        800,
        CellMetrics::new(10.0, 20.0),
        TerminalPadding::uniform(4.0),
    );

    assert_eq!(
        viewport.geometry(),
        TerminalGeometry {
            cols: 119,
            rows: 39,
            cell_width: 10,
            cell_height: 20,
        }
    );

    viewport.resize(640, 360);

    assert_eq!(
        viewport.geometry(),
        TerminalGeometry {
            cols: 63,
            rows: 17,
            cell_width: 10,
            cell_height: 20,
        }
    );
    assert_eq!(
        viewport.surface_rect(),
        SurfaceRect::from_min_size(0.0, 0.0, 640.0, 360.0)
    );
}

#[test]
fn bare_host_viewport_geometry_feeds_terminal_size_reports() {
    let viewport = BareTerminalViewport::new(
        1200,
        800,
        CellMetrics::new(9.0, 18.0),
        TerminalPadding::uniform(0.0),
    );
    let mut terminal =
        TerminalEngine::new(viewport.geometry()).expect("bare viewport geometry creates terminal");
    let output = Arc::new(Mutex::new(Vec::new()));
    let capture = output.clone();
    terminal
        .on_pty_write(move |_terminal, bytes| {
            capture
                .lock()
                .expect("pty output lock")
                .extend_from_slice(bytes);
        })
        .expect("register pty capture");

    terminal.write_vt(b"\x1b[18t\x1b[16t");

    assert_eq!(
        *output.lock().expect("pty output lock"),
        b"\x1b[8;44;133t\x1b[6;18;9t".to_vec(),
    );
}

#[test]
fn bare_renderer_surface_config_rejects_zero_sized_wgpu_surfaces() {
    let config = BareRendererSurfaceConfig::new(0, 0, wgpu::TextureFormat::Bgra8UnormSrgb);

    assert_eq!(config.width, 1);
    assert_eq!(config.height, 1);
    assert_eq!(config.format, wgpu::TextureFormat::Bgra8UnormSrgb);
}

#[test]
fn bare_viewport_marks_zero_sized_windows_not_drawable() {
    let viewport = BareTerminalViewport::new(
        0,
        0,
        CellMetrics::new(10.0, 20.0),
        TerminalPadding::default(),
    );

    assert!(!viewport.is_drawable());
}

#[test]
fn bare_host_maps_keyboard_input_without_egui() {
    let input = bare_terminal_key_input(
        KeyCode::Enter,
        ModifiersState::SHIFT | ModifiersState::ALT,
        true,
    )
    .expect("enter maps to terminal key input");

    assert_eq!(
        (input.key, input.mods, input.repeat, input.utf8),
        (
            TerminalKey::Enter,
            KeyMods {
                shift: true,
                alt: true,
                ..Default::default()
            },
            true,
            None,
        )
    );

    let shifted_q = bare_terminal_key_input(KeyCode::KeyQ, ModifiersState::SHIFT, false)
        .expect("letter maps to terminal key input");
    assert_eq!(shifted_q.utf8, Some("Q"));
    assert_eq!(shifted_q.unshifted, Some('q'));
    let numpad_one = bare_terminal_key_input(KeyCode::Numpad1, ModifiersState::empty(), false)
        .expect("numpad digit maps to terminal key input");
    assert_eq!(numpad_one.key, TerminalKey::Numpad1);
    assert_eq!(numpad_one.utf8, Some("1"));
    assert_eq!(numpad_one.unshifted, Some('1'));

    let numpad_enter =
        bare_terminal_key_input(KeyCode::NumpadEnter, ModifiersState::empty(), false)
            .expect("numpad enter maps to terminal key input");
    assert_eq!(numpad_enter.key, TerminalKey::NumpadEnter);
    assert_eq!(numpad_enter.utf8, None);

    for (code, unshifted, shifted) in [
        (KeyCode::Digit1, "1", "!"),
        (KeyCode::Digit2, "2", "@"),
        (KeyCode::Digit9, "9", "("),
        (KeyCode::Digit0, "0", ")"),
        (KeyCode::Minus, "-", "_"),
        (KeyCode::Equal, "=", "+"),
        (KeyCode::BracketLeft, "[", "{"),
        (KeyCode::BracketRight, "]", "}"),
        (KeyCode::Backslash, "\\", "|"),
        (KeyCode::Semicolon, ";", ":"),
        (KeyCode::Quote, "'", "\""),
        (KeyCode::Backquote, "`", "~"),
        (KeyCode::Comma, ",", "<"),
        (KeyCode::Period, ".", ">"),
        (KeyCode::Slash, "/", "?"),
    ] {
        let mapped = |modifiers| {
            bare_terminal_key_input(code, modifiers, false)
                .expect("printable key maps to terminal key input")
                .utf8
        };
        assert_eq!(
            (
                mapped(ModifiersState::empty()),
                mapped(ModifiersState::SHIFT)
            ),
            (Some(unshifted), Some(shifted)),
        );
    }

    let right_shift_tab = bare_terminal_key_input_with_sides(
        KeyCode::Tab,
        ModifiersState::SHIFT,
        ModifierSideState {
            right_shift: true,
            ..Default::default()
        },
        false,
    )
    .expect("right shift tab maps to terminal key input");
    assert_eq!(right_shift_tab.key, TerminalKey::Tab);
    assert_eq!(
        right_shift_tab.mods,
        KeyMods {
            shift: true,
            right_shift: true,
            ..Default::default()
        }
    );

    let mut remaps = ModifierRemapSet::default();
    remaps.parse("alt=ctrl").expect("valid modifier remap");
    remaps.finalize();
    let remapped_alt =
        bare_terminal_key_input_with_remaps(KeyCode::Enter, ModifiersState::ALT, false, &remaps)
            .expect("enter maps to terminal key input with remapped modifiers");
    assert_eq!(
        remapped_alt.mods,
        KeyMods {
            ctrl: true,
            ..Default::default()
        }
    );

    assert!(bare_terminal_key_input(KeyCode::ShiftLeft, ModifiersState::SHIFT, false).is_none());
    assert!(
        bare_terminal_key_input_with_sides(
            KeyCode::AltRight,
            ModifiersState::ALT,
            ModifierSideState {
                right_alt: true,
                ..Default::default()
            },
            false,
        )
        .is_none()
    );

    assert_eq!(
        BindingTrigger::from_key_input(numpad_one),
        BindingTrigger {
            mods: BindingMods::default(),
            key: BindingKey::Physical(TerminalKey::Numpad1)
        }
    );
}

#[test]
fn bare_host_recognizes_platform_paste_shortcut_without_encoding_v() {
    let paste_mod = if cfg!(target_os = "macos") {
        ModifiersState::SUPER
    } else {
        ModifiersState::CONTROL
    };
    let plain_v = bare_terminal_key_input(KeyCode::KeyV, ModifiersState::empty(), false)
        .expect("plain v maps to terminal key input");

    assert!(bare_terminal_paste_shortcut(KeyCode::KeyV, paste_mod));
    assert!(!bare_terminal_paste_shortcut(
        KeyCode::KeyV,
        ModifiersState::ALT | paste_mod
    ));
    assert!(!bare_terminal_paste_shortcut(KeyCode::KeyB, paste_mod));
    assert_eq!(plain_v.utf8, Some("v"));
}

#[test]
fn bare_host_maps_mouse_input_without_egui() {
    let viewport = BareTerminalViewport::new(
        240,
        120,
        CellMetrics::new(10.0, 20.0),
        TerminalPadding {
            top: 4.0,
            right: 6.0,
            bottom: 8.0,
            left: 2.0,
        },
    );

    let input = bare_terminal_mouse_input(
        eframe::egui::Pos2::new(42.0, 54.0),
        MouseAction::Press,
        Some(MouseButton::Left),
        ModifiersState::SHIFT | ModifiersState::CONTROL,
        viewport,
    )
    .expect("mouse press maps to terminal input");

    assert_eq!(
        (
            input.action,
            input.button,
            input.mods,
            input.x,
            input.y,
            input.size
        ),
        (
            MouseAction::Press,
            Some(MouseButton::Left),
            KeyMods {
                shift: true,
                ctrl: true,
                ..Default::default()
            },
            42.0,
            54.0,
            MouseEncoderSize {
                screen_width: 238,
                screen_height: 172,
                cell_width: 10,
                cell_height: 20,
                padding_top: 4,
                padding_bottom: 8,
                padding_right: 6,
                padding_left: 2,
            },
        )
    );

    assert!(
        bare_terminal_mouse_input(
            eframe::egui::Pos2::new(241.0, 54.0),
            MouseAction::Motion,
            None,
            ModifiersState::empty(),
            viewport,
        )
        .is_none()
    );

    let mut input_mapper = BareTerminalInput::default();
    input_mapper.set_cursor_position(42.0, 54.0);
    input_mapper.set_mouse_button_state(MouseButton::Left, winit::event::ElementState::Pressed);
    let motion = input_mapper
        .mouse_motion(viewport)
        .expect("cursor motion maps to terminal input");
    assert_eq!(
        (motion.action, motion.button),
        (MouseAction::Motion, Some(MouseButton::Left))
    );
    input_mapper.set_mouse_button_state(MouseButton::Left, winit::event::ElementState::Released);
    assert_eq!(
        input_mapper
            .mouse_motion(viewport)
            .expect("cursor motion without button still maps for any-motion tracking")
            .button,
        None
    );

    let wheel = |delta| {
        input_mapper
            .mouse_wheel(MouseScrollDelta::LineDelta(0.0, delta), viewport)
            .map(|input| (input.action, input.button))
    };
    assert_eq!(
        (wheel(1.0), wheel(-1.0), wheel(0.0)),
        (
            Some((MouseAction::Press, Some(MouseButton::Four))),
            Some((MouseAction::Press, Some(MouseButton::Five))),
            None,
        )
    );
}

#[test]
fn bare_host_uses_minimum_contrast_for_low_contrast_text() {
    let viewport = bare_viewport(120, 40, 10.0, 20.0);
    let mut frame = render_frame_with_text('a');
    frame.colors.background = rgb(12, 12, 12);
    frame.colors.foreground = rgb(10, 10, 10);

    let render_frame =
        terminal_render_frame_for_bare_host(&frame, viewport, &TerminalTextConfig::default());

    assert!(render_frame.commands.iter().any(|command| matches!(
        command,
        TerminalRenderCommand::Text(text)
            if text.text == "a" && text.attrs.fg == plan_color(255, 255, 255)
    )));
}

#[test]
fn bare_host_routes_cursor_and_decorations_through_structured_commands() {
    let viewport = BareTerminalViewport::new(
        120,
        40,
        CellMetrics::new(10.0, 20.0),
        TerminalPadding::default(),
    );
    let mut frame = render_frame_with_text('A');
    frame.cursor = Some(CursorSnapshot {
        x: 0,
        y: 0,
        at_wide_tail: false,
        style: CursorVisualStyle::Bar,
        blinking: false,
        color: Some(rgb(20, 21, 22)),
    });
    frame.cells[0].style = CellStyle {
        underline: Underline::Curly,
        strikethrough: true,
        overline: true,
        ..cell_style()
    };

    let render_frame =
        terminal_render_frame_for_bare_host(&frame, viewport, &TerminalTextConfig::default());

    assert!(render_frame.commands.iter().any(
        |command| matches!(command, TerminalRenderCommand::Cursor(cursor) if cursor.shape == bootty_render::paint_plan::CursorShape::Bar)
    ));
    assert!(
        render_frame
            .commands
            .iter()
            .filter(|command| matches!(command, TerminalRenderCommand::Decoration(_)))
            .count()
            >= 3
    );
    assert!(!render_frame.commands.iter().any(
        |command| matches!(command, TerminalRenderCommand::Sprite(sprite) if sprite.glyph.ch as u32 > 0x10FFFF)
    ));
}

#[test]
fn bare_host_routes_kitty_image_through_image_commands() {
    let viewport = kitty_viewport();
    let mut frame = render_frame_with_text('A');
    frame.images = KittyImageFrame {
        placements: vec![kitty_image_placement()],
        ..Default::default()
    };

    let render_frame =
        terminal_render_frame_for_bare_host(&frame, viewport, &TerminalTextConfig::default());

    assert!(matches!(
        render_frame.commands.as_slice(),
        [
            TerminalRenderCommand::FillRect(_),
            TerminalRenderCommand::Image(image),
            TerminalRenderCommand::Text(_),
        ] if image.image_id == 9
            && image.layer == KittyImageLayer::BelowText
            && image.destination == SurfaceRect::from_min_size(10.0, 0.0, 20.0, 20.0)
            && image.source == SourceRect { x: 0, y: 0, width: 2, height: 2 }
            && image.data.as_slice() == [255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 0, 255]
    ));
}

#[test]
fn bare_host_preserves_kitty_virtual_placement_metadata() {
    let viewport = kitty_viewport();
    let mut frame = render_frame_with_text('A');
    frame.images = KittyImageFrame {
        virtual_placements: vec![KittyVirtualPlacement {
            image_id: 31,
            placement_id: 7,
            columns: 2,
            rows: 1,
            z: 0,
        }],
        virtual_placeholder_rows: vec![0],
        ..Default::default()
    };

    let render_frame =
        terminal_render_frame_for_bare_host(&frame, viewport, &TerminalTextConfig::default());

    assert!(render_frame.commands.iter().any(
        |command| matches!(command, TerminalRenderCommand::KittyVirtualPlacement(placement)
            if placement.image_id == 31 && placement.placement_id == 7 && placement.columns == 2)
    ));
}

#[test]
fn bare_host_routes_kitty_unicode_placeholder_image_through_image_commands() {
    let viewport = kitty_viewport();
    let mut engine = kitty_terminal_engine();

    engine.write_vt(b"\x1b_Ga=t,t=d,f=24,i=73,s=1,v=1;AAAA\x1b\\");
    engine.write_vt(b"\x1b_Ga=p,U=1,i=73,c=1,r=1,q=1\x1b\\");
    engine.write_vt("\x1b[38;5;73m\u{10EEEE}\u{0305}\u{0305}\x1b[39m".as_bytes());
    let render_frame = extracted_render_frame(&mut engine, viewport);

    assert!(render_frame.commands.iter().any(
        |command| matches!(command, TerminalRenderCommand::Image(image)
            if image.image_id == 73
                && image.image_format == libghostty_vt::kitty::graphics::ImageFormat::Rgb
                && image.data.len() == 3)
    ));
}

#[test]
fn bare_host_receives_cleaned_kitty_image_frame_without_image_commands() {
    let viewport = kitty_viewport();
    let mut engine = kitty_terminal_engine();

    engine.write_vt(
        b"\x1b_Ga=T,f=100,q=1,i=31,p=1;iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAA\
          DUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==\x1b\\",
    );
    assert!(
        engine
            .extract_frame()
            .expect("image frame")
            .images
            .placements
            .iter()
            .any(|placement| placement.image_id == 31)
    );

    engine.write_vt(b"\x1b_Ga=d,d=A\x1b\\");
    let frame = engine.extract_frame().expect("cleaned image frame");
    let render_frame =
        terminal_render_frame_for_bare_host(frame, viewport, &TerminalTextConfig::default());

    assert!(frame.images.placements.is_empty());
    assert!(
        !render_frame
            .commands
            .iter()
            .any(|command| matches!(command, TerminalRenderCommand::Image(_)))
    );
}

#[test]
fn bare_host_preserves_kitty_storage_deletions() {
    let viewport = kitty_viewport();
    let mut engine = kitty_terminal_engine();

    engine.write_vt(b"\x1b_Ga=T,t=d,i=52,p=1,s=1,v=1;/////w==\x1b\\");
    engine.write_vt(b"\x1b_Ga=p,i=52,p=2,q=1\x1b\\");
    engine.write_vt(b"\x1b_Ga=d,d=i,i=52,p=1\x1b\\");
    let render_frame = extracted_render_frame(&mut engine, viewport);

    assert!(render_frame.commands.iter().any(
        |command| matches!(command, TerminalRenderCommand::Image(image)
            if image.image_id == 52 && image.placement_id == 2)
    ));
    assert!(!render_frame.commands.iter().any(
        |command| matches!(command, TerminalRenderCommand::Image(image)
            if image.image_id == 52 && image.placement_id == 1)
    ));
}

#[test]
fn bare_host_keeps_existing_kitty_image_after_unimplemented_animation_commands() {
    let viewport = kitty_viewport();
    let mut engine = kitty_terminal_engine();

    engine.write_vt(
        b"\x1b_Ga=T,f=100,q=1,i=31,p=1;iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAA\
          DUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==\x1b\\",
    );
    for command in [
        b"\x1b_Ga=f,i=31,c=1,q=1\x1b\\".as_slice(),
        b"\x1b_Ga=a,i=31,s=3,q=1\x1b\\".as_slice(),
        b"\x1b_Ga=c,i=31,c=1,q=1\x1b\\".as_slice(),
    ] {
        engine.write_vt(command);
    }

    let render_frame = extracted_render_frame(&mut engine, viewport);

    assert!(render_frame.commands.iter().any(
        |command| matches!(command, TerminalRenderCommand::Image(image)
            if image.image_id == 31 && image.placement_id == 1)
    ));
}

#[test]
fn bare_host_preserves_terminal_tabstop_positions() {
    let viewport = bare_viewport(600, 20, 1.0, 20.0);
    let mut engine = terminal_engine(600, 1, 1, 20);

    engine.write_vt(b"\x1b[3g\x1b[519G\x1bH\x1b[1GA\tB");
    let render_frame = extracted_render_frame(&mut engine, viewport);

    assert!(render_frame.commands.iter().any(
        |command| matches!(command, TerminalRenderCommand::Text(text)
            if text.text == "A" && text.rect == SurfaceRect::from_min_size(0.0, 0.0, 1.0, 20.0))
    ));
    assert!(render_frame.commands.iter().any(
        |command| matches!(command, TerminalRenderCommand::Text(text)
            if text.text == "B" && text.rect == SurfaceRect::from_min_size(518.0, 0.0, 1.0, 20.0))
    ));
}

#[test]
fn bare_host_preserves_utf8_replacement_text() {
    let viewport = bare_viewport(160, 20, 10.0, 20.0);
    let mut engine = terminal_engine(16, 1, 10, 20);

    engine.write_vt(b"\xF0\x9F");
    engine.write_vt("😄".as_bytes());
    let render_frame = extracted_render_frame(&mut engine, viewport);

    assert!(render_frame.commands.iter().any(
        |command| matches!(command, TerminalRenderCommand::Text(text)
                if text.text.contains('\u{FFFD}'))
    ));
    assert!(render_frame.commands.iter().any(
        |command| matches!(command, TerminalRenderCommand::Text(text)
                if text.text.contains('😄'))
    ));
}

#[test]
fn bare_host_preserves_charset_rendering() {
    let viewport = bare_viewport(160, 20, 10.0, 20.0);
    let mut engine = terminal_engine(16, 1, 10, 20);

    engine.write_vt("\x1b(A#\x1b(0qx".as_bytes());
    let render_frame = extracted_render_frame(&mut engine, viewport);

    assert!(render_frame.commands.iter().any(
        |command| matches!(command, TerminalRenderCommand::Text(text)
                if text.text == "£")
    ));
    assert!(render_frame.commands.iter().any(
        |command| matches!(command, TerminalRenderCommand::Sprite(sprite)
                if sprite.glyph.ch == '─' && sprite.glyph.family == SpriteFamily::BoxDrawing)
    ));
    assert!(render_frame.commands.iter().any(
        |command| matches!(command, TerminalRenderCommand::Sprite(sprite)
                if sprite.glyph.ch == '│' && sprite.glyph.family == SpriteFamily::BoxDrawing)
    ));
}

#[rstest]
#[case(
    b"\x1b]21;foreground=rgb:12/34/56;background=rgb:78/9a/bc;cursor=rgb:de/f0/12\x1b\\X",
    "X",
    plan_color(0x12, 0x34, 0x56),
    plan_color(0x78, 0x9a, 0xbc),
    plan_color(0xde, 0xf0, 0x12)
)]
#[case(
    b"\x1b]4;42;rgb:ff/00/00\x1b\\\x1b]10;rgb:12/34/56;rgb:78/9a/bc\x1b\\\x1b]12;rgb:de/f0/12\x1b\\\x1b[38;5;42mP",
    "P",
    plan_color(0xff, 0, 0),
    plan_color(0x78, 0x9a, 0xbc),
    plan_color(0xde, 0xf0, 0x12),
)]
#[case(
    b"\x1b]4;42;FoReStGReen\x1b\\\x1b]10;medium spring green;LawnGreen\x1b\\\x1b]12;white\x1b\\\x1b[38;5;42mX",
    "X",
    plan_color(34, 139, 34),
    plan_color(124, 252, 0),
    plan_color(255, 255, 255),
)]
fn bare_host_preserves_color_protocol_rendering(
    #[case] input: &[u8],
    #[case] glyph: &str,
    #[case] foreground: PlanColor,
    #[case] background: PlanColor,
    #[case] cursor: PlanColor,
) {
    let mut engine = terminal_engine(8, 1, 10, 20);
    engine.write_vt(input);
    let render_frame = extracted_render_frame(&mut engine, bare_viewport(80, 20, 10.0, 20.0));

    assert!(render_frame.commands.iter().any(|command| matches!(command,
        TerminalRenderCommand::FillRect(fill) if fill.role == FillRole::SurfaceBackground && fill.color == background)));
    assert!(render_frame.commands.iter().any(|command| matches!(command,
        TerminalRenderCommand::Text(text) if text.text == glyph && text.attrs.fg == foreground)));
    assert!(render_frame.commands.iter().any(|command| matches!(command,
        TerminalRenderCommand::Cursor(command) if command.color == cursor)));
}

#[test]
fn bare_host_preserves_generated_256_color_palette() {
    let viewport = bare_viewport(80, 20, 10.0, 20.0);
    let mut engine = TerminalEngine::new_with_scrollback(
        TerminalGeometry {
            cols: 8,
            rows: 1,
            cell_width: 10,
            cell_height: 20,
        },
        TerminalColorConfig {
            background: RgbColor { r: 0, g: 0, b: 0 },
            foreground: RgbColor {
                r: 255,
                g: 255,
                b: 255,
            },
            palette_generate: true,
            ..Default::default()
        },
        0,
    )
    .expect("generated-palette terminal");

    engine.write_vt(b"\x1b[38;5;16mB\x1b[38;5;231mW");
    let render_frame = extracted_render_frame(&mut engine, viewport);

    let text_commands = render_frame
        .commands
        .iter()
        .filter_map(|command| match command {
            TerminalRenderCommand::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(text_commands, ["B", "W"]);

    assert!(render_frame.commands.iter().any(
        |command| matches!(command, TerminalRenderCommand::Text(text)
            if text.text == "W" && text.attrs.fg == plan_color(255, 255, 255))
    ));
}

#[test]
fn bare_host_routes_sprite_families_through_sprite_commands() {
    for (ch, family) in [
        ('█', SpriteFamily::Block),
        ('\u{2801}', SpriteFamily::Braille),
        ('\u{E0B0}', SpriteFamily::Powerline),
    ] {
        assert_bare_host_routes_sprite(ch, family);
    }

    for ch in [
        '\u{EE00}', '\u{EE01}', '\u{EE02}', '\u{EE06}', '\u{EE09}', '\u{EE0B}',
    ] {
        assert_bare_host_routes_sprite(ch, SpriteFamily::ProgressIndicator);
    }

    for cp in 0x2500..=0x257F {
        assert_bare_host_routes_sprite_family(
            char::from_u32(cp).unwrap_or_else(|| panic!("invalid U+{cp:04X}")),
            SpriteFamily::BoxDrawing,
        );
    }
}

#[test]
fn bare_host_routes_all_owned_legacy_computing_ranges_through_sprite_commands() {
    for (codepoints, family) in [
        (
            legacy_computing_codepoints().collect::<Vec<_>>(),
            SpriteFamily::LegacyComputing,
        ),
        (
            legacy_computing_supplement_codepoints().collect(),
            SpriteFamily::LegacyComputingSupplement,
        ),
    ] {
        for cp in codepoints {
            assert_bare_host_routes_sprite_family(
                char::from_u32(cp).unwrap_or_else(|| panic!("invalid U+{cp:04X}")),
                family,
            );
        }
    }
}

fn assert_bare_host_routes_sprite(ch: char, family: SpriteFamily) {
    let command_count = assert_bare_host_routes_sprite_family(ch, family);
    assert!(
        command_count > 0,
        "sprite command route for {ch} should include renderer commands"
    );
}

fn assert_bare_host_routes_sprite_family(ch: char, family: SpriteFamily) -> usize {
    let viewport = BareTerminalViewport::new(
        120,
        40,
        CellMetrics::new(10.0, 20.0),
        TerminalPadding::default(),
    );
    let frame = render_frame_with_text(ch);

    let render_frame =
        terminal_render_frame_for_bare_host(&frame, viewport, &TerminalTextConfig::default());

    let sprite = render_frame
        .commands
        .iter()
        .find_map(|command| match command {
            TerminalRenderCommand::Sprite(sprite)
                if sprite.glyph.ch == ch && sprite.glyph.family == family =>
            {
                Some(sprite.glyph.commands_for(sprite.rect).len())
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("missing bare-host sprite route for {ch}"));
    let text_fallback = ch.to_string();
    assert!(!render_frame.commands.iter().any(
        |command| matches!(command, TerminalRenderCommand::Text(text) if text.text == text_fallback)
    ));
    sprite
}

fn legacy_computing_codepoints() -> impl Iterator<Item = u32> {
    [
        0x1FB00..=0x1FB67,
        0x1FB68..=0x1FB6F,
        0x1FB70..=0x1FB99,
        0x1FB9A..=0x1FB9F,
        0x1FBA0..=0x1FBAF,
        0x1FBBD..=0x1FBBF,
        0x1FBCE..=0x1FBCF,
        0x1FBD0..=0x1FBDF,
        0x1FBE0..=0x1FBEF,
    ]
    .into_iter()
    .flatten()
}

fn legacy_computing_supplement_codepoints() -> impl Iterator<Item = u32> {
    [
        0x1CC1B..=0x1CC1E,
        0x1CC21..=0x1CC2F,
        0x1CC30..=0x1CC3F,
        0x1CD00..=0x1CDE5,
        0x1CE00..=0x1CE01,
        0x1CE0B..=0x1CE0C,
        0x1CE16..=0x1CE19,
        0x1CE51..=0x1CE8F,
        0x1CE90..=0x1CEAF,
    ]
    .into_iter()
    .flatten()
}

fn render_frame_with_text(ch: char) -> RenderFrame {
    RenderFrame {
        cols: 1,
        rows: 1,
        dirty: Dirty::Full,
        colors: FrameColors {
            background: rgb(1, 2, 3),
            foreground: rgb(220, 221, 222),
            cursor: None,
            ..Default::default()
        },
        cursor: None,
        row_dirty: vec![true],
        row_wraps: vec![false],
        search_matches: Vec::new(),
        active_search_match: None,
        active_search_match_index: None,
        search_match_count: 0,
        search_pulse: 0,
        copy_mode: None,
        mouse_tracking: false,
        selections: Vec::new(),
        cells: vec![RenderCell {
            x: 0,
            y: 0,
            text_start: 0,
            text_len: 1,
            fg: None,
            bg: None,
            style: cell_style(),
            hyperlink: None,
        }],
        text: vec![ch],
        images: Default::default(),
        scrollbar: None,
        stats: FrameStats {
            cells: 1,
            chars: 1,
            dirty_rows: 1,
            ..Default::default()
        },
    }
}

fn kitty_image_placement() -> KittyImagePlacement {
    KittyImagePlacement {
        image_id: 9,
        placement_id: 10,
        layer: KittyImageLayer::BelowText,
        image_width: 2,
        image_height: 2,
        image_format: libghostty_vt::kitty::graphics::ImageFormat::Rgba,
        source: SourceRect {
            x: 0,
            y: 0,
            width: 2,
            height: 2,
        },
        destination: SurfaceRect::from_min_size(10.0, 0.0, 20.0, 20.0),
        data: Arc::new(vec![
            255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 0, 255,
        ]),
    }
}

fn cell_style() -> CellStyle {
    CellStyle {
        bold: false,
        italic: false,
        faint: false,
        blink: false,
        inverse: false,
        invisible: false,
        strikethrough: false,
        overline: false,
        underline: Underline::None,
    }
}

fn rgb(r: u8, g: u8, b: u8) -> RgbColor {
    RgbColor { r, g, b }
}

fn plan_color(r: u8, g: u8, b: u8) -> PlanColor {
    PlanColor { r, g, b, a: 255 }
}
