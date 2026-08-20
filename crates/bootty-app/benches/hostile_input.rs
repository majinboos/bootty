use std::hint::black_box;

use bootty_app::geometry::TerminalGeometry;
use bootty_terminal::terminal_engine::TerminalEngine;
use criterion::{BatchSize, Criterion, criterion_group, criterion_main};

fn terminal_engine(cols: u16, rows: u16) -> TerminalEngine {
    TerminalEngine::new(TerminalGeometry {
        cols,
        rows,
        cell_width: 9,
        cell_height: 22,
    })
    .expect("terminal engine")
}

fn deterministic_bytes(len: usize, seed: u64) -> Vec<u8> {
    let mut state = seed;
    let mut out = Vec::with_capacity(len);
    for _ in 0..len {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        out.push((state & 0xff) as u8);
    }
    out
}

fn invalid_utf8_payload(len: usize) -> Vec<u8> {
    let mut bytes = deterministic_bytes(len, 0x5eed_f00d);
    for index in (0..bytes.len()).step_by(17) {
        bytes[index] = 0xff;
    }
    for index in (7..bytes.len()).step_by(29) {
        bytes[index] = 0xc0;
    }
    bytes
}

fn grammar_biased_escape_storm(commands: usize) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(commands * 24);
    let fragments: [&[u8]; 14] = [
        b"\x1b[38;2;255;0;0m",
        b"\x1b[48;5;123m",
        b"\x1b[?25l",
        b"\x1b[?25h",
        b"\x1b[2J",
        b"\x1b[H",
        b"\x1b[999;999H",
        b"\x1b[1;1r",
        b"\x1b[?1049h",
        b"\x1b[?1049l",
        b"\x1b[?2026h",
        b"\x1b[?2026l",
        b"text payload ",
        b"\r\n",
    ];
    for index in 0..commands {
        bytes.extend_from_slice(fragments[index % fragments.len()]);
        if index % 11 == 0 {
            bytes.extend_from_slice(b"\x1b[");
        }
    }
    bytes
}

fn query_storm(commands: usize) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(commands * 16);
    let fragments: [&[u8]; 6] = [
        b"\x1b[c",
        b"\x1b[0c",
        b"\x1b[5n",
        b"\x1b[6n",
        b"\x1bP$qm\x1b\\",
        b"\x1bP$q q\x1b\\",
    ];
    for index in 0..commands {
        bytes.extend_from_slice(fragments[index % fragments.len()]);
    }
    bytes
}

fn reset_storm(commands: usize) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(commands * 10);
    for index in 0..commands {
        if index % 2 == 0 {
            bytes.extend_from_slice(b"\x1bc");
        } else {
            bytes.extend_from_slice(b"\x1b[!p");
        }
        bytes.extend_from_slice(b"ok\r\n");
    }
    bytes
}

fn unterminated_osc_payload(len: usize) -> Vec<u8> {
    let mut bytes = b"\x1b]52;c;".to_vec();
    bytes.extend(std::iter::repeat_n(b'A', len));
    bytes
}

fn huge_clipboard_payload(len: usize) -> Vec<u8> {
    let mut bytes = b"\x1b]52;c;".to_vec();
    bytes.extend(std::iter::repeat_n(b'Q', len));
    bytes.extend_from_slice(b"\x1b\\");
    bytes
}

fn huge_hyperlink_payload(len: usize) -> Vec<u8> {
    let mut bytes = b"\x1b]8;id=hostile;https://example.invalid/".to_vec();
    bytes.extend(std::iter::repeat_n(b'x', len));
    bytes.extend_from_slice(b"\x1b\\linked text\x1b]8;;\x1b\\");
    bytes
}

fn unterminated_dcs_payload(len: usize) -> Vec<u8> {
    let mut bytes = b"\x1bP".to_vec();
    bytes.extend(std::iter::repeat_n(b'q', len));
    bytes
}

