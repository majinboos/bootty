use std::hint::black_box;

use bootty_render::geometry::TerminalGeometry;
use bootty_terminal::terminal_engine::TerminalEngine;
use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use libghostty_vt::{
    Terminal,
    render::RenderState,
    terminal::{Mode, Point, PointCoordinate},
};

const GEOMETRY: TerminalGeometry = TerminalGeometry {
    cols: 120,
    rows: 40,
    cell_width: 9,
    cell_height: 22,
};

#[derive(Clone, Copy)]
enum Chunking {
    Whole,
    Byte,
    Prime,
}

#[derive(Clone)]
struct ParserWorkload {
    name: &'static str,
    payload: Vec<u8>,
    chunking: Chunking,
}

fn write_terminal_chunks(
    terminal: &mut Terminal<'static, 'static>,
    payload: &[u8],
    chunking: Chunking,
) {
    match chunking {
        Chunking::Whole => terminal.vt_write(payload),
        Chunking::Byte => {
            for byte in payload {
                terminal.vt_write(std::slice::from_ref(byte));
            }
        }
        Chunking::Prime => {
            let mut offset = 0;
            let chunks = [1, 2, 3, 5, 8, 13, 21];
            let mut chunk_index = 0;
            while offset < payload.len() {
                let len = chunks[chunk_index % chunks.len()].min(payload.len() - offset);
                terminal.vt_write(&payload[offset..offset + len]);
                offset += len;
                chunk_index += 1;
            }
        }
    }
}

fn write_engine_chunks(engine: &mut TerminalEngine, payload: &[u8], chunking: Chunking) {
    match chunking {
        Chunking::Whole => engine.write_vt(payload),
        Chunking::Byte => {
            for byte in payload {
                engine.write_vt(std::slice::from_ref(byte));
            }
        }
        Chunking::Prime => {
            let mut offset = 0;
            let chunks = [1, 2, 3, 5, 8, 13, 21];
            let mut chunk_index = 0;
            while offset < payload.len() {
                let len = chunks[chunk_index % chunks.len()].min(payload.len() - offset);
                engine.write_vt(&payload[offset..offset + len]);
                offset += len;
                chunk_index += 1;
            }
        }
    }
}

