use serde::Deserialize;

use super::load::{ConfigLoadError, ConfigResult};
use super::model::{ColorConfig, ResolvedTheme, ThemeInfo};
use super::raw::ColorPatch;
use super::resolve::apply_partial_colors;

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RawTheme {
    #[serde(default)]
    metadata: ThemeMetadata,
    #[serde(default)]
    colors: ColorPatch,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct ThemeMetadata {
    name: Option<String>,
    source: Option<String>,
    license: Option<String>,
}

pub(super) fn load_builtin_theme(theme: &str) -> Option<ResolvedTheme> {
    BUILTIN_THEMES
        .iter()
        .find(|builtin| theme_name_matches(builtin.name, theme))
        .map(|builtin| {
            parse_theme_source(builtin.source, &format!("built-in theme {}", builtin.name))
                .expect("built-in themes must parse")
        })
}

fn theme_name_matches(candidate: &str, requested: &str) -> bool {
    candidate.eq_ignore_ascii_case(requested)
        || requested
            .strip_prefix("iTerm2 ")
            .is_some_and(|stripped| candidate.eq_ignore_ascii_case(stripped))
}

pub fn builtin_theme_names() -> impl Iterator<Item = &'static str> {
    BUILTIN_THEMES.iter().map(|theme| theme.name)
}

pub(super) fn parse_theme_source(source: &str, label: &str) -> ConfigResult<ResolvedTheme> {
    let raw: RawTheme = toml_edit::de::from_str(source)
        .map_err(|error| ConfigLoadError::new(format!("failed to parse theme {label}: {error}")))?;
    let mut colors = ColorConfig::default();
    apply_partial_colors(&mut colors, raw.colors);
    Ok(ResolvedTheme {
        info: ThemeInfo {
            name: raw.metadata.name.unwrap_or_else(|| label.to_owned()),
            source: raw.metadata.source.unwrap_or_default(),
            license: raw.metadata.license.unwrap_or_default(),
        },
        colors,
    })
}

struct BuiltinTheme {
    name: &'static str,
    source: &'static str,
}

pub const DEFAULT_LIGHT_THEME: &str = "Catppuccin Latte";
pub const DEFAULT_DARK_THEME: &str = "Catppuccin Mocha";

