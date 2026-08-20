//! Static website content and terminal-friendly markdown rendering.

use std::sync::LazyLock;

use syntect::easy::HighlightLines;
use syntect::highlighting::{FontStyle, Style as SyntectStyle, ThemeSet};
use syntect::parsing::SyntaxSet;
use tuirealm::ratatui::style::{Color, Modifier, Style};
use tuirealm::ratatui::text::{Line, Span, Text};

const CODE_BG: Color = Color::Rgb(8, 10, 18);
const MUTED: Color = Color::Rgb(139, 149, 182);
const TEXT: Color = Color::Rgb(214, 222, 247);
const PINK: Color = Color::Rgb(255, 79, 168);
const GREEN: Color = Color::Rgb(158, 220, 106);
const CYAN: Color = Color::Rgb(125, 207, 255);
const BLUE: Color = Color::Rgb(122, 162, 247);
const YELLOW: Color = Color::Rgb(255, 199, 119);
const PURPLE: Color = Color::Rgb(187, 154, 247);

const OVERVIEW_PROMISE: &str = r#"# Bootty

Bootty is a terminal product and a reusable terminal rendering stack. The native
app is the daily driver; the crates and `bootty.js` expose the same frame model
for Rust and browser hosts.

## What ships

- native app: tmux-oriented shell chrome, status metrics, sessions, settings
- Rust crates: PTY runtime, terminal frames, renderer frame conversion, WGPU data
- JavaScript package: WebGL2 canvas renderer, browser mount helper, Node frame tools
- site backend: deterministic wasm terminal frames for docs and demos

## Hard boundary

The renderer receives structured cells, colors, cursor state, links, images, and
selection. Hosts do not scrape terminal output, replay escape sequences in UI
code, or recover state from screenshots.
"#;

const OVERVIEW_STACK: &str = r#"# Architecture map

Bootty is split so hosts can change without changing terminal state, and renderer
work can be tested without launching the full app shell.

## Frame path

```text
PTY / demo backend
  -> bootty-terminal RenderFrame
  -> bootty-render RendererFrame
  -> TerminalRenderFrame
  -> WGPU native renderer or bootty.js WebGL2 renderer
```

## Public seams

- `bootty`: facade for embedders using the native runtime or renderer
- `bootty-runtime`: shell process, PTY drain, resize, repaint wakeups
- `bootty-terminal`: VT state, cell grid, styles, links, images, selection
- `bootty-render`: paint plans, glyph atlas inputs, sprite/image data
- `bootty.js/browser`: canvas mount and WebGL2 renderer
- `bootty.js/node`: frame fixtures, snapshots, and text extraction
"#;

const OVERVIEW_ROADMAP: &str = r#"# Documentation map

The website documents how to run Bootty, embed it, configure it, and verify the
renderer. Source browsing belongs in GitHub; the docs should answer the first
questions without sending readers there.

## Pages

- Overview: product shape, architecture, public seams
- Quickstart: run commands, smoke probes, focused validation
- Docs: JavaScript package and Rust crate examples
- Renderer: frame contract, glyph probes, color/link/image checks
- Config: TOML config, keybinds, themes, reload behavior

## Example standard

Every command or API example is fenced with a language, rendered with syntax
highlighting, and written as something a reader can adapt directly.
"#;

const QUICKSTART_RUN: &str = r#"# Run Bootty

Bootty uses your macOS account login shell by default. Set `BOOTTY_SHELL` only
when a smoke test needs to force a specific shell.

## Native app

```sh
cargo run -p bootty-app --bin bootty
```

Expected: a native Bootty window opens with terminal glyphs, tmux-oriented chrome,
status metrics, and a live shell. Shell output in the launching terminal is not
enough; the window itself must render terminal content.

## Glyph probe

```sh
printf '%s\n' 'bootty glyph probe: 🥟 ABC █ ┃'
```

Run this inside Bootty after changes to font lookup, glyph atlas packing, fallback,
emoji, box drawing, renderer frame conversion, or canvas sizing.

## Explicit shell smoke

```sh
BOOTTY_SHELL=/bin/zsh cargo run -p bootty-app --bin bootty
```

Use this only to isolate shell discovery from renderer/runtime behavior.
"#;

const QUICKSTART_EXAMPLES: &str = r#"# Renderer hosts

Renderer work should be checked in the smallest host that proves the behavior.
The full app is useful, but it is not the fastest way to isolate WGPU setup,
glyph placement, or frame sizing.

