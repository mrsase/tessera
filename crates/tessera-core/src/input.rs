//! Key translation, independent of the UI toolkit.
//!
//! The front end turns its platform key event into a [`KeyPress`] plus
//! [`Mods`]; this module produces the bytes a VT expects. That keeps the whole
//! mapping table unit-testable without a windowing system.

use std::fmt;

/// A logical, layout-resolved key.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NamedKey {
    Enter,
    Backspace,
    Tab,
    Escape,
    Space,
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    Home,
    End,
    Insert,
    Delete,
    PageUp,
    PageDown,
    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    F9,
    F10,
    F11,
    F12,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum KeyPress<'a> {
    /// Printable text after the platform applied shift/layout (e.g. "A", "é").
    Character(&'a str),
    Named(NamedKey),
    #[default]
    Unidentified,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Mods {
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
}

impl Mods {
    /// xterm modifier parameter: 1 + shift + 2*alt + 4*ctrl.
    fn param(self) -> u8 {
        1 + u8::from(self.shift) + 2 * u8::from(self.alt) + 4 * u8::from(self.ctrl)
    }

    fn any(self) -> bool {
        self.shift || self.alt || self.ctrl
    }
}

impl fmt::Display for Mods {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.ctrl {
            f.write_str("ctrl+")?;
        }
        if self.alt {
            f.write_str("alt+")?;
        }
        if self.shift {
            f.write_str("shift+")?;
        }
        Ok(())
    }
}

/// Translate one key press into bytes for the child process. `app_cursor`
/// follows DECCKM (`CSI ? 1 h`), which applications enable while running.
pub fn encode(
    key: KeyPress<'_>,
    text: Option<&str>,
    mods: Mods,
    app_cursor: bool,
) -> Option<Vec<u8>> {
    match key {
        KeyPress::Character(_) => {
            if mods.ctrl {
                let ch = text.filter(|t| !t.is_empty()).or(match key {
                    KeyPress::Character(c) => Some(c),
                    _ => None,
                })?;
                Some(with_alt(vec![control_byte(ch)?], mods.alt))
            } else {
                let produced = text.filter(|t| !t.is_empty()).or(match key {
                    KeyPress::Character(c) => Some(c),
                    _ => None,
                })?;
                if produced.is_empty() {
                    None
                } else {
                    Some(with_alt(produced.as_bytes().to_vec(), mods.alt))
                }
            }
        }
        KeyPress::Named(named) => encode_named(named, text, mods, app_cursor),
        KeyPress::Unidentified => None,
    }
}

fn encode_named(
    named: NamedKey,
    text: Option<&str>,
    mods: Mods,
    app_cursor: bool,
) -> Option<Vec<u8>> {
    let modifier = mods.param();
    let bytes = match named {
        NamedKey::Enter => with_alt(vec![b'\r'], mods.alt),
        NamedKey::Backspace => with_alt(vec![0x7f], mods.alt),
        NamedKey::Tab if mods.shift => with_alt(b"\x1b[Z".to_vec(), mods.alt),
        NamedKey::Tab => with_alt(vec![b'\t'], mods.alt),
        NamedKey::Escape => with_alt(vec![0x1b], mods.alt),
        NamedKey::Space if mods.ctrl => vec![0x00],
        NamedKey::Space => {
            let produced = text.filter(|t| !t.is_empty()).unwrap_or(" ");
            with_alt(produced.as_bytes().to_vec(), mods.alt)
        }
        NamedKey::ArrowUp => cursor_sequence(b'A', modifier, app_cursor),
        NamedKey::ArrowDown => cursor_sequence(b'B', modifier, app_cursor),
        NamedKey::ArrowRight => cursor_sequence(b'C', modifier, app_cursor),
        NamedKey::ArrowLeft => cursor_sequence(b'D', modifier, app_cursor),
        NamedKey::Home => cursor_sequence(b'H', modifier, app_cursor),
        NamedKey::End => cursor_sequence(b'F', modifier, app_cursor),
        NamedKey::Insert => tilde_sequence(2, modifier),
        NamedKey::Delete => tilde_sequence(3, modifier),
        NamedKey::PageUp => tilde_sequence(5, modifier),
        NamedKey::PageDown => tilde_sequence(6, modifier),
        NamedKey::F1 => function_sequence(b'P', modifier),
        NamedKey::F2 => function_sequence(b'Q', modifier),
        NamedKey::F3 => function_sequence(b'R', modifier),
        NamedKey::F4 => function_sequence(b'S', modifier),
        NamedKey::F5 => tilde_sequence(15, modifier),
        NamedKey::F6 => tilde_sequence(17, modifier),
        NamedKey::F7 => tilde_sequence(18, modifier),
        NamedKey::F8 => tilde_sequence(19, modifier),
        NamedKey::F9 => tilde_sequence(20, modifier),
        NamedKey::F10 => tilde_sequence(21, modifier),
        NamedKey::F11 => tilde_sequence(23, modifier),
        NamedKey::F12 => tilde_sequence(24, modifier),
    };
    Some(bytes)
}

