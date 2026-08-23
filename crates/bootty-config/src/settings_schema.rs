//! What a setting *is*, as data.
//!
//! A [`SettingSpec`] names one config key once: its TOML path, the value it holds, how it is
//! labelled, and what it falls back to. The settings UI renders specs instead of hand-writing a
//! read/widget/write block per key, which is what lets an extension contribute a setting through
//! the same path a built-in uses.
//!
//! This lives in `bootty-config`, not `bootty-ui`: a spec names [`BoottyConfig`] and config paths,
//! both product types. `bootty-ui` stays a widget library.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::ops::RangeInclusive;
use std::sync::OnceLock;

use serde::Serialize;

use crate::config::{BoottyConfig, config_token};

mod builtin;

/// One editable config key.
#[derive(Clone, Debug)]
pub struct SettingSpec {
    /// TOML path, e.g. `["window", "width"]`. Also the spec's identity, joined with `.`.
    pub path: Vec<Cow<'static, str>>,
    pub label: Cow<'static, str>,
    pub help: Cow<'static, str>,
    /// Page the setting appears on, matching the settings surface's page ids.
    pub page: Cow<'static, str>,
    /// Section header within the page. Specs render in declaration order within a section.
    pub section: Cow<'static, str>,
    pub kind: SettingKind,
    /// Legacy paths removed whenever this key is written, so an old spelling cannot win the next
    /// load. Empty for almost every setting.
    pub supersedes: Vec<Vec<Cow<'static, str>>>,
    pub default: SettingDefault,
}

/// The path/page projection of one [`SettingSpec`]. The settings registry derives these values;
/// callers do not author a second declaration list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SettingDeclaration {
    /// TOML path. A `*` segment matches one dynamic table key; a trailing `*` matches the rest of
    /// a path, which is used for extension-owned settings.
    pub path: Vec<Cow<'static, str>>,
    /// Settings page that owns the editor for this value.
    pub page: Cow<'static, str>,
}

impl SettingDeclaration {
    #[must_use]
    pub fn path_parts(&self) -> Vec<&str> {
        self.path.iter().map(Cow::as_ref).collect()
    }

    #[must_use]
    pub fn matches_path(&self, path: &[&str]) -> bool {
        let pattern = self.path_parts();
        let trailing_wildcard = pattern.last() == Some(&"*");
        if !trailing_wildcard && pattern.len() != path.len() {
            return false;
        }
        if trailing_wildcard && path.len() < pattern.len().saturating_sub(1) {
            return false;
        }
        pattern
            .iter()
            .zip(path)
            .all(|(expected, actual)| *expected == "*" || *expected == *actual)
    }

    #[must_use]
    fn matches_prefix(&self, path: &[&str]) -> bool {
        let pattern = self.path_parts();
        path.len() <= pattern.len()
            && pattern
                .iter()
                .zip(path)
                .all(|(expected, actual)| *expected == "*" || *expected == *actual)
    }
}

impl SettingSpec {
    /// Stable identity: the TOML path joined with `.`.
    #[must_use]
    pub fn id(&self) -> String {
        self.path.join(".")
    }

    /// The path as the borrowed slice the document readers and writers take.
    #[must_use]
    pub fn path_parts(&self) -> Vec<&str> {
        self.path.iter().map(Cow::as_ref).collect()
    }

    /// The value shown when the document says nothing about this key.
    #[must_use]
    pub fn default_value(&self, defaults: &BoottyConfig) -> SettingValue {
        match &self.default {
            SettingDefault::Field(read) => read(defaults),
            SettingDefault::Literal(value) => value.clone(),
            SettingDefault::Unused => {
                panic!("custom setting {} has no schema default", self.id())
            }
        }
    }

    /// Whether `needle` matches this spec, for the settings search box.
    #[must_use]
    pub fn matches(&self, needle: &str) -> bool {
        let needle = needle.trim().to_ascii_lowercase();
        needle.is_empty()
            || self.label.to_ascii_lowercase().contains(&needle)
            || self.help.to_ascii_lowercase().contains(&needle)
            || self.id().to_ascii_lowercase().contains(&needle)
    }
}

/// Where a spec's fallback value comes from.
#[derive(Clone, Debug)]
pub enum SettingDefault {
    /// Built-in: read the field off the default config, so the fallback is never a literal copied
    /// out of `defaults.rs`, and a renamed field is a compile error at the spec.
    Field(fn(&BoottyConfig) -> SettingValue),
    /// Extension-declared: no typed field exists, so the declaration carries the value.
    Literal(SettingValue),
    /// A hand-written editor owns the value and reads its typed config field directly.
    Unused,
}

