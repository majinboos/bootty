use std::{env, hint::black_box};

use bootty_app::geometry::TerminalGeometry;
use bootty_terminal::terminal_engine::{
    NATIVE_MAX_SCROLLBACK, NATIVE_SCROLLBACK_BYTES_PER_ROW_ESTIMATE, TerminalEngine,
};
use criterion::{BatchSize, Criterion, criterion_group, criterion_main};

const GEOMETRY: TerminalGeometry = TerminalGeometry {
    cols: 120,
    rows: 40,
    cell_width: 9,
    cell_height: 22,
};
const DEEP_BENCH_ENV: &str = "BOOTTY_DEEP_SCROLLBACK_BENCH";

#[derive(Clone, Copy)]
enum ContentKind {
    Short,
    Wrapped,
    Unicode,
    EmojiCombining,
    AnsiOsc8TabsBox,
    VeryLongLine,
}

impl ContentKind {
    const fn name(self) -> &'static str {
        match self {
            Self::Short => "short_plain",
            Self::Wrapped => "long_wrapped",
            Self::Unicode => "unicode_wide",
            Self::EmojiCombining => "emoji_combining",
            Self::AnsiOsc8TabsBox => "ansi_osc8_tabs_box",
            Self::VeryLongLine => "very_long_line",
        }
    }

    fn append_line(self, out: &mut Vec<u8>, index: usize) {
        match self {
            Self::Short => {
                out.extend_from_slice(format!("line {index:08} target short payload").as_bytes());
            }
            Self::Wrapped => {
                out.extend_from_slice(format!("wrapped {index:08} ").as_bytes());
                out.extend_from_slice("segment ".repeat(40).as_bytes());
            }
            Self::Unicode => {
                out.extend_from_slice(
                    format!(
                        "unicode {index:08} コンニチハ Ελληνικά Кириллица عربى देवनागरी {}",
                        "┃━".repeat(18)
                    )
                    .as_bytes(),
                );
            }
            Self::EmojiCombining => {
                out.extend_from_slice(format!("emoji {index:08} ").as_bytes());
                out.extend_from_slice("🥟 👨‍👩‍👧‍👦 🇺🇳 e\u{301} a\u{0301}\u{0327} ".repeat(8).as_bytes());
            }
            Self::AnsiOsc8TabsBox => {
                out.extend_from_slice(
                    format!(
                        "\x1b[38;5;{}mansi {index:08}\x1b[0m\t┃ box ━ \x1b]8;id=row{index};https://example.invalid/{index}\x1b\\target-link\x1b]8;;\x1b\\",
                        16 + index % 200
                    )
                    .as_bytes(),
                );
            }
            Self::VeryLongLine => {
                out.extend_from_slice(format!("very-long {index:08} ").as_bytes());
                out.extend(std::iter::repeat_n(b'x', 16 * 1024));
            }
        }
        out.extend_from_slice(b"\r\n");
    }
}

#[derive(Clone, Copy)]
enum ScrollbackBudget {
    BoundedRows(usize),
    NativeBudget,
}

impl ScrollbackBudget {
    fn name(self) -> String {
        match self {
            Self::BoundedRows(rows) => format!("bounded_{rows}"),
            Self::NativeBudget => "native_budget".to_owned(),
        }
    }

    fn bytes(self) -> usize {
        match self {
            Self::BoundedRows(rows) => rows * NATIVE_SCROLLBACK_BYTES_PER_ROW_ESTIMATE,
            Self::NativeBudget => NATIVE_MAX_SCROLLBACK,
        }
    }
}

#[derive(Clone, Copy)]
struct ScrollbackScenario {
    name: &'static str,
    lines: usize,
    content: ContentKind,
    budget: ScrollbackBudget,
}

impl ScrollbackScenario {
    fn bench_name(self, prefix: &str) -> String {
        format!(
            "{prefix}_{}_{}_{}",
            self.name,
            self.content.name(),
            self.budget.name()
        )
    }
}

#[derive(Default)]
struct ScrollbackStats {
    lines: usize,
    input_bytes: usize,
    frame_cells: usize,
    frame_chars: usize,
    matches: usize,
    copied_chars: usize,
    max_scrollback_bytes: usize,
    hash: u64,
}

impl ScrollbackStats {
    fn checksum(&self) -> u64 {
        self.hash
            ^ self.lines as u64
            ^ (self.input_bytes as u64).rotate_left(7)
            ^ (self.frame_cells as u64).rotate_left(17)
            ^ (self.frame_chars as u64).rotate_left(29)
            ^ (self.matches as u64).rotate_left(37)
            ^ (self.copied_chars as u64).rotate_left(43)
            ^ (self.max_scrollback_bytes as u64).rotate_left(53)
    }
}

fn deep_scrollback_benches_enabled() -> bool {
    matches!(
        env::var(DEEP_BENCH_ENV).as_deref(),
        Ok("1" | "true" | "yes")
    )
}