const BUILTIN_THEMES: &[BuiltinTheme] = &[
    BuiltinTheme {
        name: "Catppuccin Mocha",
        source: CATPPUCCIN_MOCHA_THEME,
    },
    BuiltinTheme {
        name: "Catppuccin Latte",
        source: CATPPUCCIN_LATTE_THEME,
    },
    BuiltinTheme {
        name: "Catppuccin Frappe",
        source: CATPPUCCIN_FRAPPE_THEME,
    },
    BuiltinTheme {
        name: "Catppuccin Macchiato",
        source: CATPPUCCIN_MACCHIATO_THEME,
    },
    BuiltinTheme {
        name: "Atom One Dark",
        source: ATOM_ONE_DARK_THEME,
    },
    BuiltinTheme {
        name: "Atom One Light",
        source: ATOM_ONE_LIGHT_THEME,
    },
    BuiltinTheme {
        name: "Ayu",
        source: AYU_THEME,
    },
    BuiltinTheme {
        name: "Ayu Light",
        source: AYU_LIGHT_THEME,
    },
    BuiltinTheme {
        name: "Ayu Mirage",
        source: AYU_MIRAGE_THEME,
    },
    BuiltinTheme {
        name: "Dracula",
        source: DRACULA_THEME,
    },
    BuiltinTheme {
        name: "Everforest Dark Hard",
        source: EVERFOREST_DARK_HARD_THEME,
    },
    BuiltinTheme {
        name: "Everforest Dark Med",
        source: EVERFOREST_DARK_MED_THEME,
    },
    BuiltinTheme {
        name: "Everforest Dark Soft",
        source: EVERFOREST_DARK_SOFT_THEME,
    },
    BuiltinTheme {
        name: "Everforest Light Hard",
        source: EVERFOREST_LIGHT_HARD_THEME,
    },
    BuiltinTheme {
        name: "Everforest Light Med",
        source: EVERFOREST_LIGHT_MED_THEME,
    },
    BuiltinTheme {
        name: "Everforest Light Soft",
        source: EVERFOREST_LIGHT_SOFT_THEME,
    },
    BuiltinTheme {
        name: "Flexoki Dark",
        source: FLEXOKI_DARK_THEME,
    },
    BuiltinTheme {
        name: "Flexoki Light",
        source: FLEXOKI_LIGHT_THEME,
    },
    BuiltinTheme {
        name: "Kanagawa Dragon",
        source: KANAGAWA_DRAGON_THEME,
    },
    BuiltinTheme {
        name: "Kanagawa Lotus",
        source: KANAGAWA_LOTUS_THEME,
    },
    BuiltinTheme {
        name: "Kanagawa Wave",
        source: KANAGAWA_WAVE_THEME,
    },
    BuiltinTheme {
        name: "Rose Pine",
        source: ROSE_PINE_THEME,
    },
    BuiltinTheme {
        name: "Rose Pine Dawn",
        source: ROSE_PINE_DAWN_THEME,
    },
    BuiltinTheme {
        name: "Rose Pine Moon",
        source: ROSE_PINE_MOON_THEME,
    },
    BuiltinTheme {
        name: "TokyoNight Night",
        source: TOKYONIGHT_NIGHT_THEME,
    },
    BuiltinTheme {
        name: "TokyoNight Day",
        source: TOKYONIGHT_DAY_THEME,
    },
    BuiltinTheme {
        name: "TokyoNight Moon",
        source: TOKYONIGHT_MOON_THEME,
    },
    BuiltinTheme {
        name: "TokyoNight Storm",
        source: TOKYONIGHT_STORM_THEME,
    },
    BuiltinTheme {
        name: "Solarized Dark",
        source: ITERM2_SOLARIZED_DARK_THEME,
    },
    BuiltinTheme {
        name: "Solarized Light",
        source: ITERM2_SOLARIZED_LIGHT_THEME,
    },
    BuiltinTheme {
        name: "Xcode Dark",
        source: XCODE_DARK_THEME,
    },
    BuiltinTheme {
        name: "Xcode Light",
        source: XCODE_LIGHT_THEME,
    },
    BuiltinTheme {
        name: "Gruvbox Dark",
        source: GRUVBOX_DARK_THEME,
    },
];

const FLEXOKI_DARK_THEME: &str = r##"
[metadata]
name = "Flexoki Dark"
source = "mbadolato/iTerm2-Color-Schemes ghostty/Flexoki Dark"
license = "MIT collection; individual theme provenance applies"

[colors]
palette = ["#100f0f", "#d14d41", "#879a39", "#d0a215", "#4385be", "#ce5d97", "#3aa99f", "#878580", "#575653", "#af3029", "#66800b", "#ad8301", "#205ea6", "#a02f6f", "#24837b", "#cecdc3"]
background = "#100f0f"
foreground = "#cecdc3"
cursor = "#cecdc3"
cursor-text = "#100f0f"
selection-background = "#403e3c"
selection-foreground = "#cecdc3"
"##;

const FLEXOKI_LIGHT_THEME: &str = r##"
[metadata]
name = "Flexoki Light"
source = "mbadolato/iTerm2-Color-Schemes ghostty/Flexoki Light"
license = "MIT collection; individual theme provenance applies"

[colors]
palette = ["#100f0f", "#af3029", "#66800b", "#ad8301", "#205ea6", "#a02f6f", "#24837b", "#6f6e69", "#b7b5ac", "#d14d41", "#879a39", "#d0a215", "#4385be", "#ce5d97", "#3aa99f", "#cecdc3"]
background = "#fffcf0"
foreground = "#100f0f"
cursor = "#100f0f"
cursor-text = "#fffcf0"
selection-background = "#cecdc3"
selection-foreground = "#100f0f"
"##;