/// What the setting holds, and how to edit it.
#[derive(Clone, Debug)]
pub enum SettingKind {
    Bool,
    Text {
        placeholder: Cow<'static, str>,
        /// An empty value removes the key instead of writing an empty string.
        optional: bool,
    },
    Number {
        range: RangeInclusive<f32>,
        control: NumberControl,
        /// Decimal places shown. A count reads wrong as `3.0`.
        precision: usize,
        suffix: Cow<'static, str>,
        /// Multiplier applied for display only: a 0.0-1.0 fraction shown as a percentage uses 100.
        display_scale: f32,
    },
    Choice {
        options: Vec<SettingOption>,
    },
    /// A non-scalar setting whose editor is owned by a settings-surface module. It is still in
    /// this registry so the path cannot be accepted by the loader without an editor owner.
    Custom(SettingEditor),
}

/// The hand-written editor responsible for a non-scalar setting.
///
/// This is deliberately closed. Adding a new editor family requires adding a named owner here
/// and handling it in the settings surface, rather than silently creating another unregistered
/// path list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingEditor {
    Appearance,
    Colors,
    Text,
    General,
    Status,
    Sidebar,
    Remotes,
    Keys,
    Shell,
    Window,
    Extensions,
}

impl SettingEditor {
    /// Stable owner name used in diagnostics and exhaustive editor dispatch.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Appearance => "appearance",
            Self::Colors => "colors",
            Self::Text => "text",
            Self::General => "general",
            Self::Status => "status",
            Self::Sidebar => "sidebar",
            Self::Remotes => "remotes",
            Self::Keys => "keys",
            Self::Shell => "shell",
            Self::Window => "window",
            Self::Extensions => "extensions",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NumberControl {
    Edit,
    Slider,
}

/// One choice in a [`SettingKind::Choice`].
#[derive(Clone, Debug)]
pub struct SettingOption {
    /// The token written to `config.toml`.
    pub token: Cow<'static, str>,
    pub label: Cow<'static, str>,
    /// One line saying what picking this option does. When any option on a setting carries one, the
    /// setting renders as a described list rather than a row of bare labels.
    pub description: Option<Cow<'static, str>>,
}

impl SettingOption {
    /// Take the token from the variant's own `Serialize` impl — the same machinery that reads it
    /// back — so an option can never disagree with the parser. Only the label is authored.
    ///
    /// # Panics
    /// If `value` is not a unit variant, which cannot be a config token.
    #[must_use]
    pub fn of<T: Serialize>(value: &T, label: &'static str) -> Self {
        Self {
            token: config_token(value)
                .expect("a setting option must be a unit variant")
                .into(),
            label: label.into(),
            description: None,
        }
    }

    /// An option that explains itself.
    ///
    /// # Panics
    /// If `value` is not a unit variant, which cannot be a config token.
    #[must_use]
    pub fn described<T: Serialize>(
        value: &T,
        label: &'static str,
        description: &'static str,
    ) -> Self {
        Self {
            description: Some(description.into()),
            ..Self::of(value, label)
        }
    }
}

/// A setting's value, in the shape the document holds it.
#[derive(Clone, Debug, PartialEq)]
pub enum SettingValue {
    Bool(bool),
    Text(String),
    Number(f32),
    Token(String),
}

impl SettingValue {
    #[must_use]
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(value) => Some(*value),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_number(&self) -> Option<f32> {
        match self {
            Self::Number(value) => Some(*value),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Text(value) | Self::Token(value) => Some(value),
            _ => None,
        }
    }
}

/// One setting an extension declared for itself, as the schema needs it.
#[derive(Clone, Debug, PartialEq)]
pub struct ExtensionSetting {
    /// The declaring module's namespace, supplied by the extension host.
    pub module: String,
    pub key: String,
    pub label: String,
    pub help: String,
    pub default: crate::config::ExtensionSettingValue,
}

impl ExtensionSetting {
    /// The page every extension setting appears on.
    pub const PAGE: &'static str = "extensions";