fn append_lines(engine: &mut TerminalEngine, lines: usize, content: ContentKind) -> usize {
    let chunk_lines = match content {
        ContentKind::VeryLongLine => 4,
        _ => 256,
    };
    let mut written = 0;
    let mut next = 0;
    while next < lines {
        let end = (next + chunk_lines).min(lines);
        let mut chunk = Vec::with_capacity((end - next) * 128);
        for index in next..end {
            content.append_line(&mut chunk, index);
        }
        written += chunk.len();
        engine.write_vt(&chunk);
        next = end;
    }
    written
}

fn build_engine(scenario: ScrollbackScenario) -> (TerminalEngine, usize) {
    let mut engine =
        TerminalEngine::new_with_scrollback(GEOMETRY, Default::default(), scenario.budget.bytes())
            .expect("terminal engine");
    let input_bytes = append_lines(&mut engine, scenario.lines, scenario.content);
    (engine, input_bytes)
}

fn frame_stats(engine: &mut TerminalEngine) -> (usize, usize, u64) {
    let frame = engine.extract_frame().expect("scrollback frame");
    let hash = frame
        .text
        .iter()
        .fold(0xcbf2_9ce4_8422_2325_u64, |hash, ch| {
            (hash ^ u64::from(*ch as u32)).wrapping_mul(0x0000_0100_0000_01b3)
        });
    (frame.cells.len(), frame.text.len(), hash)
}

fn append_and_snapshot(scenario: ScrollbackScenario) -> u64 {
    let (mut engine, input_bytes) = build_engine(scenario);
    let (frame_cells, frame_chars, hash) = frame_stats(&mut engine);
    ScrollbackStats {
        lines: scenario.lines,
        input_bytes,
        frame_cells,
        frame_chars,
        max_scrollback_bytes: scenario.budget.bytes(),
        hash,
        ..ScrollbackStats::default()
    }
    .checksum()
}

fn contains_needle(text: &[char], needle: &[char]) -> bool {
    !needle.is_empty() && text.windows(needle.len()).any(|window| window == needle)
}

fn scroll_and_search(mut engine: TerminalEngine, pages: usize, needle: &[char]) -> u64 {
    let mut stats = ScrollbackStats::default();
    for _ in 0..pages {
        engine.scroll_viewport_delta(-(GEOMETRY.rows as isize));
        let frame = engine.extract_frame().expect("scrollback search frame");
        stats.matches += usize::from(contains_needle(&frame.text, needle));
        stats.frame_cells += frame.cells.len();
        stats.frame_chars += frame.text.len();
        stats.hash ^= frame
            .text
            .iter()
            .fold(0xcbf2_9ce4_8422_2325_u64, |hash, ch| {
                (hash ^ u64::from(*ch as u32)).wrapping_mul(0x0000_0100_0000_01b3)
            });
    }
    assert!(
        stats.matches > 0,
        "scrollback search should find replayed OSC8 text"
    );
    engine.scroll_viewport_bottom();
    stats.checksum()
}

fn scroll_and_copy(mut engine: TerminalEngine, pages: usize) -> u64 {
    let mut stats = ScrollbackStats::default();
    let mut copied =
        String::with_capacity(pages * usize::from(GEOMETRY.cols) * usize::from(GEOMETRY.rows));
    for _ in 0..pages {
        engine.scroll_viewport_delta(-(GEOMETRY.rows as isize));
        let frame = engine.extract_frame().expect("scrollback copy frame");
        copied.extend(frame.text.iter());
        copied.push('\n');
        stats.frame_cells += frame.cells.len();
        stats.frame_chars += frame.text.len();
    }
    assert!(
        !copied.is_empty(),
        "scrollback copy should collect visible text"
    );
    stats.copied_chars = copied.chars().count();
    stats.hash = copied.chars().fold(0xcbf2_9ce4_8422_2325_u64, |hash, ch| {
        (hash ^ u64::from(ch as u32)).wrapping_mul(0x0000_0100_0000_01b3)
    });
    stats.checksum()
}

fn clear_scrollback(mut engine: TerminalEngine) -> u64 {
    engine.write_vt(b"\x1b[3J");
    assert_eq!(engine.grid_size(), (GEOMETRY.cols, GEOMETRY.rows));
    engine.scroll_viewport_bottom();
    let (frame_cells, frame_chars, hash) = frame_stats(&mut engine);
    ScrollbackStats {
        frame_cells,
        frame_chars,
        hash,
        ..ScrollbackStats::default()
    }
    .checksum()
}

