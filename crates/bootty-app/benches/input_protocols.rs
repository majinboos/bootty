use std::hint::black_box;

use bootty_app::geometry::TerminalGeometry;
use bootty_terminal::{
    terminal_engine::TerminalEngine,
    terminal_input_model::{
        KeyInput, KeyMods, MouseAction, MouseButton, MouseEncoderSize, MouseInput, TerminalKey,
    },
};
use criterion::{BatchSize, Criterion, criterion_group, criterion_main};

const GEOMETRY: TerminalGeometry = TerminalGeometry {
    cols: 120,
    rows: 40,
    cell_width: 9,
    cell_height: 22,
};

#[derive(Clone, Copy)]
enum KeyboardCase {
    LegacyPrintable,
    ApplicationCursor,
    ModifyOtherKeys,
    CsiU,
    KittyKeyboard,
    AltMetaCtrlShift,
    FunctionKeys,
    RepeatKeys,
    DeadKeyText,
    AltGrText,
}

impl KeyboardCase {
    const fn name(self) -> &'static str {
        match self {
            Self::LegacyPrintable => "legacy_printable",
            Self::ApplicationCursor => "application_cursor",
            Self::ModifyOtherKeys => "modify_other_keys",
            Self::CsiU => "csi_u",
            Self::KittyKeyboard => "kitty_keyboard",
            Self::AltMetaCtrlShift => "alt_meta_ctrl_shift",
            Self::FunctionKeys => "function_keys",
            Self::RepeatKeys => "repeat_keys",
            Self::DeadKeyText => "dead_key_text",
            Self::AltGrText => "altgr_text",
        }
    }
}

#[derive(Clone, Copy)]
enum MouseCase {
    X10,
    Normal,
    ButtonEvent,
    AnyEvent,
    Sgr,
    Urxvt,
    Wheel,
    Drag,
    PixelPosition,
}

impl MouseCase {
    const fn name(self) -> &'static str {
        match self {
            Self::X10 => "x10",
            Self::Normal => "normal",
            Self::ButtonEvent => "button_event",
            Self::AnyEvent => "any_event",
            Self::Sgr => "sgr",
            Self::Urxvt => "urxvt",
            Self::Wheel => "wheel",
            Self::Drag => "drag",
            Self::PixelPosition => "pixel_position",
        }
    }
}

#[derive(Default)]
struct ProtocolStats {
    events: usize,
    bytes: usize,
    side_effects: usize,
    frame_chars: usize,
    hash: u64,
}

impl ProtocolStats {
    fn add_bytes(&mut self, bytes: &[u8]) {
        self.events += 1;
        self.bytes += bytes.len();
        self.hash = bytes
            .iter()
            .fold(self.hash ^ 0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
                (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
            });
    }

    fn checksum(&self) -> u64 {
        self.hash
            ^ self.events as u64
            ^ (self.bytes as u64).rotate_left(17)
            ^ (self.side_effects as u64).rotate_left(31)
            ^ (self.frame_chars as u64).rotate_left(47)
    }
}

fn engine() -> TerminalEngine {
    TerminalEngine::new(GEOMETRY).expect("terminal engine")
}

fn configure_keyboard(engine: &mut TerminalEngine, case: KeyboardCase) {
    match case {
        KeyboardCase::ApplicationCursor => engine.write_vt(b"\x1b[?1h"),
        KeyboardCase::ModifyOtherKeys => engine.write_vt(b"\x1b[>4;2m"),
        KeyboardCase::CsiU => engine.write_vt(b"\x1b[>4;1m"),
        KeyboardCase::KittyKeyboard => engine.write_vt(b"\x1b[>1u"),
        _ => {}
    }
}