    fn to_spec(&self) -> SettingSpec {
        use crate::config::ExtensionSettingValue as Value;
        let (kind, default) = match &self.default {
            Value::Bool(value) => (SettingKind::Bool, SettingValue::Bool(*value)),
            Value::Number(value) => (
                SettingKind::Number {
                    // An extension declares no range. Bound it to what a setting plausibly holds
                    // so the field sizes to its own contents rather than to f32's extremes.
                    range: -1_000_000.0..=1_000_000.0,
                    control: NumberControl::Edit,
                    // Keep a whole number whole; only show a decimal if the default has one.
                    precision: usize::from(value.fract() != 0.0),
                    suffix: String::new().into(),
                    display_scale: 1.0,
                },
                SettingValue::Number(*value as f32),
            ),
            Value::Text(value) => (
                SettingKind::Text {
                    placeholder: String::new().into(),
                    optional: true,
                },
                SettingValue::Text(value.clone()),
            ),
        };
        SettingSpec {
            path: crate::config::extension_setting_path(&self.module, &self.key)
                .into_iter()
                .map(Cow::Owned)
                .collect(),
            label: if self.label.is_empty() {
                self.key.clone().into()
            } else {
                self.label.clone().into()
            },
            help: self.help.clone().into(),
            page: Self::PAGE.into(),
            section: self
                .module
                .replace(['-', '_', '.'], " ")
                .to_uppercase()
                .into(),
            kind,
            supersedes: Vec::new(),
            default: SettingDefault::Literal(default),
        }
    }
}

/// Every setting the UI can render: the built-ins, plus whatever extensions contributed.
#[derive(Debug, Default)]
pub struct SettingsSchema {
    specs: Vec<SettingSpec>,
    declarations: Vec<SettingDeclaration>,
    by_id: BTreeMap<String, usize>,
}

impl SettingsSchema {
    #[must_use]
    pub fn new(specs: Vec<SettingSpec>) -> Self {
        let by_id = specs
            .iter()
            .enumerate()
            .map(|(index, spec)| (spec.id(), index))
            .collect();
        let declarations = specs
            .iter()
            .map(|spec| SettingDeclaration {
                path: spec.path.clone(),
                page: spec.page.clone(),
            })
            .collect();
        Self {
            specs,
            declarations,
            by_id,
        }
    }

    /// The built-ins plus one spec per extension-declared setting. Extension settings all land on
    /// the Extensions page, sectioned by module, and write under `extensions.<module>.<key>` —
    /// a path the caller derives from the module's identity, never from the module's own input.
    #[must_use]
    pub fn with_extensions(declarations: &[ExtensionSetting]) -> Self {
        let mut specs = builtin::specs();
        let extension_specs = declarations.iter().map(ExtensionSetting::to_spec);
        specs.extend(extension_specs);
        Self::new(specs)
    }

    /// The built-in settings. Shared, because they never change within a run.
    #[must_use]
    pub fn builtin() -> &'static Self {
        static SCHEMA: OnceLock<SettingsSchema> = OnceLock::new();
        SCHEMA.get_or_init(|| Self::new(builtin::specs()))
    }

    #[must_use]
    pub fn specs(&self) -> &[SettingSpec] {
        &self.specs
    }

    #[must_use]
    pub fn declarations(&self) -> &[SettingDeclaration] {
        &self.declarations
    }

    /// Whether a TOML path is a declared user setting.
    #[must_use]
    pub fn allows_path(&self, path: &[&str]) -> bool {
        (path == ["version"])
            || self
                .declarations
                .iter()
                .any(|declaration| declaration.matches_path(path))
            || builtin::compatibility_paths()
                .iter()
                .any(|pattern| path_matches(pattern, path))
    }

    /// Whether a write may target this path or a declared table below it.
    ///
    /// Hand-written editors sometimes serialize a complete table (for example one SSH profile)
    /// instead of setting each leaf independently. The table is allowed only when the registry
    /// declares at least one supported leaf below it.
    #[must_use]
    pub fn allows_write_path(&self, path: &[&str]) -> bool {
        self.allows_path(path)
            || self
                .declarations
                .iter()
                .any(|declaration| declaration.matches_prefix(path))
            || builtin::compatibility_paths()
                .iter()
                .any(|pattern| path_matches_prefix(pattern, path))
    }

    /// The settings on one page, in declaration order.
    pub fn page<'a>(&'a self, page: &'a str) -> impl Iterator<Item = &'a SettingSpec> + 'a {
        self.specs.iter().filter(move |spec| spec.page == page)
    }

    #[must_use]
    pub fn get(&self, id: &str) -> Option<&SettingSpec> {
        self.by_id.get(id).map(|index| &self.specs[*index])
    }
}

fn path_matches(pattern: &[&str], path: &[&str]) -> bool {
    let trailing_wildcard = pattern.last() == Some(&"*");
    if !trailing_wildcard && pattern.len() != path.len() {
        return false;
    }
    if trailing_wildcard && path.len() < pattern.len().saturating_sub(1) {
        return false;
    }
    pattern
        .iter()
        .zip(path)
        .all(|(expected, actual)| *expected == "*" || *expected == *actual)
}

fn path_matches_prefix(pattern: &[&str], path: &[&str]) -> bool {
    path.len() <= pattern.len()
        && pattern
            .iter()
            .zip(path)
            .all(|(expected, actual)| *expected == "*" || *expected == *actual)
}
