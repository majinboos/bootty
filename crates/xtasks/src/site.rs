use std::process::Command;

use anyhow::Result;
use clap::Args as ClapArgs;

use crate::command;

#[derive(Clone, Debug, Default, ClapArgs)]
pub struct Args {}

pub fn run(_args: Args) -> Result<()> {
    command::run(Command::new("rustup").args(["target", "add", "wasm32-unknown-unknown"]))?;
    command::run(Command::new("bun").args(["install", "--frozen-lockfile"]))?;
    command::run(Command::new("bun").args(["run", "build:web"]))
}
