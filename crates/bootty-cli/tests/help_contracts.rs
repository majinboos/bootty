use bootty_cli::Cli;
use clap::{CommandFactory, Parser};

#[test]
fn app_overrides_are_only_documented_under_the_app_command() {
    let top_level = Cli::command().render_help().to_string();
    let app = Cli::try_parse_from(["bootty", "app", "--help"])
        .expect_err("app help exits through clap")
        .to_string();

    assert!(!top_level.contains("--theme"));
    assert!(!top_level.contains("--fullscreen"));
    assert!(app.contains("--theme"));
    assert!(app.contains("--fullscreen"));
}