fn malformed_kitty_graphics_payload(len: usize) -> Vec<u8> {
    let mut bytes = b"\x1b_Ga=T,f=100,i=9999,s=2048,v=2048;".to_vec();
    bytes.extend(std::iter::repeat_n(b'!', len));
    bytes.extend_from_slice(b"\x1b\\");
    bytes
}

fn malformed_sixel_payload(len: usize) -> Vec<u8> {
    let mut bytes = b"\x1bPq\"1;1;2048;2048#1;2;255;0;0".to_vec();
    bytes.extend(std::iter::repeat_n(b'?', len));
    bytes.extend_from_slice(b"\x1b\\");
    bytes
}

fn long_line_payload(len: usize) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(len + 3);
    bytes.extend(std::iter::repeat_n(b'L', len));
    bytes.extend_from_slice(b"\r\n");
    bytes
}

fn image_quota_abuse_payload(images: usize) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(images * 96);
    for image_id in 0..images {
        bytes.extend_from_slice(
            format!("\x1b_Ga=T,t=d,f=24,i={image_id},p={image_id},s=1,v=1,q=1;////\x1b\\")
                .as_bytes(),
        );
        if image_id % 17 == 0 {
            bytes.extend_from_slice(format!("\x1b_Ga=d,d=i,i={image_id}\x1b\\").as_bytes());
        }
    }
    bytes
}

fn nested_sync_reset_payload(rounds: usize) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(rounds * 32);
    for round in 0..rounds {
        bytes.extend_from_slice(b"\x1b[?2026h");
        if round % 3 == 0 {
            bytes.extend_from_slice(b"\x1b[?2026h");
        }
        bytes.extend_from_slice(format!("sync round {round}\r\n").as_bytes());
        if round % 5 == 0 {
            bytes.extend_from_slice(b"\x1b[!p");
        }
        bytes.extend_from_slice(b"\x1b[?2026l");
    }
    bytes
}

fn grammar_biased_fuzz_payload(len: usize) -> Vec<u8> {
    let atoms: [&[u8]; 16] = [
        b"\x1b[",
        b"\x1b]",
        b"\x1bP",
        b"\x1b_G",
        b"999999999",
        b";",
        b"?2026h",
        b"?1049l",
        b"38;2;1;2;3m",
        b"\x1b\\",
        b"\x07",
        b"\r\n",
        b"payload",
        b"\xff\xc0\x80",
        b"\x1bc",
        b"\x1b[!p",
    ];
    let mut out = Vec::with_capacity(len);
    let mut state = 0x0ddc_0ffe_u64;
    while out.len() < len {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        out.extend_from_slice(atoms[state as usize % atoms.len()]);
    }
    out.truncate(len);
    out
}

fn mixed_hostile_corpus() -> Vec<Vec<u8>> {
    vec![
        invalid_utf8_payload(16 * 1024),
        grammar_biased_escape_storm(512),
        query_storm(512),
        reset_storm(128),
        unterminated_osc_payload(16 * 1024),
        unterminated_dcs_payload(16 * 1024),
        malformed_kitty_graphics_payload(16 * 1024),
        huge_hyperlink_payload(16 * 1024),
        huge_clipboard_payload(16 * 1024),
        malformed_sixel_payload(16 * 1024),
    ]
}

fn extended_hostile_corpus() -> Vec<Vec<u8>> {
    let mut corpus = mixed_hostile_corpus();
    corpus.extend([
        image_quota_abuse_payload(512),
        nested_sync_reset_payload(512),
        grammar_biased_fuzz_payload(128 * 1024),
        long_line_payload(2 * 1024 * 1024),
    ]);
    corpus
}

fn write_and_extract(mut engine: TerminalEngine, payload: &[u8]) -> usize {
    engine.write_vt(payload);
    let frame = engine
        .extract_frame()
        .expect("extract frame after hostile input");
    black_box((
        frame.cells.len(),
        frame.text.len(),
        frame.images.placements.len(),
    ));
    frame.text.len()
}

