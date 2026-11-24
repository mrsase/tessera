//! `vte::Perform` implementation: maps parsed escape sequences onto `Term`.
//!
//! All grid mechanics live in `term.rs`; this file only decodes parameters and
//! dispatches.  Unknown sequences are ignored rather than panicking.

use crate::attrs::{Attrs, Color, Pen};
use crate::screen::CursorStyle;
use crate::term::{Charset, Term};
use vte::{Params, Perform};

fn param(params: &Params, index: usize) -> u16 {
    params
        .iter()
        .nth(index)
        .and_then(|sub| sub.first().copied())
        .unwrap_or(0)
}

/// VT parameters treat zero as "use the default".
fn nz(value: u16, default: usize) -> usize {
    if value == 0 {
        default
    } else {
        value as usize
    }
}

fn charset_for(byte: u8) -> Charset {
    if byte == b'0' {
        Charset::DecSpecial
    } else {
        Charset::Ascii
    }
}

/// Flip a cursor shape between its blinking and steady variants.
fn with_blink(style: CursorStyle, blink: bool) -> CursorStyle {
    match (style, blink) {
        (CursorStyle::Block, true) => CursorStyle::BlockBlink,
        (CursorStyle::BlockBlink, false) => CursorStyle::Block,
        (CursorStyle::Underline, true) => CursorStyle::UnderlineBlink,
        (CursorStyle::UnderlineBlink, false) => CursorStyle::Underline,
        (CursorStyle::Bar, true) => CursorStyle::BarBlink,
        (CursorStyle::BarBlink, false) => CursorStyle::Bar,
        (other, _) => other,
    }
}

impl Perform for Term {
    fn print(&mut self, c: char) {
        self.print_char(c);
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            0x07 => self.bell = true,
            0x08 => {
                let s = self.cur_mut();
                s.cursor.x = s.cursor.x.saturating_sub(1);
                s.pending_wrap = false;
            }
            0x09 => {
                let x = self.tab_next();
                self.set_column(x);
            }
            0x0a..=0x0c => self.linefeed(),
            0x0d => self.set_column(0),
            0x0e => self.active_charset = 1,
            0x0f => self.active_charset = 0,
            _ => {}
        }
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
        let Some((code, rest)) = params.split_first() else {
            return;
        };
        let code = std::str::from_utf8(code)
            .ok()
            .and_then(|s| s.trim().parse::<u16>().ok());
        match code {
            Some(0) | Some(2) => {
                let mut title = String::new();
                for (i, segment) in rest.iter().enumerate() {
                    if i > 0 {
                        title.push(';');
                    }
                    let text = String::from_utf8_lossy(segment);
                    title.extend(text.chars().filter(|c| !c.is_control()));
                }
                self.title = title;
            }
            // OSC 7;file://host/path — shell working directory.
            Some(7) => {
                let mut payload = String::new();
                for (i, segment) in rest.iter().enumerate() {
                    if i > 0 {
                        payload.push(';');
                    }
                    payload.push_str(&String::from_utf8_lossy(segment));
                }
                self.set_cwd(&payload);
            }
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], _ignore: bool, action: char) {
        self.csi(params, intermediates, action);
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], _ignore: bool, byte: u8) {
        self.esc(intermediates, byte);
    }
}

