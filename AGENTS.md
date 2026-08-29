# Bootty

Bootty is a native GPU-rendered terminal workspace and a local control host. One
desktop app, one installed daemon, and a set of library crates with explicit
owners.

Four things shape almost every decision in this repo:

1. **One owner per fact.** Every durable value and every live mutation has
   exactly one crate that owns it. `docs/architecture.md` has the authority
   table. When you are unsure where code goes, that table is the answer.
2. **The terminal is not the multiplexer.** Bootty renders terminals; a mux
   backend owns processes and native topology. `native`, `rmux`, and `tmux` are
   three interchangeable backends behind one contract.
3. **Frames, not blocking.** The UI thread never waits on terminal, extension,
   or agent work. Terminal state is published as immutable frames and painted
   from a plan.
4. **One invocation path.** The palette, keybindings, CLI, local socket, and
   Luau extensions all submit the same `CommandInvocation`. There is no second
   way to do a thing.

## A small glossary

Use this language when you write code and when you talk to us.

- **you** - the agent doing the work. **we/us** - the maintainers. **user** -
  the person directing you.
- **Space** - a persisted workspace with backend bindings. Lives in SQLite,
  owned by `bootty-workspace`.
- **binding** - one Space's attachment to one mux backend, plus its pane layout,
  focus, and titles. `BindingRuntime` owns the live one.
- **backend** / **provider** - `native`, `rmux`, or `tmux`. Owns real processes
  and native session/window/pane topology.
- **pane** / **session** / **window** - backend-native topology. Bootty maps it;
  Bootty does not invent it.
- **surface** - host-neutral terminal geometry (`bootty-surface`). Host
  coordinates convert at the adapter seam, not in the middle.
- **frame** - an immutable published snapshot of VT state. Consumers read
  frames; they do not read the engine.
- **module** - one bundled `.lua`/`.luau` source or one user file under
  `<config>/extensions`. A user file with a bundled module's identity overrides
  that built-in. **generation** - one complete published version of the
  extension world.
- **identity** - Production (`bootty`) or Development (`bootty-dev`). Selects
  config tree, state tree, control endpoint, rmux endpoint, and tmux server.

## The three ways to hurt yourself

**A release build from your tree is the user's real app.** Identity is
`Development` only under `debug_assertions` or the `bootty-dev` feature. So
`cargo run` is safely `BoottyDev`, but a release-profile build without the
`bootty-dev` feature takes the Production namespace: the user's `config.toml`,
their workspace SQLite, their control endpoint, their rmux endpoint, and their
tmux server. A Production app launch exits when that identity already has a
live owner. A Production command invocation instead targets that live owner and
can mutate the user's real Spaces. Use `mise run launch` for an optimized
development launch, or `mise run install:dev` and `mise run package:dev` for
isolated development packages; all three enable the `bootty-dev` feature.

**Never reach for the standalone `rmux` executable.** Bootty owns rmux through
the embedded Rust API - `rmux-sdk`, `rmux-client`, `rmux-proto`, `rmux-server`,
and Bootty's own protocol surfaces. The CLI binary is outside the architecture:
do not execute, discover, install, or depend on it from production code, tests,
scripts, packaging, or remote commands. Test the positive SDK behavior. Do not
police this with source-text scans or forbidden-word assertions - those tests
fail for the wrong reasons and teach nothing.

**Never patch `libghostty-rs` in-tree.** It is a pinned external binding crate.
Anything you can do by preprocessing input, postprocessing frames, or calling
public `libghostty-vt` APIs belongs in `bootty-terminal`. Anything that needs
Ghostty internals the C API does not expose is unsupported. See
`docs/libghostty-rs.md`.

## Hit every surface

Bootty fans out. Fixing one arm is not fixing the feature.

- **Commands** reach the same `CommandInvocation` from seven callers
  (`CommandPalette`, `Keybinding`, `BuiltinKeybinding`, `Cli`, `Socket`, `Luau`,
  `Internal`). A command that only works from the palette is unfinished.
