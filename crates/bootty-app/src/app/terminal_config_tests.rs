use std::{path::PathBuf, sync::mpsc};

use bootty_config::{
    color::Color,
    config::{
        AppearanceVariant, BoottyConfig, ColorConfig, CursorConfig, CursorStyleConfig, FontConfig,
        MacosOptionAsAltConfig, SessionConfig, default_working_directory,
    },
};
use bootty_font::FontFeature;
use bootty_render::terminal_text::TerminalTextConfig;
use bootty_runtime::terminal_session::SessionLaunchConfig;
use bootty_terminal::{
    terminal_engine::{
        NATIVE_MAX_SCROLLBACK, TERMINAL_TERM, TerminalColorConfig, TerminalCursorConfig,
        TerminalCursorStyle, TerminalFeatureConfig, TerminalSideEffect, TerminalSideEffectEvent,
    },
    terminal_input_model::MacosOptionAsAlt,
};

use super::{
    terminal_color_config, terminal_cursor_config, terminal_feature_config, terminal_live_config,
    terminal_macos_option_as_alt, terminal_rgb_color, terminal_session_config_with_side_effects,
    terminal_text_config,
};

fn color(red: u8, green: u8, blue: u8) -> Color {
    Color {
        r: red,
        g: green,
        b: blue,
        a: u8::MAX,
    }
}

#[test]
fn terminal_session_config_maps_the_complete_runtime_contract() {
    let (side_effect_tx, side_effect_rx) = mpsc::channel();
    let working_directory = PathBuf::from("tmp/bootty-project");
    let background = color(0x11, 0x22, 0x33);
    let foreground = color(0xd4, 0xe5, 0xf6);
    let cursor = color(0xab, 0xcd, 0xef);
    let mut config = BoottyConfig {
        session: SessionConfig {
            shell: Some("/bin/zsh".to_owned()),
            working_directory: Some(working_directory.clone()),
            env: vec![("BOOTTY_TEST".to_owned(), "enabled".to_owned())],
            term: "xterm-test".to_owned(),
            colorterm: "truecolor-test".to_owned(),
            max_scrollback: 42,
            glyph_protocol: false,
        },
        ..BoottyConfig::default()
    };
    config.input.macos_option_as_alt = MacosOptionAsAltConfig::Right;
    config.cursor.style = Some(CursorStyleConfig::Underline);
    config.cursor.blink = Some(true);
    config.appearance.light.colors.background = Some(background);
    config.appearance.light.colors.foreground = Some(foreground);
    config.appearance.light.colors.cursor = Some(cursor);

    let mapped = terminal_session_config_with_side_effects(
        &config,
        AppearanceVariant::Light,
        &side_effect_tx,
    );

    assert_eq!(
        mapped.launch,
        SessionLaunchConfig {
            shell: Some("/bin/zsh".to_owned()),
            args: Vec::new(),
            working_directory: Some(working_directory),
            env: vec![("BOOTTY_TEST".to_owned(), "enabled".to_owned())],
            env_remove: Vec::new(),
            term: "xterm-test".to_owned(),
            colorterm: "truecolor-test".to_owned(),
        }
    );
    assert_eq!(mapped.colors.background, terminal_rgb_color(background));
    assert_eq!(mapped.colors.foreground, terminal_rgb_color(foreground));
    assert_eq!(mapped.colors.cursor, Some(terminal_rgb_color(cursor)));
    assert_eq!(
        mapped.cursor,
        TerminalCursorConfig {
            style: Some(TerminalCursorStyle::Underline),
            blink: Some(true),
        }
    );
    assert_eq!(
        mapped.features,
        TerminalFeatureConfig {
            glyph_protocol: false,
        }
    );
    assert_eq!(mapped.max_scrollback, 42);
    assert_eq!(mapped.macos_option_as_alt, MacosOptionAsAlt::Right);
    assert!(mapped.side_effect_tx.is_some());
    assert_eq!(mapped.side_effect_pane_id, None);
    assert!(mapped.benchmark_trace.is_none());

    mapped
        .side_effect_tx
        .as_ref()
        .expect("side-effect sender is configured")
        .send(TerminalSideEffectEvent::unscoped(TerminalSideEffect::Bell))
        .unwrap();
    assert_eq!(
        side_effect_rx.recv().unwrap(),
        TerminalSideEffectEvent::unscoped(TerminalSideEffect::Bell)
    );

    config.session.working_directory = None;
    let fallback = terminal_session_config_with_side_effects(
        &config,
        AppearanceVariant::Light,
        &side_effect_tx,
    );
    assert_eq!(
        fallback.launch.working_directory,
        default_working_directory()
    );
}

