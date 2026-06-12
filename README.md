# Bootty

Bootty is a native GPU-rendered terminal and a set of reusable terminal crates.

## Run

```sh
cargo run -p bootty-app --bin bootty
cargo run -p bootty-app --example bare
cargo run -p bootty-app --example egui-tabs
```

The default app opens the full Bootty shell with terminal rendering, status
metrics, and tmux session chrome. The `bare` example opens a minimal non-egui
winit/WGPU terminal host. The `egui-tabs` example demonstrates tabs using the
same renderer path as the main app.

## Workspace

- `bootty-app` - desktop app, default `bootty` binary, examples, app behavior,
  and integration tests.
- `bootty-ui` - shared egui UI helpers.
- `bootty-surface` - terminal geometry and surface math.
- `bootty-terminal` - Ghostty-backed terminal state and render frames.
- `bootty-runtime` - PTY sessions, shell selection, drain scheduling, and frame
  publication.
- `bootty-render` - paint plans, text shaping, sprites, and WGPU rendering.
- `bootty-winit` - native winit/WGPU host adapters.
- `bootty` - convenience re-export crate for embedders.
- `bootty-tauri` - Tauri + React + WebGL2 embedding demo and static website.

## Native app bundles

Native Bootty app bundles are built from `bootty-app --bin bootty`, not from the
Tauri demo crate.

```sh
mise run package          # macOS .app zip or Linux tarball
mise run package:windows  # Windows zip
mise run install          # package and install Bootty.app into /Applications (macOS)
```

The CI workflow runs full Rust validation on pull requests and pushes, then
uploads native macOS, Windows, and Linux app artifacts for pushes to `main` and
manual workflow runs.

## Tauri and website

```sh
cd crates/bootty-tauri
npm install
npm run tauri -- dev
npm run build:pages
```

The static build writes `crates/bootty-tauri/pages-dist`.

## Validation

```sh
cargo fmt --check
cargo clippy --workspace --all-targets -- -W clippy::all
cargo test --workspace
cargo bench --workspace --no-run
cargo bench -p bootty-app --bench paint_plan -- --noplot
```

## Docs

- Architecture and crate boundaries: `docs/architecture.md`
- Configuration path, schema, reload, and writeback: `docs/configuration.md`
- Egui oracle behavior inventory: `docs/current-egui-behavior.md`
- Input encoder contracts: `docs/input-encoders.md`
- Performance guardrails and benchmark seams: `docs/performance.md`
- Built-in theme provenance: `docs/built-in-themes.md`
- `libghostty-rs` dependency boundary: `docs/libghostty-rs.md`