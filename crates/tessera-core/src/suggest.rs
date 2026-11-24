//! Command history indexing and suggestion ranking.
//!
//! Mirrors zsh-autosuggestions semantics: commands are ranked by match class
//! (prefix, then substring, then subsequence), frecency and a same-directory
//! bonus. History can be loaded from bash, zsh and the shell integration's
//! JSONL file, or recorded directly by the UI.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Maximum number of distinct commands retained.
const MAX_ENTRIES: usize = 20_000;
/// Age in days at which the recency term halves.
const RECENCY_SCALE_DAYS: f64 = 14.0;
/// Score added when a command was last used in the queried directory.
const CWD_BONUS: f64 = 2.0;
const SECONDS_PER_DAY: f64 = 86_400.0;

/// One ranked history suggestion.
#[derive(Debug, Clone, PartialEq)]
pub struct Suggestion {
    /// The full command line.
    pub cmd: String,
    /// Ranking score; higher is better.
    pub score: f64,
}

#[derive(Debug)]
struct Entry {
    cmd: String,
    count: u64,
    ts: u64,
    cwd: Option<PathBuf>,
}

/// In-memory command history with frequency, recency and directory metadata.
#[derive(Debug, Default)]
pub struct HistoryIndex {
    entries: Vec<Entry>,
    index: HashMap<String, usize>,
}

impl HistoryIndex {
    /// Creates an empty index.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of distinct commands recorded.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether no commands have been recorded.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Records one executed command.
    ///
    /// Surrounding whitespace is trimmed. Empty commands and commands
    /// containing control characters (including newlines and tabs) are
    /// ignored. Repeated commands increment a use counter, keep the newest
    /// timestamp and the most recently supplied directory.
    pub fn record(&mut self, cmd: &str, cwd: Option<&Path>, ts: u64) {
        let cmd = cmd.trim();
        if cmd.is_empty() || cmd.chars().any(|c| (c as u32) < 0x20) {
            return;
        }
        if let Some(&i) = self.index.get(cmd) {
            let entry = &mut self.entries[i];
            entry.count += 1;
            entry.ts = entry.ts.max(ts);
            if let Some(cwd) = cwd {
                entry.cwd = Some(cwd.to_path_buf());
            }
            return;
        }
        if self.entries.len() >= MAX_ENTRIES {
            self.evict_one();
        }
        self.index.insert(cmd.to_owned(), self.entries.len());
        self.entries.push(Entry {
            cmd: cmd.to_owned(),
            count: 1,
            ts,
            cwd: cwd.map(Path::to_path_buf),
        });
    }

    /// Drops the least valuable entry to stay under the cap.
    fn evict_one(&mut self) {
        let Some(victim) = self
            .entries
            .iter()
            .enumerate()
            .min_by_key(|(_, entry)| (entry.ts, entry.count))
            .map(|(i, _)| i)
        else {
            return;
        };
        let removed = self.entries.swap_remove(victim);
        self.index.remove(&removed.cmd);
        if victim < self.entries.len() {
            let moved = self.entries[victim].cmd.clone();
            self.index.insert(moved, victim);
        }
    }