#[test]
fn terminal_live_config_is_one_complete_engine_policy() {
    let mut config = BoottyConfig::default();
    config.appearance.dark.colors.background = Some(color(1, 2, 3));
    config.cursor.style = Some(CursorStyleConfig::HollowBlock);
    config.cursor.blink = Some(false);
    config.session.glyph_protocol = false;

    let live = terminal_live_config(&config, AppearanceVariant::Dark);

    assert_eq!(live.colors.background, terminal_rgb_color(color(1, 2, 3)));
    assert_eq!(live.cursor.style, Some(TerminalCursorStyle::HollowBlock));
    assert_eq!(live.cursor.blink, Some(false));
    assert!(!live.features.glyph_protocol);
}

#[test]
fn terminal_enum_realization_maps_every_config_value() {
    for (config, terminal) in [
        (CursorStyleConfig::Bar, TerminalCursorStyle::Bar),
        (CursorStyleConfig::Block, TerminalCursorStyle::Block),
        (CursorStyleConfig::Underline, TerminalCursorStyle::Underline),
        (
            CursorStyleConfig::HollowBlock,
            TerminalCursorStyle::HollowBlock,
        ),
    ] {
        assert_eq!(
            terminal_cursor_config(&CursorConfig {
                style: Some(config),
                blink: None,
            })
            .style,
            Some(terminal)
        );
    }

    for (config, terminal) in [
        (MacosOptionAsAltConfig::None, MacosOptionAsAlt::None),
        (MacosOptionAsAltConfig::Left, MacosOptionAsAlt::Left),
        (MacosOptionAsAltConfig::Right, MacosOptionAsAlt::Right),
        (MacosOptionAsAltConfig::Both, MacosOptionAsAlt::Both),
    ] {
        assert_eq!(terminal_macos_option_as_alt(config), terminal);
    }

    let session = SessionConfig {
        glyph_protocol: false,
        ..SessionConfig::default()
    };
    assert!(!terminal_feature_config(&session).glyph_protocol);
}

#[test]
fn terminal_color_realization_preserves_every_slot_and_palette_rule() {
    let default_palette = TerminalColorConfig::default().palette;
    assert_eq!(
        terminal_color_config(&ColorConfig::default()).palette,
        default_palette
    );

    let palette = (0..300)
        .map(|index| color(index as u8, (index / 2) as u8, (index / 3) as u8))
        .collect::<Vec<_>>();
    let config = ColorConfig {
        background: Some(color(1, 2, 3)),
        foreground: Some(color(4, 5, 6)),
        cursor: Some(color(7, 8, 9)),
        cursor_text: Some(color(10, 11, 12)),
        pointer_foreground: Some(color(13, 14, 15)),
        pointer_background: Some(color(16, 17, 18)),
        tektronix_foreground: Some(color(19, 20, 21)),
        tektronix_background: Some(color(22, 23, 24)),
        highlight_background: Some(color(25, 26, 27)),
        tektronix_cursor: Some(color(28, 29, 30)),
        highlight_foreground: Some(color(31, 32, 33)),
        selection_background: Some(color(34, 35, 36)),
        selection_foreground: Some(color(37, 38, 39)),
        palette: palette.clone(),
        palette_generate: true,
        palette_harmonious: true,
    };
    let terminal = terminal_color_config(&config);

    assert_eq!(terminal.background, terminal_rgb_color(color(1, 2, 3)));
    assert_eq!(terminal.foreground, terminal_rgb_color(color(4, 5, 6)));
    assert_eq!(terminal.cursor, Some(terminal_rgb_color(color(7, 8, 9))));
    assert_eq!(
        terminal.cursor_text,
        Some(terminal_rgb_color(color(10, 11, 12)))
    );
    assert_eq!(
        terminal.pointer_foreground,
        Some(terminal_rgb_color(color(13, 14, 15)))
    );
    assert_eq!(
        terminal.pointer_background,
        Some(terminal_rgb_color(color(16, 17, 18)))
    );
    assert_eq!(
        terminal.tektronix_foreground,
        Some(terminal_rgb_color(color(19, 20, 21)))
    );
    assert_eq!(
        terminal.tektronix_background,
        Some(terminal_rgb_color(color(22, 23, 24)))
    );
    assert_eq!(
        terminal.highlight_background,
        Some(terminal_rgb_color(color(25, 26, 27)))
    );
    assert_eq!(
        terminal.tektronix_cursor,
        Some(terminal_rgb_color(color(28, 29, 30)))
    );
    assert_eq!(
        terminal.highlight_foreground,
        Some(terminal_rgb_color(color(31, 32, 33)))
    );
    assert_eq!(
        terminal.selection_background,
        Some(terminal_rgb_color(color(34, 35, 36)))
    );
    assert_eq!(
        terminal.selection_foreground,
        Some(terminal_rgb_color(color(37, 38, 39)))
    );
    assert_eq!(terminal.palette.len(), 256);
    assert!(
        terminal
            .palette
            .iter()
            .zip(&palette)
            .all(|(terminal, configured)| *terminal == terminal_rgb_color(*configured))
    );
    assert!(terminal.palette_generate);
    assert!(terminal.palette_harmonious);
}

