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


/// A configured scan root. Written either as a bare path, or as a table when
/// it needs options:
///
/// ```toml
/// roots = ["~/code", { path = "~/work/acme", nested = true, tag = "acme" }]
/// ```
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum RootSpec {
    Simple(String),
    Detailed(RootEntry),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RootEntry {
    pub path: String,
    /// Look inside repositories found here, rather than stopping at the first
    /// `.git`. Only the subdirectories the containing repository *ignores* are
    /// followed, which admits a workspace full of clones but not submodules.
    #[serde(default)]
    pub nested: bool,
    /// Tag applied to repositories discovered here, the first time each is seen.
    #[serde(default)]
    pub tag: Option<String>,
    /// Whether the containing repository itself appears in the grid. Defaults
    /// to false for a nested root, true otherwise.
    #[serde(default)]
    pub include_self: Option<bool>,
}

impl RootEntry {
    /// True when nothing but the path is set, so it can be written as a string.
    fn is_plain(&self) -> bool {
        !self.nested && self.tag.is_none() && self.include_self.is_none()
    }
}

impl RootSpec {
    pub fn path_str(&self) -> &str {
        match self {
            RootSpec::Simple(path) => path,
            RootSpec::Detailed(entry) => &entry.path,
        }
    }

    fn to_entry(&self) -> RootEntry {
        match self {
            RootSpec::Simple(path) => RootEntry {
                path: path.clone(),
                nested: false,
                tag: None,
                include_self: None,
            },
            RootSpec::Detailed(entry) => entry.clone(),
        }
    }

    /// Resolved form, with `~` expanded and the defaults filled in.
    pub fn resolve(&self) -> Root {
        let entry = self.to_entry();
        Root {
            path: expand(&entry.path),
            include_self: entry.include_self.unwrap_or(!entry.nested),
            nested: entry.nested,
            tag: entry.tag.filter(|t| !t.trim().is_empty()),
        }
    }

    fn to_toml(&self) -> String {
        match self {
            RootSpec::Simple(path) => format!("{path:?}"),
            RootSpec::Detailed(entry) => {
                let mut parts = vec![format!("path = {:?}", entry.path)];
                if entry.nested {
                    parts.push("nested = true".to_string());
                }
                if let Some(tag) = &entry.tag {
                    parts.push(format!("tag = {tag:?}"));
                }
                if let Some(include) = entry.include_self {
                    parts.push(format!("include_self = {include}"));
                }
                format!("{{ {} }}", parts.join(", "))
            }
        }
    }
}

/// A scan root with everything resolved, as discovery wants it.
#[derive(Debug, Clone)]
pub struct Root {
    pub path: PathBuf,
    pub nested: bool,
    pub tag: Option<String>,
    pub include_self: bool,
}


/// Background refresh. Off unless `every_minutes` is set, and even then it
/// only re-reads local state until `fetch` is turned on as well.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Refresh {
    /// Minutes between refreshes. Zero means never.
    pub every_minutes: u64,
    /// Run `git fetch` in each repository, updating what the remote has.
    /// Touches the network but never the working tree.
    pub fetch: bool,
    /// Fast-forward the checked-out branch when it is clean and strictly
    /// behind its upstream. Never merges, never rebases, never stashes.
    pub fast_forward: bool,
    /// Limit the refresh to repositories carrying one of these tags. Empty
    /// means every repository in the grid.
    pub only_tags: Vec<String>,
    /// How many repositories to work on at once.
    pub max_parallel: usize,
    /// Seconds before a git invocation is given up on.
    pub timeout_seconds: u64,
}

impl Default for Refresh {
    fn default() -> Self {
        Self {
            every_minutes: 0,
            fetch: false,
            fast_forward: false,
            only_tags: Vec::new(),
            max_parallel: 4,
            timeout_seconds: 60,
        }
    }
}

impl Refresh {
    pub fn enabled(&self) -> bool {
        self.every_minutes > 0
    }

    /// Fast-forwarding without fetching would only ever act on a stale remote
    /// ref, so it implies a fetch.
    pub fn will_fetch(&self) -> bool {
        self.fetch || self.fast_forward
    }

