use anyhow::{Context, Result};
use proptest::prelude::*;
use unicode_width::UnicodeWidthChar;

use super::super::*;

fn occupied_cells(frame: &RenderFrame) -> Vec<(char, u16, u16)> {
    frame
        .cells
        .iter()
        .filter(|cell| cell.text_len > 0)
        .map(|cell| (frame.text[cell.text_start], cell.x, cell.y))
        .collect()
}

fn visible_text_rows(frame: &RenderFrame) -> Vec<String> {
    let mut rows = vec![String::new(); frame.rows as usize];
    for cell in frame.cells.iter().filter(|cell| cell.text_len > 0) {
        let row = &mut rows[cell.y as usize];
        while text_cell_width(row.chars()) < cell.x {
            row.push(' ');
        }
        row.extend(frame.cell_text(cell));
    }
    while rows.last().is_some_and(|row| row.is_empty()) {
        rows.pop();
    }
    rows
}

fn text_cell_width(chars: impl Iterator<Item = char>) -> u16 {
    chars
        .map(|ch| UnicodeWidthChar::width(ch).unwrap_or(0) as u16)
        .sum()
}

fn test_geometry(cols: u16, rows: u16) -> TerminalGeometry {
    TerminalGeometry {
        cols,
        rows,
        cell_width: 8,
        cell_height: 16,
    }
}

fn assert_visible_text_rows(frame: &RenderFrame, expected: &[&str]) {
    let actual = visible_text_rows(frame);
    assert_eq!(actual.len(), expected.len(), "visible row count mismatch");
    for (actual, expected) in actual.iter().zip(expected) {
        assert_eq!(actual, expected);
    }
}

fn collect_visible_cells<T, F>(frame: &RenderFrame, map: F) -> Vec<T>
where
    F: Fn(char, &RenderCell) -> T,
{
    frame
        .cells
        .iter()
        .filter(|cell| cell.text_len > 0)
        .map(|cell| map(frame.text[cell.text_start], cell))
        .collect()
}

fn cursor_position(frame: &RenderFrame) -> Option<(u16, u16)> {
    frame.cursor.as_ref().map(|cursor| (cursor.x, cursor.y))
}

fn assert_cursor_position(frame: &RenderFrame, expected: (u16, u16)) {
    assert_eq!(cursor_position(frame), Some(expected));
}

#[test]
fn terminal_engine_applies_configured_default_colors_to_frame() -> Result<()> {
    let mut engine = TerminalEngine::new_with_colors(
        test_geometry(8, 2),
        TerminalColorConfig {
            background: RgbColor {
                r: 0x10,
                g: 0x11,
                b: 0x12,
            },
            foreground: RgbColor {
                r: 0x20,
                g: 0x21,
                b: 0x22,
            },
            cursor: Some(RgbColor {
                r: 0x30,
                g: 0x31,
                b: 0x32,
            }),
            cursor_text: Some(RgbColor {
                r: 0x40,
                g: 0x41,
                b: 0x42,
            }),
            selection_background: Some(RgbColor {
                r: 0x50,
                g: 0x51,
                b: 0x52,
            }),
            selection_foreground: Some(RgbColor {
                r: 0x60,
                g: 0x61,
                b: 0x62,
            }),
            palette: vec![RgbColor { r: 0, g: 1, b: 2 }, RgbColor { r: 3, g: 4, b: 5 }],
        },
    )?;

    let frame = engine.extract_frame()?;

    assert_eq!(
        frame.colors.background,
        RgbColor {
            r: 0x10,
            g: 0x11,
            b: 0x12
        }
    );
    assert_eq!(
        frame.colors.foreground,
        RgbColor {
            r: 0x20,
            g: 0x21,
            b: 0x22
        }
    );
    assert_eq!(
        frame.colors.cursor,
        Some(RgbColor {
            r: 0x30,
            g: 0x31,
            b: 0x32
        })
    );
    assert_eq!(
        frame.colors.cursor_text,
        Some(RgbColor {
            r: 0x40,
            g: 0x41,
            b: 0x42
        })
    );
    assert_eq!(
        frame.colors.selection_background,
        Some(RgbColor {
            r: 0x50,
            g: 0x51,
            b: 0x52
        })
    );
    assert_eq!(
        frame.colors.selection_foreground,
        Some(RgbColor {
            r: 0x60,
            g: 0x61,
            b: 0x62
        })
    );
    assert_eq!(
        engine.default_color_palette()?[0],
        RgbColor { r: 0, g: 1, b: 2 }
    );
    assert_eq!(
        engine.default_color_palette()?[1],
        RgbColor { r: 3, g: 4, b: 5 }
    );
    Ok(())
}

#[test]
fn terminal_engine_updates_default_colors_live() -> Result<()> {
    let mut engine = TerminalEngine::new(test_geometry(8, 2))?;

    engine.set_colors(TerminalColorConfig {
        background: RgbColor { r: 1, g: 2, b: 3 },
        foreground: RgbColor { r: 4, g: 5, b: 6 },
        cursor: Some(RgbColor { r: 7, g: 8, b: 9 }),
        cursor_text: Some(RgbColor {
            r: 13,
            g: 14,
            b: 15,
        }),
        selection_background: Some(RgbColor {
            r: 16,
            g: 17,
            b: 18,
        }),
        selection_foreground: Some(RgbColor {
            r: 19,
            g: 20,
            b: 21,
        }),
        palette: vec![RgbColor {
            r: 10,
            g: 11,
            b: 12,
        }],
    })?;
    let frame = engine.extract_frame()?;

    assert_eq!(frame.colors.background, RgbColor { r: 1, g: 2, b: 3 });
    assert_eq!(frame.colors.foreground, RgbColor { r: 4, g: 5, b: 6 });
    assert_eq!(frame.colors.cursor, Some(RgbColor { r: 7, g: 8, b: 9 }));
    assert_eq!(
        frame.colors.cursor_text,
        Some(RgbColor {
            r: 13,
            g: 14,
            b: 15
        })
    );
    assert_eq!(
        engine.default_color_palette()?[0],
        RgbColor {
            r: 10,
            g: 11,
            b: 12
        }
    );
    Ok(())
}

fn assert_cursor_style(
    frame: &RenderFrame,
    expected_style: CursorVisualStyle,
    expected_blinking: bool,
) {
    let cursor = frame.cursor.as_ref().expect("cursor should be visible");
    assert_eq!(cursor.style, expected_style);
    assert_eq!(cursor.blinking, expected_blinking);
}

proptest! {
    #[test]
    fn property_terminal_absolute_cursor_position_clamps_to_viewport(
        cols in 1u16..80,
        rows in 1u16..40,
        row_param in 0u16..200,
        col_param in 0u16..200,
    ) {
        let mut engine = TerminalEngine::new(test_geometry(cols, rows)).expect("terminal engine");

        engine.write_vt(format!("\x1b[{row_param};{col_param}HX").as_bytes());
        let frame = engine.extract_frame().expect("render frame");

        let expected_x = col_param.saturating_sub(1).min(cols - 1);
        let expected_y = row_param.saturating_sub(1).min(rows - 1);
        let expected_cursor_x = expected_x.saturating_add(1).min(cols - 1);
        prop_assert_eq!(occupied_cells(frame), vec![('X', expected_x, expected_y)]);
        prop_assert_eq!(cursor_position(frame), Some((expected_cursor_x, expected_y)));
    }

    #[test]
    fn property_terminal_relative_cursor_motion_clamps_to_viewport(
        cols in 1u16..80,
        rows in 1u16..40,
        raw_start_x in 0u16..80,
        raw_start_y in 0u16..40,
        delta in 0u16..200,
        direction in 0u8..4,
    ) {
        let start_x = raw_start_x.min(cols - 1);
        let start_y = raw_start_y.min(rows - 1);
        let command = match direction {
            0 => 'A',
            1 => 'B',
            2 => 'C',
            _ => 'D',
        };
        let mut engine = TerminalEngine::new(test_geometry(cols, rows)).expect("terminal engine");

        engine.write_vt(
            format!("\x1b[{};{}H\x1b[{delta}{command}X", start_y + 1, start_x + 1).as_bytes(),
        );
        let frame = engine.extract_frame().expect("render frame");

        let effective_delta = delta.max(1);
        let expected_x = match command {
            'C' => start_x.saturating_add(effective_delta).min(cols - 1),
            'D' => start_x.saturating_sub(effective_delta),
            _ => start_x,
        };
        let expected_y = match command {
            'A' => start_y.saturating_sub(effective_delta),
            'B' => start_y.saturating_add(effective_delta).min(rows - 1),
            _ => start_y,
        };
        let expected_cursor_x = expected_x.saturating_add(1).min(cols - 1);
        prop_assert_eq!(occupied_cells(frame), vec![('X', expected_x, expected_y)]);
        prop_assert_eq!(cursor_position(frame), Some((expected_cursor_x, expected_y)));
    }

    #[test]
    fn terminal_default_tab_movement_lands_on_next_tabstop_or_right_edge(
        cols in 1u16..80,
        raw_start_x in 0u16..80,
    ) {
        let start_x = raw_start_x.min(cols - 1);
        let mut engine = TerminalEngine::new(test_geometry(cols, 2)).expect("terminal engine");

        engine.write_vt(format!("\x1b[{}G\tX", start_x + 1).as_bytes());
        let frame = engine.extract_frame().expect("render frame");

        let expected_x = (((start_x / 8) + 1) * 8).min(cols - 1);
        let expected_cursor_x = expected_x.saturating_add(1).min(cols - 1);
        prop_assert_eq!(occupied_cells(frame), vec![('X', expected_x, 0)]);
        prop_assert_eq!(cursor_position(frame), Some((expected_cursor_x, 0)));
    }
}