## Bare WGPU host

```sh
cargo run -p bootty-app --example bare
```

Expected: a native terminal window drawn by the renderer without egui chrome. Use
this when debugging surface setup, glyph atlas output, cell metrics, or target
format issues.

## egui tabs host

```sh
cargo run -p bootty-app --example egui-tabs
```

Expected: an egui window with multiple terminal tabs. This is the embedding
reference for another Rust app: active-session selection, tab lifecycle, egui
input snapshots, PTY drain metrics, repaint scheduling, and WGPU target format
plumbing.

## Website renderer

```sh
bun run build:web
```

Expected: the Rust site wasm, `bootty.js`, and Vite app build together. Use this
for browser renderer, docs-content, frame-schema, or package-boundary changes.
"#;

const QUICKSTART_BUILD: &str = r#"# Build and verify

Use focused commands while iterating. Use the workspace gate before claiming a
change that affects Rust behavior outside the website/package boundary.

## Fast site loop

```sh
cargo test -p bootty-site --lib
bun run build:web
```

`bootty-site` tests cover the wasm backend, markdown rendering, tab state, frame
export, and selection serialization. The web build proves the Rust wasm package,
`bootty.js`, and the Vite site still connect.

## Default Rust gate

```sh
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --lib --tests
cargo test -p bootty-app --bench paint_plan --no-run
```

Run doc-tests, offscreen WGPU readback tests, or full Criterion measurement only
when those surfaces changed.
"#;

const DOCS_JAVASCRIPT: &str = r##"# JavaScript package

`bootty.js` is the public package for rendering Bootty frames in browser apps and
for inspecting those same frames in Node tooling.

Package page: `https://www.npmjs.com/package/bootty.js`.

## Install
"##;

const DOCS_JS_NPM: &str = r#"```sh
npm install bootty.js
```
"#;

const DOCS_JS_PNPM: &str = r#"```sh
pnpm add bootty.js
```
"#;

const DOCS_JS_BUN: &str = r#"```sh
bun add bootty.js
```
"#;

const DOCS_JS_YARN: &str = r#"```sh
yarn add bootty.js
```
"#;

const DOCS_JS_BROWSER: &str = r##"Use `bootty.js/browser` when you have a canvas and a backend that can produce
`WebTerminalFrame` values. The mount helper wires DOM input, canvas sizing,
renderer refresh, copy behavior, and disposal.

```ts
import {
  createRustSiteBackend,
  mountCanvasTerminal,
} from "bootty.js/browser";

const canvas = document.querySelector<HTMLCanvasElement>("#terminal");
if (!canvas) throw new Error("missing terminal canvas");

const terminal = await mountCanvasTerminal({
  canvas,
  backend: () => createRustSiteBackend({ page: "docs" }),
  cols: 96,
  rows: 32,
  fps: 30,
  onFrame(frame) {
    console.info(`${frame.cols}x${frame.rows}`, frame.selection);
  },
  onError(error) {
    console.error("bootty backend failed", error);
  },
});

await terminal.write("j");
```

Mounted terminals expose `refresh()`, `resize(cols, rows)`, `write(input)`, and
`dispose()`. They do not create a PTY or shell; the backend owns that.
"##;

const DOCS_JS_NODE: &str = r#"Use `bootty.js/node` in tests, snapshots, fixture generation, and CLI tools that
need Bootty's frame schema without browser globals.

```js
import {
  createBlankCell,
  createEmptyFrame,
  frameRows,
  frameToText,
} from "bootty.js/node";

const frame = createEmptyFrame({ cols: 24, rows: 4 });
frame.cells.push(
  ...Array.from("Bootty", (text, x) => createBlankCell(x, 0, { text })),
);

console.log(frameRows(frame));
console.log(frameToText(frame));
```

The Node entrypoint exports frame utilities and shared types only. It does not
pretend that `canvas`, `document`, WebGL, selection, or clipboard APIs exist.
"#;

const DOCS_RUST: &str = r#"# Rust crates

Use Rust when you are embedding the native terminal runtime or renderer. Use
`bootty` as the public facade; reach into narrower crates only when you own that
layer.

## Crate map

- `bootty`: facade for embedders
- `bootty-runtime`: PTY process, worker thread, frame publication
- `bootty-terminal`: VT state and terminal snapshots
- `bootty-render`: renderer frame conversion, paint planning, WGPU data
- `bootty-surface`: grid geometry, padding, selection coordinates

## Session lifecycle

`TerminalSession` owns the shell process. UI code writes input, drains pending
PTY bytes on ticks, resizes when the surface grid changes, and reads the latest
published frame.

```rust
use std::sync::Arc;

