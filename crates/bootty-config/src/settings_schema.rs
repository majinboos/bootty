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
        suffix: Cow<'static, str>,
        /// Multiplier applied for display only: a 0.0-1.0 fraction shown as a percentage uses 100.
        display_scale: f32,
    },
    Choice {
        options: Vec<SettingOption>,
    },
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
                    // An extension declares no range, so the editor accepts what TOML can hold.
                    range: f32::MIN..=f32::MAX,
                    control: NumberControl::Edit,
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
            section: self.module.clone().to_uppercase().into(),
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
        Self { specs, by_id }
    }

    /// The built-ins plus one spec per extension-declared setting. Extension settings all land on
    /// the Extensions page, sectioned by module, and write under `extensions.<module>.<key>` —
    /// a path the caller derives from the module's identity, never from the module's own input.
    #[must_use]
    pub fn with_extensions(declarations: &[ExtensionSetting]) -> Self {
        let mut specs = builtin::specs();
        specs.extend(declarations.iter().map(ExtensionSetting::to_spec));
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

    /// The settings on one page, in declaration order.
    pub fn page<'a>(&'a self, page: &'a str) -> impl Iterator<Item = &'a SettingSpec> + 'a {
        self.specs.iter().filter(move |spec| spec.page == page)
    }

    #[must_use]
    pub fn get(&self, id: &str) -> Option<&SettingSpec> {
        self.by_id.get(id).map(|index| &self.specs[*index])
    }
}