    /// Loads one-command-per-line bash history.
    ///
    /// `HISTTIMEFORMAT` timestamps (`#1699999999`) and comment lines are
    /// skipped.
    pub fn load_bash(&mut self, text: &str) {
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            self.record(line, None, 0);
        }
    }

    /// Loads zsh history, understanding extended `: <ts>:<duration>;command`
    /// lines as well as plain commands.
    pub fn load_zsh(&mut self, text: &str) {
        for line in text.lines() {
            let line = line.trim();
            if let Some(rest) = line.strip_prefix(": ") {
                if let Some((meta, cmd)) = rest.split_once(';') {
                    if let Some((ts, _duration)) = meta.split_once(':') {
                        if let Ok(ts) = ts.trim().parse::<u64>() {
                            self.record(cmd, None, ts);
                            continue;
                        }
                    }
                }
            }
            self.record(line, None, 0);
        }
    }

    /// Loads the shell integration's JSONL history
    /// (`{"cmd":"...","cwd":"...","ts":123}`). Malformed lines are ignored.
    pub fn load_jsonl(&mut self, text: &str) {
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let Ok(entry) = serde_json::from_str::<JsonEntry>(line) else {
                continue;
            };
            self.record(&entry.cmd, entry.cwd.as_deref().map(Path::new), entry.ts);
        }
    }

    /// Returns up to `limit` suggestions for `prefix`, best first.
    ///
    /// Classes are case-sensitive prefix, case-insensitive prefix,
    /// case-insensitive substring, then case-insensitive subsequence. The
    /// prefix itself is never suggested and results are deduplicated by
    /// command.
    pub fn suggest(
        &self,
        prefix: &str,
        cwd: Option<&Path>,
        now: u64,
        limit: usize,
    ) -> Vec<Suggestion> {
        if limit == 0 {
            return Vec::new();
        }
        let empty_prefix = prefix.is_empty();
        let lower_prefix = prefix.to_lowercase();
        let mut scored = Vec::new();
        for entry in &self.entries {
            if !empty_prefix && entry.cmd == prefix {
                continue;
            }
            let class = if empty_prefix {
                0
            } else {
                match classify(&entry.cmd, prefix, &lower_prefix) {
                    Some(class) => class,
                    None => continue,
                }
            };
            let age_days = now.saturating_sub(entry.ts) as f64 / SECONDS_PER_DAY;
            let recency = 1.0 / (1.0 + age_days / RECENCY_SCALE_DAYS);
            let cwd_bonus = match (entry.cwd.as_deref(), cwd) {
                (Some(entry_cwd), Some(query_cwd)) if entry_cwd == query_cwd => CWD_BONUS,
                _ => 0.0,
            };
            let score = (10.0 - f64::from(class) * 2.0)
                + (1.0 + entry.count as f64).ln() * 2.0
                + recency
                + cwd_bonus;
            scored.push(Suggestion {
                cmd: entry.cmd.clone(),
                score,
            });
        }
        scored.sort_by(|a, b| b.score.total_cmp(&a.score).then_with(|| a.cmd.cmp(&b.cmd)));
        scored.truncate(limit);
        scored
    }

    /// Loads history from the standard shell history files under `HOME`.
    ///
    /// Missing or unreadable files are ignored. Order: bash, zsh, JSONL.
    pub fn from_default_sources() -> Self {
        let mut index = Self::new();
        let Some(home) = std::env::var_os("HOME")
            .map(PathBuf::from)
            .or_else(dirs::home_dir)
        else {
            return index;
        };
        if let Ok(text) = std::fs::read_to_string(home.join(".bash_history")) {
            index.load_bash(&text);
        }
        if let Ok(text) = std::fs::read_to_string(home.join(".zsh_history")) {
            index.load_zsh(&text);
        }
        if let Ok(text) = std::fs::read_to_string(home.join(".local/share/tessera/history.jsonl")) {
            index.load_jsonl(&text);
        }
        index
    }
}

#[derive(Debug, Deserialize)]
struct JsonEntry {
    cmd: String,
    #[serde(default)]
    cwd: Option<String>,
    ts: u64,
}

/// Match class for a command, or `None` when it does not match at all.
fn classify(cmd: &str, prefix: &str, lower_prefix: &str) -> Option<u8> {
    if cmd.starts_with(prefix) {
        return Some(0);
    }
    let lower_cmd = cmd.to_lowercase();
    if lower_cmd.starts_with(lower_prefix) {
        return Some(1);
    }
    if lower_cmd.contains(lower_prefix) {
        return Some(2);
    }
    is_subsequence(&lower_cmd, lower_prefix).then_some(3)
}

