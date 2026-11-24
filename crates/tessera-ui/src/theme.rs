//! Colours: hex parsing, the 256-colour palette and attribute resolution.
//!
//! Cells store symbolic colours (`tessera_term::Color`); this module turns them
//! into GPU colours for one paint pass, so palette changes never touch the grid.

use gpui::{rgb, Hsla};
use tessera_core::config;
use tessera_term::{Attrs, Cell, Color};

#[derive(Clone, Debug)]
pub struct Theme {
    pub fg: Hsla,
    pub bg: Hsla,
    pub cursor: Hsla,
    pub selection: Hsla,
    pub palette: [Hsla; 16],
    /// Pre-resolved 256-colour table (ANSI 16 + 6x6x6 cube + greys).
    table: [Hsla; 256],
}

impl Theme {
    pub fn from_config(theme: &config::Theme) -> Self {
        let mut palette = [hsla(0x00ab_b2bf); 16];
        for (slot, value) in palette.iter_mut().zip(theme.palette.iter()) {
            *slot = parse_hex(value).unwrap_or(*slot);
        }
        let mut table = [hsla(0); 256];
        table[..16].copy_from_slice(&palette);
        for i in 16..232 {
            let n = i - 16;
            let levels = [0u8, 95, 135, 175, 215, 255];
            let r = levels[n / 36];
            let g = levels[(n / 6) % 6];
            let b = levels[n % 6];
            table[i] = rgb(((r as u32) << 16) | ((g as u32) << 8) | b as u32).into();
        }
        for i in 232..256 {
            let v = (8 + 10 * (i - 232)) as u32;
            table[i] = rgb((v << 16) | (v << 8) | v).into();
        }
        Self {
            fg: parse_hex(&theme.foreground).unwrap_or_else(|| hsla(0x00ab_b2bf)),
            bg: parse_hex(&theme.background).unwrap_or_else(|| hsla(0x0028_2c34)),
            cursor: parse_hex(&theme.cursor).unwrap_or_else(|| hsla(0x00ab_b2bf)),
            selection: parse_hex(&theme.selection).unwrap_or_else(|| hsla(0x003e_4451)),
            palette,
            table,
        }
    }

    pub fn resolve(&self, color: Color) -> Hsla {
        match color {
            Color::Default => self.fg,
            Color::Indexed(i) => self.table[i as usize],
            Color::Rgb(r, g, b) => rgb(((r as u32) << 16) | ((g as u32) << 8) | b as u32).into(),
        }
    }

    pub fn resolve_bg(&self, color: Color) -> Hsla {
        match color {
            Color::Default => self.bg,
            other => self.resolve(other),
        }
    }

    /// Foreground/background for a cell after reverse-video and dim handling.
    pub fn cell_colors(&self, cell: &Cell) -> (Hsla, Hsla) {
        let mut fg = self.resolve(cell.fg);
        let mut bg = self.resolve_bg(cell.bg);
        if cell.attrs.has(Attrs::REVERSE) {
            std::mem::swap(&mut fg, &mut bg);
        }
        if cell.attrs.has(Attrs::DIM) {
            fg = blend(fg, bg, 0.5);
        }
        if cell.attrs.has(Attrs::HIDDEN) {
            fg = bg;
        }
        (fg, bg)
    }
}

fn hsla(value: u32) -> Hsla {
    rgb(value).into()
}

fn blend(a: Hsla, b: Hsla, t: f32) -> Hsla {
    let ta = a.a * (1.0 - t);
    let tb = b.a * t;
    let alpha = ta + tb;
    if alpha <= f32::EPSILON {
        return a;
    }
    Hsla {
        h: a.h * (ta / alpha) + b.h * (tb / alpha),
        s: a.s * (ta / alpha) + b.s * (tb / alpha),
        l: a.l * (ta / alpha) + b.l * (tb / alpha),
        a: alpha,
    }
}

/// Parse `#rrggbb` or `rrggbb`. Returns `None` for malformed input so callers
/// can fall back to the default palette.
pub fn parse_hex(text: &str) -> Option<Hsla> {
    let digits = text.trim().trim_start_matches('#');
    if digits.len() != 6 || !digits.is_ascii() {
        return None;
    }
    let value = u32::from_str_radix(digits, 16).ok()?;
    Some(rgb(value).into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_parser_accepts_both_forms_and_rejects_junk() {
        assert_eq!(parse_hex("#abb2bf"), Some(hsla(0x00ab_b2bf)));
        assert_eq!(parse_hex("abb2bf"), Some(hsla(0x00ab_b2bf)));
        assert_eq!(parse_hex("#abc"), None);
        assert_eq!(parse_hex("#gggggg"), None);
        assert_eq!(parse_hex(""), None);
    }

    #[test]
    fn palette_resolution_covers_ansi_cube_and_greys() {
        let theme = Theme::from_config(&config::Theme::default());
        assert_eq!(theme.resolve(Color::Indexed(1)), hsla(0x00e0_6c75));
        assert_eq!(theme.resolve(Color::Indexed(196)), hsla(0x00ff_0000));
        assert_eq!(theme.resolve(Color::Indexed(232)), hsla(0x0008_0808));
        assert_eq!(theme.resolve(Color::Rgb(1, 2, 3)), hsla(0x0001_0203));
    }

    #[test]
    fn reverse_swaps_and_hidden_erases() {
        let theme = Theme::from_config(&config::Theme::default());
        let mut cell = Cell::default();
        cell.attrs.set(Attrs::REVERSE, true);
        let (fg, bg) = theme.cell_colors(&cell);
        assert_eq!(fg, theme.bg);
        assert_eq!(bg, theme.fg);

        let mut hidden = Cell::default();
        hidden.attrs.set(Attrs::HIDDEN, true);
        let (fg, bg) = theme.cell_colors(&hidden);
        assert_eq!(fg, bg);
    }
}
