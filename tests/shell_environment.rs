#![cfg(target_os = "macos")]

use bootty_app::shell_env::{
    advertised_shell, parse_login_environment, selected_login_shell, should_apply_login_environment,
};

#[test]
fn the_advertised_shell_prefers_absolute_override_then_login_shell() {
    assert_eq!(
        advertised_shell(
            Some("/opt/homebrew/bin/fish".to_owned()),
            Some("/bin/zsh".to_owned()),
        ),
        Some("/opt/homebrew/bin/fish".to_owned())
    );
    assert_eq!(
        advertised_shell(Some("fish".to_owned()), Some("/bin/zsh".to_owned())),
        Some("/bin/zsh".to_owned())
    );
    assert_eq!(advertised_shell(Some("fish".to_owned()), None), None);
}

#[test]
fn login_shell_selection_has_a_portable_fallback() {
    assert_eq!(
        selected_login_shell(
            Some("/opt/homebrew/bin/fish".to_owned()),
            Some("/bin/zsh".to_owned()),
        ),
        "/opt/homebrew/bin/fish"
    );
    assert_eq!(
        selected_login_shell(None, Some("zsh".to_owned())),
        "/bin/sh"
    );
}

#[test]
fn login_environment_parsing_preserves_multiline_and_empty_values() {
    let parsed = parse_login_environment(
        "PATH=/opt/homebrew/bin:/usr/bin\0MULTI=line1\nline2\0EMPTY=\0=orphan\0VALID=1\0",
    );

    assert_eq!(
        parsed,
        vec![
            ("PATH".to_owned(), "/opt/homebrew/bin:/usr/bin".to_owned()),
            ("MULTI".to_owned(), "line1\nline2".to_owned()),
            ("EMPTY".to_owned(), String::new()),
            ("VALID".to_owned(), "1".to_owned()),
        ]
    );
}

#[test]
fn login_environment_always_replaces_path_and_preserves_existing_platform_values() {
    assert!(should_apply_login_environment("PATH", true));
    assert!(should_apply_login_environment("PATH", false));
    assert!(should_apply_login_environment("BOOTTY_ENV_PROBE", false));
    assert!(!should_apply_login_environment("HOME", true));
}
