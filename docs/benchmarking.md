# Benchmarking process

Bootty keeps routine validation fast while preserving benchmark seams for deeper
performance work. This document is the stable process guide for benchmark use,
claim hygiene, result schemas, and host-specific evidence collection. Local
measurements and hillclimb findings are vault research artifacts.

## Runtime guardrails

- PTY drain work must stay bounded so large output bursts backlog and catch up
  instead of monopolizing UI frames.
- Frame extraction and paint planning should remain below an interactive frame
  budget for ordinary prompt usage.
- Text run grouping should keep run counts much lower than visible cell counts
  for normal shell output.
- Idle frames should avoid terminal work unless terminal state, cursor blink, or
  chrome repaint state changed.
- WGPU changes must preserve visual parity while separating CPU staging,
  render-pass, and first-upload costs.
- Startup, PTY, parser, renderer, input, image, and app-frame changes must keep
  benchmark targets compileable without making the default validation gate run
  measured Criterion suites.

## Routine validation policy

Default local validation compiles only the core benchmark harness:

```bash
cargo test -p bootty-app --bench paint_plan --no-run
```

CI keeps the blocking benchmark gate deliberately small: it validates benchmark
metadata/dashboard helpers and compile-checks the core `paint_plan` harness.
Local repro:

```bash
scripts/run-benchmark-suite.sh --ci-smoke --output artifacts/benchmark-reproduction/ci
```

The full benchmark reproduction suite runs in the nightly benchmark workflow.
It compile-checks all checked-in benchmark targets and runs the short measured
subset that used to block push CI:

```bash
scripts/run-benchmark-suite.sh --quick --output artifacts/benchmark-reproduction/overnight
```

Run task-specific compile-only checks when touching those surfaces. Keep measured
Criterion suites out of the default local gate unless validation policy changes
explicitly.

Cross-rs CI covers portable crates on non-host targets. Full GUI/app coverage
stays on native Linux, macOS, and Windows jobs because the released cross-rs
images are older Linux bases and do not carry Bootty's current GTK/WebKit app
stack.

| Surface | Compile-only check |
| --- | --- |
| Startup/config/session-order | `cargo test -p bootty-app --bench startup_config --no-run` |
| Competitive startup milestones | `cargo test -p bootty-app --bench startup_milestones --no-run` |
| WGPU staging/render pass | `cargo test -p bootty-app --bench paint_plan_wgpu --no-run` |
| Kitty graphics | `cargo test -p bootty-app --bench kitty_image --no-run` |
| Graphics protocols beyond Kitty | `cargo test -p bootty-app --bench graphics_protocols --no-run` |
| App frame/chrome orchestration | `cargo test -p bootty-app --bench app_frame --no-run` |
| Font shaping/text atlas | `cargo test -p bootty-app --bench text_atlas --no-run` |
| PTY drain/backpressure | `cargo test -p bootty-runtime --bench pty_drain --no-run` |
| Flood responsiveness | `cargo test -p bootty-runtime --bench flood_response --no-run` |
| Resize/reflow | `cargo test -p bootty-app --bench resize_reflow --no-run` |
| Scrollback memory/search/copy/clear | `cargo test -p bootty-app --bench scrollback --no-run` |
| Parser/control sequences | `cargo test -p bootty-app --bench parser_control --no-run` |
| Render throughput/frame pacing | `cargo test -p bootty-app --bench render_pacing --no-run` |
| Input latency/responsiveness | `cargo test -p bootty-app --bench input_latency --no-run` |
| Idle overhead/wakeups/memory/power | `cargo test -p bootty-app --bench idle_overhead --no-run` |
| Power-sensitive render workload models | `cargo test -p bootty-app --bench power_thermal --no-run` |
| Keyboard/mouse/paste/clipboard/IME protocols | `cargo test -p bootty-app --bench input_protocols --no-run` |
| Hostile input/recovery | `cargo test -p bootty-app --bench hostile_input --no-run` |
| Panes/tabs/multi-window | `cargo test -p bootty-app --bench panes_multiwindow --no-run` |
| Multiplexer performance/passthrough | `cargo test -p bootty-app --bench multiplexer --no-run` |
| Remote session replay | `cargo test -p bootty-app --bench remote_session --no-run` |
| Real application replay | `cargo test -p bootty-app --bench real_app_replay --no-run` |

## Measured runs