impl Term {
    fn csi(&mut self, params: &Params, intermediates: &[u8], action: char) {
        let private = intermediates.first() == Some(&b'?');
        // '>' (secondary DA, mouse), '<' and '=' are private device requests
        // we do not answer.
        if intermediates
            .iter()
            .any(|&b| matches!(b, b'<' | b'=' | b'>'))
        {
            return;
        }
        let n = |i: usize| nz(param(params, i), 1);
        match action {
            'A' => self.move_vertical(-(n(0) as isize)),
            'B' => self.move_vertical(n(0) as isize),
            'C' => self.move_horizontal(n(0) as isize),
            'D' => self.move_horizontal(-(n(0) as isize)),
            'E' => {
                self.move_vertical(n(0) as isize);
                self.set_column(0);
            }
            'F' => {
                self.move_vertical(-(n(0) as isize));
                self.set_column(0);
            }
            'G' => self.set_column(n(0) - 1),
            'H' | 'f' => self.set_cursor(n(0) - 1, n(1) - 1),
            'I' => {
                for _ in 0..n(0) {
                    let x = self.tab_next();
                    self.set_column(x);
                }
            }
            'J' => self.erase_display(param(params, 0)),
            'K' => self.erase_line(param(params, 0)),
            'L' => self.insert_lines(n(0)),
            'M' => self.delete_lines(n(0)),
            'P' => self.delete_cells(n(0)),
            '@' => self.insert_cells(n(0)),
            'S' => self.scroll_up(n(0)),
            'T' => self.scroll_down(n(0)),
            'X' => self.erase_chars(n(0)),
            'Z' => {
                for _ in 0..n(0) {
                    let x = self.tab_prev();
                    self.set_column(x);
                }
            }
            '`' => self.set_column(n(0) - 1),
            'a' => self.move_horizontal(n(0) as isize),
            'b' => self.repeat_char(n(0)),
            'c' => self.responses.extend_from_slice(b"\x1b[?6c"),
            'd' => self.set_row(n(0) - 1),
            'e' => self.move_vertical(n(0) as isize),
            'g' => match param(params, 0) {
                0 => {
                    let x = self.cur().cursor.x;
                    if x < self.tabs.len() {
                        self.tabs[x] = false;
                    }
                }
                3 => self.tabs.iter_mut().for_each(|t| *t = false),
                _ => {}
            },
            'h' => self.set_modes(params, private, true),
            'l' => self.set_modes(params, private, false),
            'm' => self.sgr(params),
            'n' => self.dsr(param(params, 0)),
            'p' => {
                if intermediates.first() == Some(&b'!') {
                    self.soft_reset();
                }
            }
            'q' => {
                if intermediates.first() == Some(&b' ') {
                    self.decscusr(param(params, 0));
                }
            }
            'r' => self.set_margins(param(params, 0) as usize, param(params, 1) as usize),
            's' => self.save_cursor(),
            'u' => self.restore_cursor(),
            _ => {}
        }
    }

    fn set_modes(&mut self, params: &Params, private: bool, on: bool) {
        let modes: Vec<u16> = params
            .iter()
            .map(|sub| sub.first().copied().unwrap_or(0))
            .collect();
        for mode in modes {
            if private {
                match mode {
                    1 => self.app_cursor = on,
                    4 => self.insert = on,
                    6 => {
                        self.cur_mut().origin = on;
                        let y = if on { self.cur().top } else { 0 };
                        let s = self.cur_mut();
                        s.cursor.y = y;
                        s.cursor.x = 0;
                        s.pending_wrap = false;
                    }
                    7 => self.autowrap = on,
                    12 => {
                        self.cursor_blink = on;
                        self.cursor_style = with_blink(self.cursor_style, on);
                    }
                    25 => self.cursor_visible = on,
                    47 => {
                        if on {
                            self.enter_alt(false, false);
                        } else {
                            self.exit_alt(false, false);
                        }
                    }
                    1047 => {
                        if on {
                            self.enter_alt(true, false);
                        } else {
                            self.exit_alt(false, true);
                        }
                    }
                    1048 => {
                        if on {
                            self.save_cursor();
                        } else {
                            self.restore_cursor();
                        }
                    }
                    1049 => {
                        if on {
                            self.enter_alt(true, true);
                        } else {
                            self.exit_alt(true, true);
                        }
                    }
                    2004 => self.bracketed_paste = on,
                    _ => {}
                }
            } else if mode == 4 {
                self.insert = on;
            }
        }
    }

    fn dsr(&mut self, mode: u16) {
        match mode {
            5 => self.responses.extend_from_slice(b"\x1b[0n"),
            6 => {
                let (x, y) = {
                    let s = self.cur();
                    (s.cursor.x, s.cursor.y)
                };
                let reply = format!("\x1b[{};{}R", y + 1, x + 1);
                self.responses.extend_from_slice(reply.as_bytes());
            }
            _ => {}
        }
    }

    fn decscusr(&mut self, value: u16) {
        let (style, blink) = match value {
            0 | 1 => (CursorStyle::Block, false),
            2 => (CursorStyle::BlockBlink, true),
            3 => (CursorStyle::Underline, false),
            4 => (CursorStyle::UnderlineBlink, true),
            5 => (CursorStyle::Bar, false),
            6 => (CursorStyle::BarBlink, true),
            _ => return,
        };
        self.cursor_style = style;
        self.cursor_blink = blink;
    }

