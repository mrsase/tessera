//! Tessera's session layer: PTY panes, tabs, tmux, history suggestions.

pub mod suggest;

pub mod config;
pub mod input;
pub mod line;
pub mod mux;
pub mod native;
pub mod selection;
pub mod shortcut;
#[cfg(unix)]
pub mod tmux;

/// Crate version, for diagnostics and the About panel.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
