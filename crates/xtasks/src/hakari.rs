use std::process::Command;

use anyhow::Result;
use clap::{Args as ClapArgs, Subcommand};

use crate::command;

#[derive(Clone, Copy, Debug, ClapArgs)]
pub struct Args {
    #[command(subcommand)]
    pub action: Action,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Subcommand)]
pub enum Action {
    Generate,
    Check,
}

pub fn run(args: Args) -> Result<()> {
    match args.action {
        Action::Generate => {
            command::run(Command::new("cargo").args(["hakari", "generate"]))?;
            command::run(Command::new("cargo").args(["hakari", "manage-deps", "--yes"]))
        }
        Action::Check => {
            command::run(Command::new("cargo").args(["hakari", "generate", "--diff"]))?;
            command::run(Command::new("cargo").args(["hakari", "manage-deps", "--dry-run"]))
        }
    }
}