#[test]
fn terminal_engine_supports_default_tabstop_interval() -> Result<()> {
    let mut engine = TerminalEngine::new(test_geometry(16, 2))?;

    engine.write_vt(b"A\tB\tC");
    let frame = engine.extract_frame()?;

    assert_eq!(
        occupied_cells(frame),
        [('A', 0, 0), ('B', 8, 0), ('C', 15, 0)]
    );
    Ok(())
}

#[test]
fn terminal_engine_supports_tabstop_set_after_clear() -> Result<()> {
    let mut engine = TerminalEngine::new(test_geometry(16, 2))?;

    engine.write_vt(b"\x1b[3g\x1b[5G\x1bH\rA\tB");
    let frame = engine.extract_frame()?;

    assert_eq!(occupied_cells(frame), [('A', 0, 0), ('B', 4, 0)]);
    Ok(())
}

#[test]
fn terminal_engine_supports_default_tabstop_count_on_80_columns() -> Result<()> {
    let mut actual_positions = Vec::new();

    for start_x in 0..80 {
        let mut engine = TerminalEngine::new(test_geometry(80, 1))?;
        engine.write_vt(format!("\x1b[{}G\tX", start_x + 1).as_bytes());
        let frame = engine.extract_frame()?;
        actual_positions.push(occupied_cells(frame)[0].1);
    }

    let unique_positions = actual_positions
        .iter()
        .copied()
        .collect::<std::collections::BTreeSet<_>>();

    assert_eq!(
        unique_positions,
        [8, 16, 24, 32, 40, 48, 56, 64, 72, 79].into()
    );
    assert_eq!(actual_positions[0], 8);
    assert_eq!(actual_positions[71], 72);
    assert_eq!(actual_positions[72], 79);
    Ok(())
}

#[test]
fn terminal_engine_supports_clear_all_tabstops_to_right_edge() -> Result<()> {
    let mut engine = TerminalEngine::new(test_geometry(16, 1))?;

    engine.write_vt(b"\x1b[3gA\tB");
    let frame = engine.extract_frame()?;

    assert_eq!(occupied_cells(frame), [('A', 0, 0), ('B', 15, 0)]);
    Ok(())
}

#[test]
fn terminal_engine_supports_large_column_custom_tabstop() -> Result<()> {
    let mut engine = TerminalEngine::new(test_geometry(600, 1))?;

    engine.write_vt(b"\x1b[3g\x1b[519G\x1bH\x1b[1GA\tB");
    let frame = engine.extract_frame()?;

    assert_eq!(occupied_cells(frame), [('A', 0, 0), ('B', 518, 0)]);
    Ok(())
}

#[test]
fn terminal_engine_decodes_ascii_bytes() -> Result<()> {
    let mut engine = TerminalEngine::new(test_geometry(16, 1))?;

    engine.write_vt(b"Hello, World!");
    let frame = engine.extract_frame()?;

    assert_visible_text_rows(frame, &["Hello, World!"]);
    Ok(())
}

#[test]
fn terminal_engine_decodes_well_formed_utf8_bytes() -> Result<()> {
    let mut engine = TerminalEngine::new(test_geometry(16, 1))?;

    engine.write_vt("😄✤ÁA".as_bytes());
    let frame = engine.extract_frame()?;

    assert_eq!(
        occupied_cells(frame),
        [('😄', 0, 0), ('✤', 2, 0), ('Á', 3, 0), ('A', 4, 0)]
    );
    Ok(())
}

#[test]
fn terminal_engine_replaces_partially_invalid_utf8_bytes() -> Result<()> {
    let mut engine = TerminalEngine::new(test_geometry(16, 1))?;

    engine.write_vt(b"\xF0\x9F");
    engine.write_vt("😄".as_bytes());
    engine.write_vt(b"\xED\xA0\x80");
    let frame = engine.extract_frame()?;

    assert_eq!(
        occupied_cells(frame),
        [
            ('\u{FFFD}', 0, 0),
            ('😄', 1, 0),
            ('\u{FFFD}', 3, 0),
            ('\u{FFFD}', 4, 0),
            ('\u{FFFD}', 5, 0),
        ]
    );
    Ok(())
}

#[test]
fn terminal_engine_supports_charset_table_mappings() -> Result<()> {
    let mut engine = TerminalEngine::new(test_geometry(16, 1))?;

    engine.write_vt("\x1b(A#\x1b(B#\x1b(0`qx😄".as_bytes());
    let frame = engine.extract_frame()?;

    assert_visible_text_rows(frame, &["£#◆─│ "]);
    Ok(())
}

#[test]
fn terminal_engine_supports_charset_gl_invocation() -> Result<()> {
    let mut engine = TerminalEngine::new(test_geometry(16, 1))?;

    engine.write_vt(b"`\x1b)0\x0e``\x0f`\x1b*0\x1bN``");
    let frame = engine.extract_frame()?;

    assert_visible_text_rows(frame, &["`◆◆`◆`"]);
    Ok(())
}

#[test]
fn terminal_engine_supports_kitty_color_protocol_specials() -> Result<()> {
    let mut engine = TerminalEngine::new(test_geometry(8, 1))?;

    engine.write_vt(
        b"\x1b]21;foreground=rgb:12/34/56;background=rgb:78/9a/bc;cursor=rgb:de/f0/12\x1b\\X",
    );
    let frame = engine.extract_frame()?;

    assert_eq!(
        frame.colors.foreground,
        RgbColor {
            r: 0x12,
            g: 0x34,
            b: 0x56,
        }
    );
    assert_eq!(
        frame.colors.background,
        RgbColor {
            r: 0x78,
            g: 0x9a,
            b: 0xbc,
        }
    );
    assert_eq!(
        frame.colors.cursor,
        Some(RgbColor {
            r: 0xde,
            g: 0xf0,
            b: 0x12,
        })
    );
    Ok(())
}

#[test]
fn terminal_engine_supports_kitty_color_protocol_palette_set_and_reset() -> Result<()> {
    let mut engine = TerminalEngine::new(test_geometry(8, 1))?;

    let magenta = RgbColor {
        r: 0xff,
        g: 0x00,
        b: 0xff,
    };
    engine.write_vt(b"\x1b]21;5=rgb:ff/00/ff\x1b\\");
    assert_eq!(engine.terminal.color_palette()?[5], magenta);

    engine.write_vt(b"\x1b[35mM");
    assert_eq!(
        engine.terminal.cursor_style()?.fg_color,
        libghostty_vt::style::StyleColor::Palette(libghostty_vt::style::PaletteIndex(5))
    );
    engine.write_vt(b"M");
    let frame = engine.extract_frame()?;
    let colored_cells = collect_visible_cells(frame, |text, cell| (text, cell.fg));

    assert_eq!(colored_cells.len(), 2);
    assert_eq!(
        colored_cells[0],
        ('M', Some(magenta)),
        "expected kitty OSC 21 numeric palette key to update SGR palette color"
    );

    engine.write_vt(b"\x1b]21;5=\x1b\\D");
    assert_ne!(engine.terminal.color_palette()?[5], magenta);
    let frame = engine.extract_frame()?;
    let reset_cells = collect_visible_cells(frame, |text, cell| (text, cell.fg));
    let reset_cell = reset_cells
        .iter()
        .find(|(text, _)| *text == 'D')
        .context("reset marker cell should be visible")?;
    assert_ne!(reset_cell.1, Some(magenta));
    Ok(())
}