fn key_event(case: KeyboardCase, index: usize) -> KeyInput {
    match case {
        KeyboardCase::ApplicationCursor => KeyInput {
            key: [
                TerminalKey::ArrowUp,
                TerminalKey::ArrowDown,
                TerminalKey::ArrowRight,
                TerminalKey::ArrowLeft,
            ][index % 4],
            mods: KeyMods::default(),
            repeat: false,
            utf8: None,
            unshifted: None,
        },
        KeyboardCase::FunctionKeys => KeyInput {
            key: [
                TerminalKey::F1,
                TerminalKey::F5,
                TerminalKey::F12,
                TerminalKey::PageDown,
            ][index % 4],
            mods: KeyMods {
                shift: index.is_multiple_of(2),
                ctrl: index.is_multiple_of(3),
                alt: index.is_multiple_of(5),
                ..KeyMods::default()
            },
            repeat: false,
            utf8: None,
            unshifted: None,
        },
        KeyboardCase::AltMetaCtrlShift => KeyInput {
            key: TerminalKey::A,
            mods: KeyMods {
                shift: true,
                alt: index.is_multiple_of(2),
                ctrl: index.is_multiple_of(3),
                command: index.is_multiple_of(5),
                ..KeyMods::default()
            },
            repeat: false,
            utf8: Some("A"),
            unshifted: Some('a'),
        },
        KeyboardCase::RepeatKeys => KeyInput {
            key: TerminalKey::J,
            mods: KeyMods::default(),
            repeat: index > 0,
            utf8: Some("j"),
            unshifted: Some('j'),
        },
        KeyboardCase::DeadKeyText => KeyInput {
            key: TerminalKey::E,
            mods: KeyMods::default(),
            repeat: false,
            utf8: Some("é"),
            unshifted: Some('e'),
        },
        KeyboardCase::AltGrText => KeyInput {
            key: TerminalKey::Q,
            mods: KeyMods {
                alt: true,
                ctrl: true,
                right_alt: true,
                ..KeyMods::default()
            },
            repeat: false,
            utf8: Some("@"),
            unshifted: Some('q'),
        },
        _ => KeyInput {
            key: TerminalKey::A,
            mods: KeyMods::default(),
            repeat: false,
            utf8: Some("a"),
            unshifted: Some('a'),
        },
    }
}

fn run_keyboard_case(case: KeyboardCase, events: usize) -> u64 {
    let mut engine = engine();
    configure_keyboard(&mut engine, case);
    let mut out = Vec::new();
    let mut stats = ProtocolStats::default();
    for index in 0..events {
        engine
            .encode_key_to_vec(key_event(case, index), &mut out)
            .expect("encode key");
        stats.add_bytes(&out);
    }
    assert_eq!(stats.events, events);
    stats.checksum()
}

fn configure_mouse(engine: &mut TerminalEngine, case: MouseCase) {
    let sequence = match case {
        MouseCase::X10 => b"\x1b[?9h".as_slice(),
        MouseCase::Normal => b"\x1b[?1000h".as_slice(),
        MouseCase::ButtonEvent => b"\x1b[?1002h".as_slice(),
        MouseCase::AnyEvent => b"\x1b[?1003h".as_slice(),
        MouseCase::Sgr | MouseCase::PixelPosition => b"\x1b[?1000h\x1b[?1006h".as_slice(),
        MouseCase::Urxvt => b"\x1b[?1000h\x1b[?1015h".as_slice(),
        MouseCase::Wheel => b"\x1b[?1000h\x1b[?1006h".as_slice(),
        MouseCase::Drag => b"\x1b[?1002h\x1b[?1006h".as_slice(),
    };
    engine.write_vt(sequence);
}

fn mouse_event(case: MouseCase, index: usize) -> MouseInput {
    let button = match case {
        MouseCase::Wheel => Some(if index.is_multiple_of(2) {
            MouseButton::Four
        } else {
            MouseButton::Five
        }),
        _ => Some(MouseButton::Left),
    };
    MouseInput {
        action: if matches!(case, MouseCase::Drag) && !index.is_multiple_of(3) {
            MouseAction::Motion
        } else if index.is_multiple_of(5) {
            MouseAction::Release
        } else {
            MouseAction::Press
        },
        button,
        mods: KeyMods {
            shift: index.is_multiple_of(7),
            alt: index.is_multiple_of(11),
            ctrl: index.is_multiple_of(13),
            ..KeyMods::default()
        },
        x: 10.0 + (index % 100) as f32 * 7.0,
        y: 8.0 + (index % 30) as f32 * 13.0,
        size: MouseEncoderSize {
            screen_width: u32::from(GEOMETRY.cols) * GEOMETRY.cell_width,
            screen_height: u32::from(GEOMETRY.rows) * GEOMETRY.cell_height,
            cell_width: GEOMETRY.cell_width,
            cell_height: GEOMETRY.cell_height,
            padding_top: 0,
            padding_bottom: 0,
            padding_right: 0,
            padding_left: 0,
        },
    }
}