use bootty::geometry::TerminalGeometry;
use bootty::runtime::TerminalSession;

let geometry = TerminalGeometry {
    cols: 80,
    rows: 24,
    cell_width: 10,
    cell_height: 22,
};

let repaint = Arc::new(|| {});
let mut session = TerminalSession::new_with_repaint_wakeup(geometry, repaint)?;

session.write_input(b"printf 'hello from Bootty\\n'")?;
let drain = session.drain_pty();
let frame = session.extract_frame()?;

println!("drained {} bytes", drain.bytes);
println!("{}x{} cells", frame.cols, frame.rows);
```

## Render-frame path

The renderer consumes a terminal `RenderFrame` plus a `TerminalSurface`. That
conversion preserves cells, colors, cursor, links, images, dirty rows, and
selection data as structured renderer input.

```rust
use bootty::geometry::{CellMetrics, TerminalPadding, TerminalSurface};
use bootty::renderer_frame::RendererFrame;
use bootty::terminal::RenderFrame;
use bootty::terminal_render::TerminalRenderFrame;
use bootty::terminal_text::TerminalTextConfig;

fn build_render_frame(frame: &RenderFrame) -> TerminalRenderFrame {
    let surface = TerminalSurface::for_logical_size(
        960.0,
        640.0,
        CellMetrics::new(10.0, 22.0),
        TerminalPadding::default(),
    );
    let text = TerminalTextConfig::default();
    let renderer_frame = RendererFrame::from_terminal(frame, surface, &text);

    renderer_frame.to_terminal_render_frame(&text)
}
```

Do not parse stdout in the UI, replay escape sequences in the renderer, or infer
links and selections from screenshots. The frame already carries those fields.
"#;

const RENDERER_FRAME: &str = r#"# Frame contract

The renderer consumes terminal frames, not terminal output. A frame contains grid
cells, style spans, cursor state, dirty rows, palette colors, selection, OSC-8
links, and Kitty image planes.

## Renderer path

Native and browser hosts draw the same structured frame. The native path lowers
it to WGPU data; `bootty.js` lowers it to WebGL2 draw calls. Neither path parses
stdout, replays escape sequences, or infers links/selection from pixels.

## Host responsibilities

- choose cell metrics and surface size
- forward keyboard, mouse, resize, and clipboard events
- call the backend for fresh frames
- dispose renderer/backend resources when the host unmounts
"#;

const RENDERER_GLYPHS: &str = r#"# Font feature probes

This page intentionally renders text that should expose shaping, fallback,
symbol, punctuation, and grid alignment regressions. These are not code examples;
they are the strings to look at in the renderer.

## Lowercase alphabet

abcdefghijklmnopqrstuvwxyz

## Punctuation and ambiguous glyphs

~!@#$%^&\* {} [] () I1l O0o

## Operators and programming ligatures

!== \\ <= #{ -> ~@ |> 0x12

|=>==<==>=|======|===|===>

<---|--|--------|-<->--<-|

## Status tokens

[INFO] [TODO] [FIXME]

Expected: operators shape consistently, ambiguous glyphs remain distinguishable,
fallback symbols do not disturb adjacent ASCII, and every probe stays on the
terminal cell grid.
"#;

const RENDERER_COLOR: &str = r#"# Color, links, and images

Color work needs explicit surfaces, not only the default foreground/background
pair. This page is a checklist for manual renderer review.

## Surfaces to cover

- ANSI 0-15 slots and bright variants
- 256-color cube samples and grayscale ramp
- truecolor cells from terminal output
- selection foreground/background contrast
- cursor, underline, decorations, and OSC-8 link highlight
- Kitty image placement, clipping, scaling, and z-order

## Selection smoke

Drag across text, double-click a word, triple-click a line, then copy. The copied
text should come from Rust/backend selection state. Browser DOM selection is not
part of the terminal contract.

## Link smoke

OSC-8 links should remain attached to their cells after scroll, resize, selection,
and renderer refresh. A hover/click target must match the visible cell range.
"#;

const CONFIG_APP: &str = r#"# Config files

Bootty loads native TOML config from `$XDG_CONFIG_HOME/bootty/config.toml`, or
`$HOME/.config/bootty/config.toml` when `XDG_CONFIG_HOME` is not set. An absent
file is not an error.

## Minimal app config

```toml
version = 1
theme = "Catppuccin Mocha"
include = ["?local.toml"]