    pub fn interval(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.every_minutes.clamp(1, 24 * 60) * 60)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Directories to scan for repositories.
    pub roots: Vec<RootSpec>,
    /// Individual repositories to include regardless of where they live.
    pub extra_repos: Vec<String>,
    /// How many directory levels below each root to look for a `.git`.
    pub max_depth: usize,
    /// Terminal emulator used by "Open in terminal".
    pub terminal: Option<String>,
    /// Editor launched inside the terminal by "Open in editor".
    pub editor: Option<String>,
    /// Command run inside the terminal by "Open in terminal", before the shell
    /// takes over. Unset means lazygit when it is installed; an empty string
    /// means a plain shell.
    pub terminal_runs: Option<String>,
    /// Follow the active Omarchy theme and restyle when it changes.
    pub follow_omarchy_theme: bool,
    /// Background refresh settings.
    pub refresh: Refresh,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            roots: vec![RootSpec::Simple("~/code".to_string())],
            extra_repos: Vec::new(),
            max_depth: 3,
            terminal: None,
            editor: None,
            terminal_runs: None,
            follow_omarchy_theme: true,
            refresh: Refresh::default(),
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

    pub fn roots(&self) -> Vec<Root> {
        self.roots.iter().map(RootSpec::resolve).collect()
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
        if self.roots.iter().any(|r| expand(r.path_str()) == path) {
            return false;
        }
        self.roots.push(RootSpec::Simple(Self::store(path)));
        true
    }

    pub fn remove_root(&mut self, path: &Path) {
        self.roots.retain(|r| expand(r.path_str()) != path);
    }

    /// Edit one root's options, collapsing it back to a bare string when the
    /// edit leaves nothing but the path.
    pub fn update_root(&mut self, path: &Path, mutate: impl FnOnce(&mut RootEntry)) {
        for spec in self.roots.iter_mut() {
            if expand(spec.path_str()) != path {
                continue;
            }
            let mut entry = spec.to_entry();
            mutate(&mut entry);
            if let Some(tag) = &entry.tag {
                if tag.trim().is_empty() {
                    entry.tag = None;
                }
            }
            *spec = if entry.is_plain() {
                RootSpec::Simple(entry.path)
            } else {
                RootSpec::Detailed(entry)
            };
            return;
        }
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
        out.push_str("# Directories scanned for repositories. A root can be a bare path, or a\n");
        out.push_str("# table: { path = \"...\", nested = true, tag = \"...\", include_self = false }.\n");
        out.push_str("# `nested` looks inside a repository for the clones it ignores.\n");
        out.push_str("roots = [\n");
        for spec in &self.roots {
            out.push_str(&format!("    {},\n", spec.to_toml()));
        }
        out.push_str("]\n\n");
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
        match &self.terminal_runs {
            Some(cmd) => out.push_str(&format!("terminal_runs = {cmd:?}\n")),
            None => out.push_str(
                "# terminal_runs = \"lazygit\"  # run in the terminal; \"\" for a plain shell\n",
            ),
        }
        out.push_str(&format!(
            "\nfollow_omarchy_theme = {}\n",
            self.follow_omarchy_theme
        ));

        // A table has to come after every bare key, so this stays last.
        let refresh = &self.refresh;
        out.push_str("\n# Background refresh. `every_minutes = 0` turns it off.\n");
        out.push_str("[refresh]\n");
        out.push_str(&format!("every_minutes = {}\n", refresh.every_minutes));
        out.push_str(&format!("fetch = {}\n", refresh.fetch));
        out.push_str(&format!("fast_forward = {}\n", refresh.fast_forward));
        out.push_str(&format!("only_tags = [{}]\n", list(&refresh.only_tags)));
        out.push_str(&format!("max_parallel = {}\n", refresh.max_parallel));
        out.push_str(&format!("timeout_seconds = {}\n", refresh.timeout_seconds));

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

    /// What to run inside the terminal before handing over to the shell.
    /// Defaults to lazygit where it exists, since a terminal opened on a
    /// repository is nearly always opened to do something with git.
    pub fn terminal_runs_command(&self) -> Option<String> {
        match &self.terminal_runs {
            Some(cmd) if cmd.trim().is_empty() => None,
            Some(cmd) => Some(cmd.trim().to_string()),
            None => which("lazygit").then(|| "lazygit".to_string()),
        }
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
