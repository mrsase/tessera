//! VT emulation core: a grid, a cursor and the escape sequences that drive it.
//!
//! No I/O and no pixels; `Term::feed` is a pure function of (state, bytes) and
//! the renderer reads the result.  The byte stream is decoded by the `vte`
//! crate, which handles UTF-8 splitting and sequence framing; the semantics
//! (CSI, SGR, modes, scroll regions, alternate screen, ...) live here.

use std::collections::VecDeque;

use unicode_width::UnicodeWidthChar;

use crate::attrs::{Cell, Color, Pen};
use crate::screen::{CursorStyle, Row, Screen};

pub const HISTORY_LIMIT: usize = 5000;

/// Returned by `history()` while the alternate screen is active.
static EMPTY_HISTORY: VecDeque<Row> = VecDeque::new();

/// Designated character set for G0/G1.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Charset {
    #[default]
    Ascii,
    DecSpecial,
}

pub struct Term {
    main: Screen,
    alt: Screen,
    alt_on: bool,
    pub(crate) pen: Pen,
    pub(crate) insert: bool,
    pub(crate) autowrap: bool,
    pub(crate) app_cursor: bool,
    pub(crate) bracketed_paste: bool,
    pub(crate) cursor_visible: bool,
    pub(crate) cursor_blink: bool,
    pub(crate) cursor_style: CursorStyle,
    pub(crate) tabs: Vec<bool>,
    history: VecDeque<Row>,
    pub(crate) title: String,
    pub(crate) bell: bool,
    pub(crate) responses: Vec<u8>,
    pub(crate) charsets: [Charset; 2],
    pub(crate) active_charset: usize,
    cwd: Option<String>,
    last_printed: char,
    parser: vte::Parser,
}

impl Term {
    pub fn new(cols: usize, rows: usize) -> Self {
        let cols = cols.max(1);
        let rows = rows.max(1);
        let mut term = Self {
            main: Screen::new(cols, rows),
            alt: Screen::new(cols, rows),
            alt_on: false,
            pen: Pen::default(),
            insert: false,
            autowrap: true,
            app_cursor: false,
            bracketed_paste: false,
            cursor_visible: true,
            cursor_blink: true,
            cursor_style: CursorStyle::Block,
            tabs: Vec::new(),
            history: VecDeque::new(),
            title: String::new(),
            bell: false,
            responses: Vec::new(),
            charsets: [Charset::Ascii; 2],
            active_charset: 0,
            cwd: None,
            last_printed: ' ',
            parser: vte::Parser::new(),
        };
        term.rebuild_tabs();
        term
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        // Move the parser out so `advance` can borrow `self` mutably.
        let mut parser = std::mem::take(&mut self.parser);
        parser.advance(self, bytes);
        self.parser = parser;
    }

    pub fn resize(&mut self, cols: usize, rows: usize) {
        let cols = cols.max(1);
        let rows = rows.max(1);
        self.main.resize(cols, rows, Color::Default);
        self.alt.resize(cols, rows, Color::Default);
        self.rebuild_tabs();
    }

    pub fn cols(&self) -> usize {
        self.cur().cols
    }

    pub fn rows(&self) -> usize {
        self.cur().height()
    }

    /// The currently displayed screen (main or alternate).
    pub fn screen(&self) -> &Screen {
        self.cur()
    }

    /// Scrollback lines; empty while the alternate screen is active.
    pub fn history(&self) -> &VecDeque<Row> {
        if self.alt_on {
            &EMPTY_HISTORY
        } else {
            &self.history
        }
    }

    /// Number of scrollback lines (zero while the alternate screen is active).
    pub fn history_len(&self) -> usize {
        self.history().len()
    }

    /// Row at absolute index `abs`: scrollback first, then the active screen.
    pub fn line(&self, abs: usize) -> Option<&Row> {
        let history = self.history();
        let history_len = history.len();
        if abs < history_len {
            history.get(abs)
        } else {
            self.screen().rows.get(abs - history_len)
        }
    }