[window]
title = "Agent Shell"
width = 1220
height = 760
fullscreen = false

[font]
family = ["monospace"]
size = 15.666
features = ["calt", "liga"]

[session]
shell = "/bin/zsh"
working-directory = "/Users/example/src"
term = "xterm-bootty"
colorterm = "truecolor"
```

Unknown fields are rejected. Invalid startup config fails startup; invalid runtime
reload keeps the last-good in-memory config and reports the error.
"#;

const CONFIG_INPUT: &str = r##"# Input and themes

Input configuration controls app shortcuts and terminal encoder behavior. Theme
configuration controls Bootty colors without allowing theme files to mutate
shell, window, font, or input settings.

## Input shape

```toml
[input]
keybind = ["cmd+shift+,=reload_config"]
sidebar-keybind = ["Enter=activate_session", "j=next_session", "k=previous_session"]
macos-option-as-alt = "both"
modifier-remap = ["right_alt=left_ctrl"]
```

## Theme shape

```toml
[metadata]
name = "My Theme"
source = "local"
license = "personal"

[colors]
background = "#000000"
foreground = "#ffffff"
cursor = "#ffffff"
selection-background = "#2f334d"
selection-foreground = "#ffffff"
palette = ["#000000", "#ff0000", "#00ff00", "#ffff00"]
```

Keyboard, focus, mouse, and paste commands are encoded through the terminal
engine. Selection and scrollback behavior stay separate from terminal mouse
reporting modes.
"##;

const CONFIG_PERF: &str = r#"# Reload and performance

Config reload validates the full effective config before applying it. Some fields
update live; others apply only to new sessions or windows.

## Live reload fields

- chrome visibility, layout, and inactive-panel dimming
- multiplexer backend selection and backend UI mode
- modifier remaps, Option-as-Meta, global keybinds, sidebar keybinds
- terminal text metrics, theme, palette, cursor, and selected colors
- window title and diagnostics trace path

## Benchmark smoke

```sh
cargo test -p bootty-app --bench paint_plan --no-run
cargo test -p bootty-app --bench paint_plan
```

Use benchmark smoke coverage for non-performance chores that touch paint planning
or render-frame conversion. Run full Criterion measurement only for performance
or rendering changes that need timing evidence.
"#;

#[derive(Clone, Copy)]
pub(crate) struct Section {
    pub(crate) slug: &'static str,
    pub(crate) label: &'static str,
    pub(crate) title: &'static str,
    pub(crate) tagline: &'static str,
    pub(crate) accent: Color,
    pub(crate) tabs: &'static [ContentTab],
    pub(crate) has_alternative_tabs: bool,
    pub(crate) cards: &'static [FeatureCard],
}

#[derive(Clone, Copy)]
pub(crate) struct ContentTab {
    pub(crate) label: &'static str,
    pub(crate) markdown: &'static str,
    pub(crate) subtabs: &'static [ContentTab],
}

const fn tab(label: &'static str, markdown: &'static str) -> ContentTab {
    ContentTab {
        label,
        markdown,
        subtabs: &[],
    }
}

const fn tab_group(
    label: &'static str,
    markdown: &'static str,
    subtabs: &'static [ContentTab],
) -> ContentTab {
    ContentTab {
        label,
        markdown,
        subtabs,
    }
}

#[derive(Clone, Copy)]
pub(crate) struct FeatureCard {
    pub(crate) title: &'static str,
    pub(crate) body: &'static [&'static str],
    pub(crate) accent: Color,
}

const OVERVIEW_TABS: &[ContentTab] = &[
    tab("Promise", OVERVIEW_PROMISE),
    tab("Stack", OVERVIEW_STACK),
    tab("Roadmap", OVERVIEW_ROADMAP),
];

const QUICKSTART_TABS: &[ContentTab] = &[
    tab("Run", QUICKSTART_RUN),
    tab("Examples", QUICKSTART_EXAMPLES),
    tab("Build", QUICKSTART_BUILD),
];
const DOCS_JS_USAGE_TABS: &[ContentTab] =
    &[tab("Browser", DOCS_JS_BROWSER), tab("Node", DOCS_JS_NODE)];
const DOCS_JS_INSTALL_TABS: &[ContentTab] = &[
    tab_group("npm", DOCS_JS_NPM, DOCS_JS_USAGE_TABS),
    tab_group("pnpm", DOCS_JS_PNPM, DOCS_JS_USAGE_TABS),
    tab_group("bun", DOCS_JS_BUN, DOCS_JS_USAGE_TABS),
    tab_group("yarn", DOCS_JS_YARN, DOCS_JS_USAGE_TABS),
];
const DOCS_TABS: &[ContentTab] = &[
    tab_group("JavaScript", DOCS_JAVASCRIPT, DOCS_JS_INSTALL_TABS),
    tab("Rust", DOCS_RUST),
];
const RENDERER_TABS: &[ContentTab] = &[
    tab("Frame", RENDERER_FRAME),
    tab("Fonts", RENDERER_GLYPHS),
    tab("Color", RENDERER_COLOR),
];
const CONFIG_TABS: &[ContentTab] = &[
    tab("App", CONFIG_APP),
    tab("Input", CONFIG_INPUT),
    tab("Perf", CONFIG_PERF),
];

const OVERVIEW_CARDS: &[FeatureCard] = &[
    FeatureCard {
        title: "Native app",
        body: &[
            "tmux-oriented shell",
            "sessions + settings",
            "Ghostty/libghostty state",
        ],
        accent: PINK,
    },
    FeatureCard {
        title: "Renderer",
        body: &[
            "WGPU glyph atlas",
            "Kitty images + sprites",
            "selection + OSC-8 links",
        ],
        accent: BLUE,
    },
    FeatureCard {
        title: "Embedders",
        body: &[
            "eframe app shell",
            "egui-tabs reference",
            "bootty.js WebGL host",
        ],
        accent: GREEN,
    },
];

pub(crate) fn sections() -> &'static [Section] {
    &[
        Section {
            slug: "overview",
            label: "Overview",
            title: "Terminal UI that renders everywhere",
            tagline: "Native app, reusable crates, and browser renderer sharing one frame contract.",
            accent: PINK,
            tabs: OVERVIEW_TABS,
            has_alternative_tabs: false,
            cards: OVERVIEW_CARDS,
        },
        Section {
            slug: "quickstart",
            label: "Quickstart",
            title: "Install, run, and verify",
            tagline: "From checkout to native app, renderer examples, web build, and correctness gates.",
            accent: GREEN,
            tabs: QUICKSTART_TABS,
            has_alternative_tabs: false,
            cards: &[],
        },
        Section {
            slug: "docs",
            label: "Docs",
            title: "Use Bootty from JavaScript or Rust",
            tagline: "Package installs, browser mounting, Node utilities, runtime sessions, and renderer frames.",
            accent: CYAN,
            tabs: DOCS_TABS,
            has_alternative_tabs: true,
            cards: &[],
        },
        Section {
            slug: "renderer",
            label: "Renderer",
            title: "Frame renderer contract",
            tagline: "Live proof for glyphs, colors, links, selections, and image-capable frames.",
            accent: BLUE,
            tabs: RENDERER_TABS,
            has_alternative_tabs: false,
            cards: &[],
        },
        Section {
            slug: "config",
            label: "Config",
            title: "Configuration reference",
            tagline: "Shell, fonts, input, colors, reload behavior, and performance guardrails.",
            accent: PURPLE,
            tabs: CONFIG_TABS,
            has_alternative_tabs: false,
            cards: &[],
        },
    ]
}

pub(crate) fn section_tab_count(section: Section) -> usize {
    section.tabs.len().max(1)
}
pub(crate) fn section_has_alternative_tabs(section: Section) -> bool {
    section.has_alternative_tabs && section_tab_count(section) > 1
}

pub(crate) fn section_tab_label(section: Section, tab: usize) -> &'static str {
    section.tabs[tab.min(section_tab_count(section) - 1)].label
}

pub(crate) fn section_subtab_count(section: Section, tab: usize) -> usize {
    let tab = section.tabs[tab.min(section_tab_count(section) - 1)];
    tab.subtabs.len()
}

pub(crate) fn section_subtab_label(section: Section, tab: usize, subtab: usize) -> &'static str {
    let tab = section.tabs[tab.min(section_tab_count(section) - 1)];
    tab.subtabs[subtab.min(tab.subtabs.len().saturating_sub(1))].label
}

pub(crate) fn section_leaf_tab_count(section: Section, tab: usize, subtab: usize) -> usize {
    let tab = section.tabs[tab.min(section_tab_count(section) - 1)];
    if tab.subtabs.is_empty() {
        return 0;
    }
    tab.subtabs[subtab.min(tab.subtabs.len().saturating_sub(1))]
        .subtabs
        .len()
}

pub(crate) fn section_leaf_tab_label(
    section: Section,
    tab: usize,
    subtab: usize,
    leaf_tab: usize,
) -> &'static str {
    let tab = section.tabs[tab.min(section_tab_count(section) - 1)];
    let subtab = tab.subtabs[subtab.min(tab.subtabs.len().saturating_sub(1))];
    subtab.subtabs[leaf_tab.min(subtab.subtabs.len().saturating_sub(1))].label
}

pub(crate) fn section_text_for_width(
    section: Section,
    tab: usize,
    subtab: usize,
    leaf_tab: usize,
    code_width: u16,
) -> Text<'static> {
    if !section_has_alternative_tabs(section) {
        let mut text = Text::from(Vec::new());
        for tab in section.tabs {
            text.lines.extend(
                render_markdown(tab.markdown, section.accent, usize::from(code_width)).lines,
            );
        }
        return text;
    }

    let tab = section.tabs[tab.min(section_tab_count(section) - 1)];
    if tab.subtabs.is_empty() {
        return render_markdown(tab.markdown, section.accent, usize::from(code_width));
    }

    let subtab = tab.subtabs[subtab.min(tab.subtabs.len().saturating_sub(1))];
    if subtab.subtabs.is_empty() {
        return render_markdown(subtab.markdown, section.accent, usize::from(code_width));
    }

    let leaf_markdown =
        subtab.subtabs[leaf_tab.min(subtab.subtabs.len().saturating_sub(1))].markdown;
    let mut text = render_markdown(tab.markdown, section.accent, usize::from(code_width));
    text.lines
        .extend(render_markdown(subtab.markdown, section.accent, usize::from(code_width)).lines);
    text.lines
        .extend(render_markdown(leaf_markdown, section.accent, usize::from(code_width)).lines);
    text
}

pub(crate) fn section_nested_texts_for_width(
    section: Section,
    tab: usize,
    subtab: usize,
    leaf_tab: usize,
    code_width: u16,
) -> Option<(Text<'static>, Text<'static>, Text<'static>)> {
    let tab = section.tabs[tab.min(section_tab_count(section) - 1)];
    if tab.subtabs.is_empty() {
        return None;
    }
    let subtab = tab.subtabs[subtab.min(tab.subtabs.len().saturating_sub(1))];
    if subtab.subtabs.is_empty() {
        return None;
    }
    let leaf_markdown =
        subtab.subtabs[leaf_tab.min(subtab.subtabs.len().saturating_sub(1))].markdown;
    Some((
        render_markdown(tab.markdown, section.accent, usize::from(code_width)),
        render_markdown(subtab.markdown, section.accent, usize::from(code_width)),
        render_markdown(leaf_markdown, section.accent, usize::from(code_width)),
    ))
}

fn render_markdown(markdown: &'static str, accent: Color, code_width: usize) -> Text<'static> {
    let mut lines = Vec::new();
    let mut highlighter: Option<HighlightLines<'static>> = None;
    let mut code_language = "";

    for raw in markdown.lines() {
        if let Some(language) = raw.strip_prefix("```") {
            if highlighter.is_some() || !code_language.is_empty() {
                lines.push(Line::from(""));
                highlighter = None;
                code_language = "";
            } else {
                code_language = language.trim();
                lines.push(Line::from(""));
                highlighter = code_highlighter(code_language);
            }
            continue;
        }

        if !code_language.is_empty() {
            lines.push(highlighted_code_line(
                raw,
                code_language,
                highlighter.as_mut(),
                code_width,
            ));
            continue;
        }

        lines.push(markdown_line(raw, accent));
    }

    Text::from(lines)
}