/// Whether every character of `needle` appears in `haystack` in order.
fn is_subsequence(haystack: &str, needle: &str) -> bool {
    let mut wanted = needle.chars();
    let mut next = wanted.next();
    for c in haystack.chars() {
        if Some(c) == next {
            next = wanted.next();
            if next.is_none() {
                return true;
            }
        }
    }
    next.is_none()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmds(index: &HistoryIndex, prefix: &str, cwd: Option<&Path>, now: u64) -> Vec<String> {
        index
            .suggest(prefix, cwd, now, 100)
            .into_iter()
            .map(|s| s.cmd)
            .collect()
    }

    #[test]
    fn default_is_empty() {
        let index = HistoryIndex::default();
        assert!(index.is_empty());
        assert_eq!(index.len(), 0);
        assert!(index.suggest("", None, 0, 10).is_empty());
    }

    #[test]
    fn prefix_beats_substring_beats_subsequence() {
        let mut index = HistoryIndex::new();
        index.record("git status", None, 1000);
        index.record("run git", None, 1000);
        index.record("go into", None, 1000);
        let found = cmds(&index, "git", None, 1000);
        assert_eq!(found, vec!["git status", "run git", "go into"]);
        let scores: Vec<f64> = index
            .suggest("git", None, 1000, 10)
            .iter()
            .map(|s| s.score)
            .collect();
        assert!(scores[0] > scores[1] && scores[1] > scores[2]);
    }

    #[test]
    fn case_insensitive_fallback_ordering() {
        let mut index = HistoryIndex::new();
        index.record("LS -la", None, 500);
        index.record("cat ls", None, 500);
        assert_eq!(cmds(&index, "ls", None, 500), vec!["LS -la", "cat ls"]);

        let mut index = HistoryIndex::new();
        index.record("LS -la", None, 500);
        index.record("ls -l", None, 500);
        // "LS -la" matches case-sensitively, "ls -l" only case-insensitively.
        assert_eq!(cmds(&index, "LS", None, 500), vec!["LS -la", "ls -l"]);
    }

    #[test]
    fn exact_duplicate_excluded() {
        let mut index = HistoryIndex::new();
        index.record("ls", None, 500);
        assert!(index.suggest("ls", None, 500, 10).is_empty());

        index.record("ls -l", None, 500);
        assert_eq!(cmds(&index, "ls", None, 500), vec!["ls -l"]);
    }

    #[test]
    fn counts_boost_score() {
        let mut index = HistoryIndex::new();
        index.record("make build", None, 1000);
        for _ in 0..4 {
            index.record("make test", None, 1000);
        }
        assert_eq!(index.len(), 2);
        let found = cmds(&index, "make", None, 1000);
        assert_eq!(found, vec!["make test", "make build"]);

        let scores = index.suggest("make", None, 1000, 2);
        assert!(
            (scores[0].score - scores[1].score - (5.0f64.ln() * 2.0 - 2.0f64.ln() * 2.0)).abs()
                < 1e-9
        );
    }

    #[test]
    fn same_cwd_bonus_reorders_near_ties() {
        let mut index = HistoryIndex::new();
        index.record("cargo run", Some(Path::new("/work/a")), 1000);
        for _ in 0..2 {
            index.record("cargo test", Some(Path::new("/work/b")), 1000);
        }
        // Count advantage alone would put "cargo test" first.
        assert_eq!(
            cmds(&index, "cargo", None, 1000),
            vec!["cargo test", "cargo run"]
        );
        assert_eq!(
            cmds(&index, "cargo", Some(Path::new("/work/a")), 1000),
            vec!["cargo run", "cargo test"]
        );
    }

    #[test]
    fn empty_prefix_returns_frecency_order() {
        let mut index = HistoryIndex::new();
        index.record("old command", None, 0);
        index.record("fresh command", None, 1_000_000);
        index.record("frequent command", None, 1_000_000);
        for _ in 0..3 {
            index.record("frequent command", None, 1_000_000);
        }
        let found = cmds(&index, "", None, 1_000_000);
        assert_eq!(found[0], "frequent command");
        assert_eq!(found[1], "fresh command");
        assert_eq!(found[2], "old command");
    }

    #[test]
    fn zsh_extended_parsing_with_timestamps() {
        let mut index = HistoryIndex::new();
        index.load_zsh(": 1700000000:42;git status\necho plain\n");
        assert_eq!(index.len(), 2);
        let suggestions = index.suggest("git", None, 1_700_000_000, 5);
        assert_eq!(suggestions.len(), 1);
        assert_eq!(suggestions[0].cmd, "git status");
        // base 10 + ln(2)*2 + full recency
        let expected = 10.0 + 2.0f64.ln() * 2.0 + 1.0;
        assert!((suggestions[0].score - expected).abs() < 1e-9);
        // The timestamp is used, so the recent command outranks the plain one.
        assert_eq!(
            cmds(&index, "", None, 1_700_000_000),
            vec!["git status", "echo plain"]
        );
    }

    #[test]
    fn bash_timestamp_lines_skipped() {
        let mut index = HistoryIndex::new();
        index.load_bash("#1699999999\n# a comment\necho hi\n\nls\n");
        assert_eq!(index.len(), 2);
        let found = cmds(&index, "", None, 0);
        assert!(!found.iter().any(|cmd| cmd.starts_with('#')));
        assert!(found.contains(&"echo hi".to_owned()));
    }

    #[test]
    fn malformed_jsonl_ignored() {
        let mut index = HistoryIndex::new();
        index.load_jsonl(
            r#"{"cmd":"ls","cwd":"/tmp","ts":5}
not json
{"cmd":1,"ts":5}
{"cmd":"pwd"}
{"cmd":"cd /tmp","cwd":"/tmp","ts":7}
"#,
        );
        assert_eq!(index.len(), 2);
        let found = index.suggest("cd", Some(Path::new("/tmp")), 7, 5);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].cmd, "cd /tmp");
        // base 10 + ln(2)*2 + full recency + cwd bonus
        let expected = 10.0 + 2.0f64.ln() * 2.0 + 1.0 + 2.0;
        assert!((found[0].score - expected).abs() < 1e-9);
    }

    #[test]
    fn control_and_multiline_commands_rejected() {
        let mut index = HistoryIndex::new();
        index.record("ls\nrm -rf /", None, 1);
        index.record("echo\thi", None, 1);
        index.record("\u{1b}[31m", None, 1);
        index.record("   ", None, 1);
        assert_eq!(index.len(), 0);
        index.record("  ls  ", None, 1);
        assert_eq!(index.len(), 1);
        assert_eq!(cmds(&index, "", None, 1), vec!["ls"]);
    }

    #[test]
    fn record_dedupes_and_keeps_newest_metadata() {
        let mut index = HistoryIndex::new();
        index.record("x", Some(Path::new("/a")), 5);
        index.record("x", None, 10);
        assert_eq!(index.len(), 1);
        let suggestions = index.suggest("", Some(Path::new("/a")), 10, 1);
        // base 10 + ln(3)*2 + full recency (ts stayed 10) + cwd bonus
        let expected = 10.0 + 3.0f64.ln() * 2.0 + 1.0 + 2.0;
        assert!((suggestions[0].score - expected).abs() < 1e-9);
    }

    #[test]
    fn limit_respected() {
        let mut index = HistoryIndex::new();
        for cmd in ["alpha", "beta", "gamma", "delta", "epsilon"] {
            index.record(cmd, None, 1);
        }
        assert_eq!(index.suggest("", None, 1, 2).len(), 2);
        assert_eq!(index.suggest("a", None, 1, 1).len(), 1);
        assert!(index.suggest("", None, 1, 0).is_empty());
    }

    #[test]
    fn deterministic_ties_break_lexicographically() {
        let mut index = HistoryIndex::new();
        index.record("beta", None, 100);
        index.record("alpha", None, 100);
        assert_eq!(cmds(&index, "", None, 100), vec!["alpha", "beta"]);
    }

    #[test]
    fn subsequence_matching_is_ordered() {
        let mut index = HistoryIndex::new();
        index.record("go into", None, 100);
        index.record("tgi", None, 100);
        // "git" is a subsequence of "go into" but not of "tgi".
        assert_eq!(cmds(&index, "git", None, 100), vec!["go into"]);
    }
}
