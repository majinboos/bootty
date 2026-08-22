use anyhow::Result;

use super::{command::MuxCommand, snapshot::MuxSnapshot};

pub trait MuxBackend {
    fn snapshot(&self) -> Result<MuxSnapshot>;
    fn execute(&mut self, command: MuxCommand) -> Result<()>;

    fn capabilities(&self, scope: MuxScope) -> BindingCapabilityDescriptor {
        BindingCapabilityDescriptor::new(scope, [])
    }

    fn execute_checked(
        &mut self,
        scope: MuxScope,
        command: MuxCommand,
    ) -> BindingOperationOutcome<Result<()>> {
        let descriptor = self.capabilities(scope);
        descriptor.invoke(
            descriptor.request(command.operation()),
            BindingOperationAvailability::Available,
            || self.execute(command),
        )
    }
}