Use measured Criterion runs only while investigating or validating the relevant
surface. For quick local comparisons, prefer short runs such as:

```bash
cargo bench -p bootty-app --bench <target> -- --sample-size 10 --measurement-time 0.2 --warm-up-time 0.1
```

Use longer runs, raw result exports, repeated randomized runs, and correctness
status before making competitive claims.

## Benchmark target map

| Target | Use when changing |
| --- | --- |
| `paint_plan` | render-frame extraction, paint planning, text grouping, sprite routing, input routing, keybinding lookup, egui chrome, and sidebar metadata slicing |
| `startup_config` | config loading, theme resolution, keybind construction, font-size preference writes, and SQLite-backed session ordering |
| `startup_milestones` | config cases, native option construction, app-state readiness, PTY first frame, sequential/concurrent window models, and startup resource snapshots |
| `paint_plan_wgpu` | WGPU CPU staging, glyph/sprite/image upload preparation, render-pass encode/submit/wait, and first-frame upload paths |
| `kitty_image` | Kitty graphics parsing, PNG decode, placement churn, storage cleanup, mixed text/image extraction, and image upload |
| `graphics_protocols` | iTerm2 image OSC, Sixel, Unicode/block fallback, unsupported-feature accounting, and text/image render command preparation |
| `app_frame` | app-state update, terminal/sidebar/status layout, frame orchestration, and sidebar/status refresh scheduling |
| `text_atlas` | font fallback, shaping, glyph cache keys, emoji/symbol rasterization, atlas reuse/growth, ligatures, Unicode classes, and atlas upload preparation |
| `pty_drain` | PTY reader queueing, bounded drain slices, burst/catch-up VT writes, backlog policy, and frame publication cadence |
| `flood_response` | deterministic flood replays, visible Ctrl-C/input/scroll injection, and live PTY Ctrl-C-to-child-exit latency |
| `resize_reflow` | fixed/random resize cycles, drag model, HiDPI/monitor moves, fullscreen toggles, main-screen reflow, alternate screen, scrollback, and image-adjacent content |
| `scrollback` | append/memory snapshots, bounded/native scrollback budgets, search/copy, clear/reclaim, and reflow |
| `parser_control` | direct parser/state update and full visible frame modes for ASCII, split UTF-8/CSI, SGR/truecolor, cursor motion, scroll margins, insert/delete, erase, OSC/DCS/query storms, and synchronized updates |
| `render_pacing` | CPU-side pacing model for cursor-only, single-cell, statusline, row/column, random cells, full repaint, scroll, alternate screen, and target Hz budgets |
| `input_latency` | internal encode-to-visible-frame contribution for shell/raw/readline/editor/tmux/SSH echo, repeat bursts, and redraw/flood contention |
| `idle_overhead` | idle tick/repaint models for prompts, tabs, panes, ligatures, IME preedit, shell integration, and notifications |
| `power_thermal` | modeled idle/typing/editor/flood/animation render workloads for power-sensitive profiling; pair with external telemetry for real power or thermal claims |
| `input_protocols` | keyboard protocols, modifiers, function/repeat/dead-key/AltGr cases, mouse tracking, paste, OSC 52, and IME text handling |
| `hostile_input` | invalid bytes, malformed controls, huge OSC/DCS payloads, reset storms, long lines, fuzz streams, image quota abuse, and recovery ladders |
| `panes_multiwindow` | native/tmux-equivalent tabs and panes, active/inactive panes, all-panes-tailing updates, tab switching, create/close models, and aggregate multi-window rendering |
| `multiplexer` | terminal-alone, native mux, tmux, screen, tmux-over-SSH, nested SSH/tmux, passthrough/fallback, feature classification, latency delta, and render overhead |
| `remote_session` | virtual-network replay for SSH, mosh, docker/podman exec, ConPTY-like sessions, resize propagation, and feature-degradation classification |
| `real_app_replay` | deterministic replay streams for editors, fuzzy finders, diffs, build logs, log tails, dashboards, mux sessions, and AI/code-generation output |

## Reproduction and dashboard workflow

Run the fast blocking-CI benchmark smoke locally:

```bash
scripts/run-benchmark-suite.sh --ci-smoke --output artifacts/benchmark-reproduction/ci
```

Compile checked-in benchmark targets and record command metadata:

```bash
scripts/run-benchmark-suite.sh --output artifacts/benchmark-reproduction/local
```

Run the nightly/full reproduction shape, including a small measured subset:

```bash
scripts/run-benchmark-suite.sh --quick --output artifacts/benchmark-reproduction/overnight
```

Validate launcher/profile manifests:

```bash
scripts/validate-benchmark-manifests.py
```

Normalize measured JSONL rows and build dashboard artifacts:

```bash
scripts/build-benchmark-dashboard.py artifacts/results/*.jsonl \
  --output-dir artifacts/benchmark-dashboard \
  --strict
```

The dashboard writes `raw-normalized.jsonl`, `summary.csv`, and `dashboard.md`.
The row schema lives in `benchmarks/result-schema.json`; launcher/profile inputs
live in `benchmarks/launcher-matrix.json` and `benchmarks/profiles.json`.

## Host-specific evidence

These scripts are opt-in and may depend on installed tools, live services, GUI
sessions, or privileges:

```bash
scripts/run-terminal-public-benchmarks.py \
  --terminal bootty --terminal kitty --terminal alacritty --terminal wezterm \
  --tool vtebench --tool termbench \
  --min-bytes 67108864 \
  --resource-sample \
  --output artifacts/external-benchmarks/terminal-public.jsonl

scripts/run-external-benchmark-adapters.py --output artifacts/external-benchmarks/results.jsonl
scripts/run-external-benchmark-adapters.py \
  --typometer-csv artifacts/latency/typometer.csv \
  --software-latency-csv artifacts/latency/software.csv \
  --hardware-latency-csv artifacts/latency/hardware.csv \
  --output artifacts/external-benchmarks/latency-import.jsonl

scripts/run-live-remote-bench.sh artifacts/live-remote/results.jsonl
scripts/run-hostile-soak.sh artifacts/hostile-soak/local
scripts/run-power-thermal-sample.sh artifacts/power/local -- cargo run -p bootty --bin bootty
scripts/record-replay-fixture.sh <fixture-name> artifacts/replays -- <command> [args...]
```

`run-terminal-public-benchmarks.py` launches tools inside actual terminals. Keep
benchmark stdout attached to the terminal PTY; redirecting stdout measures file
output and invalidates the result. `run-external-benchmark-adapters.py` is for
probing/importing public benchmark tools and CSV artifacts unless its command is
already running inside a terminal emulator.

Latency imports preserve `terminal`, `profile`, and `benchmark`/`case` columns
from CSV input as top-level normalized fields. Numeric latency columns default to
milliseconds for `--typometer-csv`, `--software-latency-csv`, and
`--hardware-latency-csv`; headers ending in `_us`, `_ns`, or `_s` override the
unit. Typometer rows are labeled `typometer_software_visual`, software event or
frame-counter imports are labeled `software_event_visual`, and hardware rig rows
are labeled `hardware_key_to_pixel`. Publishable latency claims still require
the capture method, device/display settings, run counts, and raw CSV artifacts.

The public terminal runner normalizes the focused PTY profile to an actual
80x24 terminal grid and records `actual_pty_size` in each terminal result row.
Rows whose measured PTY size does not match the target are marked
`invalidated`, with metrics retained only for diagnosis. Bootty uses a
harness-generated config by default: a calibrated non-fullscreen window,
sidebar/status chrome disabled, and native scrollback disabled. Competitor
launchers must likewise avoid user config and set an equivalent 80x24 initial
grid when their CLIs support it. Pass `--use-user-bootty-config` only for
explicitly labeled user-profile runs.

By default, the runner also executes a small terminal-response correctness gate
per terminal/profile and copies `correctness_status`, detail, and artifact paths
onto each benchmark row; failed gates invalidate otherwise passing timing rows.

By default, each public PTY benchmark then emits a visible sentinel and a DSR
query after the benchmark process exits. The resulting
`terminal_response_catch_up/post_producer_response_time` metric measures how long
the terminal takes to accept the post-producer output and answer after any PTY
backlog. This is not a visual-present timestamp; it is a terminal-agnostic
catch-up lower bound and a synchronization artifact for external video/OCR
capture. Use `--skip-catch-up-probe` to disable it or `--catch-up-timeout-ms` to
change the probe timeout.

Use `--resource-sample` when a run is intended to compare efficiency; it records
primary terminal-process RSS and CPU samples beside the timing metrics. Treat it
as process-level evidence, not full GPU/power accounting.

## Internal trace mode

