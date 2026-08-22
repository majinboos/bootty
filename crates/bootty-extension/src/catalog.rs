use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, RwLock},
};

use crate::{
    ExtensionInvocationSender, ModuleIdentity, SurfacePlacement, SurfaceSnapshot,
    surfaces::PublishedSurfaceSnapshot,
};
use bootty_command::CommandDescriptor;

#[derive(Clone, Debug)]
pub struct ExtensionGenerationToken(Arc<std::sync::atomic::AtomicBool>);

impl ExtensionGenerationToken {
    #[must_use]
    pub fn new() -> Self {
        Self(Arc::new(std::sync::atomic::AtomicBool::new(true)))
    }

    #[must_use]
    pub fn is_active(&self) -> bool {
        self.0.load(std::sync::atomic::Ordering::Acquire)
    }

    pub fn retire(&self) {
        self.0.store(false, std::sync::atomic::Ordering::Release);
    }
}

impl Default for ExtensionGenerationToken {
    fn default() -> Self {
        Self::new()
    }
}

pub struct ExtensionGenerationCandidate {
    pub identity: ModuleIdentity,
    pub generation: u64,
    pub token: ExtensionGenerationToken,
    pub commands: Vec<(CommandDescriptor, ExtensionInvocationSender)>,
    pub topics: Vec<String>,
    pub surfaces: Vec<SurfaceSnapshot>,
}

#[derive(Clone)]
struct ExtensionCommand {
    module: String,
    generation: u64,
    descriptor: CommandDescriptor,
    sender: ExtensionInvocationSender,
}

#[derive(Clone)]
struct ExtensionTopic {
    module: String,
    generation: u64,
}

#[derive(Clone)]
struct ExtensionSurface {
    module: String,
    generation: u64,
    snapshot: SurfaceSnapshot,
}

#[derive(Default)]
struct CatalogState {
    commands: BTreeMap<String, ExtensionCommand>,
    topics: BTreeMap<String, ExtensionTopic>,
    surfaces: BTreeMap<(String, String), ExtensionSurface>,
    generations: BTreeMap<String, (u64, ExtensionGenerationToken)>,
}

pub struct ExtensionCatalog {
    state: RwLock<CatalogState>,
    reserved_commands: BTreeSet<String>,
}

impl Default for ExtensionCatalog {
    fn default() -> Self {
        Self::with_reserved_commands(std::iter::empty::<String>())
    }
}