    /// Working directory reported by the shell through OSC 7.
    pub fn cwd(&self) -> Option<&str> {
        self.cwd.as_deref()
    }

    pub fn alt_active(&self) -> bool {
        self.alt_on
    }

    pub fn app_cursor(&self) -> bool {
        self.app_cursor
    }

    pub fn bracketed_paste(&self) -> bool {
        self.bracketed_paste
    }

    pub fn cursor_visible(&self) -> bool {
        self.cursor_visible
    }

    pub fn cursor_blink(&self) -> bool {
        self.cursor_blink
    }

    pub fn cursor_style(&self) -> CursorStyle {
        self.cursor_style
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    /// Apply an OSC 7 payload (`file://host/path`).  Malformed payloads are
    /// ignored; a payload without a path clears the tracked directory.
    pub(crate) fn set_cwd(&mut self, payload: &str) {
        let Some(rest) = payload.strip_prefix("file://") else {
            return;
        };
        let Some(slash) = rest.find('/') else {
            self.cwd = None;
            return;
        };
        let path = &rest[slash..];
        if path.is_empty() {
            self.cwd = None;
        } else {
            self.cwd = Some(path.to_string());
        }
    }

    /// True once since the last call if the child rang BEL.
    pub fn take_bell(&mut self) -> bool {
        std::mem::take(&mut self.bell)
    }

    /// Bytes the emulator owes the child (DSR/DA replies).
    pub fn take_responses(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.responses)
    }

    /// Colours for a cell, for debugging/tests.
    pub fn cell(&self, x: usize, y: usize) -> Option<&Cell> {
        self.cur().rows.get(y)?.cells.get(x)
    }

    // ----- screen selection -------------------------------------------------

    pub(crate) fn cur(&self) -> &Screen {
        if self.alt_on {
            &self.alt
        } else {
            &self.main
        }
    }

    pub(crate) fn cur_mut(&mut self) -> &mut Screen {
        if self.alt_on {
            &mut self.alt
        } else {
            &mut self.main
        }
    }

    pub(crate) fn push_history(&mut self, row: Row) {
        if HISTORY_LIMIT == 0 {
            return;
        }
        while self.history.len() >= HISTORY_LIMIT {
            self.history.pop_front();
        }
        self.history.push_back(row);
    }

    // ----- modes / saved state ---------------------------------------------

    pub(crate) fn rebuild_tabs(&mut self) {
        let cols = self.cols();
        self.tabs = (0..cols).map(|i| i % 8 == 0).collect();
    }

    pub(crate) fn tab_next(&self) -> usize {
        let x = self.cur().cursor.x;
        for (i, &set) in self.tabs.iter().enumerate().skip(x + 1) {
            if set {
                return i;
            }
        }
        self.cols().saturating_sub(1)
    }

    pub(crate) fn tab_prev(&self) -> usize {
        let x = self.cur().cursor.x;
        for i in (0..x).rev() {
            if self.tabs.get(i).copied().unwrap_or(false) {
                return i;
            }
        }
        0
    }

    pub(crate) fn home(&mut self) {
        let y = if self.cur().origin { self.cur().top } else { 0 };
        let s = self.cur_mut();
        s.cursor.x = 0;
        s.cursor.y = y;
        s.pending_wrap = false;
    }

    pub(crate) fn full_reset(&mut self) {
        let (cols, rows) = (self.cols(), self.rows());
        self.main = Screen::new(cols, rows);
        self.alt = Screen::new(cols, rows);
        self.alt_on = false;
        self.pen = Pen::default();
        self.insert = false;
        self.autowrap = true;
        self.app_cursor = false;
        self.bracketed_paste = false;
        self.cursor_visible = true;
        self.cursor_blink = true;
        self.cursor_style = CursorStyle::Block;
        self.charsets = [Charset::Ascii; 2];
        self.active_charset = 0;
        self.history.clear();
        self.title.clear();
        self.rebuild_tabs();
    }

