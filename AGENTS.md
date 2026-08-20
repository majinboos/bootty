# Bootty

## Branch and PR Flow

- Before editing, run `gh stack view --short` and identify the intended parent.
- Independent work starts from `main` with a new `gh stack` branch.
- Work that depends on an open PR starts from that PR's branch, then uses
  `gh stack add <branch>` before any edits or commits.
- Create the branch first; never let an existing checkout or dirty worktree
  accidentally choose the PR base.
- Before submitting, verify the full chain is exactly
  `main <- dependency PRs <- new PRs`.
- Open PRs only when requested. Merge only with explicit user instruction.
- After a merge, sync local `main`. Never commit directly to `main`.

## Run Modes

- Full app: `cargo run -p bootty-app --bin bootty`
- Bare WGPU host: `cargo run -p bootty-app --example bare`
- eframe tabs example: `cargo run -p bootty-app --example egui-tabs`

Bootty uses the macOS account login shell by default. Use `BOOTTY_SHELL=/path/to/shell`
only when a smoke test needs an explicit shell override.

## rmux Integration Boundary

Bootty owns rmux through the embedded Rust API. Use `rmux-sdk`, `rmux-client`,
`rmux-proto`, and Bootty-owned protocol surfaces for all local and remote rmux
work.

The standalone `rmux` executable and its CLI are outside Bootty's architecture.
Never execute, discover, install, or depend on that executable from production
code, tests, scripts, packaging, or remote commands.

Test the positive SDK or Bootty protocol behavior. Do not enforce this boundary
with source-text scans, forbidden-word assertions, or executable-name rejection
logic.

## Validation

Default correctness gate for code changes:

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --lib --tests
cargo test -p bootty-app --bench paint_plan --no-run
```

Run doc-tests, WGPU offscreen readback tests, or Cargo's complete default test
suite only when those surfaces changed or when explicitly validating the full
Cargo test shape:

```bash
cargo test --workspace --doc
cargo test -p bootty-app --test terminal_background_wgpu -- --ignored
cargo test --workspace
```

Use targeted tests first while iterating, for example
`cargo test -p bootty-terminal <test-name> --lib`. Do not run independent
Cargo commands in parallel; concurrent Rust builds compete for CPU, memory, disk,
and Cargo locks.

For non-performance chores that need benchmark smoke coverage, use the fast
CPU/egui benchmark harness:

```bash
cargo test -p bootty-app --bench paint_plan
```

Compile release-profile or workspace benchmarks only when a change needs those
broad bench gates:

```bash
cargo bench -p bootty-app --bench paint_plan --no-run
cargo bench -p bootty-app --bench paint_plan_wgpu --no-run
cargo bench --workspace --no-run
```

Run full Criterion measurement suites only for performance/rendering changes or
when explicitly requested:

```bash
cargo bench -p bootty-app --bench paint_plan -- --noplot
cargo bench -p bootty-app --bench paint_plan_wgpu -- --noplot
```

Use `cargo run` directly. If `cargo` does not resolve through mise shims, fix
the shell/mise setup instead of prefixing commands with `mise exec`.

Install the repository hooks locally when `git config --get core.hooksPath`
does not print `.githooks`:

```sh
git config core.hooksPath .githooks
```

The pre-commit hook runs `cargo fmt --check` and
`cargo clippy --workspace --all-targets -- -D warnings`.

## Manual Verification

- `cargo run -p bootty-app --bin bootty` must open the full Bootty window with tmux
  chrome, status metrics, and visible terminal glyphs.
- `cargo run -p bootty-app --example bare` must open a native bare terminal window;
  shell output in the launching terminal is not sufficient.
- `cargo run -p bootty-app --example egui-tabs` must open the tabs example and route
  terminal content through the shared WGPU renderer.
- For glyph smoke checks, paste and run `printf '%s\n' 'bootty glyph probe: 🥟 ABC █ ┃'`.

## Toolchain

Use the repository `mise.toml`. `cargo` should resolve through the mise shim without `mise exec`:

```sh
mise current rust
command -v cargo
cargo --version
```

## Docs

- Project overview: `README.md`
- Architecture: `docs/architecture.md`
- Egui oracle inventory: `docs/current-egui-behavior.md`
- Input encoders: `docs/input-encoders.md`
- Benchmark process and performance guardrails: `docs/benchmarking.md`
- Benchmark reports: `docs/benchmark-report.md`
- `libghostty-rs` dependency boundary: `docs/libghostty-rs.md`

## Rust Module Layout

Keep `mod.rs` and `lib.rs` as module declarations and re-exports.

Put implementation logic, state, adapters, and tests in named modules.

Do not put `#[cfg(test)]`, test declarations, fixtures, or test support in
`mod.rs` or `lib.rs`.

When a touched `mod.rs` or `lib.rs` already contains implementation logic, move
the touched responsibility into a named module instead of adding more logic.

Keep all test code out of production source files.

Do not register tests from production with `#[cfg(test)]`, `mod tests`, or a
test `#[path]`. A path bridge into `tests/` still violates this boundary.

Put crate-owned tests under that crate's `tests/` directory as normal Cargo
integration targets. Replace private white-box checks with public behavior
contracts. Delete implementation-detail checks that do not protect public
behavior. Do not make a production interface public only for a test.

Use the workspace root `tests/` directory only for cross-crate product behavior
and executable boundaries. It does not replace direct tests for each crate.

The vault owns product vocabulary, plans, progress, rationale, and durable
decisions. Keep only current developer documentation that must version with the
code in this repository. `docs/architecture.md` describes current production
structure. Do not store decision history or progress logs under `docs/`.
