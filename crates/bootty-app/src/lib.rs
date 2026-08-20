pub mod action_catalog;
pub mod app_actions;
pub use bootty_identity as application_identity;
mod assets;
pub mod commands;
mod config_runtime;
pub use bootty_config::{color, config, config_reload};
pub mod diagnostics;
pub use bootty_render::{
    geometry, paint_plan, selection, terminal_font_face, terminal_render, terminal_sprite,
    terminal_text, terminal_text_atlas, terminal_wgpu,
};
pub use bootty_runtime::{scheduler, terminal_session};
pub use bootty_terminal::{terminal_engine, terminal_frame, terminal_image, terminal_input_model};
pub use bootty_winit::{direct_input, input_binding, input_binding_set, modifier_remap};
pub mod input;
pub mod layout;
pub mod menu;
pub use bootty_mux as mux;
mod host;
pub mod native_host;
pub mod platform;
pub mod remote_catalog;
pub mod renderer;
mod state;
pub mod strings;
mod terminal_config;
mod terminal_interaction;
pub mod theme;
pub mod ui;
mod workspace_runtime;

pub use host::BoottyApp;
pub use state::{AppEffect, AppState, FrameInputs, ViewportSnapshot};
pub use ui::ModalDialog;