/// Control byte for a printable character while Ctrl is held.
fn control_byte(text: &str) -> Option<u8> {
    let ch = text.chars().next()?;
    Some(match ch {
        'a'..='z' => ch as u8 - b'a' + 1,
        'A'..='Z' => ch as u8 - b'A' + 1,
        '@' | ' ' => 0,
        '[' => 27,
        '\\' => 28,
        ']' => 29,
        '^' => 30,
        '_' => 31,
        '?' => 127,
        _ => return None,
    })
}

fn with_alt(bytes: Vec<u8>, alt: bool) -> Vec<u8> {
    if !alt {
        return bytes;
    }
    let mut out = Vec::with_capacity(bytes.len() + 1);
    out.push(0x1b);
    out.extend_from_slice(&bytes);
    out
}

fn cursor_sequence(final_byte: u8, modifier: u8, app_cursor: bool) -> Vec<u8> {
    if modifier != 1 {
        format!("\x1b[1;{modifier}{}", final_byte as char).into_bytes()
    } else if app_cursor {
        vec![0x1b, b'O', final_byte]
    } else {
        vec![0x1b, b'[', final_byte]
    }
}

fn tilde_sequence(number: u8, modifier: u8) -> Vec<u8> {
    if modifier == 1 {
        format!("\x1b[{number}~").into_bytes()
    } else {
        format!("\x1b[{number};{modifier}~").into_bytes()
    }
}

fn function_sequence(final_byte: u8, modifier: u8) -> Vec<u8> {
    if modifier == 1 {
        vec![0x1b, b'O', final_byte]
    } else {
        format!("\x1b[1;{modifier}{}", final_byte as char).into_bytes()
    }
}

/// Wrap pasted text, honouring bracketed paste mode and normalising CR/CRLF to
/// LF (the tty would turn a bare CR into a command anyway).
pub fn encode_paste(text: &str, bracketed: bool) -> Vec<u8> {
    let mut body = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\r' {
            if chars.peek() == Some(&'\n') {
                chars.next();
            }
            body.push('\n');
        } else {
            body.push(ch);
        }
    }
    if !bracketed {
        return body.into_bytes();
    }
    let mut out = Vec::with_capacity(body.len() + 12);
    out.extend_from_slice(b"\x1b[200~");
    out.extend_from_slice(body.as_bytes());
    out.extend_from_slice(b"\x1b[201~");
    out
}

