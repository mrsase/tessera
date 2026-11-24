//! User configuration: `$XDG_CONFIG_HOME/tessera/config.toml`.
//!
//! Every field has a default; an absent file is not an error, and a malformed
//! file yields a message plus defaults so the UI can still start.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Backend {
    /// tmux when a server is reachable on this platform, native otherwise.
    #[default]
    Auto,
    Native,
    Tmux,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct Theme {
    pub foreground: String,
    pub background: String,
    pub cursor: String,
    pub selection: String,
    pub palette: Vec<String>,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            foreground: "#abb2bf".into(),
            background: "#282c34".into(),
            cursor: "#abb2bf".into(),
            selection: "#3e4451".into(),
            palette: DEFAULT_PALETTE.iter().map(|c| c.to_string()).collect(),
        }
    }
}

pub const DEFAULT_PALETTE: [&str; 16] = [
    "#282c34", "#e06c75", "#98c379", "#e5c07b", "#61afef", "#c678dd", "#56b6c2", "#abb2bf",
    "#5c6370", "#e06c75", "#98c379", "#e5c07b", "#61afef", "#c678dd", "#56b6c2", "#ffffff",
];

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct Config {
    /// Program to run; `None` means `$SHELL` (or `cmd.exe` on Windows).
    pub shell: Option<String>,
    pub backend: Backend,
    pub history_suggestions: bool,
    /// Lines kept per pane (native backend; tmux has its own).
    pub scrollback: usize,
    pub font_family: String,
    pub font_size: f32,
    pub theme: Theme,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            shell: None,
            backend: Backend::Auto,
            history_suggestions: true,
            scrollback: 5000,
            font_family: "DejaVu Sans Mono".into(),
            font_size: 14.0,
            theme: Theme::default(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot read {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid config: {0}")]
    Parse(#[from] toml::de::Error),
}

impl Config {
    /// `$XDG_CONFIG_HOME/tessera/config.toml`, else `~/.config/...`.
    pub fn path() -> Option<PathBuf> {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| !p.as_os_str().is_empty())
            .or_else(|| dirs::home_dir().map(|h| h.join(".config")))?;
        Some(base.join("tessera").join("config.toml"))
    }

    pub fn load_str(text: &str) -> Result<Self, ConfigError> {
        let mut config: Config = toml::from_str(text)?;
        config.normalize();
        Ok(config)
    }

    /// Load the user config. Missing files are fine; parse errors are returned
    /// together with defaults so startup never fails because of a typo.
    pub fn load() -> (Self, Option<ConfigError>) {
        let Some(path) = Self::path() else {
            return (Self::default(), None);
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => match Self::load_str(&text) {
                Ok(config) => (config, None),
                Err(err) => (Self::default(), Some(err)),
            },
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => (Self::default(), None),
            Err(err) => (Self::default(), Some(ConfigError::Io { path, source: err })),
        }
    }

    fn normalize(&mut self) {
        self.font_size = if self.font_size.is_finite() {
            self.font_size.clamp(6.0, 48.0)
        } else {
            14.0
        };
        self.scrollback = self.scrollback.clamp(100, 100_000);
        if self.font_family.trim().is_empty() {
            self.font_family = Self::default().font_family;
        }
        if self.theme.palette.len() < 16 {
            self.theme.palette = Theme::default().palette;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_valid() {
        let cfg = Config::default();
        assert_eq!(cfg.backend, Backend::Auto);
        assert_eq!(cfg.theme.palette.len(), 16);
    }

    #[test]
    fn partial_document_keeps_defaults() {
        let cfg = Config::load_str(
            r##"
            backend = "tmux"
            font_size = 16.5
            shell = "/bin/zsh"
            [theme]
            background = "#000000"
            "##,
        )
        .unwrap();
        assert_eq!(cfg.backend, Backend::Tmux);
        assert_eq!(cfg.font_size, 16.5);
        assert_eq!(cfg.shell.as_deref(), Some("/bin/zsh"));
        assert_eq!(cfg.theme.background, "#000000");
        assert_eq!(cfg.theme.foreground, Theme::default().foreground);
        assert_eq!(cfg.history_suggestions, true);
    }

    #[test]
    fn values_are_clamped_and_theme_completed() {
        let cfg = Config::load_str(
            r##"
            font_size = 500.0
            scrollback = 1
            font_family = "  "
            [theme]
            palette = ["#000000"]
            "##,
        )
        .unwrap();
        assert_eq!(cfg.font_size, 48.0);
        assert_eq!(cfg.scrollback, 100);
        assert_eq!(cfg.font_family, "DejaVu Sans Mono");
        assert_eq!(cfg.theme.palette.len(), 16);
    }

    #[test]
    fn unknown_backend_is_an_error() {
        let err = Config::load_str("backend = \"screen\"").unwrap_err();
        assert!(matches!(err, ConfigError::Parse(_)));
    }

    #[test]
    fn empty_document_is_default() {
        assert_eq!(Config::load_str("").unwrap(), Config::default());
    }
}
