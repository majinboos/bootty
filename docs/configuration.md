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
family = ["Maple Mono", "Font Awesome 7 Brands", "Maple Mono NF", "monospace"]
size = 15.666
cell-width = 10
cell-height = 22
baseline-adjustment = 3
underline-position = 2
underline-thickness = 1

[chrome]
sidebar = true
status-bar = true
sidebar-width = 286
status-height = 30
gap = 1

[multiplexer]
backend = "rmux"

# native keeps mux state and terminals inside Bootty. rmux renders through
# rmux-sdk. tmux and zellij attach through their backend UI.

[input]
keybind = ["cmd+shift+,=reload_config"]
macos-option-as-alt = "both" # none, left, right, or both
modifier-remap = ["right_alt=left_ctrl"]

[session]
shell = "/bin/zsh"
working-directory = "/Users/example/src"
env = [{ name = "EDITOR", value = "vim" }]
term = "xterm-ghostty"
colorterm = "truecolor"

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
parser and rejects actions that the app cannot execute. Supported app actions
today are `reload_config`, `ignore`, `csi:...`, `esc:...`, `text:...`, font
size changes, `copy_to_clipboard`, `paste_from_clipboard`, `new_window`,
`close_window`, `close_surface`, `quit`, and `toggle_fullscreen`.

Reload validates the full effective config first. If parsing, theme resolution,
modifier remap parsing, keybind parsing, or live terminal color application
fails, the current in-memory config remains active.

Live-applied fields:

- `[chrome]` sidebar/status visibility and layout
- `[multiplexer]` backend selection and backend UI mode
- `[input]` modifier remaps, macOS Option-as-Meta mode, and reload keybinds
- `[font]` terminal text metrics
- `theme` and `[colors]` terminal defaults
- `[window].title`
- `[diagnostics].stability-trace`

New-session/new-window-only fields:

- `[session]` shell, working directory, environment, `TERM`, and `COLORTERM`
- `[window].width` and `[window].height`
- `[window].macos-titlebar-style`

When a reload includes new-session/new-window-only changes, Bootty keeps the
current terminal session alive and shows a status message that those settings
apply next time.

## Window chrome and fullscreen

`[window].macos-titlebar-style = "hidden"` hides the titlebar and titlebar
buttons for new windows. `window-decoration = "none"` disables native window
decorations.

`[window].fullscreen` accepts `false`, `true`/`"native"`, `"non-native"`,
`"non-native-visible-menu"`, or `"non-native-padded-notch"`. Native fullscreen
uses the platform fullscreen path. Non-native modes create a borderless
maximized window and are also used by `toggle_fullscreen` when configured. On
macOS, `"non-native"` hides the menu bar and Dock so the window covers that
space; `"non-native-visible-menu"` intentionally leaves the menu bar available.

## Preference writeback

Bootty has a round-trip TOML editing path for preference writeback. It edits the
user's `config.toml` rather than writing a generated full config, preserving
unrelated comments, ordering, includes, and tables. Current code exposes a
focused font-size writeback helper for future settings UI; no UI writes config
preferences yet.

If a writeback target file does not exist, Bootty creates it. If an existing
file cannot be parsed as TOML, writeback fails rather than replacing the file.

## Compatibility notes

- `BOOTTY_SHELL` remains a compatibility override in the session launcher.
  Configured `[session].shell` is passed as the explicit shell setting, then the
  runtime preserves its existing shell precedence.
- `BOOTTY_STABILITY_TRACE` remains a diagnostics fallback when
  `[diagnostics].stability-trace` is not set.
- Bootty config is TOML with Ghostty-inspired vocabulary; it is not Ghostty's
  config syntax.

## Manual review notes

Recorded on May 19, 2026 during the config implementation branch review:

- Default/no-config behavior: covered by `missing_config_file_loads_current_defaults`
  and startup/app default tests.
- Built-in theme smoke: covered by `builtin_theme_loads_colors_through_config_theme_name`
  and terminal color frame tests.
- User theme shadowing smoke: covered by `user_theme_shadows_builtin_theme_name`.
- Reload/writeback smoke: covered by `reload_keeps_last_good_config_when_new_config_is_invalid`,
  live keybinding/app scope tests, terminal live color tests, and round-trip
  writeback fixtures.
- Visual screenshot review was not captured in the headless agent environment.
