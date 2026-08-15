# Bootty configuration

Bootty loads a native TOML config from:

```text
$XDG_CONFIG_HOME/bootty/config.toml
```

If `XDG_CONFIG_HOME` is not set, Bootty uses
`$HOME/.config/bootty/config.toml`. If neither environment variable is set, the
fallback path is `bootty/config.toml` relative to the process working directory.

An absent config file is not an error; Bootty starts with built-in defaults.
Invalid startup config currently fails startup with a parse/load error. Runtime
reload is non-destructive: invalid reloads keep the last-good in-memory config
and show the error in the status bar.

For a one-off run that ignores live config and sidecar state, use:

```sh
bootty --defaults
```

To load a specific config file instead of the XDG path, use:

```sh
bootty --config /path/to/config.toml
```

Startup flags can override common config values after the file is loaded:

```sh
bootty --defaults --backend native --fullscreen non-native --titlebar hidden --window-decoration none --no-sidebar
```

Run `bootty --help` for the full flag list.

## Example

See `docs/sample-config.toml` for a complete sample with every supported
configuration field. The inline example below shows the same shape.

```toml
version = 1
theme = "Catppuccin Mocha"
include = ["?local.toml"]

[window]
title = "Agent Shell"
width = 1220
height = 760
fullscreen = false
window-decoration = "auto"
macos-titlebar-style = "transparent"

[font]
family = ["monospace"]
size = 15.666
# Optional fixed cell metric overrides; omit to derive from font size.
# cell-width = 10
# cell-height = 22
fit-cell-height = true

[chrome]
sidebar = true
top-bar = true
bottom-bar = false
sidebar-width = 286
status-height = 30
status-background = "#1e1e2e"
notched-fullscreen-black-chrome = true
gap = 1
unfocused-sidebar-dim = 0.16
unfocused-terminal-dim = 0.08

[multiplexer]
backend = "rmux"

# native keeps mux state and terminals inside Bootty. rmux renders through
# rmux-sdk. tmux and zellij attach through their backend UI.

# Attach a multiplexer running on another host. Bootty reaches the remote host
# over SSH and renders its sessions here. On first use, Bootty uploads the
# matching daemon bundled with the app. If the bundle has no matching target,
# Bootty downloads and checksum-verifies the release daemon. New Session uses
# that daemon to discover projects and Git worktrees on the remote filesystem.
# The full Bootty app is not required on the remote host.
[multiplexer.remote]
host = "devbox" # ~/.ssh/config alias, hostname, or address
# user = "dev"        # when ~/.ssh/config does not name one
# port = 2222          # when ~/.ssh/config does not name one
# program = "ssh"      # the SSH client to run
# args = ["-i", "~/.ssh/devbox"] # extra flags, passed before the destination

# `bootty --backend tmux --ssh-remote devbox` does the same for one run.
#
# This is the default every space inherits. A single space can name its own host
# in the space editor, next to its backend, so remote and local spaces sit side
# by side; that override lives in the workspace database, not in this file.
#
# Bootty dials with ConnectTimeout=5, ServerAliveInterval=5 and
# ServerAliveCountMax=3, so a lost connection ends in about 15 seconds instead of
# hanging. The pane then reconnects on its own, with a growing delay between
# attempts; the sessions stay on the remote host throughout. Flags listed in
# `args` come first on the command line, so any of these can be set differently.

[input]
preset = "ghostty" # ghostty (default), bootty, or tmux — which built-in default keybind set to use
prefix = "ctrl+space" # leader for prefixed chords (bootty/tmux presets); defaults to ctrl+space / ctrl+b
keybind = ["cmd+shift+,=reload_config"]
sidebar-keybind = ["Enter=activate_session", "j=next_session", "k=previous_session"]
hide-mouse-pointer-while-typing = true
macos-option-as-alt = "both" # none, left, right, or both
modifier-remap = ["right_alt=left_ctrl"]

[session]
shell = "/bin/zsh"
working-directory = "/Users/example/src"
env = [{ name = "EDITOR", value = "vim" }]
term = "xterm-bootty"
colorterm = "truecolor"
max-scrollback = 320000000
glyph-protocol = true

[cursor]
style = "block" # block, bar, underline, or hollow-block
blink = false

[diagnostics]
stability-trace = "/tmp/bootty-stability.csv"

[colors]
background = "#1e1e2e"
foreground = "#cdd6f4"
cursor = "#f5e0dc"
palette = [
  "#45475a", "#f38ba8", "#a6e3a1", "#f9e2af",
  "#89b4fa", "#f5c2e7", "#94e2d5", "#bac2de",
  "#585b70", "#f38ba8", "#a6e3a1", "#f9e2af",
  "#89b4fa", "#f5c2e7", "#94e2d5", "#a6adc8",
]
```

Only set values you want to override. Unknown fields are rejected.

By default, cell width and height are derived from the selected font and
`[font].size`; set `cell-width` or `cell-height` only when you want a fixed
override. `[font].fit-cell-height = true` is also enabled by default. It keeps
the row count implied by the current cell height, then stretches row spacing just
enough for those rows to fill the terminal area's available height. It does not
change the glyph font size.

## Includes

`include` is a top-level array of paths:

