use std::ffi::OsString;

use clap::{Parser, Subcommand};
use pretty_assertions::assert_eq;
use rstest::rstest;
use xtasks::{install, launch, package};

#[derive(Debug, Parser)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Package(package::Args),
    Install(install::Args),
    Launch(launch::Args),
}

#[rstest]
fn package_and_install_keep_the_existing_flags() {
    let cli = Cli::try_parse_from([
        "xtasks",
        "package",
        "--fast",
        "--static",
        "--dev",
        "--all-daemons",
    ])
    .unwrap();
    let Command::Package(args) = cli.command else {
        panic!("expected package command");
    };
    assert!(args.fast);
    assert!(args.r#static);
    assert!(args.dev);
    assert!(args.all_daemons);

    let cli = Cli::try_parse_from(["xtasks", "install", "--fast", "--dev"]).unwrap();
    let Command::Install(args) = cli.command else {
        panic!("expected install command");
    };
    assert!(args.package.fast);
    assert!(args.package.dev);
}

#[rstest]
fn launch_forwards_bootty_arguments_verbatim() {
    let cli = Cli::try_parse_from([
        "xtasks",
        "launch",
        "--json",
        "command",
        "space.open",
        "--stdin-json",
    ])
    .unwrap();
    let Command::Launch(args) = cli.command else {
        panic!("expected launch command");
    };
    assert_eq!(
        args.arguments,
        ["--json", "command", "space.open", "--stdin-json"]
            .map(OsString::from)
            .to_vec()
    );
}
