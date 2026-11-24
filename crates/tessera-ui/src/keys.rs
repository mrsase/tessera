//! Adapter between GPUI key events and the toolkit-agnostic encoder.

use gpui::{Keystroke, Modifiers};
use tessera_core::input::{self, KeyPress, Mods, NamedKey};
use tessera_core::shortcut::{self, Action};

pub fn to_mods(mods: Modifiers) -> Mods {
    Mods {
        shift: mods.shift,
        alt: mods.alt,
        ctrl: mods.control,
    }
}

/// Resolve an application shortcut. Returns `None` when the key belongs to the
/// terminal or the platform (Cmd/Win chords are reserved).
pub fn app_action(keystroke: &Keystroke) -> Option<Action> {
    let mods = to_mods(keystroke.modifiers);
    shortcut::resolve(&keystroke.key, mods, keystroke.modifiers.platform)
}

fn named(key: &str) -> Option<NamedKey> {
    Some(match key {
        "enter" | "return" => NamedKey::Enter,
        "backspace" => NamedKey::Backspace,
        "tab" => NamedKey::Tab,
        "escape" => NamedKey::Escape,
        "space" => NamedKey::Space,
        "up" => NamedKey::ArrowUp,
        "down" => NamedKey::ArrowDown,
        "left" => NamedKey::ArrowLeft,
        "right" => NamedKey::ArrowRight,
        "home" => NamedKey::Home,
        "end" => NamedKey::End,
        "insert" => NamedKey::Insert,
        "delete" => NamedKey::Delete,
        "pageup" => NamedKey::PageUp,
        "pagedown" => NamedKey::PageDown,
        "f1" => NamedKey::F1,
        "f2" => NamedKey::F2,
        "f3" => NamedKey::F3,
        "f4" => NamedKey::F4,
        "f5" => NamedKey::F5,
        "f6" => NamedKey::F6,
        "f7" => NamedKey::F7,
        "f8" => NamedKey::F8,
        "f9" => NamedKey::F9,
        "f10" => NamedKey::F10,
        "f11" => NamedKey::F11,
        "f12" => NamedKey::F12,
        _ => return None,
    })
}

/// Translate one key press into bytes for the child process.
pub fn translate(keystroke: &Keystroke, app_cursor: bool) -> Option<Vec<u8>> {
    let mods = to_mods(keystroke.modifiers);
    let text = keystroke.key_char.as_deref();
    let press = if let Some(named) = named(&keystroke.key) {
        KeyPress::Named(named)
    } else if let Some(text) = text {
        KeyPress::Character(text)
    } else if keystroke.key.chars().count() == 1 {
        KeyPress::Character(&keystroke.key)
    } else {
        KeyPress::Unidentified
    };
    input::encode(press, text, mods, app_cursor)
}

/// Bytes for a paste, honouring the child's bracketed-paste mode.
pub fn paste(text: &str, bracketed: bool) -> Vec<u8> {
    input::encode_paste(text, bracketed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(name: &str, text: Option<&str>, shift: bool, alt: bool, ctrl: bool) -> Keystroke {
        Keystroke {
            modifiers: Modifiers {
                shift,
                alt,
                control: ctrl,
                platform: false,
                function: false,
            },
            key: name.to_string(),
            key_char: text.map(|t| t.to_string()),
        }
    }

    #[test]
    fn shortcuts_resolve_from_keystrokes() {
        assert_eq!(
            app_action(&key("t", None, true, false, true)),
            Some(Action::NewTab)
        );
        assert_eq!(app_action(&key("c", None, false, false, true)), None);
        assert_eq!(
            app_action(&key("tab", None, true, false, true)),
            Some(Action::PrevTab)
        );
        assert_eq!(
            app_action(&key("pageup", None, true, false, false)),
            Some(Action::ScrollPageUp)
        );
    }

    #[test]
    fn terminal_keys_are_encoded() {
        assert_eq!(
            translate(&key("up", None, false, false, false), false),
            Some(b"\x1b[A".to_vec())
        );
        assert_eq!(
            translate(&key("up", None, false, false, false), true),
            Some(b"\x1bOA".to_vec())
        );
        assert_eq!(
            translate(&key("c", None, false, false, true), false),
            Some(vec![3])
        );
        assert_eq!(
            translate(&key("x", Some("x"), false, true, false), false),
            Some(b"\x1bx".to_vec())
        );
        assert_eq!(
            translate(&key("tab", None, true, false, false), false),
            Some(b"\x1b[Z".to_vec())
        );
        assert_eq!(
            translate(&key("enter", Some("\r"), false, false, false), false),
            Some(vec![b'\r'])
        );
        assert_eq!(
            translate(&key("a", Some("a"), false, false, false), false),
            Some(b"a".to_vec())
        );
        assert_eq!(
            translate(&key("f5", None, false, false, false), false),
            Some(b"\x1b[15~".to_vec())
        );
    }
}
