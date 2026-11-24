//! Screen and row storage shared by the emulation core and the renderer.
//!
//! `Screen` owns the visible grid plus per-screen modes that terminals save and
//! restore independently (cursor, scroll margins, origin mode, wrap pending).
//! The scrollback buffer lives in `Term`, not here, because it only exists for
//! the main screen.

use super::attrs::{Cell, Color, Pen};

/// DECSCUSR cursor shapes.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum CursorStyle {
    #[default]
    Block,
    BlockBlink,
    Underline,
    UnderlineBlink,
    Bar,
    BarBlink,
}

impl CursorStyle {
    pub fn blink(self) -> bool {
        matches!(
            self,
            Self::BlockBlink | Self::UnderlineBlink | Self::BarBlink
        )
    }

    pub fn is_bar(self) -> bool {
        matches!(self, Self::Bar | Self::BarBlink)
    }

    pub fn is_underline(self) -> bool {
        matches!(self, Self::Underline | Self::UnderlineBlink)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Cursor {
    pub x: usize,
    pub y: usize,
}

/// Everything DECSC/CSI s must bring back.
#[derive(Clone, Copy, Debug, Default)]
pub struct SavedCursor {
    pub x: usize,
    pub y: usize,
    pub pen: Pen,
    pub origin: bool,
    pub pending_wrap: bool,
}

#[derive(Clone)]
pub struct Row {
    pub cells: Vec<Cell>,
    /// Set when the row's last cell was written at the right margin and the
    /// text continues on the next row.  Keeps reflow-free resize and text
    /// extraction honest.
    pub wrapped: bool,
}

impl Row {
    pub fn blank(cols: usize) -> Self {
        Self {
            cells: vec![Cell::default(); cols],
            wrapped: false,
        }
    }

    pub fn blank_with(cols: usize, bg: Color) -> Self {
        Self {
            cells: vec![Cell::blank(bg); cols],
            wrapped: false,
        }
    }

    pub fn resize(&mut self, cols: usize, bg: Color) {
        self.cells.resize(cols, Cell::blank(bg));
    }

    /// Trailing blanks trimmed, wide-char halves skipped.  Used by tests and
    /// by clipboard extraction.
    pub fn text(&self) -> String {
        let mut s = String::with_capacity(self.cells.len());
        for c in &self.cells {
            if c.cont {
                continue;
            }
            s.push(c.ch);
        }
        while s.ends_with(' ') {
            s.pop();
        }
        s
    }
}

impl std::fmt::Debug for Row {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Row({:?}{})",
            self.text(),
            if self.wrapped { " wrapped" } else { "" }
        )
    }
}

pub struct Screen {
    pub rows: Vec<Row>,
    pub cursor: Cursor,
    pub saved: SavedCursor,
    /// Inclusive scroll region, 0-based.
    pub top: usize,
    pub bottom: usize,
    pub origin: bool,
    pub pending_wrap: bool,
    pub cols: usize,
}

impl Screen {
    pub fn new(cols: usize, rows: usize) -> Self {
        let cols = cols.max(1);
        let rows = rows.max(1);
        Self {
            rows: vec![Row::blank(cols); rows],
            cursor: Cursor::default(),
            saved: SavedCursor::default(),
            top: 0,
            bottom: rows - 1,
            origin: false,
            pending_wrap: false,
            cols,
        }
    }

    pub fn height(&self) -> usize {
        self.rows.len()
    }

    /// Resize in place, top-left anchored.  No reflow: columns are truncated
    /// or padded, rows dropped from the bottom.  Cursor and margins are
    /// clamped so the grid is always internally consistent.
    pub fn resize(&mut self, cols: usize, rows: usize, bg: Color) {
        let cols = cols.max(1);
        let rows = rows.max(1);
        self.cols = cols;
        for row in &mut self.rows {
            row.resize(cols, bg);
        }
        self.rows.resize(rows, Row::blank_with(cols, bg));
        self.cursor.x = self.cursor.x.min(cols - 1);
        self.cursor.y = self.cursor.y.min(rows - 1);
        self.top = self.top.min(rows - 1);
        self.bottom = self.bottom.min(rows - 1);
        if self.bottom <= self.top {
            self.top = 0;
            self.bottom = rows - 1;
        }
        self.pending_wrap = false;
    }

    pub fn save(&mut self, pen: Pen) {
        self.saved = SavedCursor {
            x: self.cursor.x,
            y: self.cursor.y,
            pen,
            origin: self.origin,
            pending_wrap: self.pending_wrap,
        };
    }

    /// Restores cursor state and returns the pen to continue drawing with.
    pub fn restore(&mut self) -> Pen {
        let s = self.saved;
        self.cursor.x = s.x.min(self.cols.saturating_sub(1));
        self.cursor.y = s.y.min(self.rows.len().saturating_sub(1));
        self.origin = s.origin;
        self.pending_wrap = s.pending_wrap;
        s.pen
    }

    /// Fill `x0..=x1` on row `y` with blanks carrying `bg`.
    pub fn erase_cells(&mut self, y: usize, x0: usize, x1: usize, bg: Color) {
        let (cols, Some(row)) = (self.cols, self.rows.get_mut(y)) else {
            return;
        };
        if x0 > x1 || cols == 0 {
            return;
        }
        let end = x1.min(cols - 1);
        for cell in &mut row.cells[x0.min(end)..=end] {
            *cell = Cell::blank(bg);
        }
        row.wrapped = false;
    }
}