fn hash_bytes(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

fn ascii_payload(lines: usize) -> Vec<u8> {
    let mut payload = Vec::with_capacity(lines * 48);
    for index in 0..lines {
        payload.extend_from_slice(
            format!("ascii baseline line {index:06} parser payload\r\n").as_bytes(),
        );
    }
    payload
}

fn split_utf8_payload(repeats: usize) -> Vec<u8> {
    "utf8 split コンニチハ 🥟 e\u{301} عربى देवनागरी\r\n"
        .repeat(repeats)
        .into_bytes()
}

fn split_csi_payload(repeats: usize) -> Vec<u8> {
    let mut payload = Vec::with_capacity(repeats * 64);
    for index in 0..repeats {
        payload.extend_from_slice(
            format!(
                "\x1b[{};{}Hsplit-csi-{index:06}\x1b[0K",
                1 + index % 40,
                1 + index % 100
            )
            .as_bytes(),
        );
    }
    payload
}

fn sgr_churn_payload(repeats: usize) -> Vec<u8> {
    let mut payload = Vec::with_capacity(repeats * 48);
    for index in 0..repeats {
        payload.extend_from_slice(
            format!(
                "\x1b[{};{};{}mSGR{index:06}\x1b[0m",
                30 + index % 8,
                40 + (index / 3) % 8,
                if index % 2 == 0 { 1 } else { 22 }
            )
            .as_bytes(),
        );
    }
    payload
}

fn truecolor_churn_payload(repeats: usize) -> Vec<u8> {
    let mut payload = Vec::with_capacity(repeats * 72);
    for index in 0..repeats {
        payload.extend_from_slice(
            format!(
                "\x1b[38;2;{};{};{}m\x1b[48;2;{};{};{}mtrue{index:06}\x1b[0m",
                index % 256,
                (index * 3) % 256,
                (index * 7) % 256,
                (index * 11) % 256,
                (index * 13) % 256,
                (index * 17) % 256,
            )
            .as_bytes(),
        );
    }
    payload
}

fn cursor_walk_payload(repeats: usize) -> Vec<u8> {
    let mut payload = Vec::with_capacity(repeats * 28);
    for index in 0..repeats {
        payload.extend_from_slice(match index % 8 {
            0 => b"\x1b[A",
            1 => b"\x1b[B",
            2 => b"\x1b[C",
            3 => b"\x1b[D",
            4 => b"\x1b[5C",
            5 => b"\x1b[3D",
            6 => b"\x1b[2B",
            _ => b"\x1b[1A",
        });
        payload.extend_from_slice(b"x");
    }
    payload
}

fn public_light_cells_payload() -> Vec<u8> {
    let mut payload = b"\x1b[?1049h".to_vec();
    for ch in b'A'..=b'Z' {
        payload.extend_from_slice(b"\x1b[H");
        payload.extend(std::iter::repeat_n(
            ch,
            usize::from(GEOMETRY.cols) * usize::from(GEOMETRY.rows),
        ));
    }
    payload
}

fn public_dense_cells_payload() -> Vec<u8> {
    let mut payload = b"\x1b[?1049h".to_vec();
    for (offset, ch) in (b'A'..=b'Z').enumerate() {
        let offset = offset as u16;
        payload.extend_from_slice(b"\x1b[H");
        for line in 1..=GEOMETRY.rows {
            for column in 1..=GEOMETRY.cols {
                let index = line + column + offset;
                let fg_col = index % 156 + 100;
                let bg_col = 255 - index % 156 + 100;
                payload.extend_from_slice(
                    format!("\x1b[38;5;{fg_col};48;5;{bg_col};1;3;4m{}", ch as char).as_bytes(),
                );
            }
        }
    }
    payload
}

fn public_cursor_motion_payload() -> Vec<u8> {
    let mut payload = Vec::new();
    for ch in b'A'..=b'Z' {
        let mut column_start = 1_u16;
        let mut column_end = GEOMETRY.cols;
        let mut line_start = 1_u16;
        let mut line_end = GEOMETRY.rows;
        loop {
            let mut column = column_start;
            let mut line = line_start;

            while column < column_end {
                payload
                    .extend_from_slice(format!("\x1b[{line};{column}H{}", ch as char).as_bytes());
                column += 1;
            }
            while line < line_end {
                payload
                    .extend_from_slice(format!("\x1b[{line};{column}H{}", ch as char).as_bytes());
                line += 1;
            }
            while column > column_start {
                payload
                    .extend_from_slice(format!("\x1b[{line};{column}H{}", ch as char).as_bytes());
                column -= 1;
            }
            while line > line_start {
                payload
                    .extend_from_slice(format!("\x1b[{line};{column}H{}", ch as char).as_bytes());
                line -= 1;
            }

            column_start += 1;
            line_start += 1;
            column_end -= 1;
            line_end -= 1;
            if column_start > column_end || line_start > line_end {
                break;
            }
        }
    }
    payload
}

fn public_unicode_payload(repeats: usize) -> Vec<u8> {
    let symbols = "¡¢£¤¥¦§¨©ª«¬®¯°±²³´µ¶·¸¹º»¼½¾¿ÀÁÂÃÄÅÆÇÈÉÊËÌÍÎÏÐÑÒÕÖ×ØÙÚÛÜÝÞßàáâãäåæçèéêëìíîïðñòóôõö÷øùúûüýþÿĀāĂąĆćĈĉĊċČčĎďĐđΓΔΘΛΞΠΣΦΨΩБГДЖЗИЙКЛПФЦЧШЩאבגדהוזחטיךכלםמןנסעףפץצקרשת가각간갈감갑값갓강개객갠갤갬갭갯갱😀😁😂😃😄😅😆😇😈😉😊😋😌😍😎😏";
    symbols.repeat(repeats).into_bytes()
}

fn scroll_insert_erase_payload(repeats: usize) -> Vec<u8> {
    let mut payload = b"\x1b[2J\x1b[1;40r".to_vec();
    for index in 0..repeats {
        payload.extend_from_slice(
            format!(
                "\x1b[{};1Hrow{index:06}\x1b[{}@\x1b[{}P\x1b[{}L\x1b[{}M\x1b[2K\x1b[0J",
                1 + index % 40,
                1 + index % 4,
                1 + index % 4,
                1 + index % 3,
                1 + index % 3,
            )
            .as_bytes(),
        );
    }
    payload.extend_from_slice(b"\x1b[r");
    payload
}

fn top_region_scroll_payload(repeats: usize, top_row: u16) -> Vec<u8> {
    let mut payload = format!("\x1b[?1049h\x1b[{top_row};{}r", GEOMETRY.rows).into_bytes();
    for _ in 0..repeats {
        payload.extend_from_slice(b"y\n");
    }
    payload
}

fn rep_tabs_alt_payload(repeats: usize) -> Vec<u8> {
    let mut payload = b"\x1b[?1049h\x1b[2J\x1b[H".to_vec();
    for index in 0..repeats {
        payload.extend_from_slice(
            format!("rep-tabs-{index:06}\t\x1b[{}b\r\n", 1 + index % 16).as_bytes(),
        );
    }
    payload.extend_from_slice(b"\x1b[?1049l");
    payload
}

fn osc_dcs_query_payload(repeats: usize) -> Vec<u8> {
    let mut payload = Vec::with_capacity(repeats * 128);
    for index in 0..repeats {
        payload.extend_from_slice(
            format!(
                "\x1b]0;title-{index}\x1b\\\x1b]8;id=p{index};https://example.invalid/{index}\x1b\\link\x1b]8;;\x1b\\\x1b]52;c;SGVsbG8=\x1b\\\x1bP$qm\x1b\\\x1b[c\x1b[6n"
            )
            .as_bytes(),
        );
    }
    payload
}

fn synchronized_update_payload(repeats: usize) -> Vec<u8> {
    let mut payload = Vec::with_capacity(repeats * 64);
    for index in 0..repeats {
        payload
            .extend_from_slice(format!("\x1b[?2026hupdate {index:06}\r\n\x1b[?2026l").as_bytes());
    }
    payload
}

fn workloads() -> Vec<ParserWorkload> {
    vec![
        ParserWorkload {
            name: "ascii_whole",
            payload: ascii_payload(4_096),
            chunking: Chunking::Whole,
        },
        ParserWorkload {
            name: "utf8_byte_split",
            payload: split_utf8_payload(2_048),
            chunking: Chunking::Byte,
        },
        ParserWorkload {
            name: "csi_prime_split",
            payload: split_csi_payload(2_048),
            chunking: Chunking::Prime,
        },
        ParserWorkload {
            name: "sgr_churn",
            payload: sgr_churn_payload(4_096),
            chunking: Chunking::Whole,
        },
        ParserWorkload {
            name: "truecolor_churn",
            payload: truecolor_churn_payload(2_048),
            chunking: Chunking::Whole,
        },
        ParserWorkload {
            name: "cursor_walk",
            payload: cursor_walk_payload(8_192),
            chunking: Chunking::Whole,
        },
        ParserWorkload {
            name: "public_light_cells",
            payload: public_light_cells_payload(),
            chunking: Chunking::Whole,
        },
        ParserWorkload {
            name: "public_dense_cells",
            payload: public_dense_cells_payload(),
            chunking: Chunking::Whole,
        },
        ParserWorkload {
            name: "public_cursor_motion",
            payload: public_cursor_motion_payload(),
            chunking: Chunking::Whole,
        },
        ParserWorkload {
            name: "public_unicode",
            payload: public_unicode_payload(512),
            chunking: Chunking::Whole,
        },
        ParserWorkload {
            name: "scroll_insert_erase",
            payload: scroll_insert_erase_payload(1_024),
            chunking: Chunking::Whole,
        },
        ParserWorkload {
            name: "scroll_top_region",
            payload: top_region_scroll_payload(8_192, 2),
            chunking: Chunking::Whole,
        },
        ParserWorkload {
            name: "scroll_top_small_region",
            payload: top_region_scroll_payload(8_192, GEOMETRY.rows / 2),
            chunking: Chunking::Whole,
        },
        ParserWorkload {
            name: "rep_tabs_alt_screen",
            payload: rep_tabs_alt_payload(2_048),
            chunking: Chunking::Whole,
        },
        ParserWorkload {
            name: "osc_dcs_query",
            payload: osc_dcs_query_payload(512),
            chunking: Chunking::Whole,
        },
        ParserWorkload {
            name: "sync_update",
            payload: synchronized_update_payload(2_048),
            chunking: Chunking::Whole,
        },
    ]
}

fn state_terminal() -> Terminal<'static, 'static> {
    let mut terminal = Terminal::new(GEOMETRY.cols, GEOMETRY.rows).expect("terminal");
    terminal
        .set_scrollback_max_bytes(Some(0))
        .expect("scrollback limit");
    terminal
}

fn run_parse_state(workload: &ParserWorkload) -> u64 {
    let mut terminal = state_terminal();
    write_terminal_chunks(&mut terminal, &workload.payload, workload.chunking);
    let mut render_state = RenderState::new().expect("render state");
    let snapshot = render_state.update(&terminal).expect("state snapshot");
    let cursor_hash = snapshot
        .cursor_viewport()
        .expect("cursor viewport")
        .map_or(0, |cursor| u64::from(cursor.x) << 32 | u64::from(cursor.y));
    let mode_hash = u64::from(terminal.mode(Mode::WRAPAROUND).expect("wrap mode"));
    let cell_hash = terminal
        .grid_ref(Point::Viewport(PointCoordinate { x: 0, y: 0 }))
        .map(|_| 0x9e37_79b9_7f4a_7c15)
        .unwrap_or(0);
    hash_bytes(&workload.payload)
        ^ u64::from(snapshot.cols().expect("snapshot cols"))
        ^ (u64::from(snapshot.rows().expect("snapshot rows")) << 8)
        ^ cursor_hash
        ^ mode_hash
        ^ cell_hash
}

fn run_full_visible(workload: &ParserWorkload) -> u64 {
    let mut engine = TerminalEngine::new(GEOMETRY).expect("terminal engine");
    write_engine_chunks(&mut engine, &workload.payload, workload.chunking);
    let frame = engine.extract_frame().expect("visible parser frame");
    let text_hash = frame
        .text
        .iter()
        .fold(0xcbf2_9ce4_8422_2325_u64, |hash, ch| {
            (hash ^ u64::from(*ch as u32)).wrapping_mul(0x0000_0100_0000_01b3)
        });
    assert_eq!((frame.cols, frame.rows), (GEOMETRY.cols, GEOMETRY.rows));
    text_hash ^ (frame.cells.len() as u64) ^ ((frame.text.len() as u64) << 32)
}

fn bench_parse_state(c: &mut Criterion) {
    for workload in workloads() {
        c.bench_function(&format!("parser_state_{}", workload.name), |b| {
            b.iter_batched(
                || workload.clone(),
                |workload| black_box(run_parse_state(&workload)),
                BatchSize::SmallInput,
            )
        });
    }
}

fn bench_full_visible(c: &mut Criterion) {
    for workload in workloads() {
        c.bench_function(&format!("parser_visible_{}", workload.name), |b| {
            b.iter_batched(
                || workload.clone(),
                |workload| black_box(run_full_visible(&workload)),
                BatchSize::SmallInput,
            )
        });
    }
}

criterion_group! {
    name = benches;
    config = Criterion::default().sample_size(10).noise_threshold(0.20);
    targets = bench_parse_state, bench_full_visible,
}
criterion_main!(benches);