    pub(crate) fn soft_reset(&mut self) {
        self.pen = Pen::default();
        self.insert = false;
        self.autowrap = true;
        self.app_cursor = false;
        self.cursor_visible = true;
        self.cursor_blink = true;
        self.cursor_style = CursorStyle::Block;
        self.charsets = [Charset::Ascii; 2];
        self.active_charset = 0;
        let s = self.cur_mut();
        s.origin = false;
        s.top = 0;
        s.bottom = s.rows.len().saturating_sub(1);
        s.cursor.x = 0;
        s.cursor.y = 0;
        s.pending_wrap = false;
    }

    pub(crate) fn save_cursor(&mut self) {
        let pen = self.pen;
        self.cur_mut().save(pen);
    }

    pub(crate) fn restore_cursor(&mut self) {
        self.pen = self.cur_mut().restore();
    }

    pub(crate) fn enter_alt(&mut self, clear: bool, save: bool) {
        if self.alt_on {
            return;
        }
        if save {
            self.main.save(self.pen);
        }
        if clear {
            self.alt = Screen::new(self.cols(), self.rows());
        }
        self.alt_on = true;
        if save {
            self.alt.pending_wrap = false;
            self.alt.cursor.x = 0;
            self.alt.cursor.y = 0;
        }
    }

    pub(crate) fn exit_alt(&mut self, restore: bool, clear: bool) {
        if !self.alt_on {
            return;
        }
        self.alt_on = false;
        if restore {
            self.pen = self.main.restore();
        }
        self.main.pending_wrap = false;
        if clear {
            let (cols, rows) = (self.cols(), self.rows());
            self.alt = Screen::new(cols, rows);
        }
    }

    // ----- printing ---------------------------------------------------------

    pub(crate) fn map_charset(&self, ch: char) -> char {
        match self.charsets[self.active_charset] {
            Charset::Ascii => ch,
            Charset::DecSpecial => dec_special(ch).unwrap_or(ch),
        }
    }

    pub(crate) fn print_char(&mut self, ch: char) {
        let ch = self.map_charset(ch);
        let width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if width == 0 {
            return;
        }
        let cols = self.cols();
        if cols == 0 {
            return;
        }

        if self.cur().pending_wrap {
            self.cur_mut().pending_wrap = false;
            if self.autowrap {
                self.cur_mut().cursor.x = 0;
                self.linefeed();
            }
        }

        let x = self.cur().cursor.x.min(cols - 1);
        if width == 2 && x + 2 > cols {
            if cols < 2 {
                return;
            }
            if self.autowrap {
                self.cur_mut().cursor.x = 0;
                self.linefeed();
            } else {
                self.cur_mut().cursor.x = cols - 2;
            }
        }

        let x = self.cur().cursor.x.min(cols - 1);
        let y = self.cur().cursor.y;
        if y >= self.cur().height() {
            return;
        }

        if self.insert {
            let bg = self.pen.bg;
            let row = &mut self.cur_mut().rows[y];
            if x + width <= cols {
                for i in (x + width..cols).rev() {
                    row.cells[i] = row.cells[i - width];
                }
            }
            for cell in &mut row.cells[x..(x + width).min(cols)] {
                *cell = Cell::blank(bg);
            }
        }

        // Overwriting either half of a wide character clears the other half.
        let (left_cont, right_cont) = {
            let row = &self.cur().rows[y];
            let left = x > 0 && row.cells[x].cont;
            let right = x + 1 < cols && row.cells[x + 1].cont;
            (left, right)
        };
        let bg = self.pen.bg;
        if left_cont {
            self.cur_mut().rows[y].cells[x - 1] = Cell::blank(bg);
        }
        if right_cont {
            self.cur_mut().rows[y].cells[x + 1] = Cell::blank(bg);
        }

        let pen = self.pen;
        {
            let row = &mut self.cur_mut().rows[y];
            row.cells[x] = pen.cell(ch);
            if width == 2 {
                let mut cont = pen.cell(' ');
                cont.cont = true;
                row.cells[x + 1] = cont;
            }
        }

        let next = x + width;
        let autowrap = self.autowrap;
        let s = self.cur_mut();
        s.cursor.y = y;
        if next >= cols {
            s.cursor.x = cols - 1;
            if autowrap {
                s.pending_wrap = true;
                s.rows[y].wrapped = true;
            }
        } else {
            s.cursor.x = next;
        }
        self.last_printed = ch;
    }