#[test]
fn terminal_color_realization_drops_only_alpha() {
    let transparent = Color {
        r: 0x12,
        g: 0x34,
        b: 0x56,
        a: 0x78,
    };
    assert_eq!(
        terminal_rgb_color(transparent),
        terminal_rgb_color(color(0x12, 0x34, 0x56))
    );
}

#[test]
fn terminal_text_realization_maps_every_product_font_value() {
    let config = FontConfig {
        family: vec!["Berkeley Mono".to_owned(), "monospace".to_owned()],
        ui_family: vec!["Inter".to_owned()],
        ui_use_terminal_family: false,
        features: vec![FontFeature::new(*b"liga", 0), FontFeature::new(*b"ss05", 1)],
        size: 18.0,
        cell_width: Some(10.0),
        cell_height: Some(22.0),
        fit_cell_height: false,
        fit_cell_width: true,
        baseline_adjustment: 4.0,
        underline_position: 3.0,
        underline_thickness: 2.0,
    };

    let terminal = terminal_text_config(&config);

    assert_eq!(terminal.families, config.family);
    assert_eq!(terminal.font_features, config.features);
    assert_eq!(terminal.font_size, config.size);
    assert_eq!(terminal.cell_width, config.cell_width);
    assert_eq!(terminal.cell_height, config.cell_height);
    assert_eq!(terminal.fit_cell_height, config.fit_cell_height);
    assert_eq!(terminal.fit_cell_width, config.fit_cell_width);
    assert_eq!(terminal.baseline_adjustment, config.baseline_adjustment);
    assert_eq!(terminal.underline_position, config.underline_position);
    assert_eq!(terminal.underline_thickness, config.underline_thickness);
    assert!(terminal.codepoint_overrides.is_empty());
}

#[test]
fn bootty_product_font_defaults_match_the_renderer_fallback_contract() {
    let product = terminal_text_config(&FontConfig::default());
    let fallback = TerminalTextConfig::default();

    assert_eq!(product.families, fallback.families);
    assert_eq!(product.font_features, fallback.font_features);
    assert_eq!(product.font_size, fallback.font_size);
    assert_eq!(product.cell_width, fallback.cell_width);
    assert_eq!(product.cell_height, fallback.cell_height);
    assert_eq!(product.fit_cell_height, fallback.fit_cell_height);
    assert_eq!(product.fit_cell_width, fallback.fit_cell_width);
    assert_eq!(product.baseline_adjustment, fallback.baseline_adjustment);
    assert_eq!(product.underline_position, fallback.underline_position);
    assert_eq!(product.underline_thickness, fallback.underline_thickness);
    assert!(product.codepoint_overrides.is_empty());
}

#[test]
fn bootty_product_defaults_match_the_terminal_fallback_contract() {
    let config = BoottyConfig::default();

    assert_eq!(config.session.term, TERMINAL_TERM);
    assert_eq!(config.session.colorterm, "truecolor");
    assert_eq!(config.session.max_scrollback, NATIVE_MAX_SCROLLBACK);
    assert_eq!(
        terminal_feature_config(&config.session),
        TerminalFeatureConfig::default()
    );
    assert_eq!(
        terminal_macos_option_as_alt(config.input.macos_option_as_alt),
        MacosOptionAsAlt::default()
    );
}