#[test]
fn terminal_engine_supports_generated_256_color_palette() -> Result<()> {
    let mut engine = TerminalEngine::new(test_geometry(8, 1))?;
    let base = engine.terminal.default_color_palette()?;
    let generated = crate::terminal_palette::generate_256_palette(
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
    engine.terminal.set_default_color_palette(Some(generated))?;
    engine.write_vt(b"\x1b[38;5;16mB\x1b[38;5;231mW");

    let frame = engine.extract_frame()?;
    let colored_cells = collect_visible_cells(frame, |text, cell| (text, cell.fg));

    assert_eq!(
        colored_cells,
        [
            ('B', Some(RgbColor { r: 0, g: 0, b: 0 })),
            (
                'W',
                Some(RgbColor {
                    r: 255,
                    g: 255,
                    b: 255,
                })
            ),
        ]
    );
    Ok(())
}

#[test]
fn terminal_engine_supports_osc_color_operations() -> Result<()> {
    let mut engine = TerminalEngine::new(test_geometry(12, 1))?;

    let red = RgbColor {
        r: 0xff,
        g: 0x00,
        b: 0x00,
    };
    let green = RgbColor {
        r: 0x00,
        g: 0xff,
        b: 0x00,
    };
    let blue = RgbColor {
        r: 0x00,
        g: 0x00,
        b: 0xff,
    };

    let default_palette = engine.terminal.default_color_palette()?;
    assert_ne!(default_palette[42], red);

    engine.write_vt(b"\x1b]4;42;rgb:ff/00/00;43;rgb:00/ff/00\x1b\\");
    assert_eq!(engine.terminal.color_palette()?[42], red);
    assert_eq!(engine.terminal.color_palette()?[43], green);

    engine.write_vt(b"\x1b[38;5;42mR\x1b[38;5;43mG");
    let frame = engine.extract_frame()?;
    let colored_cells = collect_visible_cells(frame, |text, cell| (text, cell.fg));
    assert!(colored_cells.contains(&('R', Some(red))));
    assert!(colored_cells.contains(&('G', Some(green))));

    engine.write_vt(b"\x1b]104;42;;43\x1b\\");
    assert_eq!(engine.terminal.color_palette()?[42], default_palette[42]);
    assert_eq!(engine.terminal.color_palette()?[43], default_palette[43]);

    engine.write_vt(b"\x1b]10;rgb:ff/00/00;rgb:00/00/ff\x1b\\");
    let frame = engine.extract_frame()?;
    assert_eq!(frame.colors.foreground, red);
    assert_eq!(frame.colors.background, blue);

    engine.write_vt(b"\x1b]12;rgb:00/ff/00\x1b\\");
    let frame = engine.extract_frame()?;
    assert_eq!(frame.colors.cursor, Some(green));

    engine.write_vt(b"\x1b]110\x1b\\\x1b]111\x1b\\\x1b]112\x1b\\");
    let frame = engine.extract_frame()?;
    assert_ne!(frame.colors.foreground, red);
    assert_ne!(frame.colors.background, blue);
    assert_ne!(frame.colors.cursor, Some(green));

    Ok(())
}

#[test]
fn terminal_engine_supports_x11_color_names_in_color_operations() -> Result<()> {
    let mut engine = TerminalEngine::new(test_geometry(12, 1))?;

    let white = RgbColor {
        r: 255,
        g: 255,
        b: 255,
    };
    let red = RgbColor { r: 255, g: 0, b: 0 };
    let green = RgbColor { r: 0, g: 255, b: 0 };
    let blue = RgbColor { r: 0, g: 0, b: 255 };
    let forest_green = RgbColor {
        r: 34,
        g: 139,
        b: 34,
    };
    let medium_spring_green = RgbColor {
        r: 0,
        g: 250,
        b: 154,
    };
    let lawn_green = RgbColor {
        r: 124,
        g: 252,
        b: 0,
    };
    let black = RgbColor { r: 0, g: 0, b: 0 };

    let default_palette = engine.terminal.default_color_palette()?;
    engine.write_vt(
        b"\x1b]4;1;red;2;green;4;blue;7;white;42;FoReStGReen;43;mediumspringgreen;44;black\x1b\\",
    );
    let palette = engine.terminal.color_palette()?;
    assert_eq!(palette[1], red);
    assert_eq!(palette[2], green);
    assert_eq!(palette[4], blue);
    assert_eq!(palette[7], white);
    assert_eq!(palette[42], forest_green);
    assert_eq!(palette[43], medium_spring_green);
    assert_eq!(palette[44], black);

    engine.write_vt(
        b"\x1b]4;45;rgbi:1.0/0/0;46;rgb:7f/a0a0/0;47;rgb:f/ff/fff;48;#fff;49;#fffffffff;50;#ffffffffffff;51;#ff0010\x1b\\",
    );
    let palette = engine.terminal.color_palette()?;
    assert_eq!(palette[45], red);
    assert_eq!(
        palette[46],
        RgbColor {
            r: 127,
            g: 160,
            b: 0,
        }
    );
    assert_eq!(palette[47], white);
    assert_eq!(palette[48], white);
    assert_eq!(palette[49], white);
    assert_eq!(palette[50], white);
    assert_eq!(
        palette[51],
        RgbColor {
            r: 255,
            g: 0,
            b: 16,
        }
    );

    engine.write_vt(b"\x1b[38;5;42mF\x1b[38;5;43mM\x1b[38;5;44mK");
    let frame = engine.extract_frame()?;
    let colored_cells = collect_visible_cells(frame, |text, cell| (text, cell.fg));
    assert!(colored_cells.contains(&('F', Some(forest_green))));
    assert!(colored_cells.contains(&('M', Some(medium_spring_green))));
    assert!(colored_cells.contains(&('K', Some(black))));

    engine.write_vt(b"\x1b]10;medium spring green;ForestGreen\x1b\\\x1b]12;lawngreen\x1b\\");
    let frame = engine.extract_frame()?;
    assert_eq!(frame.colors.foreground, medium_spring_green);
    assert_eq!(frame.colors.background, forest_green);
    assert_eq!(frame.colors.cursor, Some(lawn_green));

    engine.write_vt(b"\x1b]21;foreground= Forest Green ;background=LawnGreen;cursor=white\x1b\\");
    let frame = engine.extract_frame()?;
    assert_eq!(frame.colors.foreground, forest_green);
    assert_eq!(frame.colors.background, lawn_green);
    assert_eq!(frame.colors.cursor, Some(white));

    engine.write_vt(b"\x1b]4;42;nosuchcolor\x1b\\");
    assert_eq!(engine.terminal.color_palette()?[42], forest_green);
    engine.write_vt(b"\x1b]4;51;rgb:not/hex/zz\x1b\\");
    assert_eq!(
        engine.terminal.color_palette()?[51],
        RgbColor {
            r: 255,
            g: 0,
            b: 16,
        }
    );

    engine.write_vt(b"\x1b]104;42;43;44;45;46;47;48;49;50;51\x1b\\");
    let palette = engine.terminal.color_palette()?;
    for index in 42..=51 {
        assert_eq!(palette[index], default_palette[index]);
    }

    Ok(())
}

#[test]
fn terminal_engine_supports_kitty_keyboard_flag_stack() -> Result<()> {
    let mut engine = TerminalEngine::new(test_geometry(8, 1))?;

    assert_eq!(
        engine.terminal.kitty_keyboard_flags()?,
        key::KittyKeyFlags::DISABLED
    );

    for (command, expected) in [
        (b"\x1b[>1u".as_ref(), key::KittyKeyFlags::DISAMBIGUATE),
        (
            b"\x1b[=2;2u".as_ref(),
            key::KittyKeyFlags::DISAMBIGUATE | key::KittyKeyFlags::REPORT_EVENTS,
        ),
        (b"\x1b[=2;3u".as_ref(), key::KittyKeyFlags::DISAMBIGUATE),
        (b"\x1b[>2u".as_ref(), key::KittyKeyFlags::REPORT_EVENTS),
        (b"\x1b[<u".as_ref(), key::KittyKeyFlags::DISAMBIGUATE),
        (b"\x1b[<100u".as_ref(), key::KittyKeyFlags::DISABLED),
    ] {
        engine.write_vt(command);
        assert_eq!(engine.terminal.kitty_keyboard_flags()?, expected);
    }
    Ok(())
}

#[test]
fn terminal_engine_supports_screen_style_state_and_reset() -> Result<()> {
    let mut engine = TerminalEngine::new(test_geometry(8, 2))?;

    engine.write_vt(b"\x1b[1mB\x1b[22mN\x1b[3mI\x1b[0mP");
    let frame = engine.extract_frame()?;
    let styled_cells = collect_visible_cells(frame, |text, cell| {
        (text, cell.style.bold, cell.style.italic)
    });

    assert_eq!(
        styled_cells,
        [
            ('B', true, false),
            ('N', false, false),
            ('I', false, true),
            ('P', false, false),
        ]
    );
    Ok(())
}

#[test]
fn terminal_engine_supports_screen_cursor_positioning() -> Result<()> {
    let mut engine = TerminalEngine::new(test_geometry(8, 4))?;

    engine.write_vt(b"A\x1b[2;3HB");
    let frame = engine.extract_frame()?;

    assert_eq!(occupied_cells(frame), [('A', 0, 0), ('B', 2, 1)]);
    assert_cursor_position(frame, (3, 1));
    Ok(())
}

#[test]
fn terminal_engine_supports_terminal_cursor_position_edges() -> Result<()> {
    let mut resets_wrap = TerminalEngine::new(test_geometry(5, 5))?;
    resets_wrap.write_vt(b"ABCDE\x1b[1;1HX");
    assert_visible_text_rows(resets_wrap.extract_frame()?, &["XBCDE"]);

    let mut off_screen = TerminalEngine::new(test_geometry(5, 5))?;
    off_screen.write_vt(b"\x1b[500;500HX");
    assert_visible_text_rows(off_screen.extract_frame()?, &["", "", "", "", "    X"]);

    let mut origin_mode = TerminalEngine::new(test_geometry(5, 5))?;
    origin_mode.write_vt(b"\x1b[3;4r\x1b[?6h\x1b[1;1HX");
    assert_visible_text_rows(origin_mode.extract_frame()?, &["", "", "X"]);

    let mut origin_mode_clamped = TerminalEngine::new(test_geometry(5, 5))?;
    origin_mode_clamped.write_vt(b"\x1b[3;4r\x1b[?6h\x1b[500;500HX");
    assert_visible_text_rows(origin_mode_clamped.extract_frame()?, &["", "", "", "    X"]);

    Ok(())
}

#[test]
fn terminal_engine_supports_terminal_cursor_line_and_axis_controls() -> Result<()> {
    let mut horizontal_absolute = TerminalEngine::new(test_geometry(5, 5))?;
    horizontal_absolute.write_vt(b"\x1b[3GX");
    assert_visible_text_rows(horizontal_absolute.extract_frame()?, &["  X"]);

    let mut vertical_absolute = TerminalEngine::new(test_geometry(5, 5))?;
    vertical_absolute.write_vt(b"\x1b[3dX");
    assert_visible_text_rows(vertical_absolute.extract_frame()?, &["", "", "X"]);

    let mut horizontal_relative = TerminalEngine::new(test_geometry(5, 5))?;
    horizontal_relative.write_vt(b"A\x1b[2aX");
    assert_visible_text_rows(horizontal_relative.extract_frame()?, &["A  X"]);

    let mut vertical_relative = TerminalEngine::new(test_geometry(5, 5))?;
    vertical_relative.write_vt(b"A\x1b[2eX");
    assert_visible_text_rows(vertical_relative.extract_frame()?, &["A", "", " X"]);

    let mut cursor_next_line = TerminalEngine::new(test_geometry(5, 5))?;
    cursor_next_line.write_vt(b"\x1b[3;4HB\x1b[EX");
    assert_visible_text_rows(cursor_next_line.extract_frame()?, &["", "", "   B", "X"]);

    let mut cursor_previous_line = TerminalEngine::new(test_geometry(5, 5))?;
    cursor_previous_line.write_vt(b"\x1b[3;4HB\x1b[FX");
    assert_visible_text_rows(cursor_previous_line.extract_frame()?, &["", "X", "   B"]);

    Ok(())
}

#[test]
fn terminal_engine_supports_terminal_margin_setting_controls() -> Result<()> {
    for (input, expected_rows) in [
        (
            b"ABC\r\nDEF\r\nGHI\x1b[2r\x1b[T".as_ref(),
            &["ABC", "", "DEF", "GHI"][..],
        ),
        (
            b"ABC\r\nDEF\r\nGHI\x1b[1;2r\x1b[T".as_ref(),
            &["", "ABC", "GHI"][..],
        ),
        (
            b"ABC\r\nDEF\r\nGHI\x1b[?69h\x1b[2s\x1b[1;2H\x1b[L".as_ref(),
            &["A", "DBC", "GEF", " HI"][..],
        ),
        (
            b"ABC\r\nDEF\r\nGHI\x1b[?69h\x1b[1;2s\x1b[1;2H\x1b[L".as_ref(),
            &["  C", "ABF", "DEI", "GH"][..],
        ),
        (
            b"ABC\r\nDEF\r\nGHI\x1b[1;2s\x1b[1;2H\x1b[L".as_ref(),
            &["", "ABC", "DEF", "GHI"][..],
        ),
    ] {
        let mut engine = TerminalEngine::new(test_geometry(5, 5))?;
        engine.write_vt(input);
        assert_visible_text_rows(engine.extract_frame()?, expected_rows);
    }

    Ok(())
}

#[test]
fn terminal_engine_supports_terminal_cursor_style_controls() -> Result<()> {
    let mut engine = TerminalEngine::new(test_geometry(5, 5))?;

    for (command, style, blinking) in [
        (b"\x1b[1 q".as_ref(), CursorVisualStyle::Block, true),
        (b"\x1b[2 q".as_ref(), CursorVisualStyle::Block, false),
        (b"\x1b[3 q".as_ref(), CursorVisualStyle::Underline, true),
        (b"\x1b[5 q".as_ref(), CursorVisualStyle::Bar, true),
        (b"\x1b[q".as_ref(), CursorVisualStyle::Bar, true),
    ] {
        engine.write_vt(command);
        let frame = engine.extract_frame()?;
        assert_cursor_style(frame, style, blinking);
    }

    Ok(())
}

#[test]
fn terminal_engine_supports_terminal_variation_selector_width() -> Result<()> {
    let mut vs15_pending_wrap = TerminalEngine::new(test_geometry(4, 5))?;
    vs15_pending_wrap.write_vt("\x1b[?2027h🍋☔\u{fe0e}".as_bytes());
    let frame = vs15_pending_wrap.extract_frame()?;
    assert_visible_text_rows(frame, &["🍋☔\u{fe0e}"]);
    assert_cursor_position(frame, (3, 0));

    let mut vs16_next_line = TerminalEngine::new(test_geometry(3, 5))?;
    vs16_next_line.write_vt("\x1b[?2027h#\x1b[3G#\u{fe0f}".as_bytes());
    let frame = vs16_next_line.extract_frame()?;
    assert_visible_text_rows(frame, &["#", "#\u{fe0f}"]);
    assert_cursor_position(frame, (2, 1));

    let mut vs16_pending_wrap = TerminalEngine::new(test_geometry(3, 5))?;
    vs16_pending_wrap.write_vt("\x1b[?2027h\x1b[2G#\u{fe0f}".as_bytes());
    let frame = vs16_pending_wrap.extract_frame()?;
    assert_visible_text_rows(frame, &[" #\u{fe0f}"]);
    assert_cursor_position(frame, (2, 0));

    Ok(())
}

#[test]
fn terminal_engine_supports_terminal_devanagari_grapheme_wrap() -> Result<()> {
    let mut engine = TerminalEngine::new(test_geometry(3, 5))?;

    engine.write_vt("\x1b[?2027h\x1b[3G\u{0915}\u{094d}\u{200d}\u{0937}".as_bytes());
    let frame = engine.extract_frame()?;

    assert_visible_text_rows(frame, &["", "क\u{094d}\u{200d}ष"]);
    assert_cursor_position(frame, (2, 1));

    Ok(())
}

#[test]
fn terminal_engine_supports_terminal_cursor_movement() -> Result<()> {
    let mut cursor_up = TerminalEngine::new(test_geometry(5, 5))?;
    cursor_up.write_vt(b"\x1b[3;1HA\x1b[10AX");
    assert_visible_text_rows(cursor_up.extract_frame()?, &[" X", "", "A"]);

    let mut cursor_down = TerminalEngine::new(test_geometry(5, 5))?;
    cursor_down.write_vt(b"A\x1b[10BX");
    assert_visible_text_rows(cursor_down.extract_frame()?, &["A", "", "", "", " X"]);

    let mut cursor_up_in_scroll_region = TerminalEngine::new(test_geometry(5, 5))?;
    cursor_up_in_scroll_region.write_vt(b"\x1b[2;4r\x1b[3;1HA\x1b[5AX");
    assert_visible_text_rows(
        cursor_up_in_scroll_region.extract_frame()?,
        &["", " X", "A"],
    );

    let mut cursor_down_in_scroll_region = TerminalEngine::new(test_geometry(5, 5))?;
    cursor_down_in_scroll_region.write_vt(b"\x1b[1;3rA\x1b[10BX");
    assert_visible_text_rows(
        cursor_down_in_scroll_region.extract_frame()?,
        &["A", "", " X"],
    );

    let mut cursor_left_resets_wrap = TerminalEngine::new(test_geometry(5, 5))?;
    cursor_left_resets_wrap.write_vt(b"ABCDE\x1b[DX");
    assert_visible_text_rows(cursor_left_resets_wrap.extract_frame()?, &["ABCXE"]);

    let mut cursor_right_resets_wrap = TerminalEngine::new(test_geometry(5, 5))?;
    cursor_right_resets_wrap.write_vt(b"ABCDE\x1b[CX");
    assert_visible_text_rows(cursor_right_resets_wrap.extract_frame()?, &["ABCDX"]);

    let mut cursor_right_to_edge = TerminalEngine::new(test_geometry(5, 5))?;
    cursor_right_to_edge.write_vt(b"\x1b[100CX");
    assert_visible_text_rows(cursor_right_to_edge.extract_frame()?, &["    X"]);

    Ok(())
}

#[test]
fn terminal_engine_supports_terminal_control_chars_and_tabs() -> Result<()> {
    let mut linefeed_and_cr = TerminalEngine::new(test_geometry(10, 5))?;
    linefeed_and_cr.write_vt(b"hello\r\nworld");
    let frame = linefeed_and_cr.extract_frame()?;
    assert_visible_text_rows(frame, &["hello", "world"]);
    assert_cursor_position(frame, (5, 1));

    let mut linefeed_resets_wrap = TerminalEngine::new(test_geometry(5, 5))?;
    linefeed_resets_wrap.write_vt(b"hello\nX");
    assert_visible_text_rows(linefeed_resets_wrap.extract_frame()?, &["hello", "    X"]);

    let mut linefeed_mode = TerminalEngine::new(test_geometry(10, 5))?;
    linefeed_mode.write_vt(b"\x1b[20h123456\nX");
    assert_visible_text_rows(linefeed_mode.extract_frame()?, &["123456", "X"]);

    let mut carriage_return_resets_wrap = TerminalEngine::new(test_geometry(5, 5))?;
    carriage_return_resets_wrap.write_vt(b"hello\rX");
    assert_visible_text_rows(carriage_return_resets_wrap.extract_frame()?, &["Xello"]);

    let mut backspace = TerminalEngine::new(test_geometry(10, 5))?;
    backspace.write_vt(b"hello\x08y");
    let frame = backspace.extract_frame()?;
    assert_visible_text_rows(frame, &["helly"]);
    assert_cursor_position(frame, (5, 0));

    let mut horizontal_tabs = TerminalEngine::new(test_geometry(20, 5))?;
    horizontal_tabs.write_vt(b"1\tA\tB\tC");
    assert_visible_text_rows(horizontal_tabs.extract_frame()?, &["1       A       B  C"]);

    let mut horizontal_tab_back = TerminalEngine::new(test_geometry(20, 5))?;
    horizontal_tab_back.write_vt(b"\x1b[20G\x1b[ZB\x1b[2ZC");
    assert_visible_text_rows(horizontal_tab_back.extract_frame()?, &["        C       B"]);

    Ok(())
}

#[test]
fn terminal_engine_supports_terminal_index_and_reverse_index() -> Result<()> {
    let mut index = TerminalEngine::new(test_geometry(5, 5))?;
    index.write_vt(b"\x1bDX");
    assert_visible_text_rows(index.extract_frame()?, &["", "X"]);

    let mut next_line = TerminalEngine::new(test_geometry(5, 5))?;
    next_line.write_vt(b"\x1bEX");
    assert_visible_text_rows(next_line.extract_frame()?, &["", "X"]);

    let mut index_bottom = TerminalEngine::new(test_geometry(5, 5))?;
    index_bottom.write_vt(b"\x1b[5;1HA\x1b[D\x1bDX");
    assert_visible_text_rows(index_bottom.extract_frame()?, &["", "", "", "A", "X"]);

    let mut reverse_top = TerminalEngine::new(test_geometry(5, 5))?;
    reverse_top.write_vt(b"A\x1b[2;1HB\x1b[3;1HC\x1b[1;1H\x1bMX");
    assert_visible_text_rows(reverse_top.extract_frame()?, &["X", "A", "B", "C"]);

    let mut reverse_not_top = TerminalEngine::new(test_geometry(5, 5))?;
    reverse_not_top.write_vt(b"A\x1b[2;1HB\x1b[3;1HC\x1b[2;1H\x1bMX");
    assert_visible_text_rows(reverse_not_top.extract_frame()?, &["X", "B", "C"]);

    let mut reverse_region_top = TerminalEngine::new(test_geometry(5, 5))?;
    reverse_region_top.write_vt(b"A\x1b[2;1HB\x1b[3;1HC\x1b[2;3r\x1b[2;1H\x1bM");
    assert_visible_text_rows(reverse_region_top.extract_frame()?, &["A", "", "B"]);

    let mut reverse_outside_region = TerminalEngine::new(test_geometry(5, 5))?;
    reverse_outside_region.write_vt(b"A\x1b[2;1HB\x1b[3;1HC\x1b[2;3r\x1b[1;1H\x1bM");
    assert_visible_text_rows(reverse_outside_region.extract_frame()?, &["A", "B", "C"]);

    Ok(())
}

#[test]
fn terminal_engine_supports_terminal_scroll_up_and_down() -> Result<()> {
    let mut scroll_up = TerminalEngine::new(test_geometry(5, 5))?;
    scroll_up.write_vt(b"ABC\r\nDEF\r\nGHI\x1b[2;2H\x1b[S");
    let frame = scroll_up.extract_frame()?;
    assert_visible_text_rows(frame, &["DEF", "GHI"]);
    assert_cursor_position(frame, (1, 1));

    let mut scroll_down = TerminalEngine::new(test_geometry(5, 5))?;
    scroll_down.write_vt(b"ABC\r\nDEF\r\nGHI\x1b[2;2H\x1b[T");
    let frame = scroll_down.extract_frame()?;
    assert_visible_text_rows(frame, &["", "ABC", "DEF", "GHI"]);
    assert_cursor_position(frame, (1, 1));

    let mut scroll_up_region = TerminalEngine::new(test_geometry(5, 5))?;
    scroll_up_region.write_vt(b"ABC\r\nDEF\r\nGHI\x1b[2;3r\x1b[1;1H\x1b[S");
    assert_visible_text_rows(scroll_up_region.extract_frame()?, &["ABC", "GHI"]);

    let mut scroll_down_region = TerminalEngine::new(test_geometry(5, 5))?;
    scroll_down_region.write_vt(b"ABC\r\nDEF\r\nGHI\x1b[3;4r\x1b[2;2H\x1b[T");
    assert_visible_text_rows(
        scroll_down_region.extract_frame()?,
        &["ABC", "DEF", "", "GHI"],
    );

    let mut scroll_up_count = TerminalEngine::new(test_geometry(5, 5))?;
    scroll_up_count.write_vt(b"AAAAA\r\nBBBBB\r\nCCCCC\r\nDDDDD\x1b[2S");
    assert_visible_text_rows(scroll_up_count.extract_frame()?, &["CCCCC", "DDDDD"]);

    Ok(())
}

#[test]
fn terminal_engine_supports_terminal_tab_clear_set_and_reset() -> Result<()> {
    let mut clear_current = TerminalEngine::new(test_geometry(30, 5))?;
    clear_current.write_vt(b"\t\x1b[2W\x1b[1;1HA\tX");
    assert_visible_text_rows(clear_current.extract_frame()?, &["A               X"]);

    let mut clear_all = TerminalEngine::new(test_geometry(30, 5))?;
    clear_all.write_vt(b"\x1b[5W\x1b[1;1HA\tX");
    assert_visible_text_rows(
        clear_all.extract_frame()?,
        &["A                            X"],
    );

    let mut set_current = TerminalEngine::new(test_geometry(30, 5))?;
    set_current.write_vt(b"\x1b[5W\x1b[5G\x1b[W\x1b[1;1HA\tX");
    assert_visible_text_rows(set_current.extract_frame()?, &["A   X"]);

    let mut reset_all = TerminalEngine::new(test_geometry(30, 5))?;
    reset_all.write_vt(b"\x1b[5W\x1b[5G\x1b[W\x1b[?5W\x1b[1;1HA\tX");
    assert_visible_text_rows(reset_all.extract_frame()?, &["A       X"]);

    Ok(())
}

#[test]
fn terminal_engine_supports_terminal_print_repeat() -> Result<()> {
    let mut simple = TerminalEngine::new(test_geometry(5, 5))?;
    simple.write_vt(b"A\x1b[b");
    assert_visible_text_rows(simple.extract_frame()?, &["AA"]);

    let mut explicit_count = TerminalEngine::new(test_geometry(5, 5))?;
    explicit_count.write_vt(b"A\x1b[2b");
    assert_visible_text_rows(explicit_count.extract_frame()?, &["AAA"]);

    let mut wrap = TerminalEngine::new(test_geometry(5, 5))?;
    wrap.write_vt(b"    A\x1b[b");
    assert_visible_text_rows(wrap.extract_frame()?, &["    A", "A"]);

    let mut no_previous_char = TerminalEngine::new(test_geometry(5, 5))?;
    no_previous_char.write_vt(b"\x1b[b");
    assert_visible_text_rows(no_previous_char.extract_frame()?, &[]);

    Ok(())
}

#[test]
fn terminal_engine_supports_terminal_alternate_screen_modes() -> Result<()> {
    let mut mode_47 = TerminalEngine::new(test_geometry(5, 5))?;
    mode_47.write_vt(b"1A\x1b[?47h");
    assert_visible_text_rows(mode_47.extract_frame()?, &[]);
    mode_47.write_vt(b"2B");
    assert_visible_text_rows(mode_47.extract_frame()?, &["  2B"]);
    mode_47.write_vt(b"\x1b[?47l");
    assert_visible_text_rows(mode_47.extract_frame()?, &["1A"]);
    mode_47.write_vt(b"\x1b[?47h");
    assert_visible_text_rows(mode_47.extract_frame()?, &["  2B"]);

    let mut mode_1047 = TerminalEngine::new(test_geometry(5, 5))?;
    mode_1047.write_vt(b"1A\x1b[?1047h");
    assert_visible_text_rows(mode_1047.extract_frame()?, &[]);
    mode_1047.write_vt(b"2B");
    assert_visible_text_rows(mode_1047.extract_frame()?, &["  2B"]);
    mode_1047.write_vt(b"\x1b[?1047l");
    assert_visible_text_rows(mode_1047.extract_frame()?, &["1A"]);
    mode_1047.write_vt(b"\x1b[?1047h");
    assert_visible_text_rows(mode_1047.extract_frame()?, &[]);

    let mut mode_1049 = TerminalEngine::new(test_geometry(5, 5))?;
    mode_1049.write_vt(b"1A\x1b[?1049h");
    assert_visible_text_rows(mode_1049.extract_frame()?, &[]);
    mode_1049.write_vt(b"2B");
    assert_visible_text_rows(mode_1049.extract_frame()?, &["  2B"]);
    mode_1049.write_vt(b"\x1b[?1049lC");
    assert_visible_text_rows(mode_1049.extract_frame()?, &["1AC"]);
    mode_1049.write_vt(b"\x1b[?1049h");
    assert_visible_text_rows(mode_1049.extract_frame()?, &[]);

    Ok(())
}

#[test]
fn terminal_engine_supports_terminal_full_reset() -> Result<()> {
    let mut origin_mode = TerminalEngine::new(test_geometry(10, 10))?;
    origin_mode.write_vt(b"\x1b[3;4r\x1b[?6h\x1b[1;1HA\x1bcX");
    assert_visible_text_rows(origin_mode.extract_frame()?, &["X"]);

    let mut saved_cursor = TerminalEngine::new(test_geometry(10, 10))?;
    saved_cursor.write_vt(b"\x1b[3;5H\x1b7\x1bc\x1b8X");
    let frame = saved_cursor.extract_frame()?;
    assert_visible_text_rows(frame, &["X"]);
    assert_cursor_position(frame, (1, 0));

    let mut alternate = TerminalEngine::new(test_geometry(10, 10))?;
    alternate.write_vt(b"primary\x1b[?1049halt\x1b[?1049l\x1bc");
    assert_visible_text_rows(alternate.extract_frame()?, &[]);
    alternate.write_vt(b"\x1b[?1049h");
    assert_visible_text_rows(alternate.extract_frame()?, &[]);

    let mut style_reset = TerminalEngine::new(test_geometry(10, 10))?;
    style_reset.write_vt(b"\x1b[1;3mA\x1bcX");
    let frame = style_reset.extract_frame()?;
    let cell = frame
        .cells
        .iter()
        .find(|cell| cell.text_len > 0)
        .expect("reset should leave one printed cell");
    assert_eq!(frame.cell_text(cell), &['X']);
    assert!(!cell.style.bold);
    assert!(!cell.style.italic);

    Ok(())
}

#[test]
fn terminal_engine_supports_terminal_plain_text_input() -> Result<()> {
    let mut engine = TerminalEngine::new(test_geometry(40, 4))?;

    engine.write_vt(b"hello");
    let frame = engine.extract_frame()?;

    assert_visible_text_rows(frame, &["hello"]);
    assert_cursor_position(frame, (5, 0));
    assert_eq!(frame.row_dirty.first().copied(), Some(true));
    Ok(())
}

#[test]
fn terminal_engine_supports_terminal_basic_wraparound_printing() -> Result<()> {
    let mut engine = TerminalEngine::new(test_geometry(5, 4))?;

    engine.write_vt(b"helloworldabc12");
    let frame = engine.extract_frame()?;

    assert_visible_text_rows(frame, &["hello", "world", "abc12"]);
    assert_cursor_position(frame, (4, 2));
    Ok(())
}

#[test]
fn terminal_engine_supports_terminal_input_forces_scroll() -> Result<()> {
    let mut engine = TerminalEngine::new(test_geometry(1, 5))?;

    engine.write_vt(b"abcdef");
    let frame = engine.extract_frame()?;

    assert_visible_text_rows(frame, &["b", "c", "d", "e", "f"]);
    assert_cursor_position(frame, (0, 4));
    Ok(())
}

#[test]
fn terminal_engine_supports_terminal_single_very_long_line() -> Result<()> {
    let mut engine = TerminalEngine::new(test_geometry(5, 5))?;

    engine.write_vt(&vec![b'x'; 1000]);
    let frame = engine.extract_frame()?;

    assert_eq!(frame.rows, 5);
    assert_eq!(frame.cols, 5);
    assert_cursor_position(frame, (4, 4));
    assert_eq!(visible_text_rows(frame), vec!["xxxxx"; 5]);
    Ok(())
}

#[test]
fn terminal_engine_supports_terminal_unique_style_per_cell() -> Result<()> {
    let mut engine = TerminalEngine::new(test_geometry(30, 30))?;

    for y in 0..30 {
        for x in 0..30 {
            engine
                .write_vt(format!("\x1b[{};{}H\x1b[48;2;{};{};0mx", y + 1, x + 1, x, y).as_bytes());
        }
    }
    let frame = engine.extract_frame()?;

    assert_eq!(collect_visible_cells(frame, |text, _| text).len(), 900);
    for (x, y) in [(0, 0), (7, 3), (29, 29)] {
        let cell = frame
            .cells
            .iter()
            .find(|cell| cell.x == x && cell.y == y)
            .unwrap_or_else(|| panic!("missing styled cell at {x},{y}"));
        assert_eq!(frame.cell_text(cell), &['x']);
        assert_eq!(
            cell.bg,
            Some(RgbColor {
                r: x as u8,
                g: y as u8,
                b: 0
            })
        );
    }
    Ok(())
}

#[test]
fn terminal_engine_supports_terminal_resize_wraparound_mode() -> Result<()> {
    let mut wraparound = TerminalEngine::new(test_geometry(4, 2))?;
    wraparound.write_vt(b"0123");
    wraparound.resize(test_geometry(2, 2))?;
    assert_visible_text_rows(wraparound.extract_frame()?, &["01", "23"]);

    let mut no_wraparound = TerminalEngine::new(test_geometry(4, 2))?;
    no_wraparound.write_vt(b"\x1b[?7l0123");
    no_wraparound.resize(test_geometry(2, 2))?;
    assert_visible_text_rows(no_wraparound.extract_frame()?, &["01"]);

    Ok(())
}

#[test]
fn terminal_engine_supports_terminal_wide_char_printing() -> Result<()> {
    let mut engine = TerminalEngine::new(test_geometry(8, 2))?;

    engine.write_vt("\u{1F600}".as_bytes());
    let frame = engine.extract_frame()?;

    assert_eq!(occupied_cells(frame), [('\u{1F600}', 0, 0)]);
    assert_cursor_position(frame, (2, 0));
    Ok(())
}

#[test]
fn terminal_engine_supports_terminal_wide_char_edge_wrap() -> Result<()> {
    let mut engine = TerminalEngine::new(test_geometry(10, 3))?;

    engine.write_vt("\x1b[1;10H\u{1F600}".as_bytes());
    let frame = engine.extract_frame()?;

    assert_eq!(occupied_cells(frame), [('\u{1F600}', 0, 1)]);
    assert_cursor_position(frame, (2, 1));
    Ok(())
}

#[test]
fn terminal_engine_supports_terminal_zero_width_at_start_is_ignored() -> Result<()> {
    let mut engine = TerminalEngine::new(test_geometry(8, 2))?;

    engine.write_vt("\u{200D}".as_bytes());
    let frame = engine.extract_frame()?;

    assert!(occupied_cells(frame).is_empty());
    assert_cursor_position(frame, (0, 0));
    Ok(())
}

#[test]
#[ignore = "original libghostty-rs snapshot orders this pending wrap combining mark differently"]
fn terminal_engine_supports_terminal_combining_mark_pending_wrap_cell() -> Result<()> {
    let mut engine = TerminalEngine::new(test_geometry(2, 2))?;

    engine
        .terminal
        .set_mode(Mode::GRAPHEME_CLUSTER, false)
        .context("grapheme cluster mode should be configurable")?;
    engine.write_vt("x\u{00E5}\u{0332}".as_bytes());
    let frame = engine.extract_frame()?;

    assert_visible_text_rows(frame, &["x\u{00E5}\u{0332}"]);
    assert_cursor_position(frame, (1, 0));
    Ok(())
}

#[test]
fn terminal_engine_supports_terminal_resize_reflow_visible_content() -> Result<()> {
    let mut grow_rows = TerminalEngine::new(test_geometry(10, 3))?;
    grow_rows.write_vt(b"1ABCD\r\n2EFGH\r\n3IJKL");
    grow_rows.resize(test_geometry(10, 10))?;
    assert_visible_text_rows(grow_rows.extract_frame()?, &["1ABCD", "2EFGH", "3IJKL"]);

    let mut shrink_rows = TerminalEngine::new(test_geometry(10, 3))?;
    shrink_rows.write_vt(b"1ABCD\r\n2EFGH\r\n3IJKL");
    shrink_rows.resize(test_geometry(10, 2))?;
    let frame = shrink_rows.extract_frame()?;
    assert_visible_text_rows(frame, &["2EFGH", "3IJKL"]);
    assert_cursor_position(frame, (5, 1));

    let mut grow_cols = TerminalEngine::new(test_geometry(10, 3))?;
    grow_cols.write_vt(b"1ABCD\r\n2EFGH\r\n3IJKL");
    grow_cols.resize(test_geometry(20, 3))?;
    assert_visible_text_rows(grow_cols.extract_frame()?, &["1ABCD", "2EFGH", "3IJKL"]);

    let mut shrink_cols = TerminalEngine::new(test_geometry(5, 3))?;
    shrink_cols.write_vt(b"1ABCD");
    shrink_cols.resize(test_geometry(3, 3))?;
    assert_visible_text_rows(shrink_cols.extract_frame()?, &["1AB", "CD"]);

    Ok(())
}

#[test]
fn terminal_engine_supports_alternate_screen_resize_no_reflow() -> Result<()> {
    let mut engine = TerminalEngine::new(test_geometry(5, 3))?;

    engine.write_vt(b"\x1b[?1049h1ABCD");
    engine.resize(test_geometry(3, 3))?;

    let frame = engine.extract_frame()?;
    assert_visible_text_rows(frame, &["1AB"]);
    assert_cursor_position(frame, (2, 0));

    Ok(())
}

#[test]
fn terminal_engine_supports_screen_clear_active_line() -> Result<()> {
    let mut engine = TerminalEngine::new(test_geometry(8, 2))?;

    engine.write_vt(b"hello\r\x1b[K");
    let frame = engine.extract_frame()?;

    assert!(occupied_cells(frame).is_empty());
    Ok(())
}

#[test]
fn terminal_engine_supports_terminal_erase_chars() -> Result<()> {
    let mut simple = TerminalEngine::new(test_geometry(5, 5))?;
    simple.write_vt(b"ABC\x1b[1;1H\x1b[2XX");
    assert_visible_text_rows(simple.extract_frame()?, &["X C"]);

    let mut minimum_one = TerminalEngine::new(test_geometry(5, 5))?;
    minimum_one.write_vt(b"ABC\x1b[1;1H\x1b[0XX");
    assert_visible_text_rows(minimum_one.extract_frame()?, &["XBC"]);

    let mut beyond_edge = TerminalEngine::new(test_geometry(5, 5))?;
    beyond_edge.write_vt(b"  ABC\x1b[1;4H\x1b[10X");
    assert_visible_text_rows(beyond_edge.extract_frame()?, &["  A"]);

    let mut pending_wrap = TerminalEngine::new(test_geometry(5, 5))?;
    pending_wrap.write_vt(b"ABCDE\x1b[XB");
    assert_visible_text_rows(pending_wrap.extract_frame()?, &["ABCDB"]);

    Ok(())
}

#[test]
fn terminal_engine_supports_terminal_erase_line() -> Result<()> {
    let mut erase_right = TerminalEngine::new(test_geometry(5, 5))?;
    erase_right.write_vt(b"ABCDE\x1b[1;3H\x1b[K");
    assert_visible_text_rows(erase_right.extract_frame()?, &["AB"]);

    let mut pending_wrap = TerminalEngine::new(test_geometry(5, 5))?;
    pending_wrap.write_vt(b"ABCDE\x1b[KB");
    assert_visible_text_rows(pending_wrap.extract_frame()?, &["ABCDB"]);

    let mut reset_wrap = TerminalEngine::new(test_geometry(5, 5))?;
    reset_wrap.write_vt(b"ABCDE123\x1b[1;1H\x1b[KX");
    assert_visible_text_rows(reset_wrap.extract_frame()?, &["X", "123"]);

    Ok(())
}

#[test]
fn terminal_engine_supports_terminal_insert_blanks() -> Result<()> {
    let mut insert = TerminalEngine::new(test_geometry(5, 2))?;
    insert.write_vt(b"ABC\x1b[1;1H\x1b[2@");
    assert_visible_text_rows(insert.extract_frame()?, &["  ABC"]);

    let mut pushed_off_end = TerminalEngine::new(test_geometry(3, 2))?;
    pushed_off_end.write_vt(b"ABC\x1b[1;1H\x1b[2@");
    assert_visible_text_rows(pushed_off_end.extract_frame()?, &["  A"]);

    Ok(())
}

#[test]
fn terminal_engine_supports_terminal_insert_mode_printing() -> Result<()> {
    let mut with_space = TerminalEngine::new(test_geometry(10, 2))?;
    with_space.write_vt(b"hello\x1b[1;2H\x1b[4hX");
    assert_visible_text_rows(with_space.extract_frame()?, &["hXello"]);

    let mut no_wrap_pushed = TerminalEngine::new(test_geometry(5, 2))?;
    no_wrap_pushed.write_vt(b"hello\x1b[1;2H\x1b[4hX");
    assert_visible_text_rows(no_wrap_pushed.extract_frame()?, &["hXell"]);

    let mut at_end = TerminalEngine::new(test_geometry(5, 2))?;
    at_end.write_vt(b"hello\x1b[4hX");
    assert_visible_text_rows(at_end.extract_frame()?, &["hello", "X"]);

    let mut wide = TerminalEngine::new(test_geometry(5, 2))?;
    wide.write_vt("hello\x1b[1;2H\x1b[4h\u{1F600}".as_bytes());
    assert_visible_text_rows(wide.extract_frame()?, &["h\u{1F600}el"]);

    let mut wide_pushed_off = TerminalEngine::new(test_geometry(5, 2))?;
    wide_pushed_off.write_vt("123\u{1F600}\x1b[1;1H\x1b[4hX".as_bytes());
    assert_visible_text_rows(wide_pushed_off.extract_frame()?, &["X123"]);

    Ok(())
}

#[test]
fn terminal_engine_supports_terminal_delete_chars() -> Result<()> {
    let mut delete_two = TerminalEngine::new(test_geometry(5, 5))?;
    delete_two.write_vt(b"ABCDE\x1b[1;2H\x1b[2P");
    assert_visible_text_rows(delete_two.extract_frame()?, &["ADE"]);

    let mut delete_past_width = TerminalEngine::new(test_geometry(5, 5))?;
    delete_past_width.write_vt(b"ABCDE\x1b[1;2H\x1b[10P");
    assert_visible_text_rows(delete_past_width.extract_frame()?, &["A"]);

    let mut pending_wrap = TerminalEngine::new(test_geometry(5, 5))?;
    pending_wrap.write_vt(b"ABCDE\x1b[PX");
    assert_visible_text_rows(pending_wrap.extract_frame()?, &["ABCDX"]);

    let mut reset_wrap = TerminalEngine::new(test_geometry(5, 5))?;
    reset_wrap.write_vt(b"ABCDE123\x1b[1;1H\x1b[PX");
    assert_visible_text_rows(reset_wrap.extract_frame()?, &["XCDE", "123"]);

    Ok(())
}

#[test]
fn terminal_engine_supports_terminal_insert_lines() -> Result<()> {
    let mut simple = TerminalEngine::new(test_geometry(5, 5))?;
    simple.write_vt(b"ABC\r\nDEF\r\nGHI\x1b[2;2H\x1b[L");
    assert_visible_text_rows(simple.extract_frame()?, &["ABC", "", "DEF", "GHI"]);

    let mut scroll_region = TerminalEngine::new(test_geometry(5, 5))?;
    scroll_region.write_vt(b"ABC\r\nDEF\r\nGHI\r\n123\x1b[1;3r\x1b[2;2H\x1b[L");
    assert_visible_text_rows(scroll_region.extract_frame()?, &["ABC", "", "DEF", "123"]);

    let mut more_than_remaining = TerminalEngine::new(test_geometry(2, 5))?;
    more_than_remaining.write_vt(b"A\r\nB\r\nC\r\nD\r\nE\x1b[2;1H\x1b[20L");
    assert_visible_text_rows(more_than_remaining.extract_frame()?, &["A"]);

    let mut pending_wrap = TerminalEngine::new(test_geometry(5, 5))?;
    pending_wrap.write_vt(b"ABCDE\x1b[LB");
    assert_visible_text_rows(pending_wrap.extract_frame()?, &["B", "ABCDE"]);

    Ok(())
}

#[test]
fn terminal_engine_supports_terminal_delete_lines() -> Result<()> {
    let mut simple = TerminalEngine::new(test_geometry(5, 5))?;
    simple.write_vt(b"ABC\r\nDEF\r\nGHI\x1b[2;2H\x1b[M");
    assert_visible_text_rows(simple.extract_frame()?, &["ABC", "GHI"]);

    let mut scroll_region = TerminalEngine::new(test_geometry(5, 5))?;
    scroll_region.write_vt(b"A\r\nB\r\nC\r\nD\x1b[1;3r\x1b[1;1H\x1b[ME\r\n");
    assert_visible_text_rows(scroll_region.extract_frame()?, &["E", "C", "", "D"]);

    let mut large_scroll_region_count = TerminalEngine::new(test_geometry(5, 5))?;
    large_scroll_region_count.write_vt(b"A\r\nB\r\nC\r\nD\x1b[1;3r\x1b[1;1H\x1b[5ME\r\n");
    assert_visible_text_rows(
        large_scroll_region_count.extract_frame()?,
        &["E", "", "", "D"],
    );

    let mut pending_wrap = TerminalEngine::new(test_geometry(5, 5))?;
    pending_wrap.write_vt(b"ABCDE\x1b[MB");
    assert_visible_text_rows(pending_wrap.extract_frame()?, &["B"]);

    Ok(())
}

#[test]
fn terminal_engine_supports_terminal_erase_display() -> Result<()> {
    let mut erase_below = TerminalEngine::new(test_geometry(5, 5))?;
    erase_below.write_vt(b"ABC\r\nDEF\r\nGHI\x1b[2;2H\x1b[J");
    assert_visible_text_rows(erase_below.extract_frame()?, &["ABC", "D"]);

    let mut erase_above = TerminalEngine::new(test_geometry(5, 5))?;
    erase_above.write_vt(b"ABC\r\nDEF\r\nGHI\x1b[2;2H\x1b[1J");
    assert_visible_text_rows(erase_above.extract_frame()?, &["", "  F", "GHI"]);

    let mut complete = TerminalEngine::new(test_geometry(5, 5))?;
    complete.write_vt(b"ABC\r\nDEF\r\nGHI\x1b[2;2H\x1b[2J");
    let complete_frame = complete.extract_frame()?;
    assert!(occupied_cells(complete_frame).is_empty());
    assert_cursor_position(complete_frame, (1, 1));

    Ok(())
}

#[test]
fn terminal_engine_supports_terminal_save_restore_cursor() -> Result<()> {
    let mut position = TerminalEngine::new(test_geometry(10, 5))?;
    position.write_vt(b"\x1b[1;5HA\x1b7\x1b[1;1HB\x1b8X");
    assert_visible_text_rows(position.extract_frame()?, &["B   AX"]);

    let mut pending_wrap = TerminalEngine::new(test_geometry(5, 5))?;
    pending_wrap.write_vt(b"\x1b[1;5HA\x1b7\x1b[1;1HB\x1b8X");
    assert_visible_text_rows(pending_wrap.extract_frame()?, &["B   A", "X"]);

    let mut resized = TerminalEngine::new(test_geometry(10, 5))?;
    resized.write_vt(b"\x1b[1;10H\x1b7");
    resized.resize(test_geometry(5, 5))?;
    resized.write_vt(b"\x1b8X");
    assert_visible_text_rows(resized.extract_frame()?, &["    X"]);

    let mut style = TerminalEngine::new(test_geometry(5, 2))?;
    style.write_vt(b"\x1b[1m\x1b7\x1b[22mn\x1b8b");
    let frame = style.extract_frame()?;
    let styled_cells = collect_visible_cells(frame, |text, cell| (text, cell.style.bold));
    assert_eq!(styled_cells, [('b', true)]);

    Ok(())
}

#[test]
fn terminal_engine_supports_terminal_protected_erase() -> Result<()> {
    let mut iso_protected = TerminalEngine::new(test_geometry(5, 5))?;
    iso_protected.write_vt(b"\x1bVABC\r\nDEF\r\nGHI\x1b[2;2H\x1b[J");
    assert_visible_text_rows(iso_protected.extract_frame()?, &["ABC", "DEF", "GHI"]);

    let mut dec_ordinary_erase = TerminalEngine::new(test_geometry(5, 5))?;
    dec_ordinary_erase.write_vt(b"\x1b[1\"qABC\r\nDEF\r\nGHI\x1b[2;2H\x1b[J");
    assert_visible_text_rows(dec_ordinary_erase.extract_frame()?, &["ABC", "D"]);

    let mut dec_requested_protected = TerminalEngine::new(test_geometry(5, 5))?;
    dec_requested_protected.write_vt(b"\x1b[1\"qABC\r\nDEF\r\nGHI\x1b[2;2H\x1b[?J");
    assert_visible_text_rows(
        dec_requested_protected.extract_frame()?,
        &["ABC", "DEF", "GHI"],
    );

    let mut dec_line_requested_protected = TerminalEngine::new(test_geometry(5, 2))?;
    dec_line_requested_protected.write_vt(b"\x1b[1\"qABCDE\x1b[1;3H\x1b[?K");
    assert_visible_text_rows(dec_line_requested_protected.extract_frame()?, &["ABCDE"]);

    Ok(())
}

#[test]
fn terminal_engine_supports_terminal_decaln() -> Result<()> {
    let mut simple = TerminalEngine::new(test_geometry(2, 2))?;
    simple.write_vt(b"A\r\nB\x1b#8");
    let frame = simple.extract_frame()?;
    assert_visible_text_rows(frame, &["EE", "EE"]);
    assert_cursor_position(frame, (0, 0));
    assert_eq!(frame.row_dirty, [true, true]);

    let mut color = TerminalEngine::new(test_geometry(3, 3))?;
    color.write_vt(b"\x1b[48;2;255;0;0m\x1b#8");
    let frame = color.extract_frame()?;
    assert_visible_text_rows(frame, &["EEE", "EEE", "EEE"]);
    assert!(
        frame
            .cells
            .iter()
            .all(|cell| { cell.bg == Some(RgbColor { r: 255, g: 0, b: 0 }) })
    );

    Ok(())
}

#[test]
fn terminal_engine_supports_screen_active_view_scroll() -> Result<()> {
    let mut engine = TerminalEngine::new(test_geometry(8, 2))?;

    engine.write_vt(b"one\r\ntwo\r\nthree");
    let frame = engine.extract_frame()?;

    assert_eq!(
        occupied_cells(frame),
        [
            ('t', 0, 0),
            ('w', 1, 0),
            ('o', 2, 0),
            ('t', 0, 1),
            ('h', 1, 1),
            ('r', 2, 1),
            ('e', 3, 1),
            ('e', 4, 1),
        ]
    );
    Ok(())
}
