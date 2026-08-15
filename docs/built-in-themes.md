# Built-in theme provenance

Bootty's built-in theme catalog uses the same restricted theme schema as user
themes: metadata plus `[colors]`. Theme files must not configure shell, input,
window, font, or app chrome settings.

See `docs/configuration.md` for theme lookup order and user theme file
locations.

Current built-ins:

| Theme | Source | License note |
| --- | --- | --- |
| Catppuccin Mocha | `catppuccin/ghostty` template and `mbadolato/iTerm2-Color-Schemes/ghostty/Catppuccin Mocha` | Catppuccin repository is MIT; iTerm2-Color-Schemes collection is MIT and notes individual theme authorship |
| Catppuccin Latte | `catppuccin/ghostty` template and `mbadolato/iTerm2-Color-Schemes/ghostty/Catppuccin Latte` | Catppuccin repository is MIT; iTerm2-Color-Schemes collection is MIT and notes individual theme authorship |
| TokyoNight Night | `mbadolato/iTerm2-Color-Schemes/ghostty/TokyoNight Night` | iTerm2-Color-Schemes collection is MIT and notes individual theme authorship |
| Gruvbox Dark | `mbadolato/iTerm2-Color-Schemes/ghostty/Gruvbox Dark` | iTerm2-Color-Schemes collection is MIT and notes individual theme authorship |

The resolver and schema are large-catalog-ready, but new built-ins should be
added only with source and license notes.
