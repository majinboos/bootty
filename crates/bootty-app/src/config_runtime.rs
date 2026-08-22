use std::time::Instant;

use anyhow::Result;
use bootty_command::CommandInvocation;
use bootty_config::{
    config::{
        AppearanceVariant, BoottyConfig, ConfigDocument, ConfigWriteOutcome,
        MultiplexerBackendConfig, commit_config_document, load_or_create_config_document,
    },
    config_reload::ConfigHotReload,
};
use bootty_render::terminal_text::TerminalTextConfig;
use bootty_winit::{direct_input::ModifierSideState, modifier_remap::ModifierRemapSet};
use eframe::egui;

use crate::terminal_config::{
    terminal_live_config, terminal_macos_option_as_alt, terminal_text_config,
};
use crate::{
    app_actions::{
        AppKeyBindings, SidebarKeyBindings, split_app_actions_for_bindings_with_modifier_sides,
    },
    diagnostics::{StabilityTrace, StabilityTraceSample},
    input::{
        InputSnapshot, TerminalInputCommand, WheelScrollState, resolve_modifier_remaps,
        terminal_input_commands_with_wheel_state,
    },
};
use bootty_terminal::terminal_engine::TerminalLiveConfig;
use bootty_terminal::terminal_input_model::{KeyInput, KeyMods, MacosOptionAsAlt};

pub(super) struct AcceptedConfigChange {
    pub(super) config: BoottyConfig,
    pub(super) live_config: Option<TerminalLiveConfig>,
    pub(super) text_config: Option<TerminalTextConfig>,
    pub(super) ui_fonts: Option<Vec<String>>,
    pub(super) window_title: Option<String>,
    pub(super) ssh_profiles_changed: bool,
    pub(super) compatibility_warning: Option<String>,
}

#[allow(clippy::float_cmp)]
fn new_session_only_config_changed(previous: &BoottyConfig, next: &BoottyConfig) -> bool {
    previous.session != next.session
        || previous.window.width != next.window.width
        || previous.window.height != next.window.height
        || previous.window.fullscreen != next.window.fullscreen
        || previous.window.window_decoration != next.window.window_decoration
        || previous.window.macos_titlebar_style != next.window.macos_titlebar_style
}

pub(super) struct AppConfigRuntime {
    current: BoottyConfig,
    document: ConfigDocument,
    hot_reload: ConfigHotReload,
    modifier_remaps: ModifierRemapSet,
    backend_key_bindings: BackendKeyBindings,
    app_key_bindings: AppKeyBindings,
    sidebar_key_bindings: SidebarKeyBindings,
    macos_option_as_alt: MacosOptionAsAlt,
    has_new_session_config_changes: bool,
    stability_trace: Option<StabilityTrace>,
    /// Bumped by every accepted or live change to `current`/`document`. Views hold the
    /// revision they last read so they can skip re-cloning an unchanged config.
    revision: u64,
}

struct BackendKeyBindings {
    native: AppKeyBindings,
    rmux: AppKeyBindings,
    tmux: AppKeyBindings,
}

impl BackendKeyBindings {
    fn from_config(config: &BoottyConfig) -> Result<Self> {
        let input = &config.input;
        Ok(Self {
            native: AppKeyBindings::from_keybinds(
                &input.keybinds_for_backend(MultiplexerBackendConfig::Native),
            )?,
            rmux: AppKeyBindings::from_keybinds(
                &input.keybinds_for_backend(MultiplexerBackendConfig::Rmux),
            )?,
            tmux: AppKeyBindings::from_keybinds(
                &input.keybinds_for_backend(MultiplexerBackendConfig::Tmux),
            )?,
        })
    }

    fn for_backend(&self, backend: MultiplexerBackendConfig) -> AppKeyBindings {
        match backend {
            MultiplexerBackendConfig::Native => self.native.clone(),
            MultiplexerBackendConfig::Rmux => self.rmux.clone(),
            MultiplexerBackendConfig::Tmux => self.tmux.clone(),
        }
    }
}