fn reflow_scrollback(mut engine: TerminalEngine) -> u64 {
    let narrow = TerminalGeometry {
        cols: 80,
        rows: 24,
        cell_width: 9,
        cell_height: 22,
    };
    let wide = TerminalGeometry {
        cols: 160,
        rows: 48,
        cell_width: 9,
        cell_height: 22,
    };
    engine.resize(narrow).expect("narrow scrollback reflow");
    assert_eq!(engine.grid_size(), (narrow.cols, narrow.rows));
    let narrow_stats = frame_stats(&mut engine);
    engine.resize(wide).expect("wide scrollback reflow");
    assert_eq!(engine.grid_size(), (wide.cols, wide.rows));
    let wide_stats = frame_stats(&mut engine);
    ScrollbackStats {
        frame_cells: narrow_stats.0 + wide_stats.0,
        frame_chars: narrow_stats.1 + wide_stats.1,
        hash: narrow_stats.2 ^ wide_stats.2,
        ..ScrollbackStats::default()
    }
    .checksum()
}

fn default_append_scenarios() -> Vec<ScrollbackScenario> {
    vec![
        ScrollbackScenario {
            name: "10k",
            lines: 10_000,
            content: ContentKind::Short,
            budget: ScrollbackBudget::BoundedRows(10_000),
        },
        ScrollbackScenario {
            name: "10k",
            lines: 10_000,
            content: ContentKind::Wrapped,
            budget: ScrollbackBudget::BoundedRows(10_000),
        },
        ScrollbackScenario {
            name: "10k",
            lines: 10_000,
            content: ContentKind::Unicode,
            budget: ScrollbackBudget::BoundedRows(10_000),
        },
        ScrollbackScenario {
            name: "10k",
            lines: 10_000,
            content: ContentKind::EmojiCombining,
            budget: ScrollbackBudget::BoundedRows(10_000),
        },
        ScrollbackScenario {
            name: "10k",
            lines: 10_000,
            content: ContentKind::AnsiOsc8TabsBox,
            budget: ScrollbackBudget::BoundedRows(10_000),
        },
        ScrollbackScenario {
            name: "100k",
            lines: 100_000,
            content: ContentKind::Short,
            budget: ScrollbackBudget::NativeBudget,
        },
        ScrollbackScenario {
            name: "100k",
            lines: 100_000,
            content: ContentKind::AnsiOsc8TabsBox,
            budget: ScrollbackBudget::NativeBudget,
        },
        ScrollbackScenario {
            name: "1mb_line",
            lines: 64,
            content: ContentKind::VeryLongLine,
            budget: ScrollbackBudget::NativeBudget,
        },
    ]
}

fn deep_append_scenarios() -> Vec<ScrollbackScenario> {
    if !deep_scrollback_benches_enabled() {
        return Vec::new();
    }
    vec![
        ScrollbackScenario {
            name: "1m",
            lines: 1_000_000,
            content: ContentKind::Short,
            budget: ScrollbackBudget::NativeBudget,
        },
        ScrollbackScenario {
            name: "10m",
            lines: 10_000_000,
            content: ContentKind::Short,
            budget: ScrollbackBudget::NativeBudget,
        },
        ScrollbackScenario {
            name: "16mb_line",
            lines: 1_024,
            content: ContentKind::VeryLongLine,
            budget: ScrollbackBudget::NativeBudget,
        },
    ]
}

fn operation_scenario() -> ScrollbackScenario {
    ScrollbackScenario {
        name: "100k",
        lines: 100_000,
        content: ContentKind::AnsiOsc8TabsBox,
        budget: ScrollbackBudget::NativeBudget,
    }
}

fn bench_append_and_memory(c: &mut Criterion) {
    let scenarios = default_append_scenarios()
        .into_iter()
        .chain(deep_append_scenarios())
        .collect::<Vec<_>>();
    for scenario in scenarios {
        c.bench_function(&scenario.bench_name("scrollback_append_snapshot"), |b| {
            b.iter(|| black_box(append_and_snapshot(scenario)))
        });
    }
}

fn bench_search_copy_clear_reflow(c: &mut Criterion) {
    let scenario = operation_scenario();
    let needle = "target-link".chars().collect::<Vec<_>>();
    c.bench_function("scrollback_search_100k_ansi_osc8_pages", |b| {
        b.iter_batched(
            || build_engine(scenario).0,
            |engine| black_box(scroll_and_search(engine, 64, &needle)),
            BatchSize::LargeInput,
        )
    });
    c.bench_function("scrollback_copy_100k_ansi_osc8_pages", |b| {
        b.iter_batched(
            || build_engine(scenario).0,
            |engine| black_box(scroll_and_copy(engine, 64)),
            BatchSize::LargeInput,
        )
    });
    c.bench_function("scrollback_clear_reclaim_100k_ansi_osc8", |b| {
        b.iter_batched(
            || build_engine(scenario).0,
            |engine| black_box(clear_scrollback(engine)),
            BatchSize::LargeInput,
        )
    });
    c.bench_function("scrollback_reflow_100k_ansi_osc8", |b| {
        b.iter_batched(
            || build_engine(scenario).0,
            |engine| black_box(reflow_scrollback(engine)),
            BatchSize::LargeInput,
        )
    });
}

criterion_group! {
    name = benches;
    config = Criterion::default().sample_size(10).noise_threshold(0.20);
    targets = bench_append_and_memory, bench_search_copy_clear_reflow,
}
criterion_main!(benches);
