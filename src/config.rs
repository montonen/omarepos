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
    /// Individual repositories to include regardless of where they live.
    pub extra_repos: Vec<String>,
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
            extra_repos: Vec::new(),
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

    pub fn extra_repos(&self) -> Vec<PathBuf> {
        self.extra_repos.iter().map(|r| expand(r)).collect()
    }

    /// Store a path the way the user is most likely to recognise it.
    fn store(path: &Path) -> String {
        tildify(path)
    }

    /// Returns false when the root was already listed.
    pub fn add_root(&mut self, path: &Path) -> bool {
        let candidate = Self::store(path);
        if self.roots.iter().any(|r| expand(r) == path) {
            return false;
        }
        self.roots.push(candidate);
        true
    }

    pub fn remove_root(&mut self, path: &Path) {
        self.roots.retain(|r| expand(r) != path);
    }

    pub fn add_extra_repo(&mut self, path: &Path) -> bool {
        let candidate = Self::store(path);
        if self.extra_repos.iter().any(|r| expand(r) == path) {
            return false;
        }
        self.extra_repos.push(candidate);
        true
    }

    pub fn remove_extra_repo(&mut self, path: &Path) {
        self.extra_repos.retain(|r| expand(r) != path);
    }

    /// Rewrite the config file. Comments in an existing file are lost; the
    /// generated one documents itself instead.
    pub fn save(&self) -> std::io::Result<()> {
        let path = Self::path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let list = |values: &[String]| -> String {
            values
                .iter()
                .map(|v| format!("{:?}", v))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let mut out = String::from("# omarepos configuration — written by the app.\n\n");
        out.push_str("# Directories scanned for repositories.\n");
        out.push_str(&format!("roots = [{}]\n\n", list(&self.roots)));
        out.push_str("# Individual repositories included wherever they live.\n");
        out.push_str(&format!("extra_repos = [{}]\n\n", list(&self.extra_repos)));
        out.push_str("# Levels below each root to search for a `.git`.\n");
        out.push_str(&format!("max_depth = {}\n\n", self.max_depth));
        match &self.terminal {
            Some(term) => out.push_str(&format!("terminal = {term:?}\n")),
            None => out.push_str("# terminal = \"alacritty\"   # defaults to $TERMINAL\n"),
        }
        match &self.editor {
            Some(editor) => out.push_str(&format!("editor = {editor:?}\n")),
            None => out.push_str("# editor = \"nvim\"          # defaults to $EDITOR\n"),
        }
        out.push_str(&format!(
            "\nfollow_omarchy_theme = {}\n",
            self.follow_omarchy_theme
        ));
        std::fs::write(&path, out)
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
