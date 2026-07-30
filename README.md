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

## Native app bundles

Native Bootty app bundles are built from `bootty-app --bin bootty`.

```sh
mise run package          # local dynamic package using dynamic-release
mise run package --static # static release package for distribution/CI
mise run package:windows  # local dynamic Windows zip using dynamic-release
mise run install          # local dynamic package and install for the current OS
mise run build --fast     # dynamic build with --profile fast-release
mise run install --fast   # dynamic install using --profile fast-release
```

The CI workflow runs full Rust validation on pull requests and pushes. Pushing
a version tag matching `Cargo.toml` creates a GitHub Release with native macOS,
Windows, and Linux bundles. Installed Bootty releases check for updates on
startup; use `bootty update` to update explicitly. See `docs/releases.md`.

## Website

Cloudflare Pages deploys `bootty.org` and `www.bootty.org` from `main`.
The Cloudflare project builds from the repository root and uploads root
`pages-dist` from `sites/bootty-web`. GitHub Actions does not deploy the site.

Run the same source build locally with `mise run site:build`.

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
- Benchmark process and performance guardrails: `docs/benchmarking.md`
- Benchmark reports and measured findings: `docs/benchmark-report.md`
- Built-in theme provenance: `docs/built-in-themes.md`
- `libghostty-rs` dependency boundary: `docs/libghostty-rs.md`
- Release publishing and verified updates: `docs/releases.md`