const EVERFOREST_DARK_HARD_THEME: &str = r##"
[metadata]
name = "Everforest Dark Hard"
source = "mbadolato/iTerm2-Color-Schemes ghostty/Everforest Dark Hard"
license = "MIT collection; individual theme provenance applies"

[colors]
palette = ["#7a8478", "#e67e80", "#a7c080", "#dbbc7f", "#7fbbb3", "#d699b6", "#83c092", "#f2efdf", "#a6b0a0", "#f85552", "#8da101", "#dfa000", "#3a94c5", "#df69ba", "#35a77c", "#fffbef"]
background = "#1e2326"
foreground = "#d3c6aa"
cursor = "#e69875"
cursor-text = "#4c3743"
selection-background = "#4c3743"
selection-foreground = "#d3c6aa"
"##;

const EVERFOREST_DARK_MED_THEME: &str = r##"
[metadata]
name = "Everforest Dark Med"
source = "mbadolato/iTerm2-Color-Schemes ghostty/Everforest Dark Med"
license = "MIT collection; individual theme provenance applies"

[colors]
palette = ["#7a8478", "#e67e80", "#a7c080", "#dbbc7f", "#7fbbb3", "#d699b6", "#83c092", "#f2efdf", "#a6b0a0", "#f85552", "#8da101", "#dfa000", "#3a94c5", "#df69ba", "#35a77c", "#fffbef"]
background = "#232a2e"
foreground = "#d3c6aa"
cursor = "#e69875"
cursor-text = "#543a48"
selection-background = "#543a48"
selection-foreground = "#d3c6aa"
"##;

const EVERFOREST_DARK_SOFT_THEME: &str = r##"
[metadata]
name = "Everforest Dark Soft"
source = "mbadolato/iTerm2-Color-Schemes ghostty/Everforest Dark Soft"
license = "MIT collection; individual theme provenance applies"

[colors]
palette = ["#7a8478", "#e67e80", "#a7c080", "#dbbc7f", "#7fbbb3", "#d699b6", "#83c092", "#f2efdf", "#a6b0a0", "#f85552", "#8da101", "#dfa000", "#3a94c5", "#df69ba", "#35a77c", "#fffbef"]
background = "#293136"
foreground = "#d3c6aa"
cursor = "#e69875"
cursor-text = "#5c3f4f"
selection-background = "#5c3f4f"
selection-foreground = "#d3c6aa"
"##;

const EVERFOREST_LIGHT_HARD_THEME: &str = r##"
[metadata]
name = "Everforest Light Hard"
source = "mbadolato/iTerm2-Color-Schemes ghostty/Everforest Light Hard"
license = "MIT collection; individual theme provenance applies"

[colors]
palette = ["#7a8478", "#e67e80", "#9ab373", "#ceaf72", "#7fbbb3", "#d699b6", "#83c092", "#b2af9f", "#a6b0a0", "#f85552", "#8da101", "#dfa000", "#3a94c5", "#df69ba", "#35a77c", "#fffbef"]
background = "#f2efdf"
foreground = "#5c6a72"
cursor = "#f57d26"
cursor-text = "#f0f2d4"
selection-background = "#f0f2d4"
selection-foreground = "#5c6a72"
"##;

const EVERFOREST_LIGHT_MED_THEME: &str = r##"
[metadata]
name = "Everforest Light Med"
source = "mbadolato/iTerm2-Color-Schemes ghostty/Everforest Light Med"
license = "MIT collection; individual theme provenance applies"

[colors]
palette = ["#7a8478", "#e67e80", "#9ab373", "#c1a266", "#7fbbb3", "#d699b6", "#83c092", "#b2af9f", "#a6b0a0", "#f85552", "#8da101", "#dfa000", "#3a94c5", "#df69ba", "#35a77c", "#fffbef"]
background = "#efebd4"
foreground = "#5c6a72"
cursor = "#f57d26"
cursor-text = "#eaedc8"
selection-background = "#eaedc8"
selection-foreground = "#5c6a72"
"##;