impl AppConfigRuntime {
    pub(super) fn new(config: BoottyConfig) -> Result<Self> {
        let document = load_or_create_config_document(&config.config_path)?;
        let modifier_remaps = resolve_modifier_remaps(&config.input.modifier_remap)?;
        let sidebar_key_bindings =
            SidebarKeyBindings::from_keybinds(&config.input.sidebar_keybind)?;
        let backend_key_bindings = BackendKeyBindings::from_config(&config)?;
        let macos_option_as_alt = terminal_macos_option_as_alt(config.input.macos_option_as_alt);
        let stability_trace = StabilityTrace::from_config(&config);
        let hot_reload = ConfigHotReload::new(&config.config_path);
        Ok(Self {
            current: config,
            document,
            hot_reload,
            modifier_remaps,
            app_key_bindings: backend_key_bindings.native.clone(),
            backend_key_bindings,
            sidebar_key_bindings,
            macos_option_as_alt,
            has_new_session_config_changes: false,
            stability_trace,
            revision: 0,
        })
    }

    pub(super) fn current(&self) -> &BoottyConfig {
        &self.current
    }

    pub(super) fn revision(&self) -> u64 {
        self.revision
    }

    /// The only mutable path to the accepted config: taking it records the change.
    fn current_mut(&mut self) -> &mut BoottyConfig {
        self.revision = self.revision.wrapping_add(1);
        &mut self.current
    }

    pub(super) fn document(&self) -> &ConfigDocument {
        &self.document
    }

    pub(super) fn prepare_backend_keybindings(
        &self,
        backend: MultiplexerBackendConfig,
    ) -> AppKeyBindings {
        self.backend_key_bindings.for_backend(backend)
    }

    pub(super) fn publish_backend_keybindings(&mut self, bindings: AppKeyBindings) {
        self.app_key_bindings = bindings;
    }

    pub(super) fn reload(
        &mut self,
        backend: MultiplexerBackendConfig,
        appearance: AppearanceVariant,
    ) -> Result<AcceptedConfigChange> {
        let next = self.hot_reload.reload_config()?;
        let document = load_or_create_config_document(&next.config_path)?;
        let change = self.accept(next, backend, appearance)?;
        self.document = document;
        Ok(change)
    }

    pub(super) fn commit_document(
        &mut self,
        document: ConfigDocument,
        backend: MultiplexerBackendConfig,
        appearance: AppearanceVariant,
    ) -> Result<(AcceptedConfigChange, ConfigDocument, ConfigWriteOutcome)> {
        let path = self.current.config_path.clone();
        let (accepted, ()) = commit_config_document(&path, document, |candidate| {
            // App-owned input parsing is part of acceptance, so reject before replacement.
            resolve_modifier_remaps(&candidate.input.modifier_remap)
                .map_err(|error| error.to_string())?;
            BackendKeyBindings::from_config(candidate).map_err(|error| error.to_string())?;
            SidebarKeyBindings::from_keybinds(&candidate.input.sidebar_keybind)
                .map_err(|error| error.to_string())?;
            Ok(())
        })?;
        let change = self.accept(accepted.config, backend, appearance)?;
        self.hot_reload.refresh_dependency_graph();
        self.document = accepted.document.clone();
        Ok((change, accepted.document, accepted.write_outcome))
    }