fn write_reset_and_extract(mut engine: TerminalEngine, payload: &[u8]) -> (usize, usize) {
    engine.write_vt(payload);
    engine.write_vt(b"\x1bcafter hostile reset\r\n");
    let frame = engine.extract_frame().expect("extract frame after reset");
    (frame.cells.len(), frame.text.len())
}

fn bench_invalid_and_random_bytes(c: &mut Criterion) {
    c.bench_function("hostile_invalid_utf8_256kb_extract", |b| {
        let payload = invalid_utf8_payload(256 * 1024);
        b.iter_batched(
            || terminal_engine(120, 40),
            |engine| black_box(write_and_extract(engine, black_box(&payload))),
            BatchSize::SmallInput,
        )
    });

    c.bench_function("hostile_random_bytes_256kb_extract", |b| {
        let payload = deterministic_bytes(256 * 1024, 0xfeed_face_cafe_babe);
        b.iter_batched(
            || terminal_engine(120, 40),
            |engine| black_box(write_and_extract(engine, black_box(&payload))),
            BatchSize::SmallInput,
        )
    });
}

fn bench_escape_and_query_storms(c: &mut Criterion) {
    c.bench_function("hostile_grammar_escape_storm_4096_extract", |b| {
        let payload = grammar_biased_escape_storm(4096);
        b.iter_batched(
            || terminal_engine(120, 40),
            |engine| black_box(write_and_extract(engine, black_box(&payload))),
            BatchSize::SmallInput,
        )
    });

    c.bench_function("hostile_query_storm_4096_extract", |b| {
        let payload = query_storm(4096);
        b.iter_batched(
            || terminal_engine(120, 40),
            |engine| black_box(write_and_extract(engine, black_box(&payload))),
            BatchSize::SmallInput,
        )
    });

    c.bench_function("hostile_reset_storm_1024_extract", |b| {
        let payload = reset_storm(1024);
        b.iter_batched(
            || terminal_engine(120, 40),
            |engine| black_box(write_and_extract(engine, black_box(&payload))),
            BatchSize::SmallInput,
        )
    });
}

fn bench_unterminated_and_huge_controls(c: &mut Criterion) {
    c.bench_function("hostile_unterminated_osc_256kb_extract", |b| {
        let payload = unterminated_osc_payload(256 * 1024);
        b.iter_batched(
            || terminal_engine(120, 40),
            |engine| black_box(write_and_extract(engine, black_box(&payload))),
            BatchSize::SmallInput,
        )
    });

    c.bench_function("hostile_unterminated_dcs_256kb_extract", |b| {
        let payload = unterminated_dcs_payload(256 * 1024);
        b.iter_batched(
            || terminal_engine(120, 40),
            |engine| black_box(write_and_extract(engine, black_box(&payload))),
            BatchSize::SmallInput,
        )
    });

    c.bench_function("hostile_osc8_hyperlink_256kb_extract", |b| {
        let payload = huge_hyperlink_payload(256 * 1024);
        b.iter_batched(
            || terminal_engine(120, 40),
            |engine| black_box(write_and_extract(engine, black_box(&payload))),
            BatchSize::SmallInput,
        )
    });

    c.bench_function("hostile_osc52_clipboard_256kb_extract", |b| {
        let payload = huge_clipboard_payload(256 * 1024);
        b.iter_batched(
            || terminal_engine(120, 40),
            |engine| black_box(write_and_extract(engine, black_box(&payload))),
            BatchSize::SmallInput,
        )
    });

    c.bench_function("hostile_malformed_kitty_graphics_256kb_extract", |b| {
        let payload = malformed_kitty_graphics_payload(256 * 1024);
        b.iter_batched(
            || terminal_engine(120, 40),
            |engine| black_box(write_and_extract(engine, black_box(&payload))),
            BatchSize::SmallInput,
        )
    });

    c.bench_function("hostile_malformed_sixel_256kb_extract", |b| {
        let payload = malformed_sixel_payload(256 * 1024);
        b.iter_batched(
            || terminal_engine(120, 40),
            |engine| black_box(write_and_extract(engine, black_box(&payload))),
            BatchSize::SmallInput,
        )
    });
}