const EVERFOREST_LIGHT_SOFT_THEME: &str = r##"
[metadata]
name = "Everforest Light Soft"
source = "mbadolato/iTerm2-Color-Schemes ghostty/Everforest Light Soft"
license = "MIT collection; individual theme provenance applies"

[colors]
palette = ["#7a8478", "#e67e80", "#8da666", "#c1a266", "#72aea6", "#c98ca9", "#76b385", "#a5a292", "#99a393", "#f85552", "#8da101", "#d29300", "#3a94c5", "#df69ba", "#35a77c", "#fffbef"]
background = "#e5dfc5"
foreground = "#5c6a72"
cursor = "#f57d26"
cursor-text = "#e1e4bd"
selection-background = "#e1e4bd"
selection-foreground = "#5c6a72"
"##;

const KANAGAWA_DRAGON_THEME: &str = r##"
[metadata]
name = "Kanagawa Dragon"
source = "mbadolato/iTerm2-Color-Schemes ghostty/Kanagawa Dragon"
license = "MIT collection; individual theme provenance applies"

[colors]
palette = ["#0d0c0c", "#c4746e", "#8a9a7b", "#c4b28a", "#8ba4b0", "#a292a3", "#8ea4a2", "#c8c093", "#a6a69c", "#e46876", "#87a987", "#e6c384", "#7fb4ca", "#938aa9", "#7aa89f", "#c5c9c5"]
background = "#181616"
foreground = "#c5c9c5"
cursor = "#c8c093"
cursor-text = "#181616"
selection-background = "#c5c9c5"
selection-foreground = "#181616"
"##;

const KANAGAWA_LOTUS_THEME: &str = r##"
[metadata]
name = "Kanagawa Lotus"
source = "mbadolato/iTerm2-Color-Schemes ghostty/Kanagawa Lotus"
license = "MIT collection; individual theme provenance applies"

[colors]
palette = ["#1f1f28", "#c84053", "#6f894e", "#77713f", "#4d699b", "#b35b79", "#597b75", "#545464", "#8a8980", "#d7474b", "#6e915f", "#836f4a", "#6693bf", "#624c83", "#5e857a", "#43436c"]
background = "#f2ecbc"
foreground = "#545464"
cursor = "#43436c"
cursor-text = "#f2ecbc"
selection-background = "#545464"
selection-foreground = "#f2ecbc"
"##;

const KANAGAWA_WAVE_THEME: &str = r##"
[metadata]
name = "Kanagawa Wave"
source = "mbadolato/iTerm2-Color-Schemes ghostty/Kanagawa Wave"
license = "MIT collection; individual theme provenance applies"

[colors]
palette = ["#090618", "#c34043", "#76946a", "#c0a36e", "#7e9cd8", "#957fb8", "#6a9589", "#c8c093", "#727169", "#e82424", "#98bb6c", "#e6c384", "#7fb4ca", "#938aa9", "#7aa89f", "#dcd7ba"]
background = "#1f1f28"
foreground = "#dcd7ba"
cursor = "#dcd7ba"
cursor-text = "#1f1f28"
selection-background = "#dcd7ba"
selection-foreground = "#1f1f28"
"##;

const ROSE_PINE_THEME: &str = r##"
[metadata]
name = "Rose Pine"
source = "mbadolato/iTerm2-Color-Schemes ghostty/Rose Pine"
license = "MIT collection; individual theme provenance applies"

[colors]
palette = ["#26233a", "#eb6f92", "#31748f", "#f6c177", "#9ccfd8", "#c4a7e7", "#ebbcba", "#e0def4", "#6e6a86", "#eb6f92", "#31748f", "#f6c177", "#9ccfd8", "#c4a7e7", "#ebbcba", "#e0def4"]
background = "#191724"
foreground = "#e0def4"
cursor = "#e0def4"
cursor-text = "#191724"
selection-background = "#403d52"
selection-foreground = "#e0def4"
"##;

const ROSE_PINE_DAWN_THEME: &str = r##"
[metadata]
name = "Rose Pine Dawn"
source = "mbadolato/iTerm2-Color-Schemes ghostty/Rose Pine Dawn"
license = "MIT collection; individual theme provenance applies"