fn markdown_line(raw: &'static str, accent: Color) -> Line<'static> {
    let trimmed = raw.trim_end();
    if trimmed.is_empty() {
        return Line::from("");
    }
    if let Some(text) = trimmed.strip_prefix("# ") {
        return Line::from(Span::styled(
            text.to_owned(),
            Style::default()
                .fg(accent)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        ));
    }
    if let Some(text) = trimmed.strip_prefix("## ") {
        return Line::from(Span::styled(
            text.to_owned(),
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        ));
    }
    if let Some(text) = trimmed.strip_prefix("### ") {
        return Line::from(Span::styled(
            text.to_owned(),
            Style::default().fg(MUTED).add_modifier(Modifier::BOLD),
        ));
    }
    if let Some(text) = trimmed.strip_prefix("- ") {
        let mut spans = vec![Span::styled("  - ", Style::default().fg(accent))];
        spans.extend(inline_spans(text, Style::default().fg(TEXT)));
        return Line::from(spans);
    }
    if let Some((prefix, text)) = trimmed.split_once(". ")
        && !prefix.is_empty()
        && prefix.chars().all(|ch| ch.is_ascii_digit())
    {
        let mut spans = vec![Span::styled(
            format!("{prefix}. "),
            Style::default().fg(accent),
        )];
        spans.extend(inline_spans(text, Style::default().fg(TEXT)));
        return Line::from(spans);
    }
    if let Some(text) = trimmed.strip_prefix("> ") {
        let mut spans = vec![Span::styled("│ ", Style::default().fg(accent))];
        spans.extend(inline_spans(text, Style::default().fg(MUTED)));
        return Line::from(spans);
    }
    Line::from(inline_spans(trimmed, Style::default().fg(TEXT)))
}

fn inline_spans(text: &str, base: Style) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    for (index, part) in text.split('`').enumerate() {
        if part.is_empty() {
            continue;
        }
        let style = if index % 2 == 1 {
            Style::default().fg(YELLOW).bg(CODE_BG)
        } else {
            base
        };
        spans.push(Span::styled(part.to_owned(), style));
    }
    spans
}