    fn sgr(&mut self, params: &Params) {
        let list: Vec<&[u16]> = params.iter().collect();
        if list.is_empty() {
            self.pen = Pen::default();
            return;
        }
        let mut i = 0;
        while i < list.len() {
            let p = list[i];
            let code = p.first().copied().unwrap_or(0);
            match code {
                0 => self.pen = Pen::default(),
                1 => self.pen.attrs.set(Attrs::BOLD, true),
                2 => self.pen.attrs.set(Attrs::DIM, true),
                3 => self.pen.attrs.set(Attrs::ITALIC, true),
                4 => {
                    let on = if p.len() > 1 { p[1] != 0 } else { true };
                    self.pen.attrs.set(Attrs::UNDERLINE, on);
                }
                5 | 6 => self.pen.attrs.set(Attrs::BLINK, true),
                7 => self.pen.attrs.set(Attrs::REVERSE, true),
                8 => self.pen.attrs.set(Attrs::HIDDEN, true),
                9 => self.pen.attrs.set(Attrs::STRIKE, true),
                21 => self.pen.attrs.set(Attrs::BOLD, false),
                22 => {
                    self.pen.attrs.set(Attrs::BOLD, false);
                    self.pen.attrs.set(Attrs::DIM, false);
                }
                23 => self.pen.attrs.set(Attrs::ITALIC, false),
                24 => self.pen.attrs.set(Attrs::UNDERLINE, false),
                25 => self.pen.attrs.set(Attrs::BLINK, false),
                27 => self.pen.attrs.set(Attrs::REVERSE, false),
                28 => self.pen.attrs.set(Attrs::HIDDEN, false),
                29 => self.pen.attrs.set(Attrs::STRIKE, false),
                30..=37 => self.pen.fg = Color::Indexed((code - 30) as u8),
                38 | 48 => {
                    let (color, consumed) = extended_color(&list, i);
                    if let Some(color) = color {
                        if code == 38 {
                            self.pen.fg = color;
                        } else {
                            self.pen.bg = color;
                        }
                    }
                    i += consumed;
                }
                39 => self.pen.fg = Color::Default,
                40..=47 => self.pen.bg = Color::Indexed((code - 40) as u8),
                49 => self.pen.bg = Color::Default,
                90..=97 => self.pen.fg = Color::Indexed((code - 90 + 8) as u8),
                100..=107 => self.pen.bg = Color::Indexed((code - 100 + 8) as u8),
                _ => {}
            }
            i += 1;
        }
    }

    fn esc(&mut self, intermediates: &[u8], byte: u8) {
        if let Some(&first) = intermediates.first() {
            match first {
                b'(' => self.charsets[0] = charset_for(byte),
                b')' => self.charsets[1] = charset_for(byte),
                _ => {}
            }
            return;
        }
        match byte {
            b'7' => self.save_cursor(),
            b'8' => self.restore_cursor(),
            b'D' => self.linefeed(),
            b'E' => {
                self.linefeed();
                self.set_column(0);
            }
            b'M' => self.reverse_index(),
            b'H' => {
                let x = self.cur().cursor.x;
                if x < self.tabs.len() {
                    self.tabs[x] = true;
                }
            }
            b'c' => self.full_reset(),
            // Keypad mode ('=' / '>') and charset shifts we do not model.
            _ => {}
        }
    }
}

/// Decode `SGR 38`/`48` extended colours, returning the colour and how many
/// extra top-level parameters were consumed.
fn extended_color(list: &[&[u16]], i: usize) -> (Option<Color>, usize) {
    let p = list[i];
    if p.len() > 1 {
        match p[1] {
            5 => (
                Some(Color::Indexed(p.get(2).copied().unwrap_or(0) as u8)),
                0,
            ),
            2 => {
                let values = &p[2..];
                if values.len() >= 3 {
                    let n = values.len();
                    (
                        Some(Color::Rgb(
                            values[n - 3] as u8,
                            values[n - 2] as u8,
                            values[n - 1] as u8,
                        )),
                        0,
                    )
                } else {
                    (None, 0)
                }
            }
            _ => (None, 0),
        }
    } else {
        let Some(kind) = list.get(i + 1).and_then(|s| s.first()).copied() else {
            return (None, 0);
        };
        match kind {
            5 => match list.get(i + 2).and_then(|s| s.first()).copied() {
                Some(v) => (Some(Color::Indexed(v as u8)), 2),
                None => (None, 0),
            },
            2 => {
                let r = list.get(i + 2).and_then(|s| s.first()).copied();
                let g = list.get(i + 3).and_then(|s| s.first()).copied();
                let b = list.get(i + 4).and_then(|s| s.first()).copied();
                match (r, g, b) {
                    (Some(r), Some(g), Some(b)) => (Some(Color::Rgb(r as u8, g as u8, b as u8)), 4),
                    _ => (None, 0),
                }
            }
            _ => (None, 0),
        }
    }
}
