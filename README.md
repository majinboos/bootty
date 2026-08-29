# Bootty

Bootty is a native GPU-rendered terminal and a set of reusable terminal crates.

## Run

```sh
cargo run -p bootty --bin bootty
cargo run -p bootty-app --example bare
cargo run -p bootty-app --example egui-tabs
```

The default app opens the full Bootty shell with terminal rendering, status
metrics, and tmux session chrome. The `bare` example opens a minimal non-egui
winit/WGPU terminal host. The `egui-tabs` example demonstrates tabs using the
same renderer path as the main app.

## Workspace

- `bootty` - executable startup, CLI dispatch, and native packaging.
- `bootty-app` - desktop application library, examples, app behavior, and
  integration tests.
- `bootty-ui` - shared egui UI helpers.
- `bootty-surface` - terminal geometry and surface math.
- `bootty-terminal` - Ghostty-backed terminal state and render frames.
- `bootty-runtime` - PTY sessions, shell selection, drain scheduling, and frame
  publication.
- `bootty-font` - OpenType feature values, parsing, and canonical formatting.
- `bootty-render` - paint plans, text shaping, sprites, and WGPU rendering.
- `bootty-winit` - native winit/WGPU host adapters.

## Native app bundles

Native Bootty app bundles are built from `bootty --bin bootty`.

```sh
mise run package          # local dynamic package with the host daemon
mise run package --static # static package with the host daemon
mise run package --all-daemons --static # static package with every remote daemon
mise run package:windows  # Windows zip from a staged complete daemon set
mise run install          # local dynamic package and install for the current OS
mise run build --fast     # dynamic build with --profile fast-release
mise run install --fast   # dynamic install using --profile fast-release
```

CI and release packages contain the five daemon targets owned by `xtasks`.
Local package and install tasks build only
the host daemon unless `--all-daemons` is passed. On non-macOS hosts, Apple
targets require an installed Apple SDK in `SDKROOT`. Windows packaging requires
a complete staged daemon directory through `BOOTTY_DAEMON_OUTPUT_DIR`. CI builds
that directory on target-capable runners.

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
mise run fmt
mise run clippy
mise run test
mise run bench -- --ci-smoke
```

## Docs

- Architecture and crate boundaries: `docs/architecture.md`
- Pi and Codex integration setup: `docs/agent-integrations.md`
- Configuration path, schema, reload, and writeback: `docs/configuration.md`
- Input encoder contracts: `docs/input-encoders.md`
- Benchmark process and performance guardrails: `docs/benchmarking.md`
- Built-in theme provenance: `docs/built-in-themes.md`
- `libghostty-rs` dependency boundary: `docs/libghostty-rs.md`
- Release publishing and verified updates: `docs/releases.md`