    fn accept(
        &mut self,
        next: BoottyConfig,
        backend: MultiplexerBackendConfig,
        appearance: AppearanceVariant,
    ) -> Result<AcceptedConfigChange> {
        let modifier_remaps = resolve_modifier_remaps(&next.input.modifier_remap)?;
        let backend_key_bindings = BackendKeyBindings::from_config(&next)?;
        let app_key_bindings = backend_key_bindings.for_backend(backend);
        let sidebar_key_bindings = SidebarKeyBindings::from_keybinds(&next.input.sidebar_keybind)?;
        let previous = &self.current;
        let live_config_changed = previous.colors_for_appearance(appearance)
            != next.colors_for_appearance(appearance)
            || previous.cursor != next.cursor
            || previous.session.glyph_protocol != next.session.glyph_protocol;
        let text_config = (previous.font != next.font).then(|| terminal_text_config(&next.font));
        let ui_fonts = (previous.font.ui_families() != next.font.ui_families())
            .then(|| next.font.ui_families().to_vec());
        let window_title =
            (previous.window.title != next.window.title).then(|| next.window.title.clone());
        let ssh_profiles_changed = previous.ssh_profiles != next.ssh_profiles;
        let compatibility_warning = (!next.compatibility_warnings.is_empty())
            .then(|| next.compatibility_warnings.join("; "));
        self.has_new_session_config_changes =
            self.has_new_session_config_changes || new_session_only_config_changed(previous, &next);
        if previous.diagnostics != next.diagnostics {
            self.stability_trace = StabilityTrace::from_config(&next);
        }
        let live_config = live_config_changed.then(|| terminal_live_config(&next, appearance));
        self.macos_option_as_alt = terminal_macos_option_as_alt(next.input.macos_option_as_alt);
        *self.current_mut() = next.clone();
        self.modifier_remaps = modifier_remaps;
        self.backend_key_bindings = backend_key_bindings;
        self.app_key_bindings = app_key_bindings;
        self.sidebar_key_bindings = sidebar_key_bindings;

        Ok(AcceptedConfigChange {
            config: next,
            live_config,
            text_config,
            ui_fonts,
            window_title,
            ssh_profiles_changed,
            compatibility_warning,
        })
    }

    pub(super) fn reload_due(&mut self, now: Instant) -> bool {
        self.hot_reload.changed(now)
    }

    pub(super) fn has_new_session_config_changes(&self) -> bool {
        self.has_new_session_config_changes
    }

    pub(super) fn record_stability(&mut self, sample: StabilityTraceSample<'_>) {
        if let Some(trace) = &mut self.stability_trace {
            trace.record(sample);
        }
    }

    pub(super) fn split_app_actions(
        &mut self,
        events: Vec<egui::Event>,
        modifier_sides: ModifierSideState,
    ) -> (Vec<egui::Event>, Vec<CommandInvocation>) {
        split_app_actions_for_bindings_with_modifier_sides(
            &mut self.app_key_bindings,
            events,
            modifier_sides,
        )
    }

    pub(super) fn remap_mods(&self, mods: KeyMods) -> KeyMods {
        self.modifier_remaps.apply(mods)
    }

    pub(super) fn invocation_for_input(&mut self, input: KeyInput) -> Option<CommandInvocation> {
        self.app_key_bindings.invocation_for_input(input)
    }

    pub(super) fn sidebar_invocation(
        &self,
        key: egui::Key,
        modifiers: egui::Modifiers,
    ) -> Option<CommandInvocation> {
        self.sidebar_key_bindings.invocation_for_key(key, modifiers)
    }

    pub(super) fn terminal_input_commands(
        &self,
        snapshot: InputSnapshot,
        wheel_scroll_state: &mut WheelScrollState,
    ) -> Vec<TerminalInputCommand> {
        terminal_input_commands_with_wheel_state(
            snapshot,
            &self.modifier_remaps,
            self.macos_option_as_alt,
            wheel_scroll_state,
        )
    }

    pub(super) fn set_sidebar_width(&mut self, width: f32) {
        self.current_mut().chrome.sidebar_width = width;
    }

    pub(super) fn show_sidebar(&mut self) {
        self.current_mut().chrome.sidebar = true;
    }

    pub(super) fn toggle_sidebar(&mut self) -> bool {
        let config = self.current_mut();
        config.chrome.sidebar = !config.chrome.sidebar;
        config.chrome.sidebar
    }

    pub(super) fn replace_preview_config(&mut self, config: BoottyConfig) {
        *self.current_mut() = config;
    }

    pub(super) fn set_font_size(&mut self, size: f32) {
        self.current_mut().font.size = size;
    }
}
