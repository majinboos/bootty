# libghostty-rs dependency boundary

Bootty uses `libghostty-rs` as an external binding crate for Ghostty terminal
state and parsing.

- Source: `https://github.com/Uzaaft/libghostty-rs.git`
- Ref: `e025ef03e8a3f10603c7a3253e63c49b36f1ff0d`
- Dependency: workspace `libghostty-vt` Git dependency in `Cargo.toml`
- License: see the upstream repository

Bootty must not patch or extend `libghostty-rs` in-tree. Functionality that can
be implemented by preprocessing terminal input, postprocessing frame data, or
using public `libghostty-vt` APIs belongs in Bootty crates, primarily
`crates/bootty-terminal`.

Functionality that requires Ghostty internals not exposed through the
`libghostty-vt` C API is unsupported unless it can be approximated entirely in
Bootty without modifying the binding crate.

`bootty-config` owns neutral parsed RGBA values. `bootty-app::app::terminal_config`
owns the conversion from those values into `libghostty-vt` terminal RGB values.
The conversion intentionally drops alpha because the terminal RGB type has no
alpha channel.