/// True if `text` is purely a control-key chord that the terminal should not
/// treat as typed command text (used by the suggestion tracker).
pub fn is_control_chord(mods: Mods) -> bool {
    mods.ctrl || mods.alt
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mods(shift: bool, alt: bool, ctrl: bool) -> Mods {
        Mods { shift, alt, ctrl }
    }

    fn named(n: NamedKey) -> Option<Vec<u8>> {
        encode(KeyPress::Named(n), None, Mods::default(), false)
    }

    #[test]
    fn plain_text_uses_layout_result() {
        assert_eq!(
            encode(
                KeyPress::Character("a"),
                Some("A"),
                Mods {
                    shift: true,
                    ..Default::default()
                },
                false
            ),
            Some(b"A".to_vec())
        );
        assert_eq!(
            encode(KeyPress::Character("é"), None, Mods::default(), false),
            Some("é".as_bytes().to_vec())
        );
    }

    #[test]
    fn ctrl_letters_become_control_bytes() {
        assert_eq!(
            encode(
                KeyPress::Character("c"),
                Some("c"),
                mods(false, false, true),
                false
            ),
            Some(vec![3])
        );
        assert_eq!(
            encode(
                KeyPress::Character(" "),
                None,
                mods(false, false, true),
                false
            ),
            Some(vec![0])
        );
        assert_eq!(
            encode(
                KeyPress::Character("["),
                None,
                mods(false, false, true),
                false
            ),
            Some(vec![27])
        );
    }

    #[test]
    fn alt_is_an_escape_prefix() {
        assert_eq!(
            encode(
                KeyPress::Character("x"),
                None,
                mods(false, true, false),
                false
            ),
            Some(vec![0x1b, b'x'])
        );
        assert_eq!(
            encode(
                KeyPress::Named(NamedKey::Backspace),
                None,
                mods(false, true, false),
                false
            ),
            Some(vec![0x1b, 0x7f])
        );
    }

    #[test]
    fn arrows_respect_app_cursor_mode_and_modifiers() {
        assert_eq!(named(NamedKey::ArrowUp), Some(b"\x1b[A".to_vec()));
        assert_eq!(
            encode(
                KeyPress::Named(NamedKey::ArrowUp),
                None,
                Mods::default(),
                true
            ),
            Some(b"\x1bOA".to_vec())
        );
        assert_eq!(
            encode(
                KeyPress::Named(NamedKey::ArrowRight),
                None,
                mods(false, false, true),
                false
            ),
            Some(b"\x1b[1;5C".to_vec())
        );
        assert_eq!(
            encode(
                KeyPress::Named(NamedKey::ArrowLeft),
                None,
                mods(true, false, false),
                true
            ),
            Some(b"\x1b[1;2D".to_vec())
        );
    }

    #[test]
    fn function_and_editing_keys_match_xterm() {
        assert_eq!(named(NamedKey::F1), Some(b"\x1bOP".to_vec()));
        assert_eq!(named(NamedKey::F5), Some(b"\x1b[15~".to_vec()));
        assert_eq!(named(NamedKey::F12), Some(b"\x1b[24~".to_vec()));
        assert_eq!(named(NamedKey::Insert), Some(b"\x1b[2~".to_vec()));
        assert_eq!(named(NamedKey::Delete), Some(b"\x1b[3~".to_vec()));
        assert_eq!(named(NamedKey::PageUp), Some(b"\x1b[5~".to_vec()));
        assert_eq!(
            encode(
                KeyPress::Named(NamedKey::PageDown),
                None,
                mods(true, false, false),
                false
            ),
            Some(b"\x1b[6;2~".to_vec())
        );
    }

    #[test]
    fn enter_tab_backtab_escape() {
        assert_eq!(named(NamedKey::Enter), Some(vec![b'\r']));
        assert_eq!(named(NamedKey::Tab), Some(vec![b'\t']));
        assert_eq!(
            encode(
                KeyPress::Named(NamedKey::Tab),
                None,
                mods(true, false, false),
                false
            ),
            Some(b"\x1b[Z".to_vec())
        );
        assert_eq!(named(NamedKey::Escape), Some(vec![0x1b]));
    }

    #[test]
    fn paste_is_normalised_and_bracketed() {
        assert_eq!(encode_paste("a\r\nb\rc", false), b"a\nb\nc".to_vec());
        assert_eq!(encode_paste("hi", true), b"\x1b[200~hi\x1b[201~".to_vec());
        assert_eq!(encode_paste("", true), b"\x1b[200~\x1b[201~".to_vec());
    }
}
