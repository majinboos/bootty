use std::process::Command;

use anyhow::Result;

use crate::{command, hakari};

pub fn run() -> Result<()> {
    command::run(Command::new("cargo").args(["fmt", "--all", "--", "--check"]))?;
    hakari::run(hakari::Args {
        action: hakari::Action::Check,
    })?;
    command::run(Command::new("cargo").args([
        "clippy",
        "--workspace",
        "--all-targets",
        "--",
        "-D",
        "warnings",
    ]))
}