fn highlighted_code_line(
    line: &str,
    language: &str,
    highlighter: Option<&mut HighlightLines<'static>>,
    code_width: usize,
) -> Line<'static> {
    if language.eq_ignore_ascii_case("toml") {
        return toml_code_line(line, code_width);
    }

    let mut spans = vec![Span::styled("  ", Style::default().bg(CODE_BG))];
    let Some(highlighter) = highlighter else {
        spans.push(Span::styled(
            line.to_owned(),
            Style::default().fg(YELLOW).bg(CODE_BG),
        ));
        return padded_code_line(spans, code_width);
    };
    let Ok(ranges) = highlighter.highlight_line(line, syntax_set()) else {
        spans.push(Span::styled(
            line.to_owned(),
            Style::default().fg(YELLOW).bg(CODE_BG),
        ));
        return padded_code_line(spans, code_width);
    };
    spans.extend(
        ranges
            .into_iter()
            .map(|(style, text)| Span::styled(text.to_owned(), syntect_style(style))),
    );
    padded_code_line(spans, code_width)
}

fn toml_code_line(line: &str, code_width: usize) -> Line<'static> {
    let mut spans = vec![Span::styled("  ", Style::default().bg(CODE_BG))];
    let trimmed = line.trim_start();
    let leading = line.len().saturating_sub(trimmed.len());
    if leading > 0 {
        spans.push(Span::styled(
            line[..leading].to_owned(),
            Style::default().bg(CODE_BG),
        ));
    }
    if trimmed.is_empty() {
        return padded_code_line(spans, code_width);
    }
    if trimmed.starts_with('#') {
        spans.push(Span::styled(trimmed.to_owned(), code_style(MUTED)));
        return padded_code_line(spans, code_width);
    }
    if trimmed.starts_with('[') && trimmed.ends_with(']') {
        spans.push(Span::styled(
            trimmed.to_owned(),
            code_style(CYAN).add_modifier(Modifier::BOLD),
        ));
        return padded_code_line(spans, code_width);
    }
    let Some((key, value)) = trimmed.split_once('=') else {
        spans.push(Span::styled(trimmed.to_owned(), code_style(TEXT)));
        return padded_code_line(spans, code_width);
    };
    spans.push(Span::styled(key.trim_end().to_owned(), code_style(BLUE)));
    spans.push(Span::styled(" = ".to_owned(), code_style(MUTED)));
    spans.extend(toml_value_spans(value.trim_start()));
    padded_code_line(spans, code_width)
}