fn run_mouse_case(case: MouseCase, events: usize) -> u64 {
    let mut engine = engine();
    configure_mouse(&mut engine, case);
    let mut out = Vec::new();
    let mut stats = ProtocolStats::default();
    for index in 0..events {
        engine
            .encode_mouse_to_vec(mouse_event(case, index), &mut out)
            .expect("encode mouse");
        stats.add_bytes(&out);
    }
    assert_eq!(stats.events, events);
    stats.checksum()
}

fn run_paste_clipboard_ime() -> u64 {
    let mut engine = engine();
    let mut out = Vec::new();
    let mut stats = ProtocolStats::default();
    let large_paste = "x".repeat(1024 * 1024);
    for text in ["small paste", "line1\nline2\n", large_paste.as_str()] {
        engine
            .encode_paste_to_vec(text, &mut out)
            .expect("encode paste");
        stats.add_bytes(&out);
    }
    engine.write_vt(b"\x1b[?2004h");
    engine
        .encode_paste_to_vec("bracketed\ncontrol\x1b[201~", &mut out)
        .expect("encode bracketed paste");
    stats.add_bytes(&out);
    engine.write_vt(b"\x1b]52;c;Ym9vdHR5\x1b\\");
    stats.side_effects += engine.drain_side_effects().len();
    engine.write_vt("IME preedit かな漢字 dead-key e\u{301}\r\n".as_bytes());
    let frame = engine.extract_frame().expect("ime frame");
    stats.frame_chars += frame.text.len();
    stats.hash ^= frame
        .text
        .iter()
        .fold(0xcbf2_9ce4_8422_2325_u64, |hash, ch| {
            (hash ^ u64::from(*ch as u32)).wrapping_mul(0x0000_0100_0000_01b3)
        });
    stats.checksum()
}

fn bench_keyboard(c: &mut Criterion) {
    let cases = [
        KeyboardCase::LegacyPrintable,
        KeyboardCase::ApplicationCursor,
        KeyboardCase::ModifyOtherKeys,
        KeyboardCase::CsiU,
        KeyboardCase::KittyKeyboard,
        KeyboardCase::AltMetaCtrlShift,
        KeyboardCase::FunctionKeys,
        KeyboardCase::RepeatKeys,
        KeyboardCase::DeadKeyText,
        KeyboardCase::AltGrText,
    ];
    for case in cases {
        c.bench_function(&format!("input_protocol_keyboard_{}", case.name()), |b| {
            b.iter_batched(
                || case,
                |case| black_box(run_keyboard_case(case, 10_000)),
                BatchSize::SmallInput,
            )
        });
    }
}

fn bench_mouse(c: &mut Criterion) {
    let cases = [
        MouseCase::X10,
        MouseCase::Normal,
        MouseCase::ButtonEvent,
        MouseCase::AnyEvent,
        MouseCase::Sgr,
        MouseCase::Urxvt,
        MouseCase::Wheel,
        MouseCase::Drag,
        MouseCase::PixelPosition,
    ];
    for case in cases {
        c.bench_function(&format!("input_protocol_mouse_{}", case.name()), |b| {
            b.iter_batched(
                || case,
                |case| black_box(run_mouse_case(case, 10_000)),
                BatchSize::SmallInput,
            )
        });
    }
}

fn bench_paste_clipboard_ime(c: &mut Criterion) {
    c.bench_function("input_protocol_paste_clipboard_ime", |b| {
        b.iter(|| black_box(run_paste_clipboard_ime()))
    });
}

criterion_group! {
    name = benches;
    config = Criterion::default().sample_size(10).noise_threshold(0.20);
    targets = bench_keyboard, bench_mouse, bench_paste_clipboard_ime,
}
criterion_main!(benches);
