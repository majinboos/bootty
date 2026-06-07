use std::{
    fs,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use bootty_app::{
    bare_host::{
        BareRendererSurfaceConfig, BareTerminalInput, BareTerminalViewport,
        bare_terminal_key_input, bare_terminal_key_input_with_remaps,
        bare_terminal_key_input_with_sides, bare_terminal_mouse_input,
        bare_terminal_paste_shortcut, terminal_render_frame_for_bare_host,
    },
    direct_input::ModifierSideState,
    geometry::{CellMetrics, SurfaceRect, TerminalGeometry, TerminalPadding},
    input_binding::{BindingKey, BindingMods, BindingTrigger},
    modifier_remap::ModifierRemapSet,
    paint_plan::PlanColor,
    terminal::TerminalEngine,
    terminal::{
        CellStyle, CursorSnapshot, FrameColors, FrameStats, KeyMods, MouseAction, MouseButton,
        MouseEncoderSize, RenderCell, RenderFrame, TerminalKey,
    },
    terminal_image::{
        KittyImageFrame, KittyImageLayer, KittyImagePlacement, KittyVirtualPlacement,
    },
    terminal_render::{FillRole, TerminalRenderCommand},
    terminal_sprite::SpriteFamily,
    terminal_text::TerminalTextConfig,
};
use bootty_terminal::terminal_palette::generate_256_palette;
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

    assert_eq!(input.key, TerminalKey::Enter);
    assert_eq!(
        input.mods,
        KeyMods {
            shift: true,
            alt: true,
            ctrl: false,
            command: false,
            ..Default::default()
        }
    );
    assert!(input.repeat);
    assert_eq!(input.utf8, None);

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
        assert_eq!(
            bare_terminal_key_input(code, ModifiersState::empty(), false)
                .expect("printable key maps to terminal key input")
                .utf8,
            Some(unshifted)
        );
        assert_eq!(
            bare_terminal_key_input(code, ModifiersState::SHIFT, false)
                .expect("shifted printable key maps to terminal key input")
                .utf8,
            Some(shifted)
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

    assert_eq!(input.action, MouseAction::Press);
    assert_eq!(input.button, Some(MouseButton::Left));
    assert_eq!(
        input.mods,
        KeyMods {
            shift: true,
            ctrl: true,
            ..Default::default()
        }
    );
    assert_eq!(input.x, 42.0);
    assert_eq!(input.y, 54.0);
    assert_eq!(
        input.size,
        MouseEncoderSize {
            screen_width: 240,
            screen_height: 120,
            cell_width: 10,
            cell_height: 20,
            padding_top: 4,
            padding_bottom: 8,
            padding_right: 6,
            padding_left: 2,
        }
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
    assert_eq!(motion.action, MouseAction::Motion);
    assert_eq!(motion.button, Some(MouseButton::Left));
    input_mapper.set_mouse_button_state(MouseButton::Left, winit::event::ElementState::Released);
    assert_eq!(
        input_mapper
            .mouse_motion(viewport)
            .expect("cursor motion without button still maps for any-motion tracking")
            .button,
        None
    );

    let wheel_up = input_mapper
        .mouse_wheel(MouseScrollDelta::LineDelta(0.0, 1.0), viewport)
        .expect("wheel up maps to terminal button four");
    assert_eq!(wheel_up.action, MouseAction::Press);
    assert_eq!(wheel_up.button, Some(MouseButton::Four));

    let wheel_down = input_mapper
        .mouse_wheel(MouseScrollDelta::LineDelta(0.0, -1.0), viewport)
        .expect("wheel down maps to terminal button five");
    assert_eq!(wheel_down.action, MouseAction::Press);
    assert_eq!(wheel_down.button, Some(MouseButton::Five));

    assert!(
        input_mapper
            .mouse_wheel(MouseScrollDelta::LineDelta(0.0, 0.0), viewport)
            .is_none()
    );
}

#[test]
fn bare_host_render_frame_preserves_terminal_text_commands_without_egui_callback() {
    let viewport = BareTerminalViewport::new(
        120,
        40,
        CellMetrics::new(10.0, 20.0),
        TerminalPadding::default(),
    );
    let frame = render_frame_with_text('A');

    let render_frame =
        terminal_render_frame_for_bare_host(&frame, viewport, &TerminalTextConfig::default());

    assert!(
        render_frame.commands.iter().any(
            |command| matches!(command, TerminalRenderCommand::Text(text) if text.text == "A")
        )
    );
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
        |command| matches!(command, TerminalRenderCommand::Cursor(cursor) if cursor.shape == bootty_app::paint_plan::CursorShape::Bar)
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
        |command| matches!(command, TerminalRenderCommand::Sprite(sprite) if sprite.ch as u32 > 0x10FFFF)
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
fn bare_host_routes_default_rgba_kitty_image_through_image_commands() {
    let viewport = kitty_viewport();
    let mut engine = kitty_terminal_engine();

    engine.write_vt(b"\x1b_Ga=T,t=d,i=41,p=2,s=1,v=2,c=1,r=1;///////////\x1b\\");
    let frame = engine.extract_frame().expect("rgba image frame");
    let render_frame =
        terminal_render_frame_for_bare_host(frame, viewport, &TerminalTextConfig::default());

    assert!(render_frame.commands.iter().any(
        |command| matches!(command, TerminalRenderCommand::Image(image)
            if image.image_id == 41
                && image.placement_id == 2
                && image.image_format == libghostty_vt::kitty::graphics::ImageFormat::Rgba
                && image.data.len() == 8)
    ));
}

#[test]
fn bare_host_routes_rgb_kitty_image_through_image_commands() {
    let viewport = kitty_viewport();
    let mut engine = kitty_terminal_engine();

    engine.write_vt(b"\x1b_Ga=T,f=24,t=d,i=73,p=1,s=1,v=1;AAAA\x1b\\");
    let frame = engine.extract_frame().expect("RGB image frame");
    let render_frame =
        terminal_render_frame_for_bare_host(frame, viewport, &TerminalTextConfig::default());

    assert!(render_frame.commands.iter().any(
        |command| matches!(command, TerminalRenderCommand::Image(image)
            if image.image_id == 73
                && image.placement_id == 1
                && image.image_format == libghostty_vt::kitty::graphics::ImageFormat::Rgb
                && image.data.len() == 3)
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
    let frame = engine.extract_frame().expect("unicode kitty image frame");
    let render_frame =
        terminal_render_frame_for_bare_host(frame, viewport, &TerminalTextConfig::default());

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
    let frame = engine.extract_frame().expect("kitty storage frame");
    let render_frame =
        terminal_render_frame_for_bare_host(frame, viewport, &TerminalTextConfig::default());

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

    let frame = engine.extract_frame().expect("animation frame");
    let render_frame =
        terminal_render_frame_for_bare_host(frame, viewport, &TerminalTextConfig::default());

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
    let frame = engine.extract_frame().expect("tabstop frame");
    let render_frame =
        terminal_render_frame_for_bare_host(frame, viewport, &TerminalTextConfig::default());

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
    let frame = engine.extract_frame().expect("utf8 frame");
    let render_frame =
        terminal_render_frame_for_bare_host(frame, viewport, &TerminalTextConfig::default());

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
    let frame = engine.extract_frame().expect("charset frame");
    let render_frame =
        terminal_render_frame_for_bare_host(frame, viewport, &TerminalTextConfig::default());

    assert!(render_frame.commands.iter().any(
        |command| matches!(command, TerminalRenderCommand::Text(text)
                if text.text == "£")
    ));
    assert!(render_frame.commands.iter().any(
        |command| matches!(command, TerminalRenderCommand::Sprite(sprite)
                if sprite.ch == '─' && sprite.glyph.family == SpriteFamily::BoxDrawing)
    ));
    assert!(render_frame.commands.iter().any(
        |command| matches!(command, TerminalRenderCommand::Sprite(sprite)
                if sprite.ch == '│' && sprite.glyph.family == SpriteFamily::BoxDrawing)
    ));
}

#[test]
fn bare_host_preserves_kitty_color_protocol_rendering() {
    let viewport = bare_viewport(80, 20, 10.0, 20.0);
    let mut engine = terminal_engine(8, 1, 10, 20);

    engine.write_vt(
        b"\x1b]21;foreground=rgb:12/34/56;background=rgb:78/9a/bc;cursor=rgb:de/f0/12\x1b\\X",
    );
    let frame = engine.extract_frame().expect("kitty color frame");
    let render_frame =
        terminal_render_frame_for_bare_host(frame, viewport, &TerminalTextConfig::default());

    assert!(render_frame.commands.iter().any(
        |command| matches!(command, TerminalRenderCommand::FillRect(fill)
            if fill.role == FillRole::SurfaceBackground && fill.color == plan_color(0x78, 0x9a, 0xbc))
    ));
    assert!(render_frame.commands.iter().any(
        |command| matches!(command, TerminalRenderCommand::Text(text)
            if text.text == "X" && text.attrs.fg == plan_color(0x12, 0x34, 0x56))
    ));
    assert!(render_frame.commands.iter().any(
        |command| matches!(command, TerminalRenderCommand::Cursor(cursor)
            if cursor.color == plan_color(0xde, 0xf0, 0x12))
    ));
}

#[test]
fn bare_host_preserves_osc_color_operation_rendering() {
    let viewport = bare_viewport(80, 20, 10.0, 20.0);
    let mut engine = terminal_engine(8, 1, 10, 20);

    engine.write_vt(
        b"\x1b]4;42;rgb:ff/00/00\x1b\\\x1b]10;rgb:12/34/56;rgb:78/9a/bc\x1b\\\x1b]12;rgb:de/f0/12\x1b\\\x1b[38;5;42mP",
    );
    let frame = engine.extract_frame().expect("OSC color frame");
    let render_frame =
        terminal_render_frame_for_bare_host(frame, viewport, &TerminalTextConfig::default());

    assert!(render_frame.commands.iter().any(
        |command| matches!(command, TerminalRenderCommand::FillRect(fill)
            if fill.role == FillRole::SurfaceBackground && fill.color == plan_color(0x78, 0x9a, 0xbc))
    ));
    assert!(render_frame.commands.iter().any(
        |command| matches!(command, TerminalRenderCommand::Text(text)
            if text.text == "P" && text.attrs.fg == plan_color(0xff, 0x00, 0x00))
    ));
    assert!(render_frame.commands.iter().any(
        |command| matches!(command, TerminalRenderCommand::Cursor(cursor)
            if cursor.color == plan_color(0xde, 0xf0, 0x12))
    ));
}

#[test]
fn bare_host_preserves_generated_256_color_palette() {
    let viewport = bare_viewport(80, 20, 10.0, 20.0);
    let mut engine = terminal_engine(8, 1, 10, 20);
    let base = engine.default_color_palette().expect("default palette");
    let generated = generate_256_palette(
        &base,
        &[false; 256],
        RgbColor { r: 0, g: 0, b: 0 },
        RgbColor {
            r: 255,
            g: 255,
            b: 255,
        },
        false,
    );
    engine
        .set_default_color_palette(generated)
        .expect("set generated palette");

    engine.write_vt(b"\x1b[38;5;16mB\x1b[38;5;231mW");
    let frame = engine.extract_frame().expect("generated palette frame");
    let render_frame =
        terminal_render_frame_for_bare_host(frame, viewport, &TerminalTextConfig::default());

    assert!(render_frame.commands.iter().any(
        |command| matches!(command, TerminalRenderCommand::Text(text)
            if text.text == "B")
    ));
    assert!(render_frame.commands.iter().any(
        |command| matches!(command, TerminalRenderCommand::Text(text)
            if text.text == "W" && text.attrs.fg == plan_color(255, 255, 255))
    ));
}

#[test]
fn bare_host_preserves_x11_color_name_rendering() {
    let viewport = bare_viewport(80, 20, 10.0, 20.0);
    let mut engine = terminal_engine(8, 1, 10, 20);

    engine.write_vt(
        b"\x1b]4;42;FoReStGReen\x1b\\\x1b]10;medium spring green;LawnGreen\x1b\\\x1b]12;white\x1b\\\x1b[38;5;42mX",
    );
    let frame = engine.extract_frame().expect("X11 color-name frame");
    let render_frame =
        terminal_render_frame_for_bare_host(frame, viewport, &TerminalTextConfig::default());

    assert!(render_frame.commands.iter().any(
        |command| matches!(command, TerminalRenderCommand::FillRect(fill)
            if fill.role == FillRole::SurfaceBackground && fill.color == plan_color(124, 252, 0))
    ));
    assert!(render_frame.commands.iter().any(
        |command| matches!(command, TerminalRenderCommand::Text(text)
            if text.text == "X" && text.attrs.fg == plan_color(34, 139, 34))
    ));
    assert!(render_frame.commands.iter().any(
        |command| matches!(command, TerminalRenderCommand::Cursor(cursor)
            if cursor.color == plan_color(255, 255, 255))
    ));
}

#[test]
fn bare_host_routes_sprite_families_through_sprite_commands() {
    for (ch, family) in [
        ('█', SpriteFamily::Block),
        ('\u{2801}', SpriteFamily::Braille),
        ('\u{E0B0}', SpriteFamily::Powerline),
        ('\u{1FB00}', SpriteFamily::LegacyComputing),
        ('\u{1FB3C}', SpriteFamily::LegacyComputing),
        ('\u{1FB6C}', SpriteFamily::LegacyComputing),
        ('\u{1FB70}', SpriteFamily::LegacyComputing),
        ('\u{1FB98}', SpriteFamily::LegacyComputing),
        ('\u{1FB9C}', SpriteFamily::LegacyComputing),
        ('\u{1FBA0}', SpriteFamily::LegacyComputing),
        ('\u{1FBAF}', SpriteFamily::LegacyComputing),
        ('\u{1FBBD}', SpriteFamily::LegacyComputing),
        ('\u{1FBCE}', SpriteFamily::LegacyComputing),
        ('\u{1FBD0}', SpriteFamily::LegacyComputing),
        ('\u{1FBE8}', SpriteFamily::LegacyComputing),
        ('\u{1CC21}', SpriteFamily::LegacyComputingSupplement),
        ('\u{1CC1B}', SpriteFamily::LegacyComputingSupplement),
        ('\u{1CC30}', SpriteFamily::LegacyComputingSupplement),
        ('\u{1CD00}', SpriteFamily::LegacyComputingSupplement),
        ('\u{1CE00}', SpriteFamily::LegacyComputingSupplement),
        ('\u{1CE0B}', SpriteFamily::LegacyComputingSupplement),
        ('\u{1CE51}', SpriteFamily::LegacyComputingSupplement),
        ('\u{1CE90}', SpriteFamily::LegacyComputingSupplement),
    ] {
        assert_bare_host_routes_sprite(ch, family);
    }

    for ch in [
        '\u{EE00}', '\u{EE01}', '\u{EE02}', '\u{EE06}', '\u{EE09}', '\u{EE0B}',
    ] {
        assert_bare_host_routes_sprite(ch, SpriteFamily::ProgressIndicator);
    }

    for cp in box_line_junction_codepoints() {
        assert_bare_host_routes_sprite_family(
            char::from_u32(cp).unwrap_or_else(|| panic!("invalid U+{cp:04X}")),
            SpriteFamily::BoxDrawing,
        );
    }

    for cp in box_dash_diagonal_codepoints() {
        assert_bare_host_routes_sprite_family(
            char::from_u32(cp).unwrap_or_else(|| panic!("invalid U+{cp:04X}")),
            SpriteFamily::BoxDrawing,
        );
    }

    for cp in box_double_line_codepoints() {
        assert_bare_host_routes_sprite_family(
            char::from_u32(cp).unwrap_or_else(|| panic!("invalid U+{cp:04X}")),
            SpriteFamily::BoxDrawing,
        );
    }

    for cp in 0x2500..=0x257F {
        assert_bare_host_routes_sprite_family(
            char::from_u32(cp).unwrap_or_else(|| panic!("invalid U+{cp:04X}")),
            SpriteFamily::BoxDrawing,
        );
    }
}

#[test]
#[ignore = "requires Ghostty sprite range fixture that is not vendored in this rewrite"]
fn bare_host_routes_all_legacy_computing_supplement_draw_ranges_through_sprite_commands() {
    for cp in ghostty_legacy_computing_supplement_draw_codepoints() {
        assert_bare_host_routes_sprite_family(
            char::from_u32(cp).unwrap_or_else(|| panic!("invalid upstream U+{cp:04X}")),
            SpriteFamily::LegacyComputingSupplement,
        );
    }
}

#[test]
#[ignore = "requires Ghostty sprite range fixture that is not vendored in this rewrite"]
fn bare_host_routes_all_legacy_computing_draw_ranges_through_sprite_commands() {
    for cp in ghostty_legacy_computing_draw_codepoints() {
        assert_bare_host_routes_sprite_family(
            char::from_u32(cp).unwrap_or_else(|| panic!("invalid upstream U+{cp:04X}")),
            SpriteFamily::LegacyComputing,
        );
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
                if sprite.ch == ch && sprite.glyph.family == family =>
            {
                Some(sprite.commands.len())
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

fn ghostty_legacy_computing_draw_codepoints() -> Vec<u32> {
    let source_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../vendor/ghostty/src/font/sprite/draw/symbols_for_legacy_computing.zig");
    let source = fs::read_to_string(&source_path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", source_path.display()));
    let mut codepoints = Vec::new();

    for line in source.lines() {
        let Some(rest) = line.trim_start().strip_prefix("pub fn draw") else {
            continue;
        };
        let Some(name) = rest.split('(').next() else {
            continue;
        };
        let Some((start, end)) = parse_draw_range(name) else {
            continue;
        };
        if !(0x1FB00..=0x1FBFF).contains(&start) {
            continue;
        }
        codepoints.extend(start..=end);
    }

    codepoints.sort_unstable();
    codepoints.dedup();
    codepoints
}

fn box_line_junction_codepoints() -> impl Iterator<Item = u32> {
    (0x2500..=0x254B)
        .filter(|cp| !matches!(cp, 0x2504..=0x250B))
        .chain(0x2574..=0x257F)
}

fn box_dash_diagonal_codepoints() -> impl Iterator<Item = u32> {
    [
        0x2504, 0x2505, 0x2506, 0x2507, 0x2508, 0x2509, 0x250A, 0x250B, 0x254C, 0x254D, 0x254E,
        0x254F, 0x2571, 0x2572, 0x2573,
    ]
    .into_iter()
}

fn box_double_line_codepoints() -> impl Iterator<Item = u32> {
    0x2550..=0x256C
}

fn ghostty_legacy_computing_supplement_draw_codepoints() -> Vec<u32> {
    let source_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(
        "../../vendor/ghostty/src/font/sprite/draw/symbols_for_legacy_computing_supplement.zig",
    );
    let source = fs::read_to_string(&source_path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", source_path.display()));
    let mut codepoints = Vec::new();

    for line in source.lines() {
        let Some(rest) = line.trim_start().strip_prefix("pub fn draw") else {
            continue;
        };
        let Some(name) = rest.split('(').next() else {
            continue;
        };
        let Some((start, end)) = parse_draw_range(name) else {
            continue;
        };
        if !(0x1CC00..=0x1CEBF).contains(&start) {
            continue;
        }
        codepoints.extend(start..=end);
    }

    codepoints.sort_unstable();
    codepoints.dedup();
    codepoints
}

fn parse_draw_range(name: &str) -> Option<(u32, u32)> {
    let (start, end) = name.split_once('_').unwrap_or((name, name));
    let start = u32::from_str_radix(start, 16).ok()?;
    let end = u32::from_str_radix(end, 16).ok()?;
    Some((start, end))
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
        cells: vec![RenderCell {
            x: 0,
            y: 0,
            text_start: 0,
            text_len: 1,
            fg: None,
            bg: None,
            style: cell_style(),
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