- **Mux backends** are three, not one. A change to session, window, or pane
  behavior has a `native`, an `rmux`, and a `tmux` answer - and each of those
  has a local and a remote answer.
- **Config fields** are not one edit. A new field needs a typed config value, a
  default, and one `SettingSpec` whose path the loader reads and whose page owns
  its editor. Scalar specs render their own settings row; only non-scalar
  settings need a custom editor. Update `docs/sample-config.toml` and
  `docs/configuration.md`. Tests in `bootty-config` enforce this contract.
- **Hosts** are three: the full app, `crates/bootty-app/examples/bare.rs`
  (winit/WGPU, no egui), and `crates/bootty-app/examples/egui-tabs.rs`.
  Renderer and input changes must survive all three.
- **Identities** are two. Anything touching paths, endpoints, or namespaces must
  keep Production and Development apart.
- **Ownership moves belong in `docs/architecture.md`.** If your change moves a
  durable fact to a different crate, that table is now wrong.

## Releases

When Luan asks you to make a release, read and follow `docs/releases.md`. Build
the release notes from commits, then include them in the body of the prepare
commit made directly on `main`. Do not create a release branch or pull request.

## Verifying

Run targeted tests while you work: `cargo nextest run -p bootty-terminal
<test-name>`. Do not run Cargo commands in parallel - concurrent Rust builds
fight over CPU, memory, disk, and the Cargo lock, and the "parallel" run is
slower than the serial one.

Before you hand work back, the local gate is:

```sh
mise run fmt
mise run clippy
mise run test
mise run bench -- --ci-smoke
```

CI also runs `mise run hakari:check`. Two things bite people there:

- **Changed a dependency?** Run `mise run hakari:generate`. The workspace-hack
  crate is generated, and a stale one fails CI with a diff that looks unrelated
  to your change.
- **Benchmarks are compile gates, not measurement gates.** `--ci-smoke` is
  enough for any non-performance change. Run measured Criterion suites only for
  rendering and performance work, or when asked. `docs/benchmarking.md` has the
  guardrails and the claim hygiene rules.

Nextest does not run doctests, so executable documentation examples live in
integration tests.

**GUI changes need a window.** Output in the launching terminal does not prove a
render path works. `cargo run -p bootty --bin bootty` should show tmux chrome,
status metrics, and real glyphs; the two examples should each open their own
native window. For a glyph smoke check, run
`printf '%s\n' 'bootty glyph probe: 🥟 ABC █ ┃'` inside the app.

## Taste

- Complexity belongs behind a small interface, not spread across callers. A
  caller should never reconstruct an invariant or mirror state it does not own.
- Persistence commits before live publication. A failed commit leaves the prior
  state active. Never publish a half-applied mutation and reconcile later.
- Backend failure is reported, never invented. Bootty does not fake success it
  did not observe.
- `mod.rs` and `lib.rs` hold module declarations and re-exports. Nothing else.
- Keep test code out of production source: no `#[cfg(test)]`, fixtures, or test
  support in a non-test module, and no path bridge into `tests/`. Tests are
  Cargo integration targets under the crate's `tests/`. The root `tests/` is
  only for cross-crate product behavior and executable boundaries.
- Large files are allowed while each keeps one cohesive owner and a small
  interface. File size is a signal for review, not proof of depth.
- `unsafe` is denied workspace-wide. Clippy runs `all` at deny and `pedantic` at
  warn.
- Reuse the names already in this repo. New vocabulary for an existing concept
  is a bug.

If a rule here fights the task in front of you, say so out loud rather than
quietly working around it. These are the defaults we hold, not law.

## Plans and work artifacts

Do not commit implementation plans, research notes, or scratch files. The vault
owns product language, plans, rationale, and durable decisions. `docs/` owns the
current production structure - update it when the structure changes, so the next
agent reads facts instead of history.

## Where to read next

`README.md` has the crate map, the run and packaging commands, and the full doc
index. Beyond that, the three you will want most:

- `docs/architecture.md` - ownership, authority table, and crate boundaries.
- `docs/configuration.md` - config path, schema, reload, and writeback.
- `docs/input-encoders.md` - the terminal input contract.