Bootty can emit internal JSONL trace records for optimization-only runs. This is
not apples-to-apples competitive evidence because competitors cannot expose the
same internal milestones.

```bash
BOOTTY_BENCH_TRACE=/tmp/bootty-trace.jsonl cargo run -p bootty --bin bootty
BOOTTY_BENCH_TRACE=/tmp/bootty-trace.jsonl \
BOOTTY_BENCH_TRACE_SAMPLE_EVERY=10 \
cargo run -p bootty --bin bootty
```

Trace records include `schema_version`, `ts_ns`, and `event`. Current events
include `worker_start`, `worker_stop`, `input_commands`, `pty_read`,
`pty_collect_done`, `parse_start`, `parse_done`, `frame_submitted`, and
`frame_presented`. Consumers must ignore unknown fields/events. When tracing is
disabled the worker stores `None`; when enabled records are written
synchronously so crash evidence can be recovered.

When `run-terminal-public-benchmarks.py --bootty-trace` is used for Bootty, the
runner also imports trace-derived `bootty_trace` metrics. `visual_catch_up_time`
is the time from the last `parse_done` event to the last `frame_presented` event
in that Bootty trace. Treat it as Bootty-owned diagnostic evidence. For
competitors, use the public runner's post-producer sentinel plus compositor,
video, OCR, or frame-counter capture when a true visual-present timestamp is
required.

## Competitive claim rules

Do not publish one global fastest-terminal score. Publish independent,
category-specific claims. Every candidate claim starts as `insufficient_data` and
must become `candidate` before it can become `publishable`.

| State | Meaning |
| --- | --- |
| `unsupported` | The competitor or Bootty does not support the feature; report separately from speed. |
| `invalidated` | Correctness, visual parity, crash, hang, or timeout failure makes numbers non-comparable. |
| `insufficient_data` | Data lacks enough runs, metadata, opponents, or tail statistics. |
| `candidate` | Raw data and metadata exist but still need review for caveats/outliers/reproducibility. |
| `publishable` | Correctness, raw data, metadata, tail statistics, confidence data, and caveats are present. |

A publishable claim needs correctness status, raw JSON/CSV, command lines,
benchmark commit/workload hashes, terminal versions/config hashes, profile,
`TERM`, shell, font/grid/scrollback settings, platform/display/GPU metadata, run
counts, warmups, outlier policy, p50/p95/p99/min/max/stddev/CI/CV, producer vs
visible catch-up timing where applicable, and resource counters when speed can
trade against memory or power.

Hillclimb work is complete only when the benchmark has a stable baseline and
high-tail sentinel, at least one profiled optimization attempt or evidence that
no meaningful Bootty-owned hot path remains, correctness still passes, and the
report states what was tried, what was rejected, and the next suspected
bottleneck.

## Strong opponents by claim area

| Category | Strong opponents |
| --- | --- |
| Low input latency | xterm, st, Alacritty, foot, tuned kitty |
| Parser throughput | kitty, Alacritty, Ghostty |
| Wayland efficiency | foot, kitty, Alacritty, Ghostty |
| Render/frame pacing | foot, Alacritty, kitty, Ghostty, WezTerm |
| Graphics protocols | kitty, WezTerm, Ghostty, Konsole |
| Memory and scrollback efficiency | foot, xterm, st, Alacritty |
| macOS native behavior | Terminal.app, iTerm2, Ghostty |
| Multiplexer workflows | WezTerm, tmux inside kitty/Alacritty/Ghostty |
| Cross-platform feature coverage | WezTerm, kitty, Ghostty |
| Fault resistance | xterm, kitty, Ghostty, WezTerm, Alacritty |

## Result hygiene

- Store raw benchmark outputs, plots, and summaries under artifacts or PR/task
  evidence, not in this document.
- Record benchmark command lines, commit hashes, platform metadata, terminal
  config hashes, run counts, warmups, and outlier policy with the result.
- Separate producer completion, parse/update completion, and visible render
  catch-up when measuring output-heavy workloads.
- Report p50/p95/p99/max and variance for publishable results; median-only
  summaries are not enough.
- Mark unsupported features as unsupported, not slow.
- Invalidate performance claims for any feature class with failed correctness,
  visual parity, crash, hang, or timeout evidence.
- Do not compare tuned Bootty against default competitors without labeling both
  profiles.
- Do not mix Wayland, X11, macOS, Windows, refresh rates, DPI scales, or power
  profiles in one chart without labels.