[colors]
palette = ["#f2e9e1", "#b4637a", "#286983", "#ea9d34", "#56949f", "#907aa9", "#d7827e", "#575279", "#9893a5", "#b4637a", "#286983", "#ea9d34", "#56949f", "#907aa9", "#d7827e", "#575279"]
background = "#faf4ed"
foreground = "#575279"
cursor = "#575279"
cursor-text = "#faf4ed"
selection-background = "#dfdad9"
selection-foreground = "#575279"
"##;

const ROSE_PINE_MOON_THEME: &str = r##"
[metadata]
name = "Rose Pine Moon"
source = "mbadolato/iTerm2-Color-Schemes ghostty/Rose Pine Moon"
license = "MIT collection; individual theme provenance applies"

[colors]
palette = ["#393552", "#eb6f92", "#3e8fb0", "#f6c177", "#9ccfd8", "#c4a7e7", "#ea9a97", "#e0def4", "#6e6a86", "#eb6f92", "#3e8fb0", "#f6c177", "#9ccfd8", "#c4a7e7", "#ea9a97", "#e0def4"]
background = "#232136"
foreground = "#e0def4"
cursor = "#e0def4"
cursor-text = "#232136"
selection-background = "#44415a"
selection-foreground = "#e0def4"
"##;

const CATPPUCCIN_MOCHA_THEME: &str = r##"
[metadata]
name = "Catppuccin Mocha"
source = "catppuccin/ghostty and mbadolato/iTerm2-Color-Schemes ghostty/Catppuccin Mocha"
license = "MIT"

[colors]
palette = ["#45475a", "#f38ba8", "#a6e3a1", "#f9e2af", "#89b4fa", "#f5c2e7", "#94e2d5", "#a6adc8", "#585b70", "#f37799", "#89d88b", "#ebd391", "#74a8fc", "#f2aede", "#6bd7ca", "#bac2de"]
background = "#1e1e2e"
foreground = "#cdd6f4"
cursor = "#f5e0dc"
cursor-text = "#1e1e2e"
selection-background = "#585b70"
selection-foreground = "#cdd6f4"
"##;

const CATPPUCCIN_LATTE_THEME: &str = r##"
[metadata]
name = "Catppuccin Latte"
source = "catppuccin/ghostty and mbadolato/iTerm2-Color-Schemes ghostty/Catppuccin Latte"
license = "MIT"

[colors]
palette = ["#5c5f77", "#d20f39", "#40a02b", "#df8e1d", "#1e66f5", "#ea76cb", "#179299", "#acb0be", "#6c6f85", "#d20f39", "#40a02b", "#df8e1d", "#1e66f5", "#ea76cb", "#179299", "#bcc0cc"]
background = "#eff1f5"
foreground = "#4c4f69"
cursor = "#dc8a78"
cursor-text = "#eff1f5"
selection-background = "#acb0be"
selection-foreground = "#4c4f69"
"##;

const CATPPUCCIN_FRAPPE_THEME: &str = r##"
[metadata]
name = "Catppuccin Frappe"
source = "mbadolato/iTerm2-Color-Schemes ghostty/Catppuccin Frappe"
license = "MIT collection; individual theme provenance applies"

[colors]
palette = ["#51576d", "#e78284", "#a6d189", "#e5c890", "#8caaee", "#f4b8e4", "#81c8be", "#b5bfe2", "#626880", "#eda0a2", "#b9dba2", "#ecd7ae", "#adc2f3", "#f38ed8", "#98d2ca", "#a5adce"]
background = "#303446"
foreground = "#c6d0f5"
cursor = "#f2d5cf"
cursor-text = "#303446"
selection-background = "#f2d5cf"
selection-foreground = "#303446"
"##;

const CATPPUCCIN_MACCHIATO_THEME: &str = r##"
[metadata]
name = "Catppuccin Macchiato"
source = "mbadolato/iTerm2-Color-Schemes ghostty/Catppuccin Macchiato"
license = "MIT collection; individual theme provenance applies"

