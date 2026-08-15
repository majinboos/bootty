//! Resource-utilization report for the per-frame pipeline.
//!
//! The timing benches answer "how long"; this answers "how much work and how
//! much memory churn" per layer for a localized (single-row) edit, and states
//! the theoretical floor for each. Run:
//!
//! ```text
//! cargo bench -p bootty-app --bench pipeline_resources
//! ```
//!
//! Floors:
//! - work-ratio: a localized edit changes `dirty_rows / rows` of the screen, so
//!   an ideal incremental layer touches that fraction of the cells. `extract` is
//!   incremental; `plan` and `from_plan` still process every cell (ratio 1.0).
//! - allocations: a warmed pipeline should allocate nothing per frame (pooled
//!   buffers), so the floor is 0 allocations / 0 bytes.

use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use bootty_app::{
    geometry::{CellMetrics, SurfaceRect, TerminalGeometry, TerminalPadding, TerminalSurface},
    paint_plan::PaintPlanner,
    terminal::TerminalEngine,
    terminal_render::{RenderFramePool, TerminalRenderFrame},
    terminal_text::{TerminalTextConfig, TerminalTextContract},
};

static ALLOC_COUNT: AtomicU64 = AtomicU64::new(0);
static ALLOC_BYTES: AtomicU64 = AtomicU64::new(0);

struct CountingAllocator;

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOC_COUNT.fetch_add(1, Ordering::Relaxed);
        ALLOC_BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOC_COUNT.fetch_add(1, Ordering::Relaxed);
        if new_size > layout.size() {
            ALLOC_BYTES.fetch_add((new_size - layout.size()) as u64, Ordering::Relaxed);
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: CountingAllocator = CountingAllocator;

struct Sample {
    allocs: u64,
    bytes: u64,
    micros: f64,
}

fn measure<T>(f: impl FnOnce() -> T) -> (T, Sample) {
    let c0 = ALLOC_COUNT.load(Ordering::Relaxed);
    let b0 = ALLOC_BYTES.load(Ordering::Relaxed);
    let start = Instant::now();
    let value = f();
    let micros = start.elapsed().as_nanos() as f64 / 1000.0;
    let sample = Sample {
        allocs: ALLOC_COUNT.load(Ordering::Relaxed) - c0,
        bytes: ALLOC_BYTES.load(Ordering::Relaxed) - b0,
        micros,
    };
    (value, sample)
}

fn filled_engine(cols: u16, rows: u16, colored: bool) -> TerminalEngine {
    let mut engine = TerminalEngine::new(TerminalGeometry {
        cols,
        rows,
        cell_width: 9,
        cell_height: 22,
    })
    .expect("engine");
    for row in 0..rows {
        let line = if colored {
            format!(
                "\x1b[{};1H\x1b[1;38;2;125;207;255mrow {row:03}\x1b[0m \
                 \x1b[48;5;238;38;5;{}mindexed\x1b[0m \
                 \x1b[3;4;38;2;200;160;90mstyled run with assorted glyphs 0123456789\x1b[0m",
                row + 1,
                16 + row % 216,
            )
        } else {
            format!(
                "\x1b[{};1Hrow {row:03}  abcdefghijklmnopqrstuvwxyz  0123456789",
                row + 1
            )
        };
        engine.write_vt(line.as_bytes());
    }
    engine
}

fn surface_for(cols: u16, rows: u16) -> TerminalSurface {
    TerminalSurface::for_logical_size(
        f32::from(cols) * 9.0 + 20.0,
        f32::from(rows) * 22.0 + 20.0,
        CellMetrics::new(9.0, 22.0),
        TerminalPadding::uniform(10.0),
    )
}

fn report_scenario(name: &str, cols: u16, rows: u16, colored: bool) {
    let mut engine = filled_engine(cols, rows, colored);
    let surface = surface_for(cols, rows);
    let mut planner = PaintPlanner::default();
    let config = TerminalTextConfig::default();
    // Persistent render frame + pool, mirroring the production render cache: the pool
    // recycles the previous frame's command buffer and text strings, so a warm steady
    // state should rebuild `from_plan` with zero allocations.
    let mut pool = RenderFramePool::default();
    let mut render_frame = TerminalRenderFrame {
        surface: SurfaceRect::from_min_size(0.0, 0.0, 0.0, 0.0),
        commands: Vec::new(),
    };

    // Warm steady state: drive many consecutive localized edits so the run-string pool,
    // the incremental-extraction row cache, AND the render-frame pool are all populated.
    // (A no-op extract takes the clean-reuse path and never warms row_cache, so each
    // warm pass must actually mutate a row.) The iteration count must be high enough to
    // SATURATE the string pools: pooled buffers grow to the longest run they ever hold,
    // and LIFO reuse means a buffer must be handed to a long run at least once before it
    // stops reallocating. Too few passes (e.g. 6) reports dozens of phantom allocations
    // that are pure warm-up churn, not the steady-state floor a long-running renderer hits.
    for i in 0..60 {
        engine.write_vt(format!("\x1b[{};1Hwarm{i:02}", (u32::from(rows) / 3) + i + 1).as_bytes());
        let frame = engine.extract_frame().expect("frame").clone();
        let plan = planner.plan(surface, &frame, 16.0).clone();
        let contract = TerminalTextContract::for_terminal_paint_plan(&plan, &config);
        pool.rebuild_from_plan(&mut render_frame, &plan, &contract);
        black_box(render_frame.commands.len());
    }

    // Apply a localized edit: rewrite one row's leading cells.
    engine.write_vt(format!("\x1b[{};1Hedited", rows / 2 + 1).as_bytes());

    let ((dirty_rows, total_cells), extract) = measure(|| {
        let frame = engine.extract_frame().expect("frame");
        (frame.stats.dirty_rows, frame.cells.len())
    });
    let frame = engine.extract_frame().expect("frame").clone();

    let (_, plan_sample) = measure(|| planner.plan(surface, &frame, 16.0).text_runs.len());
    let plan = planner.plan(surface, &frame, 16.0).clone();
    let contract = TerminalTextContract::for_terminal_paint_plan(&plan, &config);
    let (_, render_sample) = measure(|| {
        pool.rebuild_from_plan(&mut render_frame, &plan, &contract);
        render_frame.commands.len()
    });

    let work_ratio = dirty_rows as f64 / f64::from(rows).max(1.0);
    println!(
        "\n{name}  ({cols}x{rows}, {total_cells} cells, {dirty_rows} dirty rows, work-ratio {:.3})",
        work_ratio
    );
    println!(
        "  {:<10} {:>8} {:>11} {:>10}",
        "layer", "allocs", "alloc_bytes", "time_us"
    );
    let row = |layer: &str, s: &Sample| {
        println!(
            "  {:<10} {:>8} {:>11} {:>10.1}",
            layer, s.allocs, s.bytes, s.micros
        );
    };
    row("extract", &extract);
    row("plan", &plan_sample);
    row("from_plan", &render_sample);
}

fn main() {
    println!("per-frame resource utilization for a single localized edit");
    println!("floors: work-ratio -> dirty_rows/rows; allocations -> 0/frame (warmed pools)");
    report_scenario("plain_shell", 120, 40, false);
    report_scenario("colored_shell", 180, 80, true);
    report_scenario("wide_colored", 240, 90, true);
}
