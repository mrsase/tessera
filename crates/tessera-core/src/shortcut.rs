//! Application shortcuts, resolved without any UI toolkit.
//!
//! The front end lowercases the platform key name and passes its modifiers;
//! unknown chords fall through to the terminal.

use crate::input::Mods;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FocusDir {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    NewTab,
    ClosePane,
    NextTab,
    PrevTab,
    SelectTab(u8),
    SplitRight,
    SplitDown,
    FocusPane(FocusDir),
    NudgeDivider(FocusDir),
    ZoomPane,
    Copy,
    Paste,
    SelectAll,
    ScrollPageUp,
    ScrollPageDown,
    ClearScrollback,
    Palette,
    Search,
    RenameTab,
    FontIncrease,
    FontDecrease,
    FontReset,
    OpenConfig,
    Quit,
}

/// Resolve `key` (lowercase, e.g. `"t"`, `"tab"`, `"pageup"`, `"1"`) plus
/// modifiers into an application action. `meta` is the platform Cmd/Win key.
pub fn resolve(key: &str, mods: Mods, meta: bool) -> Option<Action> {
    let shift = mods.shift;
    let ctrl = mods.ctrl;
    let alt = mods.alt;

    if meta {
        return None; // leave Cmd/Win chords to the platform
    }

    if ctrl && shift {
        return match key {
            "t" => Some(Action::NewTab),
            "w" => Some(Action::ClosePane),
            "e" => Some(Action::SplitRight),
            "o" => Some(Action::SplitDown),
            "c" => Some(Action::Copy),
            "v" => Some(Action::Paste),
            "a" => Some(Action::SelectAll),
            "k" => Some(Action::ClearScrollback),
            "f" => Some(Action::Search),
            "p" => Some(Action::Palette),
            "z" => Some(Action::ZoomPane),
            "q" => Some(Action::Quit),
            "tab" => Some(Action::PrevTab),
            "=" | "+" | "plus" => Some(Action::FontIncrease),
            "-" | "minus" => Some(Action::FontDecrease),
            "0" => Some(Action::FontReset),
            "," => Some(Action::OpenConfig),
            "left" => Some(Action::FocusPane(FocusDir::Left)),
            "right" => Some(Action::FocusPane(FocusDir::Right)),
            "up" => Some(Action::FocusPane(FocusDir::Up)),
            "down" => Some(Action::FocusPane(FocusDir::Down)),
            _ => None,
        };
    }

    if alt && shift {
        return match key {
            "left" => Some(Action::NudgeDivider(FocusDir::Left)),
            "right" => Some(Action::NudgeDivider(FocusDir::Right)),
            "up" => Some(Action::NudgeDivider(FocusDir::Up)),
            "down" => Some(Action::NudgeDivider(FocusDir::Down)),
            _ => None,
        };
    }

    if ctrl {
        return match key {
            "tab" => Some(Action::NextTab),
            "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" => {
                let n = key.parse::<u8>().ok()?;
                Some(Action::SelectTab(n - 1))
            }
            _ => None,
        };
    }

    match key {
        "f2" if mods == Mods::default() => Some(Action::RenameTab),
        "pageup" if shift => Some(Action::ScrollPageUp),
        "pagedown" if shift => Some(Action::ScrollPageDown),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(shift: bool, alt: bool, ctrl: bool) -> Mods {
        Mods { shift, alt, ctrl }
    }

    #[test]
    fn tab_shortcuts() {
        assert_eq!(
            resolve("t", m(true, false, true), false),
            Some(Action::NewTab)
        );
        assert_eq!(
            resolve("w", m(true, false, true), false),
            Some(Action::ClosePane)
        );
        assert_eq!(
            resolve("tab", m(false, false, true), false),
            Some(Action::NextTab)
        );
        assert_eq!(
            resolve("tab", m(true, false, true), false),
            Some(Action::PrevTab)
        );
        assert_eq!(
            resolve("3", m(false, false, true), false),
            Some(Action::SelectTab(2))
        );
        assert_eq!(resolve("0", m(false, false, true), false), None);
    }

    #[test]
    fn pane_shortcuts() {
        assert_eq!(
            resolve("e", m(true, false, true), false),
            Some(Action::SplitRight)
        );
        assert_eq!(
            resolve("o", m(true, false, true), false),
            Some(Action::SplitDown)
        );
        assert_eq!(
            resolve("left", m(true, false, true), false),
            Some(Action::FocusPane(FocusDir::Left))
        );
        assert_eq!(
            resolve("down", m(true, true, false), false),
            Some(Action::NudgeDivider(FocusDir::Down))
        );
    }

    #[test]
    fn terminal_shortcuts() {
        assert_eq!(
            resolve("c", m(true, false, true), false),
            Some(Action::Copy)
        );
        assert_eq!(
            resolve("v", m(true, false, true), false),
            Some(Action::Paste)
        );
        assert_eq!(
            resolve("pageup", m(true, false, false), false),
            Some(Action::ScrollPageUp)
        );
        assert_eq!(
            resolve("p", m(true, false, true), false),
            Some(Action::Palette)
        );
        assert_eq!(
            resolve("=", m(true, false, true), false),
            Some(Action::FontIncrease)
        );
    }

    #[test]
    fn plain_keys_fall_through() {
        assert_eq!(
            resolve("c", m(false, false, true), false),
            None,
            "Ctrl+C is the shell's"
        );
        assert_eq!(
            resolve("tab", m(false, false, false), false),
            None,
            "completion"
        );
        assert_eq!(resolve("q", m(false, false, false), false), None);
        assert_eq!(resolve("q", m(false, false, true), false), None);
    }

    #[test]
    fn platform_meta_is_reserved() {
        assert_eq!(resolve("t", m(false, false, false), true), None);
        assert_eq!(resolve("t", m(true, false, true), true), None);
    }
}