fn toml_value_spans(value: &str) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut token = String::new();
    let mut in_string = false;
    for ch in value.chars() {
        if ch == '"' {
            token.push(ch);
            if in_string {
                spans.push(Span::styled(std::mem::take(&mut token), code_style(YELLOW)));
            }
            in_string = !in_string;
            continue;
        }
        if in_string {
            token.push(ch);
            continue;
        }
        if ch == '#' {
            if !token.is_empty() {
                spans.push(toml_value_token(&std::mem::take(&mut token)));
            }
            spans.push(Span::styled(
                value[value.find('#').unwrap_or(value.len())..].to_owned(),
                code_style(MUTED),
            ));
            return spans;
        }
        if matches!(ch, '[' | ']' | ',') {
            if !token.is_empty() {
                spans.push(toml_value_token(&std::mem::take(&mut token)));
            }
            spans.push(Span::styled(ch.to_string(), code_style(MUTED)));
        } else {
            token.push(ch);
        }
    }
    if !token.is_empty() {
        spans.push(toml_value_token(&token));
    }
    spans
}

fn toml_value_token(token: &str) -> Span<'static> {
    let trimmed = token.trim();
    let color = if trimmed.parse::<f64>().is_ok() || matches!(trimmed, "true" | "false") {
        GREEN
    } else {
        TEXT
    };
    Span::styled(token.to_owned(), code_style(color))
}

