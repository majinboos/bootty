# Bootty Tauri WebGL demo

This crate is a small example of embedding Bootty's non-egui terminal stack in a
Tauri application with a React frontend.

## Backend boundary

`src/lib.rs` owns the Tauri bridge:

- starts a `bootty_runtime::TerminalSession`
- resizes the PTY/grid from browser canvas dimensions
- writes raw terminal input bytes
- serializes `RenderFrame` into a small web DTO

The backend does not depend on egui.

## Frontend boundary

`src-ui/src` is intentionally split into three pieces:

- `terminal-api.ts` wraps the Tauri commands
- `main.tsx` is the React shell and polling/input loop
- `webgl-terminal.ts` renders terminal frames with WebGL2

The WebGL renderer uses instanced quads for backgrounds, text, underlines, and
cursor. Text glyphs are cached in a DPR-aware atlas; Canvas2D is only used when
rasterizing a new glyph into that atlas.

## Run

```sh
npm install
npm run tauri -- dev
```

## Build check

```sh
npm run build
cargo test -p bootty-tauri
npm run tauri -- build --no-bundle
```