fn bench_long_lines_and_soak(c: &mut Criterion) {
    c.bench_function("hostile_long_line_1mb_extract", |b| {
        let payload = long_line_payload(1024 * 1024);
        b.iter_batched(
            || terminal_engine(120, 40),
            |engine| black_box(write_and_extract(engine, black_box(&payload))),
            BatchSize::SmallInput,
        )
    });

    c.bench_function("hostile_long_line_16mb_write", |b| {
        let payload = long_line_payload(16 * 1024 * 1024);
        b.iter_batched(
            || terminal_engine(120, 40),
            |mut engine| {
                engine.write_vt(black_box(&payload));
                black_box(engine);
            },
            BatchSize::SmallInput,
        )
    });

    c.bench_function("hostile_mixed_soak_256_rounds", |b| {
        let corpus = mixed_hostile_corpus();
        b.iter_batched(
            || terminal_engine(120, 40),
            |mut engine| {
                for round in 0..256 {
                    let payload = &corpus[round % corpus.len()];
                    engine.write_vt(black_box(payload));
                    if round % 31 == 0 {
                        engine.write_vt(b"\x1b[!psoak checkpoint\r\n");
                    }
                }
                let frame = engine.extract_frame().expect("extract frame after soak");
                black_box((
                    frame.cells.len(),
                    frame.text.len(),
                    frame.images.placements.len(),
                ))
            },
            BatchSize::SmallInput,
        )
    });

    c.bench_function("hostile_recovery_after_mixed_corpus", |b| {
        let corpus = mixed_hostile_corpus().concat();
        b.iter_batched(
            || terminal_engine(120, 40),
            |engine| black_box(write_reset_and_extract(engine, black_box(&corpus))),
            BatchSize::SmallInput,
        )
    });
}

fn bench_fuzz_soak_and_recovery_gates(c: &mut Criterion) {
    c.bench_function("hostile_grammar_biased_fuzz_512kb_extract", |b| {
        let payload = grammar_biased_fuzz_payload(512 * 1024);
        b.iter_batched(
            || terminal_engine(120, 40),
            |engine| black_box(write_and_extract(engine, black_box(&payload))),
            BatchSize::SmallInput,
        )
    });

    c.bench_function("hostile_image_quota_abuse_4096_extract", |b| {
        let payload = image_quota_abuse_payload(4096);
        b.iter_batched(
            || terminal_engine(120, 40),
            |engine| black_box(write_and_extract(engine, black_box(&payload))),
            BatchSize::SmallInput,
        )
    });

    c.bench_function("hostile_nested_sync_reset_4096_extract", |b| {
        let payload = nested_sync_reset_payload(4096);
        b.iter_batched(
            || terminal_engine(120, 40),
            |engine| black_box(write_and_extract(engine, black_box(&payload))),
            BatchSize::SmallInput,
        )
    });

    c.bench_function("hostile_extended_recovery_ladder", |b| {
        let corpus = extended_hostile_corpus();
        b.iter_batched(
            || terminal_engine(120, 40),
            |mut engine| {
                let mut checksum = 0_usize;
                for payload in &corpus {
                    engine.write_vt(black_box(payload));
                    engine.write_vt(b"\x1bcafter hostile step\r\n");
                    let frame = engine.extract_frame().expect("recover after hostile step");
                    checksum ^=
                        frame.cells.len() ^ frame.text.len() ^ frame.images.placements.len();
                }
                black_box(checksum)
            },
            BatchSize::SmallInput,
        )
    });
}

criterion_group!(
    name = benches;
    config = Criterion::default().noise_threshold(0.15);
    targets =
        bench_invalid_and_random_bytes,
        bench_escape_and_query_storms,
        bench_unterminated_and_huge_controls,
        bench_long_lines_and_soak,
        bench_fuzz_soak_and_recovery_gates
);
criterion_main!(benches);
