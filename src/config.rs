//! User configuration, read from `~/.config/omarepos/config.toml`.
//!
//! Every field is optional; a missing file just means defaults.

use std::path::{Path, PathBuf};

use serde::Deserialize;

pub fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

/// Expand a leading `~` and `$HOME`, leaving everything else alone.
pub fn expand(raw: &str) -> PathBuf {
    let raw = raw.trim();
    if raw == "~" {
        return home();
    }
    if let Some(rest) = raw.strip_prefix("~/") {
        return home().join(rest);
    }
    if let Some(rest) = raw.strip_prefix("$HOME/") {
        return home().join(rest);
    }
    PathBuf::from(raw)
}

/// Shorten a path back to `~/...` for display.
pub fn tildify(path: &Path) -> String {
    let home = home();
    match path.strip_prefix(&home) {
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Directories to scan for repositories.
    pub roots: Vec<String>,
    /// How many directory levels below each root to look for a `.git`.
    pub max_depth: usize,
    /// Terminal emulator used by "Open in terminal".
    pub terminal: Option<String>,
    /// Editor launched inside the terminal by "Open in editor".
    pub editor: Option<String>,
    /// Follow the active Omarchy theme and restyle when it changes.
    pub follow_omarchy_theme: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            roots: vec!["~/code".to_string()],
            max_depth: 3,
            terminal: None,
            editor: None,
            follow_omarchy_theme: true,
        }
    }
}

impl Config {
    pub fn path() -> PathBuf {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home().join(".config"));
        base.join("omarepos/config.toml")
    }

    /// Load the config file, falling back to defaults. A malformed file is
    /// reported on stderr rather than being fatal — the app is still usable.
    pub fn load() -> Self {
        let path = Self::path();
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        match toml::from_str::<Config>(&text) {
            Ok(mut cfg) => {
                if cfg.roots.is_empty() {
                    cfg.roots = Self::default().roots;
                }
                cfg
            }
            Err(err) => {
                eprintln!("omarepos: ignoring {}: {err}", path.display());
                Self::default()
            }
        }
    }

    pub fn roots(&self) -> Vec<PathBuf> {
        self.roots.iter().map(|r| expand(r)).collect()
    }

    /// Terminal command, honouring `$TERMINAL` before guessing.
    pub fn terminal_command(&self) -> String {
        if let Some(term) = &self.terminal {
            return term.clone();
        }
        if let Ok(term) = std::env::var("TERMINAL") {
            if !term.trim().is_empty() {
                return term;
            }
        }
        for candidate in ["alacritty", "ghostty", "kitty", "foot", "wezterm"] {
            if which(candidate) {
                return candidate.to_string();
            }
        }
        "xterm".to_string()
    }

    pub fn editor_command(&self) -> String {
        self.editor
            .clone()
            .or_else(|| std::env::var("EDITOR").ok().filter(|e| !e.trim().is_empty()))
            .unwrap_or_else(|| "nvim".to_string())
    }
}

fn which(bin: &str) -> bool {
    let Some(paths) = std::env::var_os("PATH") else { return false };
    std::env::split_paths(&paths).any(|dir| dir.join(bin).is_file())
}
