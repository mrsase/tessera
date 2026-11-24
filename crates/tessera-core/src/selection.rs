//! Text selection over a terminal grid, in absolute line coordinates.
//!
//! Lines are indexed across scrollback + screen (`Term::line`), so a selection
//! survives scrolling. The UI converts pointer positions into [`Pos`] values.

use tessera_term::Term;

#[derive(Clone, Copy, PartialEq, Eq, Debug, PartialOrd, Ord)]
pub struct Pos {
    pub line: usize,
    pub col: usize,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Selection {
    pub anchor: Pos,
    pub focus: Pos,
}

impl Selection {
    pub fn new(anchor: Pos) -> Self {
        Self {
            anchor,
            focus: anchor,
        }
    }

    pub fn extend_to(&mut self, pos: Pos) {
        self.focus = pos;
    }

    /// Start and end, ordered so `start <= end` lexicographically.
    pub fn range(&self) -> (Pos, Pos) {
        if self.anchor <= self.focus {
            (self.anchor, self.focus)
        } else {
            (self.focus, self.anchor)
        }
    }

    pub fn is_empty(&self) -> bool {
        self.anchor == self.focus
    }

    pub fn contains(&self, pos: Pos) -> bool {
        let (start, end) = self.range();
        start <= pos && pos <= end
    }

    /// Selected text, trailing blanks trimmed per line. Line breaks are added
    /// between lines, except when the row is marked as wrapped by the emulator.
    pub fn text(&self, term: &Term) -> String {
        if self.is_empty() {
            return String::new();
        }
        let (start, end) = self.range();
        let mut out = String::new();
        let mut line = start.line;
        while line <= end.line {
            let Some(row) = term.line(line) else { break };
            let len = row.cells.len();
            if len == 0 {
                line += 1;
                continue;
            }
            let x0 = if line == start.line {
                start.col.min(len - 1)
            } else {
                0
            };
            let x1 = if line == end.line {
                end.col.min(len - 1)
            } else {
                len - 1
            };
            let mut text = String::with_capacity(x1 + 1 - x0);
            for cell in &row.cells[x0..=x1] {
                if !cell.cont {
                    text.push(cell.ch);
                }
            }
            while text.ends_with(' ') {
                text.pop();
            }
            out.push_str(&text);
            if line != end.line && !row.wrapped {
                out.push('\n');
            }
            line += 1;
        }
        while out.ends_with('\n') {
            out.pop();
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tessera_term::Term;

    fn term_with(lines: &[&str]) -> Term {
        let mut term = Term::new(20, 6);
        for line in lines {
            term.feed(line.as_bytes());
            term.feed(b"\r\n");
        }
        term
    }

    #[test]
    fn selects_within_one_line() {
        let term = term_with(&["hello world"]);
        let sel = Selection {
            anchor: Pos { line: 0, col: 0 },
            focus: Pos { line: 0, col: 5 },
        };
        assert_eq!(sel.text(&term), "hello");
    }

    #[test]
    fn selects_across_lines_and_trims_trailing_blanks() {
        let term = term_with(&["first", "second", "third"]);
        let sel = Selection {
            anchor: Pos { line: 0, col: 2 },
            focus: Pos { line: 2, col: 3 },
        };
        assert_eq!(sel.text(&term), "rst\nsecond\nthir");
    }

    #[test]
    fn reversed_selection_is_normalized() {
        let term = term_with(&["alpha"]);
        let sel = Selection {
            anchor: Pos { line: 0, col: 4 },
            focus: Pos { line: 0, col: 1 },
        };
        assert_eq!(sel.text(&term), "lpha");
        assert!(sel.contains(Pos { line: 0, col: 3 }));
        assert!(!sel.contains(Pos { line: 0, col: 0 }));
    }

    #[test]
    fn empty_selection_yields_nothing() {
        let term = term_with(&["x"]);
        let sel = Selection::new(Pos { line: 0, col: 0 });
        assert!(sel.is_empty());
        assert_eq!(sel.text(&term), "");
    }

    #[test]
    fn out_of_range_lines_are_skipped() {
        let term = term_with(&["one"]);
        let sel = Selection {
            anchor: Pos { line: 0, col: 0 },
            focus: Pos { line: 5, col: 2 },
        };
        assert_eq!(sel.text(&term), "one");
    }
}
