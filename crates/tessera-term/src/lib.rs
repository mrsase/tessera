//! Tessera's terminal emulation core.
//!
//! A pure state machine: bytes in, grid out. No I/O, no pixels, no platform
//! assumptions, so the whole emulator is testable on any target.

mod parser;
mod term;
#[cfg(test)]
mod tests;

pub mod attrs;
pub mod screen;

pub use attrs::{Attrs, Cell, Color, Pen};
pub use screen::{Cursor, CursorStyle, Row, SavedCursor, Screen};
pub use term::{Term, HISTORY_LIMIT};

/// Crate version, for diagnostics and the About panel.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