fn code_style(color: Color) -> Style {
    Style::default().fg(color).bg(CODE_BG)
}

fn padded_code_line(mut spans: Vec<Span<'static>>, code_width: usize) -> Line<'static> {
    let used = spans
        .iter()
        .map(|span| span.content.chars().count())
        .sum::<usize>();
    let pad = code_width.saturating_sub(used);
    if pad > 0 {
        spans.push(Span::styled(
            "\u{00a0}".repeat(pad),
            Style::default().bg(CODE_BG),
        ));
    }
    Line::from(spans)
}

fn code_highlighter(language: &str) -> Option<HighlightLines<'static>> {
    let language = language.trim().to_ascii_lowercase();
    let candidates: &[&str] = match language.as_str() {
        "ts" | "typescript" => &["ts", "tsx", "TypeScript", "JavaScript"],
        "tsx" => &["tsx", "ts", "TypeScript", "JavaScript"],
        "js" | "jsx" | "javascript" => &["js", "jsx", "JavaScript"],
        "sh" | "shell" | "bash" => &["sh", "bash", "Bourne Again Shell (bash)"],
        "toml" => &["toml", "TOML"],
        "rust" | "rs" => &["rs", "rust", "Rust"],
        "text" => &["txt", "Plain Text"],
        other => &[other],
    };
    let syntax = candidates.iter().find_map(|candidate| {
        syntax_set()
            .find_syntax_by_extension(candidate)
            .or_else(|| syntax_set().find_syntax_by_token(candidate))
            .or_else(|| syntax_set().find_syntax_by_name(candidate))
    })?;
    let theme = theme_set()
        .themes
        .get("base16-ocean.dark")
        .or_else(|| theme_set().themes.get("Solarized (dark)"))?;
    Some(HighlightLines::new(syntax, theme))
}

fn syntect_style(style: SyntectStyle) -> Style {
    let color = boost_color(style.foreground.r, style.foreground.g, style.foreground.b);
    let mut ratatui_style = Style::default().fg(color).bg(CODE_BG);
    if style.font_style.contains(FontStyle::BOLD) {
        ratatui_style = ratatui_style.add_modifier(Modifier::BOLD);
    }
    if style.font_style.contains(FontStyle::ITALIC) {
        ratatui_style = ratatui_style.add_modifier(Modifier::ITALIC);
    }
    if style.font_style.contains(FontStyle::UNDERLINE) {
        ratatui_style = ratatui_style.add_modifier(Modifier::UNDERLINED);
    }
    ratatui_style
}

fn boost_color(r: u8, g: u8, b: u8) -> Color {
    let boost = |value: u8| value.saturating_add(48);
    Color::Rgb(boost(r), boost(g), boost(b))
}

fn syntax_set() -> &'static SyntaxSet {
    static SYNTAX_SET: LazyLock<SyntaxSet> = LazyLock::new(SyntaxSet::load_defaults_newlines);
    &SYNTAX_SET
}

fn theme_set() -> &'static ThemeSet {
    static THEME_SET: LazyLock<ThemeSet> = LazyLock::new(ThemeSet::load_defaults);
    &THEME_SET
}