[colors]
palette = ["#494d64", "#ed8796", "#a6da95", "#eed49f", "#8aadf4", "#f5bde6", "#8bd5ca", "#b8c0e0", "#5b6078", "#f2a7b2", "#bde3b0", "#f4e3c1", "#adc5f7", "#f493da", "#a5ded6", "#a5adcb"]
background = "#24273a"
foreground = "#cad3f5"
cursor = "#f4dbd6"
cursor-text = "#24273a"
selection-background = "#f4dbd6"
selection-foreground = "#24273a"
"##;

const ATOM_ONE_DARK_THEME: &str = r##"
[metadata]
name = "Atom One Dark"
source = "mbadolato/iTerm2-Color-Schemes ghostty/Atom One Dark"
license = "MIT collection; individual theme provenance applies"

[colors]
palette = ["#21252b", "#e06c75", "#98c379", "#e5c07b", "#61afef", "#c678dd", "#56b6c2", "#abb2bf", "#767676", "#e06c75", "#98c379", "#e5c07b", "#61afef", "#c678dd", "#56b6c2", "#abb2bf"]
background = "#21252b"
foreground = "#abb2bf"
cursor = "#abb2bf"
cursor-text = "#21252b"
selection-background = "#323844"
selection-foreground = "#abb2bf"
"##;

const ATOM_ONE_LIGHT_THEME: &str = r##"
[metadata]
name = "Atom One Light"
source = "mbadolato/iTerm2-Color-Schemes ghostty/Atom One Light"
license = "MIT collection; individual theme provenance applies"

[colors]
palette = ["#000000", "#de3e35", "#3f953a", "#d2b67c", "#2f5af3", "#950095", "#3f953a", "#bbbbbb", "#000000", "#de3e35", "#3f953a", "#d2b67c", "#2f5af3", "#a00095", "#3f953a", "#ffffff"]
background = "#f9f9f9"
foreground = "#2a2c33"
cursor = "#bbbbbb"
cursor-text = "#ffffff"
selection-background = "#ededed"
selection-foreground = "#2a2c33"
"##;

const AYU_THEME: &str = r##"
[metadata]
name = "Ayu"
source = "mbadolato/iTerm2-Color-Schemes ghostty/Ayu"
license = "MIT collection; individual theme provenance applies"

[colors]
palette = ["#11151c", "#ea6c73", "#7fd962", "#f9af4f", "#53bdfa", "#cda1fa", "#90e1c6", "#c7c7c7", "#686868", "#f07178", "#aad94c", "#ffb454", "#59c2ff", "#d2a6ff", "#95e6cb", "#ffffff"]
background = "#0b0e14"
foreground = "#bfbdb6"
cursor = "#e6b450"
cursor-text = "#0b0e14"
selection-background = "#409fff"
selection-foreground = "#0b0e14"
"##;

const AYU_LIGHT_THEME: &str = r##"
[metadata]
name = "Ayu Light"
source = "mbadolato/iTerm2-Color-Schemes ghostty/Ayu Light"
license = "MIT collection; individual theme provenance applies"

[colors]
palette = ["#000000", "#ea6c6d", "#6cbf43", "#eca944", "#3199e1", "#9e75c7", "#46ba94", "#bababa", "#686868", "#f07171", "#86b300", "#f2ae49", "#399ee6", "#a37acc", "#4cbf99", "#d1d1d1"]
background = "#f8f9fa"
foreground = "#5c6166"
cursor = "#ffaa33"
cursor-text = "#f8f9fa"
selection-background = "#035bd6"
selection-foreground = "#f8f9fa"
"##;

const AYU_MIRAGE_THEME: &str = r##"
[metadata]
name = "Ayu Mirage"
source = "mbadolato/iTerm2-Color-Schemes ghostty/Ayu Mirage"
license = "MIT collection; individual theme provenance applies"