    pub(crate) fn repeat_char(&mut self, count: usize) {
        let cap = self.cols().saturating_mul(self.rows()).max(1);
        let ch = self.last_printed;
        for _ in 0..count.min(cap) {
            self.print_char(ch);
        }
    }

    // ----- controls and scrolling ------------------------------------------

    pub(crate) fn linefeed(&mut self) {
        let (y, top, bottom, height) = {
            let s = self.cur();
            (s.cursor.y, s.top, s.bottom, s.height())
        };
        if y == bottom && bottom >= top {
            self.scroll_up(1);
        } else if y + 1 < height {
            self.cur_mut().cursor.y = y + 1;
        }
        self.cur_mut().pending_wrap = false;
    }

    pub(crate) fn reverse_index(&mut self) {
        let (y, top) = {
            let s = self.cur();
            (s.cursor.y, s.top)
        };
        if y == top {
            self.scroll_down(1);
        } else if y > 0 {
            self.cur_mut().cursor.y = y - 1;
        }
        self.cur_mut().pending_wrap = false;
    }

    pub(crate) fn scroll_up(&mut self, n: usize) {
        let (top, bottom, cols, height) = {
            let s = self.cur();
            (s.top, s.bottom, s.cols, s.height())
        };
        if top > bottom || top >= height {
            return;
        }
        let n = n.min(bottom - top + 1);
        if n == 0 {
            return;
        }
        let full = !self.alt_on && top == 0 && bottom == height - 1;
        let bg = self.pen.bg;
        let mut evicted = Vec::new();
        {
            let s = self.cur_mut();
            for _ in 0..n {
                let row = s.rows.remove(top);
                if full {
                    evicted.push(row);
                }
                let at = bottom.min(s.rows.len());
                s.rows.insert(at, Row::blank_with(cols, bg));
            }
        }
        for row in evicted {
            self.push_history(row);
        }
    }

    pub(crate) fn scroll_down(&mut self, n: usize) {
        let (top, bottom, cols, height) = {
            let s = self.cur();
            (s.top, s.bottom, s.cols, s.height())
        };
        if top > bottom || top >= height {
            return;
        }
        let n = n.min(bottom - top + 1);
        if n == 0 {
            return;
        }
        let bg = self.pen.bg;
        let s = self.cur_mut();
        for _ in 0..n {
            s.rows.remove(bottom);
            s.rows.insert(top, Row::blank_with(cols, bg));
        }
    }

    // ----- cursor movement --------------------------------------------------

    pub(crate) fn move_vertical(&mut self, delta: isize) {
        let (y, top, bottom, height) = {
            let s = self.cur();
            (s.cursor.y, s.top, s.bottom, s.height())
        };
        let (lo, hi) = if y >= top && y <= bottom && top <= bottom {
            (top, bottom)
        } else {
            (0, height.saturating_sub(1))
        };
        let ny = (y as isize + delta).clamp(lo as isize, hi as isize) as usize;
        let s = self.cur_mut();
        s.cursor.y = ny;
        s.pending_wrap = false;
    }

    pub(crate) fn move_horizontal(&mut self, delta: isize) {
        let (x, cols) = {
            let s = self.cur();
            (s.cursor.x, s.cols)
        };
        let nx = (x as isize + delta).clamp(0, cols.saturating_sub(1) as isize) as usize;
        let s = self.cur_mut();
        s.cursor.x = nx;
        s.pending_wrap = false;
    }

