# Bootty

Bootty is a reusable terminal embedding library extracted from Majin.

The repository is intentionally split into small crates:

- `bootty-surface` - terminal geometry and surface math.
- `bootty-terminal` - Ghostty-backed terminal state and render frames.
- `bootty-runtime` - PTY-backed terminal sessions.
- `bootty-render` - paint plans, text shaping, sprites, and WGPU rendering.
- `bootty` - convenience re-export crate for embedders.
- `bootty-winit` - native Winit/WGPU host example support.
- `bootty-tauri` - Tauri + React + WebGL2 embedding demo.

## Run the examples

Native Winit/WGPU terminal:

```sh
cargo run -p bootty-winit --example winit
```

Tauri/WebGL2 terminal:

```sh
cd crates/bootty-tauri
npm install
npm run tauri -- dev
```

## Library shape

Embedders usually need:

1. `bootty_runtime::TerminalSession` to own the PTY and terminal worker.
2. `TerminalSession::extract_frame()` to read the latest terminal frame.
3. A renderer that consumes `bootty_terminal::terminal::RenderFrame`.

The Winit example uses the Rust `bootty-render` WGPU path. The Tauri example
serializes frames into a small DTO and renders them with WebGL2 in the frontend.