use bootty_cli::Cli;
use clap::{CommandFactory, Parser};
use pretty_assertions::assert_eq;
use rstest::rstest;

#[rstest]
#[case("--theme")]
#[case("--fullscreen")]
fn app_override_is_documented_only_under_the_app_command(#[case] option: &str) {
    let top_level = Cli::command().render_help().to_string();
    let app = Cli::try_parse_from(["bootty", "app", "--help"])
        .expect_err("app help exits through clap")
        .to_string();

    assert_eq!(
        (top_level.contains(option), app.contains(option)),
        (false, true)
    );
}