    pub(crate) fn set_cursor(&mut self, row: usize, col: usize) {
        let (cols, height, top, bottom, origin) = {
            let s = self.cur();
            (s.cols, s.height(), s.top, s.bottom, s.origin)
        };
        let x = col.min(cols.saturating_sub(1));
        let y = if origin {
            top.saturating_add(row).min(bottom)
        } else {
            row.min(height.saturating_sub(1))
        };
        let s = self.cur_mut();
        s.cursor.x = x;
        s.cursor.y = y;
        s.pending_wrap = false;
    }

    pub(crate) fn set_column(&mut self, col: usize) {
        let cols = self.cols();
        let s = self.cur_mut();
        s.cursor.x = col.min(cols.saturating_sub(1));
        s.pending_wrap = false;
    }

    pub(crate) fn set_row(&mut self, row: usize) {
        let (height, top, bottom, origin) = {
            let s = self.cur();
            (s.height(), s.top, s.bottom, s.origin)
        };
        let y = if origin {
            top.saturating_add(row).min(bottom)
        } else {
            row.min(height.saturating_sub(1))
        };
        let s = self.cur_mut();
        s.cursor.y = y;
        s.pending_wrap = false;
    }

    pub(crate) fn set_margins(&mut self, top: usize, bottom: usize) {
        let height = self.rows();
        let top = top.max(1) - 1;
        let bottom = if bottom == 0 {
            height
        } else {
            bottom.min(height)
        }
        .saturating_sub(1);
        if top >= bottom {
            return;
        }
        let s = self.cur_mut();
        s.top = top;
        s.bottom = bottom;
        let y = if s.origin { top } else { 0 };
        s.cursor.x = 0;
        s.cursor.y = y;
        s.pending_wrap = false;
    }

    // ----- erasing / inserting ---------------------------------------------

    pub(crate) fn clear_row(&mut self, y: usize) {
        let bg = self.pen.bg;
        let cols = self.cols();
        self.cur_mut().erase_cells(y, 0, cols.saturating_sub(1), bg);
    }

    pub(crate) fn erase_display(&mut self, mode: u16) {
        let (y, x, cols, height) = {
            let s = self.cur();
            (s.cursor.y, s.cursor.x, s.cols, s.height())
        };
        let bg = self.pen.bg;
        match mode {
            0 => {
                self.cur_mut().erase_cells(y, x, cols.saturating_sub(1), bg);
                for yy in y + 1..height {
                    self.clear_row(yy);
                }
            }
            1 => {
                for yy in 0..y {
                    self.clear_row(yy);
                }
                self.cur_mut().erase_cells(y, 0, x, bg);
            }
            2 => {
                for yy in 0..height {
                    self.clear_row(yy);
                }
                self.home();
            }
            3 => self.history.clear(),
            _ => {}
        }
        self.cur_mut().pending_wrap = false;
    }

    pub(crate) fn erase_line(&mut self, mode: u16) {
        let (y, x, cols) = {
            let s = self.cur();
            (s.cursor.y, s.cursor.x, s.cols)
        };
        let bg = self.pen.bg;
        match mode {
            0 => self.cur_mut().erase_cells(y, x, cols.saturating_sub(1), bg),
            1 => self.cur_mut().erase_cells(y, 0, x, bg),
            2 => self.cur_mut().erase_cells(y, 0, cols.saturating_sub(1), bg),
            _ => {}
        }
        self.cur_mut().pending_wrap = false;
    }

    pub(crate) fn insert_lines(&mut self, n: usize) {
        let (y, top, bottom, cols) = {
            let s = self.cur();
            (s.cursor.y, s.top, s.bottom, s.cols)
        };
        if y < top || y > bottom {
            return;
        }
        let n = n.min(bottom - y + 1);
        let bg = self.pen.bg;
        let s = self.cur_mut();
        for _ in 0..n {
            s.rows.remove(bottom);
            s.rows.insert(y, Row::blank_with(cols, bg));
        }
        s.pending_wrap = false;
    }