[colors]
palette = ["#171b24", "#ed8274", "#87d96c", "#facc6e", "#6dcbfa", "#dabafa", "#90e1c6", "#c7c7c7", "#686868", "#f28779", "#d5ff80", "#ffd173", "#73d0ff", "#dfbfff", "#95e6cb", "#ffffff"]
background = "#1f2430"
foreground = "#cccac2"
cursor = "#ffcc66"
cursor-text = "#1f2430"
selection-background = "#409fff"
selection-foreground = "#1f2430"
"##;

const DRACULA_THEME: &str = r##"
[metadata]
name = "Dracula"
source = "mbadolato/iTerm2-Color-Schemes ghostty/Dracula"
license = "MIT collection; individual theme provenance applies"

[colors]
palette = ["#21222c", "#ff5555", "#50fa7b", "#f1fa8c", "#bd93f9", "#ff79c6", "#8be9fd", "#f8f8f2", "#6272a4", "#ff6e6e", "#69ff94", "#ffffa5", "#d6acff", "#ff92df", "#a4ffff", "#ffffff"]
background = "#282a36"
foreground = "#f8f8f2"
cursor = "#f8f8f2"
cursor-text = "#282a36"
selection-background = "#44475a"
selection-foreground = "#ffffff"
"##;

const TOKYONIGHT_NIGHT_THEME: &str = r##"
[metadata]
name = "TokyoNight Night"
source = "mbadolato/iTerm2-Color-Schemes ghostty/TokyoNight Night"
license = "MIT collection; individual theme provenance applies"

[colors]
palette = ["#15161e", "#f7768e", "#9ece6a", "#e0af68", "#7aa2f7", "#bb9af7", "#7dcfff", "#a9b1d6", "#414868", "#f7768e", "#9ece6a", "#e0af68", "#7aa2f7", "#bb9af7", "#7dcfff", "#c0caf5"]
background = "#1a1b26"
foreground = "#c0caf5"
cursor = "#c0caf5"
selection-background = "#33467c"
selection-foreground = "#c0caf5"
"##;

const TOKYONIGHT_DAY_THEME: &str = r##"
[metadata]
name = "TokyoNight Day"
source = "mbadolato/iTerm2-Color-Schemes ghostty/TokyoNight Day"
license = "MIT collection; individual theme provenance applies"

[colors]
palette = ["#e9e9ed", "#f52a65", "#587539", "#8c6c3e", "#2e7de9", "#9854f1", "#007197", "#6172b0", "#a1a6c5", "#f52a65", "#587539", "#8c6c3e", "#2e7de9", "#9854f1", "#007197", "#3760bf"]
background = "#e1e2e7"
foreground = "#3760bf"
cursor = "#3760bf"
cursor-text = "#e1e2e7"
selection-background = "#99a7df"
selection-foreground = "#3760bf"
"##;

const TOKYONIGHT_MOON_THEME: &str = r##"
[metadata]
name = "TokyoNight Moon"
source = "mbadolato/iTerm2-Color-Schemes ghostty/TokyoNight Moon"
license = "MIT collection; individual theme provenance applies"

[colors]
palette = ["#1b1d2b", "#ff757f", "#c3e88d", "#ffc777", "#82aaff", "#c099ff", "#86e1fc", "#828bb8", "#444a73", "#ff757f", "#c3e88d", "#ffc777", "#82aaff", "#c099ff", "#86e1fc", "#c8d3f5"]
background = "#222436"
foreground = "#c8d3f5"
cursor = "#c8d3f5"
cursor-text = "#222436"
selection-background = "#2d3f76"
selection-foreground = "#c8d3f5"
"##;

const TOKYONIGHT_STORM_THEME: &str = r##"
[metadata]
name = "TokyoNight Storm"
source = "mbadolato/iTerm2-Color-Schemes ghostty/TokyoNight Storm"
license = "MIT collection; individual theme provenance applies"

[colors]
palette = ["#1d202f", "#f7768e", "#9ece6a", "#e0af68", "#7aa2f7", "#bb9af7", "#7dcfff", "#a9b1d6", "#4e5575", "#f7768e", "#9ece6a", "#e0af68", "#7aa2f7", "#bb9af7", "#7dcfff", "#c0caf5"]
background = "#24283b"
foreground = "#c0caf5"
cursor = "#c0caf5"
cursor-text = "#1d202f"
selection-background = "#364a82"
selection-foreground = "#c0caf5"
"##;

