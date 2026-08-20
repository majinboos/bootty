use std::sync::Arc;

use bootty_command::CommandDescriptor;
use bootty_extension::ExtensionCatalog;

/// Read-only command metadata for the local control protocol.
#[derive(Clone)]
pub struct ControlCatalog {
    core: Arc<[CommandDescriptor]>,
    extensions: Arc<ExtensionCatalog>,
}

impl ControlCatalog {
    pub fn new(core: Vec<CommandDescriptor>, extensions: Arc<ExtensionCatalog>) -> Self {
        Self {
            core: Arc::from(core),
            extensions,
        }
    }

    pub fn list(&self) -> Vec<CommandDescriptor> {
        let mut commands = self.core.iter().cloned().collect::<Vec<_>>();
        commands.extend(
            self.extensions
                .list()
                .into_iter()
                .filter(|extension| !self.core.iter().any(|core| core.id == extension.id)),
        );
        commands.sort_by(|left, right| left.id.cmp(&right.id));
        commands
    }

    pub fn describe(&self, id: &str) -> Option<CommandDescriptor> {
        self.core
            .iter()
            .find(|command| command.id == id)
            .cloned()
            .or_else(|| self.extensions.describe(id))
    }

    pub fn extensions(&self) -> &ExtensionCatalog {
        &self.extensions
    }
}