```toml
include = ["shared.toml", "?local.toml"]
```

- Paths are relative to the containing config file.
- Included files are applied after the containing file, so included values can
  override earlier values.
- Prefix a path with `?` to make it optional.
- Include cycles are rejected.

## Themes

`theme = "name"` resolves in this order:

1. user themes in the config directory, under `themes/<name>.toml` or
   `themes/<name>/theme.toml`
2. the built-in catalog

User themes shadow built-ins with the same name. Explicit `[colors]` values in
`config.toml` override the selected theme.

User and built-in theme files use a restricted schema:

```toml
[metadata]
name = "My Theme"
source = "local"
license = "personal"

[colors]
background = "#000000"
foreground = "#ffffff"
cursor = "#ffffff"
palette = ["#000000", "#ff0000"]
```

Theme files must not configure shell, input, window, font, or chrome settings.
Built-in theme source and license notes are tracked in
`docs/built-in-themes.md`.

## Reload behavior

Bootty automatically checks the config file and any current includes for changes
and reloads after saves. The reload keybinding is still available for manual
retry or edge cases:

```toml
[input]
keybind = ["cmd+shift+,=reload_config"]
```

Bootty parses configured keybind strings with the shared Ghostty-style binding
parser and rejects actions that the app cannot execute. Supported global app
actions today include reload/ignore, terminal byte writes (`csi:...`,
`esc:...`, `text:...`), font size changes, clipboard paste/copy, window
lifecycle, fullscreen/sidebar chrome, mux session navigation, native tab/pane
actions, and terminal scroll actions. `[input].sidebar-keybind` is sidebar-local
and supports `ignore`, `previous_session`, `next_session`, `activate_session`,
and `focus_terminal`.
Wheel triggers use `scroll_up` and `scroll_down`; the default `alt+shift+scroll`
bindings adjust font size in quarter-point steps. The settings keybind recorder
captures the wheel as well as the keyboard, so wheel triggers can be bound
without hand-editing the config.

Reload validates the full effective config first. If parsing, theme resolution,
modifier remap parsing, keybind parsing, or live terminal color/cursor
application fails, the current in-memory config remains active.

Libghostty-vt logs are bridged to stderr. Set `BOOTTY_LIBGHOSTTY_LOG=error|warn|info|debug|off` to adjust verbosity; unset defaults to warnings and errors.

Live-applied fields:

- `[chrome]` sidebar/status visibility, layout, and inactive panel dimming
- `[multiplexer]` backend selection and backend UI mode
- `[input]` modifier remaps, macOS Option-as-Meta mode, global keybinds, and sidebar keybinds
- `theme`, `[colors]` terminal defaults, `[cursor]` defaults, `[font]` metrics,
  and `[session].glyph-protocol`
- `[window].title`
- `[diagnostics].stability-trace`

New-session/new-window-only fields:

- `[session]` shell, working directory, environment, `TERM`, `COLORTERM`, and max scrollback
- `[window].width` and `[window].height`
- `[window].fullscreen`
- `[window].window-decoration`
- `[window].macos-titlebar-style`

When a reload includes new-session/new-window-only changes, Bootty keeps the
current terminal session alive and shows a status message. Open a new Bootty
window, or restart Bootty, for those settings to take effect.

## Window chrome and fullscreen

`[window].macos-titlebar-style = "hidden"` hides the titlebar and titlebar
buttons for new windows. `window-decoration = "none"` disables native window
decorations.

`[window].fullscreen` accepts `false`, `true`/`"native"`, `"non-native"`,
`"non-native-visible-menu"`, or `"non-native-padded-notch"`. Native fullscreen
uses the platform fullscreen path. Non-native modes create a borderless
fullscreen-style window and are also used by `toggle_fullscreen` when configured.
On macOS, `"non-native"` and `"non-native-padded-notch"` auto-hide the menu bar
and Dock so they reappear when the pointer reaches the screen edge;
`"non-native-visible-menu"` intentionally leaves the menu bar visible.

## Preference writeback

Bootty has a round-trip TOML editing path for preference writeback. It edits the
user's `config.toml` rather than writing a generated full config, preserving
unrelated comments, ordering, includes, and tables. The settings UI, sidebar
width, appearance mode, active theme, and font-size actions use this path.

Bootty writes a complete temporary file beside the config. It synchronizes the
file before it atomically replaces the config. Existing symlinks remain links.
Existing Unix permission bits remain unchanged. A new Unix config uses mode
`0600` because it can contain SSH connection metadata.

If a writeback target file does not exist, Bootty creates it. If an existing
file cannot be parsed as TOML, writeback fails rather than replacing the file.
Bootty writers for one config path are serialized. External editors do not use
the Bootty writer lease, so writeback is not a cross-program compare-and-swap.

## Compatibility notes

- `BOOTTY_SHELL` remains a compatibility override in the session launcher.
  Configured `[session].shell` is passed as the explicit shell setting, then the
  runtime preserves its existing shell precedence.
- `BOOTTY_STABILITY_TRACE` remains a diagnostics fallback when
  `[diagnostics].stability-trace` is not set.
- Bootty config is TOML with Ghostty-inspired vocabulary; it is not Ghostty's
  config syntax.
