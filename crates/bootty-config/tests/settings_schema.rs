//! Every built-in setting spec must describe a key the loader actually reads.
//!
//! Paths used to be bare string literals at the call site with nothing checking them. These tests
//! are what replaces that: a wrong path, a kebab/underscore typo, a fallback that drifted from
//! `defaults.rs`, or a choice token the parser rejects all fail here.
use assert_fs::prelude::*;
use bootty_config::config::{BoottyConfig, load_config_from_path};
use bootty_config::settings_schema::{SettingKind, SettingSpec, SettingValue, SettingsSchema};
use pretty_assertions::{assert_eq, assert_ne};

/// Write one key into an otherwise empty config and load it back.
fn load_with(path: &[&str], toml_value: &str) -> BoottyConfig {
    let directory = assert_fs::TempDir::new().expect("temporary config directory");
    let config_path = directory.child("config.toml");
    let (leaf, parents) = path.split_last().expect("non-empty path");
    let source = if parents.is_empty() {
        format!("{leaf} = {toml_value}\n")
    } else {
        format!("[{}]\n{leaf} = {toml_value}\n", parents.join("."))
    };
    config_path.write_str(&source).expect("write config");
    load_config_from_path(config_path.path())
        .unwrap_or_else(|error| panic!("{source}\nfailed to load: {error}"))
}

#[test]
fn every_spec_default_matches_the_config_default() {
    let defaults = BoottyConfig::default();
    for spec in SettingsSchema::builtin().specs() {
        if matches!(&spec.kind, SettingKind::Custom(_)) {
            continue;
        }
        let value = spec.default_value(&defaults);
        let matches_kind = matches!(
            (&spec.kind, &value),
            (SettingKind::Bool, SettingValue::Bool(_))
                | (SettingKind::Text { .. }, SettingValue::Text(_))
                | (SettingKind::Number { .. }, SettingValue::Number(_))
                | (SettingKind::Choice { .. }, SettingValue::Token(_))
                | (SettingKind::Custom(_), _)
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
        if matches!(&spec.kind, SettingKind::Custom(_)) {
            continue;
        }
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
            SettingKind::Custom(_) => unreachable!("custom settings are not scalar probes"),
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

#[test]
fn every_hand_written_spec_has_a_known_editor_owner() {
    for spec in SettingsSchema::builtin().specs() {
        if let SettingKind::Custom(editor) = &spec.kind {
            assert_eq!(
                spec.page,
                editor.name(),
                "{} has a mismatched owner",
                spec.id()
            );
        }
    }
}

#[test]
fn an_unregistered_toml_leaf_fails_at_the_schema_boundary() {
    let directory = assert_fs::TempDir::new().expect("temporary config directory");
    let config_path = directory.child("config.toml");
    config_path
        .write_str("[window]\nsetting-that-has-no-editor = true\n")
        .expect("write config");

    let error = load_config_from_path(config_path.path()).expect_err("unknown setting must fail");
    assert!(
        error
            .to_string()
            .contains("unsupported config setting window.setting-that-has-no-editor")
    );
    assert!(error.to_string().contains("declare it in SettingsSchema"));
}

#[test]
fn extension_schemas_keep_the_builtin_hand_written_declarations() {
    let schema = SettingsSchema::with_extensions(&[]);
    assert!(schema.allows_path(&["cursor", "style"]));
    assert!(schema.allows_path(&["cursor", "dim-inactive-pane"]));
    assert!(schema.allows_path(&["input", "copy-on-select"]));
    assert!(schema.allows_path(&["multiplexer", "remote", "args"]));
    assert_eq!(schema.get("cursor.style").unwrap().page, "appearance");
    assert_eq!(
        schema
            .get("input.hide-mouse-pointer-while-typing")
            .unwrap()
            .page,
        "appearance"
    );
}
