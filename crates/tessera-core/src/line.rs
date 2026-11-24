//! Tracking of the shell's current input line.
//!
//! The terminal does not own line editing (readline/zle do), so it can only
//! follow the line while every edit is visible: printable input and backspace.
//! Any cursor motion, completion or history recall invalidates the buffer until
//! the next prompt. Suggestions are only offered while the buffer is valid, so
//! ghost text can never disagree with what the shell will execute.

/// Maximum input line length we keep; longer lines disable tracking.
pub const MAX_LINE: usize = 4096;

#[derive(Debug, Clone, Default)]
pub struct InputLine {
    text: String,
    valid: bool,
    dismissed: bool,
}

impl InputLine {
    pub fn new() -> Self {
        Self {
            text: String::new(),
            valid: true,
            dismissed: false,
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn is_valid(&self) -> bool {
        self.valid
    }

    pub fn is_dismissed(&self) -> bool {
        self.dismissed
    }

    /// Printable text arrived from the keyboard.
    pub fn push(&mut self, text: &str) {
        if !self.valid || text.is_empty() {
            return;
        }
        if self.text.len() + text.len() > MAX_LINE {
            self.invalidate();
            return;
        }
        self.text.push_str(text);
        self.dismissed = false;
    }

    /// Backspace: remove one character, never half a code point.
    pub fn backspace(&mut self) {
        if !self.valid {
            return;
        }
        self.text.pop();
        self.dismissed = false;
    }

    /// Enter was sent: the next line starts empty.
    pub fn enter(&mut self) {
        self.text.clear();
        self.valid = true;
        self.dismissed = false;
    }

    /// Something invisible to us edited the line (arrows, history, completion,
    /// a full-screen program). Stop suggesting until the next prompt.
    pub fn invalidate(&mut self) {
        self.text.clear();
        self.valid = false;
        self.dismissed = false;
    }

    /// OSC 133 `A`: a fresh prompt is about to be drawn.
    pub fn prompt(&mut self) {
        self.text.clear();
        self.valid = true;
        self.dismissed = false;
    }

    /// OSC 133 `C`: the shell started executing a command line.
    pub fn command_started(&mut self) {
        self.text.clear();
        self.valid = false;
        self.dismissed = false;
    }

    /// Esc: hide the suggestion for this line without losing the buffer.
    pub fn dismiss(&mut self) {
        self.dismissed = true;
    }

    /// Accept `suggestion`: returns the bytes still to send, or `None` when the
    /// suggestion does not extend the current line (or tracking is off).
    pub fn accept(&mut self, suggestion: &str) -> Option<Vec<u8>> {
        if !self.valid || self.dismissed {
            return None;
        }
        let suffix = suggestion.strip_prefix(self.text.as_str())?;
        if suffix.is_empty() {
            return None;
        }
        self.text.push_str(suffix);
        Some(suffix.as_bytes().to_vec())
    }

    /// Accept only up to the next word boundary of the suggestion.
    pub fn accept_word(&mut self, suggestion: &str) -> Option<Vec<u8>> {
        if !self.valid || self.dismissed {
            return None;
        }
        let suffix = suggestion.strip_prefix(self.text.as_str())?;
        if suffix.is_empty() {
            return None;
        }
        let take = suffix
            .char_indices()
            .find(|(_, ch)| ch.is_whitespace() || *ch == '/' || *ch == '-')
            .map(|(i, _)| i)
            .unwrap_or(suffix.len());
        let bytes = suffix.as_bytes()[..take].to_vec();
        self.text.push_str(&suffix[..take]);
        Some(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracks_typing_and_backspace() {
        let mut line = InputLine::new();
        line.push("git ch");
        assert_eq!(line.text(), "git ch");
        line.backspace();
        assert_eq!(line.text(), "git c");
        line.enter();
        assert_eq!(line.text(), "");
        assert!(line.is_valid());
    }

    #[test]
    fn invalidated_line_never_accepts() {
        let mut line = InputLine::new();
        line.push("gi");
        line.invalidate();
        assert!(!line.is_valid());
        assert_eq!(line.accept("git status"), None);
        line.push("x");
        assert_eq!(line.text(), "");
        line.prompt();
        assert!(line.is_valid());
    }

    #[test]
    fn accept_sends_only_the_suffix() {
        let mut line = InputLine::new();
        line.push("git sta");
        assert_eq!(line.accept("git status"), Some(b"tus".to_vec()));
        assert_eq!(line.text(), "git status");
        assert_eq!(line.accept("git status"), None, "no progress, no bytes");
        assert_eq!(line.accept("make"), None, "not a prefix extension");
    }

    #[test]
    fn accept_word_stops_at_boundaries() {
        let mut line = InputLine::new();
        line.push("cargo b");
        assert_eq!(
            line.accept_word("cargo build --release"),
            Some(b"uild".to_vec())
        );
        assert_eq!(line.text(), "cargo build");
    }

    #[test]
    fn dismissal_lasts_until_next_line() {
        let mut line = InputLine::new();
        line.push("ls");
        line.dismiss();
        assert_eq!(line.accept("ls -la"), None);
        line.enter();
        line.push("ls");
        assert_eq!(line.accept("ls -la"), Some(b" -la".to_vec()));
    }

    #[test]
    fn command_started_turns_tracking_off() {
        let mut line = InputLine::new();
        line.push("sleep 10");
        line.command_started();
        assert!(!line.is_valid());
        assert_eq!(line.accept("sleep 100"), None);
    }

    #[test]
    fn backspace_is_utf8_aware() {
        let mut line = InputLine::new();
        line.push("héllo");
        line.backspace();
        assert_eq!(line.text(), "héll");
    }
}
