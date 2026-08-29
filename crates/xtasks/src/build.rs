use std::process::Command;

use anyhow::Result;
use clap::Args;

use crate::command::{append_env, run as run_command};

#[derive(Args, Clone, Debug, Default, Eq, PartialEq)]
pub struct BuildArgs {
    /// Use the faster release profile.
    #[arg(long)]
    pub fast: bool,

    /// Statically link the Rust runtime where supported.
    #[arg(long = "static")]
    pub static_linkage: bool,
}

pub fn run(args: &BuildArgs) -> Result<()> {
    run_with_features(args, cfg!(not(windows)))
}

pub fn run_with_features(args: &BuildArgs, development: bool) -> Result<()> {
    let mut command = Command::new("cargo");
    command.arg("build");
    command.args(profile_args(args));
    if development {
        command.args(["--features", "bootty-dev"]);
    }
    command.args(["-p", "bootty", "--bin", "bootty"]);

    if !args.static_linkage {
        let rustflags = if cfg!(windows) {
            "-C prefer-dynamic"
        } else {
            "-C prefer-dynamic -C rpath"
        };
        command.env("RUSTFLAGS", append_env("RUSTFLAGS", rustflags));
    }

    run_command(&mut command)
}

pub fn profile(args: &BuildArgs) -> &'static str {
    if args.fast {
        "fast-release"
    } else if args.static_linkage {
        "release"
    } else {
        "dynamic-release"
    }
}

pub fn profile_args(args: &BuildArgs) -> Vec<&'static str> {
    if profile(args) == "release" {
        vec!["--release"]
    } else {
        vec!["--profile", profile(args)]
    }
}
