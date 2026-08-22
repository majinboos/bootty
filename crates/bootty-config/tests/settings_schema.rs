//! Every built-in setting spec must describe a key the loader actually reads.
//!
//! Paths used to be bare string literals at the call site with nothing checking them. These tests
//! are what replaces that: a wrong path, a kebab/underscore typo, a fallback that drifted from
//! `defaults.rs`, or a choice token the parser rejects all fail here.

use std::fs;

use bootty_config::config::{BoottyConfig, load_config_from_path};
use bootty_config::settings_schema::{SettingKind, SettingSpec, SettingValue, SettingsSchema};

/// Write one key into an otherwise empty config and load it back.
fn load_with(path: &[&str], toml_value: &str) -> BoottyConfig {
    let directory = tempfile::tempdir().expect("temporary config directory");
    let config_path = directory.path().join("config.toml");
    let (leaf, parents) = path.split_last().expect("non-empty path");
    let source = if parents.is_empty() {
        format!("{leaf} = {toml_value}\n")
    } else {
        format!("[{}]\n{leaf} = {toml_value}\n", parents.join("."))
    };
    fs::write(&config_path, &source).expect("write config");
    load_config_from_path(&config_path)
        .unwrap_or_else(|error| panic!("{source}\nfailed to load: {error}"))
}

#[test]
fn every_spec_default_matches_the_config_default() {
    let defaults = BoottyConfig::default();
    for spec in SettingsSchema::builtin().specs() {
        let value = spec.default_value(&defaults);
        let matches_kind = matches!(
            (&spec.kind, &value),
            (SettingKind::Bool, SettingValue::Bool(_))
                | (SettingKind::Text { .. }, SettingValue::Text(_))
                | (SettingKind::Number { .. }, SettingValue::Number(_))
                | (SettingKind::Choice { .. }, SettingValue::Token(_))
        );
        assert!(
            matches_kind,
            "{}: default {value:?} does not match its kind",
            spec.id()
        );
        if let (SettingKind::Number { range, .. }, SettingValue::Number(number)) =
            (&spec.kind, &value)
        {
            assert!(
                range.contains(number),
                "{}: default {number} is outside {range:?}",
                spec.id()
            );
        }
        if let (SettingKind::Choice { options }, SettingValue::Token(token)) = (&spec.kind, &value)
        {
            assert!(
                options.iter().any(|option| option.token == *token),
                "{}: default token {token:?} is not one of its options",
                spec.id()
            );
        }
    }
}

#[test]
fn every_spec_path_round_trips_through_the_loader() {
    for spec in SettingsSchema::builtin().specs() {
        let path = spec.path_parts();
        let default = spec.default_value(&BoottyConfig::default());
        let (written, expected) = match &spec.kind {
            SettingKind::Bool => {
                // Probe the opposite of the default, or the write could land nowhere unnoticed.
                let probe = !default.as_bool().unwrap_or_default();
                (probe.to_string(), SettingValue::Bool(probe))
            }
            SettingKind::Text { .. } => (
                "\"round trip\"".to_owned(),
                SettingValue::Text("round trip".to_owned()),
            ),
            SettingKind::Number { range, .. } => {
                // Pick a value inside the range that is not the default, so a path that silently
                // writes nowhere cannot pass by reading the default back.
                let value = (range.start() + range.end()) / 2.0;
                let value = (value * 10.0).round() / 10.0;
                (format!("{value}"), SettingValue::Number(value))
            }
            SettingKind::Choice { options } => {
                let option = options.last().expect("a choice has options");
                (
                    format!("\"{}\"", option.token),
                    SettingValue::Token(option.token.to_string()),
                )
            }
        };

        // A value equal to the default would let a path that writes nowhere pass.
        assert_ne!(
            expected,
            default,
            "{}: the probe value must differ from the default",
            spec.id()
        );

        let config = load_with(&path, &written);
        let read_back = spec.default_value(&config);
        assert_eq!(
            read_back,
            expected,
            "{}: writing {written} at {path:?} did not read back",
            spec.id()
        );
    }
}

#[test]
fn spec_ids_are_unique_and_resolvable() {
    let schema = SettingsSchema::builtin();
    for spec in schema.specs() {
        assert!(
            schema.get(&spec.id()).is_some(),
            "{} is not resolvable by id",
            spec.id()
        );
    }
    let mut ids: Vec<String> = schema.specs().iter().map(SettingSpec::id).collect();
    ids.sort();
    let count = ids.len();
    ids.dedup();
    assert_eq!(ids.len(), count, "duplicate setting ids");
}