    pub(crate) fn delete_lines(&mut self, n: usize) {
        let (y, top, bottom, cols) = {
            let s = self.cur();
            (s.cursor.y, s.top, s.bottom, s.cols)
        };
        if y < top || y > bottom {
            return;
        }
        let n = n.min(bottom - y + 1);
        let bg = self.pen.bg;
        let s = self.cur_mut();
        for _ in 0..n {
            s.rows.remove(y);
            s.rows.insert(bottom, Row::blank_with(cols, bg));
        }
        s.pending_wrap = false;
    }

    pub(crate) fn insert_cells(&mut self, n: usize) {
        let (y, x, cols) = {
            let s = self.cur();
            (s.cursor.y, s.cursor.x, s.cols)
        };
        let n = n.min(cols.saturating_sub(x));
        if n == 0 {
            return;
        }
        let bg = self.pen.bg;
        let row = &mut self.cur_mut().rows[y];
        for i in (x + n..cols).rev() {
            row.cells[i] = row.cells[i - n];
        }
        for cell in &mut row.cells[x..x + n] {
            *cell = Cell::blank(bg);
        }
        self.cur_mut().pending_wrap = false;
    }

    pub(crate) fn delete_cells(&mut self, n: usize) {
        let (y, x, cols) = {
            let s = self.cur();
            (s.cursor.y, s.cursor.x, s.cols)
        };
        let n = n.min(cols.saturating_sub(x));
        if n == 0 {
            return;
        }
        let bg = self.pen.bg;
        let row = &mut self.cur_mut().rows[y];
        for i in x..cols - n {
            row.cells[i] = row.cells[i + n];
        }
        for cell in &mut row.cells[cols - n..cols] {
            *cell = Cell::blank(bg);
        }
        self.cur_mut().pending_wrap = false;
    }

    pub(crate) fn erase_chars(&mut self, n: usize) {
        let (y, x, cols) = {
            let s = self.cur();
            (s.cursor.y, s.cursor.x, s.cols)
        };
        let n = n.min(cols.saturating_sub(x));
        if n == 0 {
            return;
        }
        let bg = self.pen.bg;
        let row = &mut self.cur_mut().rows[y];
        for cell in &mut row.cells[x..x + n] {
            *cell = Cell::blank(bg);
        }
        self.cur_mut().pending_wrap = false;
    }
}

/// xterm's DEC Special Graphics mapping (charset `0`).
fn dec_special(ch: char) -> Option<char> {
    Some(match ch {
        '`' => '\u{25c6}', // ◆
        'a' => '\u{2592}', // ▒
        'b' => '\u{2409}', // ␉
        'c' => '\u{240c}', // ␌
        'd' => '\u{240d}', // ␍
        'e' => '\u{240a}', // ␊
        'f' => '\u{00b0}', // °
        'g' => '\u{00b1}', // ±
        'h' => '\u{2424}', // ␤
        'i' => '\u{240b}', // ␋
        'j' => '\u{2518}', // ┘
        'k' => '\u{2510}', // ┐
        'l' => '\u{250c}', // ┌
        'm' => '\u{2514}', // └
        'n' => '\u{253c}', // ┼
        'o' => '\u{23ba}', // ⎺
        'p' => '\u{23bb}', // ⎻
        'q' => '\u{2500}', // ─
        'r' => '\u{23bc}', // ⎼
        's' => '\u{23bd}', // ⎽
        't' => '\u{251c}', // ├
        'u' => '\u{2524}', // ┤
        'v' => '\u{2534}', // ┴
        'w' => '\u{252c}', // ┬
        'x' => '\u{2502}', // │
        'y' => '\u{2264}', // ≤
        'z' => '\u{2265}', // ≥
        '{' => '\u{03c0}', // π
        '|' => '\u{2260}', // ≠
        '}' => '\u{00a3}', // £
        '~' => '\u{00b7}', // ·
        _ => return None,
    })
}