const ITERM2_SOLARIZED_DARK_THEME: &str = r##"
[metadata]
name = "iTerm2 Solarized Dark"
source = "mbadolato/iTerm2-Color-Schemes ghostty/iTerm2 Solarized Dark"
license = "MIT collection; individual theme provenance applies"

[colors]
palette = ["#073642", "#dc322f", "#859900", "#b58900", "#268bd2", "#d33682", "#2aa198", "#eee8d5", "#335e69", "#cb4b16", "#586e75", "#657b83", "#839496", "#6c71c4", "#93a1a1", "#fdf6e3"]
background = "#002b36"
foreground = "#839496"
cursor = "#839496"
cursor-text = "#073642"
selection-background = "#073642"
selection-foreground = "#93a1a1"
"##;

const ITERM2_SOLARIZED_LIGHT_THEME: &str = r##"
[metadata]
name = "iTerm2 Solarized Light"
source = "mbadolato/iTerm2-Color-Schemes ghostty/iTerm2 Solarized Light"
license = "MIT collection; individual theme provenance applies"

[colors]
palette = ["#073642", "#dc322f", "#859900", "#b58900", "#268bd2", "#d33682", "#2aa198", "#bbb5a2", "#002b36", "#cb4b16", "#586e75", "#657b83", "#839496", "#6c71c4", "#93a1a1", "#fdf6e3"]
background = "#fdf6e3"
foreground = "#657b83"
cursor = "#657b83"
cursor-text = "#eee8d5"
selection-background = "#eee8d5"
selection-foreground = "#586e75"
"##;

const XCODE_DARK_THEME: &str = r##"
[metadata]
name = "Xcode Dark"
source = "mbadolato/iTerm2-Color-Schemes ghostty/Xcode Dark"
license = "MIT collection; individual theme provenance applies"

[colors]
palette = ["#414453", "#ff8170", "#78c2b3", "#d9c97c", "#4eb0cc", "#ff7ab2", "#b281eb", "#dfdfe0", "#7f8c98", "#ff8170", "#acf2e4", "#ffa14f", "#6bdfff", "#ff7ab2", "#dabaff", "#dfdfe0"]
background = "#292a30"
foreground = "#dfdfe0"
cursor = "#dfdfe0"
cursor-text = "#292a30"
selection-background = "#414453"
selection-foreground = "#dfdfe0"
"##;

const XCODE_LIGHT_THEME: &str = r##"
[metadata]
name = "Xcode Light"
source = "mbadolato/iTerm2-Color-Schemes ghostty/Xcode Light"
license = "MIT collection; individual theme provenance applies"

[colors]
palette = ["#b4d8fd", "#d12f1b", "#3e8087", "#78492a", "#0f68a0", "#ad3da4", "#804fb8", "#262626", "#8a99a6", "#d12f1b", "#23575c", "#78492a", "#0b4f79", "#ad3da4", "#4b21b0", "#262626"]
background = "#ffffff"
foreground = "#262626"
cursor = "#262626"
cursor-text = "#ffffff"
selection-background = "#b4d8fd"
selection-foreground = "#262626"
"##;

const GRUVBOX_DARK_THEME: &str = r##"
[metadata]
name = "Gruvbox Dark"
source = "mbadolato/iTerm2-Color-Schemes ghostty/Gruvbox Dark"
license = "MIT collection; individual theme provenance applies"

[colors]
palette = ["#282828", "#cc241d", "#98971a", "#d79921", "#458588", "#b16286", "#689d6a", "#a89984", "#928374", "#fb4934", "#b8bb26", "#fabd2f", "#83a598", "#d3869b", "#8ec07c", "#ebdbb2"]
background = "#282828"
foreground = "#ebdbb2"
cursor = "#ebdbb2"
selection-background = "#504945"
selection-foreground = "#ebdbb2"
"##;
