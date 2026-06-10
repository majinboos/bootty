use std::sync::Arc;

pub type RepaintHandle = Arc<dyn Fn() + Send + Sync + 'static>;

pub mod backend;
pub mod command;
pub mod config;
pub mod controller;
pub mod native;
pub mod order;
pub mod process;
pub mod rmux;
pub mod sidebar_meta;
pub mod snapshot;
pub mod terminal;
pub mod tmux;
pub mod tmux_protocol;
pub mod zellij;
