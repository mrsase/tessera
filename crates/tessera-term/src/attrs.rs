//! Shared cell/colour/attribute types for the emulation layer.
//!
//! These types are deliberately small and `Copy` so a whole row can be moved,
//! copied and compared cheaply.  Colours are stored symbolically and resolved
//! against the palette at paint time.

use std::fmt;

/// A colour as it appears in the byte stream.  Resolution to RGB happens in the
/// renderer so palette changes never require a grid rewrite.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum Color {
    #[default]
    Default,
    Indexed(u8),
    Rgb(u8, u8, u8),
}

impl fmt::Debug for Color {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Color::Default => f.write_str("Default"),
            Color::Indexed(i) => write!(f, "Idx({i})"),
            Color::Rgb(r, g, b) => write!(f, "Rgb({r},{g},{b})"),
        }
    }
}

/// Character attributes packed into a bitfield.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub struct Attrs(u16);

impl Attrs {
    pub const BOLD: Self = Self(1 << 0);
    pub const DIM: Self = Self(1 << 1);
    pub const ITALIC: Self = Self(1 << 2);
    pub const UNDERLINE: Self = Self(1 << 3);
    pub const BLINK: Self = Self(1 << 4);
    pub const REVERSE: Self = Self(1 << 5);
    pub const HIDDEN: Self = Self(1 << 6);
    pub const STRIKE: Self = Self(1 << 7);

    pub const fn has(self, flag: Self) -> bool {
        self.0 & flag.0 != 0
    }

    pub fn set(&mut self, flag: Self, on: bool) {
        if on {
            self.0 |= flag.0;
        } else {
            self.0 &= !flag.0;
        }
    }

    pub fn clear(&mut self) {
        self.0 = 0;
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Bits, exposed for the renderer's glyph cache key.
    pub const fn bits(self) -> u16 {
        self.0
    }
}

impl fmt::Debug for Attrs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Attrs({:#06x})", self.0)
    }
}

/// The current drawing pen: what `print` applies to every new cell.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Pen {
    pub fg: Color,
    pub bg: Color,
    pub attrs: Attrs,
}

impl Pen {
    pub fn cell(&self, ch: char) -> Cell {
        Cell {
            ch,
            fg: self.fg,
            bg: self.bg,
            attrs: self.attrs,
            cont: false,
        }
    }
}

/// One character cell.  16 bytes: char + two colours + attrs + wide flag.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    pub ch: char,
    pub fg: Color,
    pub bg: Color,
    pub attrs: Attrs,
    /// True for the right half of a double-width character; never painted on
    /// its own and skipped when extracting text.
    pub cont: bool,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            ch: ' ',
            fg: Color::Default,
            bg: Color::Default,
            attrs: Attrs::default(),
            cont: false,
        }
    }
}

impl Cell {
    pub fn blank(bg: Color) -> Self {
        Self {
            bg,
            ..Self::default()
        }
    }

    pub fn is_blank_of(&self, bg: Color) -> bool {
        self.ch == ' '
            && !self.cont
            && self.attrs.is_empty()
            && self.fg == Color::Default
            && self.bg == bg
    }
}

impl fmt::Debug for Cell {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Cell({:?} fg={:?} bg={:?}", self.ch, self.fg, self.bg)?;
        if !self.attrs.is_empty() {
            write!(f, " {:?}", self.attrs)?;
        }
        if self.cont {
            f.write_str(" cont")?;
        }
        f.write_str(")")
    }
}
