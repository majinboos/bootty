use anyhow::Result;

use super::{command::MuxCommand, snapshot::MuxSnapshot};

pub trait MuxBackend {
    fn snapshot(&self) -> Result<MuxSnapshot>;
    fn execute(&mut self, command: MuxCommand) -> Result<()>;
}