impl ExtensionCatalog {
    pub fn with_reserved_commands<I, S>(reserved_commands: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            state: RwLock::new(CatalogState::default()),
            reserved_commands: reserved_commands.into_iter().map(Into::into).collect(),
        }
    }

    #[must_use]
    pub fn list(&self) -> Vec<CommandDescriptor> {
        self.state
            .read()
            .map(|state| {
                state
                    .commands
                    .values()
                    .map(|command| command.descriptor.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    #[must_use]
    pub fn describe(&self, id: &str) -> Option<CommandDescriptor> {
        self.state.read().ok().and_then(|state| {
            state
                .commands
                .get(id)
                .map(|command| command.descriptor.clone())
        })
    }

    pub fn publish_generation(
        &self,
        candidate: ExtensionGenerationCandidate,
    ) -> Result<(), String> {
        let ExtensionGenerationCandidate {
            identity,
            generation,
            token,
            commands,
            topics,
            surfaces,
        } = candidate;
        let module = identity.as_str();
        let namespace = identity.namespace();
        let mut command_ids = BTreeSet::new();
        for (descriptor, _) in &commands {
            if self.reserved_commands.contains(&descriptor.id) {
                return Err("extension command cannot replace a built-in command".to_owned());
            }
            if !is_namespaced(&descriptor.id, &namespace) {
                return Err("extension command must be namespaced by its module".to_owned());
            }
            if !command_ids.insert(descriptor.id.clone()) {
                return Err(format!(
                    "command {} is registered more than once",
                    descriptor.id
                ));
            }
        }
        let mut topic_ids = BTreeSet::new();
        for topic in &topics {
            if !is_namespaced(topic, &namespace) {
                return Err("extension event topic must be namespaced by its module".to_owned());
            }
            if !topic_ids.insert(topic.clone()) {
                return Err(format!("event topic {topic} is registered more than once"));
            }
        }
        validate_surfaces(&surfaces)?;

        // All collision checks and replacement happen under this one lock.
        let mut state = self
            .state
            .write()
            .map_err(|_| "extension catalog is unavailable".to_owned())?;
        if !token.is_active() {
            return Err("extension generation is not active".to_owned());
        }
        for command in &command_ids {
            if state
                .commands
                .get(command)
                .is_some_and(|registered| registered.module != module)
            {
                return Err(format!("command {command} is already registered"));
            }
        }
        for topic in &topic_ids {
            if state
                .topics
                .get(topic)
                .is_some_and(|registered| registered.module != module)
            {
                return Err(format!("event topic {topic} is already registered"));
            }
        }
        ensure_surfaces_available(&state, module, &surfaces)?;

        if let Some((_, previous)) = state.generations.get(module) {
            previous.retire();
        }
        state.commands.retain(|_, command| command.module != module);
        state.topics.retain(|_, topic| topic.module != module);
        for (descriptor, sender) in commands {
            state.commands.insert(
                descriptor.id.clone(),
                ExtensionCommand {
                    module: module.to_owned(),
                    generation,
                    descriptor,
                    sender,
                },
            );
        }
        for topic in topics {
            state.topics.insert(
                topic,
                ExtensionTopic {
                    module: module.to_owned(),
                    generation,
                },
            );
        }
        replace_surfaces(&mut state, module, generation, surfaces);
        state
            .generations
            .insert(module.to_owned(), (generation, token));
        Ok(())
    }

    pub fn remove_generation(&self, module: &str, generation: u64) {
        if let Ok(mut state) = self.state.write() {
            let matches = state
                .generations
                .get(module)
                .is_some_and(|(active, _)| *active == generation);
            if !matches {
                return;
            }
            if let Some((_, token)) = state.generations.remove(module) {
                token.retire();
            }
            state
                .commands
                .retain(|_, command| command.module != module || command.generation != generation);
            state
                .topics
                .retain(|_, topic| topic.module != module || topic.generation != generation);
            state
                .surfaces
                .retain(|_, surface| surface.module != module || surface.generation != generation);
        }
    }

    #[must_use]
    pub fn command(&self, id: &str) -> Option<(CommandDescriptor, ExtensionInvocationSender)> {
        self.state.read().ok().and_then(|state| {
            state
                .commands
                .get(id)
                .map(|command| (command.descriptor.clone(), command.sender.clone()))
        })
    }

    #[must_use]
    pub fn topics(&self) -> BTreeSet<String> {
        self.state
            .read()
            .map(|state| state.topics.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// Snapshots for one placement, cloned inside the lock so a caller that wants the status bar
    /// does not copy every sidebar and session surface as well.
    #[must_use]
    pub fn surfaces_for(&self, placement: SurfacePlacement) -> Vec<PublishedSurfaceSnapshot> {
        self.state
            .read()
            .map(|state| {
                state
                    .surfaces
                    .values()
                    .filter(|surface| surface.snapshot.declaration.placement == placement)
                    .map(|surface| PublishedSurfaceSnapshot {
                        module: surface.module.clone(),
                        generation: surface.generation,
                        snapshot: surface.snapshot.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Whether any surface is published for `placement`, without cloning a snapshot.
    #[must_use]
    pub fn has_surfaces(&self, placement: SurfacePlacement) -> bool {
        self.state.read().is_ok_and(|state| {
            state
                .surfaces
                .values()
                .any(|surface| surface.snapshot.declaration.placement == placement)
        })
    }

    #[must_use]
    pub fn surfaces(&self) -> Vec<PublishedSurfaceSnapshot> {
        self.state
            .read()
            .map(|state| {
                state
                    .surfaces
                    .values()
                    .map(|surface| PublishedSurfaceSnapshot {
                        module: surface.module.clone(),
                        generation: surface.generation,
                        snapshot: surface.snapshot.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn publish_surfaces(
        &self,
        module: &str,
        generation: u64,
        surfaces: Vec<SurfaceSnapshot>,
    ) -> Result<(), String> {
        validate_surfaces(&surfaces)?;
        let mut state = self
            .state
            .write()
            .map_err(|_| "extension catalog is unavailable".to_owned())?;
        if !generation_is_active(&state, module, generation) {
            return Err("extension generation is not active".to_owned());
        }
        ensure_surfaces_available(&state, module, &surfaces)?;
        replace_surfaces(&mut state, module, generation, surfaces);
        Ok(())
    }

    pub fn with_active_topic<T>(
        &self,
        module: &str,
        generation: u64,
        topic: &str,
        publish: impl FnOnce() -> T,
    ) -> Result<T, String> {
        let state = self
            .state
            .read()
            .map_err(|_| "extension catalog is unavailable".to_owned())?;
        let topic_is_active = state.topics.get(topic).is_some_and(|registered| {
            registered.module == module && registered.generation == generation
        });
        if !generation_is_active(&state, module, generation) || !topic_is_active {
            return Err("extension event topic is not active".to_owned());
        }
        Ok(publish())
    }

    pub fn with_active_generation<T>(
        &self,
        module: &str,
        generation: u64,
        action: impl FnOnce() -> T,
    ) -> Result<T, String> {
        let state = self
            .state
            .read()
            .map_err(|_| "extension catalog is unavailable".to_owned())?;
        if !generation_is_active(&state, module, generation) {
            return Err("extension generation is not active".to_owned());
        }
        Ok(action())
    }
}

fn validate_surfaces(surfaces: &[SurfaceSnapshot]) -> Result<(), String> {
    let mut ids = BTreeSet::new();
    for surface in surfaces {
        if surface.declaration.id.is_empty() {
            return Err("extension surface identity is invalid".to_owned());
        }
        if !ids.insert(&surface.declaration.id) {
            return Err(format!(
                "surface {} is registered more than once",
                surface.declaration.id
            ));
        }
    }
    Ok(())
}

fn ensure_surfaces_available(
    state: &CatalogState,
    module: &str,
    surfaces: &[SurfaceSnapshot],
) -> Result<(), String> {
    for surface in surfaces {
        if state.surfaces.values().any(|registered| {
            registered.module != module
                && registered.snapshot.declaration.placement == surface.declaration.placement
                && registered.snapshot.declaration.id == surface.declaration.id
        }) {
            return Err(format!(
                "surface {} is already registered for {:?}",
                surface.declaration.id, surface.declaration.placement
            ));
        }
    }
    Ok(())
}

fn replace_surfaces(
    state: &mut CatalogState,
    module: &str,
    generation: u64,
    surfaces: Vec<SurfaceSnapshot>,
) {
    state.surfaces.retain(|_, surface| surface.module != module);
    for snapshot in surfaces {
        state.surfaces.insert(
            (module.to_owned(), snapshot.declaration.id.clone()),
            ExtensionSurface {
                module: module.to_owned(),
                generation,
                snapshot,
            },
        );
    }
}

fn generation_is_active(state: &CatalogState, module: &str, generation: u64) -> bool {
    state
        .generations
        .get(module)
        .is_some_and(|(active, token)| *active == generation && token.is_active())
}

/// Whether `id` is `package` followed by a dot and a non-empty leaf, the namespacing
/// every extension-supplied command id and event topic must satisfy.
pub fn is_namespaced(id: &str, package: &str) -> bool {
    let Some(local) = id
        .strip_prefix(package)
        .and_then(|value| value.strip_prefix('.'))
    else {
        return false;
    };
    !local.is_empty() && local.split('.').all(|part| !part.is_empty())
}
