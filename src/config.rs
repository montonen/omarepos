//! User configuration, read from `~/.config/omarepos/config.toml`.
//!
//! Every field is optional; a missing file just means defaults.

use std::cell::RefCell;
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
/// roots = ["~/Work", { path = "~/clients/acme", nested = true, tag = "acme" }]
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
    /// `.git`. The clones the containing repository *ignores* are found, at
    /// any depth; its own submodules are left out by path.
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
            RootSpec::Simple(path) => quote(path),
            RootSpec::Detailed(entry) => {
                let mut parts = vec![format!("path = {}", quote(&entry.path))];
                if entry.nested {
                    parts.push("nested = true".to_string());
                }
                if let Some(tag) = &entry.tag {
                    parts.push(format!("tag = {}", quote(tag)));
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
    /// Not a setting any more. Every file the Settings dialog wrote before
    /// the fast-forward was taken out has this key, and a key nobody knows
    /// fails the whole file: so it is still read, means nothing, and is not
    /// written back.
    #[serde(rename = "fast_forward")]
    _fast_forward: Option<bool>,
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
            _fast_forward: None,
            only_tags: Vec::new(),
            max_parallel: 4,
            timeout_seconds: 60,
        }
    }
}

/// A string as TOML writes one. The writer used Rust's `{:?}` for this, which
/// is TOML for plain text and not for the rest: it spells a combining accent,
/// a variation selector or a no-break space as `\u{301}`, and TOML reads
/// `\uXXXX` only. A directory named on a Mac, or a tag with a heart in it,
/// produced a file the next launch could not parse.
fn quote(text: &str) -> String {
    toml::Value::String(text.to_string()).to_string()
}

/// What reading the file again came to.
#[derive(Debug, PartialEq)]
pub enum Reread {
    /// The file is as it was last read or written.
    Unchanged,
    /// It had changed, and what it says is now in force.
    Loaded,
    /// It had changed into something that cannot be read. What was in force
    /// stays in force, and nothing is saved over the file until it can be.
    Broken(String),
}

/// One line saying why a file was not used: where the parser stopped, and
/// what it objected to. The parser's own message runs to five lines with a
/// drawing of the offending one, which is right for a terminal and wrong for a
/// banner.
pub fn why_unusable(path: &Path, why: &str) -> String {
    let mut lines = why.lines().map(str::trim).filter(|line| !line.is_empty());
    let first = lines.next().unwrap_or("could not be read");
    let said = match lines.last() {
        Some(last) => format!("{first}: {last}"),
        None => first.to_string(),
    };
    format!("{} — {said}", tildify(path))
}

/// The error for a save that must not happen.
pub fn refuse_to_replace(load_error: &Option<String>) -> std::io::Result<()> {
    match load_error {
        Some(why) => Err(std::io::Error::other(format!(
            "not saved, so as not to replace a file that could not be read ({why})"
        ))),
        None => Ok(()),
    }
}

/// Replace a file whole or not at all: write beside it, then rename over it.
/// Writing in place truncates first, so a full disk or a crash in the middle
/// leaves half a file — and for the settings and the tags there is no other
/// copy. The version being replaced is kept beside it as `<name>.bak`.
pub fn replace_file(path: &Path, body: &str) -> std::io::Result<()> {
    use std::io::Write;

    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let fresh = dir.join(format!(".{name}.{}.tmp", std::process::id()));
    let written = (|| {
        let mut file = std::fs::File::create(&fresh)?;
        file.write_all(body.as_bytes())?;
        file.sync_all()
    })();
    if let Err(err) = written {
        let _ = std::fs::remove_file(&fresh);
        return Err(err);
    }
    if path.exists() {
        let _ = std::fs::copy(path, dir.join(format!("{name}.bak")));
    }
    std::fs::rename(&fresh, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&fresh);
    })
}

/// Where the last refresh time is kept. Not in the config file: it is state,
/// not configuration, and rewriting a file the user edits by hand every half
/// hour is a good way to lose their comments.
pub fn last_refresh_path() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".local/state"))
        .join("omarepos/last-refresh")
}

pub fn last_refresh() -> Option<i64> {
    std::fs::read_to_string(last_refresh_path())
        .ok()
        .and_then(|text| text.trim().parse().ok())
}

/// Repositories the last fetch failed in, and what git said. Kept with the
/// refresh time, in the state directory: the failure outlives the session
/// that discovered it, and a card that cannot say why it is stale is the
/// thing this exists to fix.
pub fn fetch_errors_path() -> PathBuf {
    last_refresh_path().with_file_name("fetch-errors.toml")
}

pub fn load_fetch_errors() -> std::collections::BTreeMap<String, String> {
    let read: std::collections::BTreeMap<String, toml::Value> =
        std::fs::read_to_string(fetch_errors_path())
            .ok()
            .and_then(|text| toml::from_str(&text).ok())
            .unwrap_or_default();
    // A file from before the fast-forward was taken out can also hold what
    // an update was refused for, written as a table. Those were never fetch
    // failures, and are left behind here; read as text, one of them would
    // have failed the whole file and lost the failures with it.
    read.into_iter()
        .filter_map(|(path, said)| match said {
            toml::Value::String(said) => Some((path, said)),
            _ => None,
        })
        .collect()
}

pub fn save_fetch_errors(errors: &std::collections::BTreeMap<String, String>) {
    let path = fetch_errors_path();
    if errors.is_empty() {
        let _ = std::fs::remove_file(&path);
        return;
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match toml::to_string_pretty(errors) {
        Ok(body) => {
            if let Err(err) = std::fs::write(&path, body) {
                note!("omarepos: could not record fetch failures: {err}");
            }
        }
        Err(err) => note!("omarepos: could not encode fetch failures: {err}"),
    }
}

/// The repositories the user has seen, one path to a line.
///
/// A pass across the fleet runs git inside every repository in scope, and git
/// does what a repository's own config tells it: there are more keys that name
/// a command than any list of overrides covers. So a pass only enters
/// repositories on this list. A directory that turns up under a scan root —
/// an unpacked archive, a synced folder — gets a card like any other, and is
/// not fetched by anything unattended until the user has opened it or asked
/// for it by name.
pub fn known_repos_path() -> PathBuf {
    last_refresh_path().with_file_name("known-repos")
}

/// `None` when nothing has been recorded yet. That is the first run with this
/// list, when the repositories already on the grid are the ones the user has
/// been fetching all along, and all of them are taken as seen.
pub fn load_known_repos() -> Option<std::collections::BTreeSet<String>> {
    let text = std::fs::read_to_string(known_repos_path()).ok()?;
    Some(
        text.lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_string)
            .collect(),
    )
}

pub fn save_known_repos(known: &std::collections::BTreeSet<String>) {
    let body: String = known.iter().map(|path| format!("{path}\n")).collect();
    if let Err(err) = replace_file(&known_repos_path(), &body) {
        note!("omarepos: could not record which repositories have been seen: {err}");
    }
}

/// Split the repositories a pass would cover into those it may enter and the
/// number it must leave for the user to look at first.
pub fn only_known(
    scope: Vec<PathBuf>,
    known: Option<&std::collections::BTreeSet<String>>,
) -> (Vec<PathBuf>, usize) {
    let Some(known) = known else { return (scope, 0) };
    let total = scope.len();
    let seen: Vec<PathBuf> = scope
        .into_iter()
        .filter(|path| known.contains(&path.display().to_string()))
        .collect();
    let unseen = total - seen.len();
    (seen, unseen)
}

pub fn record_refresh(at: i64) {
    let path = last_refresh_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Err(err) = std::fs::write(&path, at.to_string()) {
        note!("omarepos: could not record the refresh time: {err}");
    }
}

impl Refresh {
    pub fn enabled(&self) -> bool {
        self.every_minutes > 0
    }

    /// Whether a pass fetches, or only reads local state again.
    pub fn will_fetch(&self) -> bool {
        self.fetch
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
    /// Keep the commit detail pane open below the graph. A graph read for its
    /// shape rather than its contents wants the full height.
    pub commit_detail: bool,
    /// Background refresh settings.
    pub refresh: Refresh,
    /// Roots given on the command line, for this run only. Used in place of
    /// `roots` and never written: `omarepos -r <dir>` is a look at somewhere
    /// else, not a change of configuration. They used to be assigned straight
    /// into `roots`, and the next save — hiding the detail pane was enough —
    /// made the one-off permanent.
    #[serde(skip)]
    pub roots_override: Option<Vec<RootSpec>>,
    /// `--depth`, for this run only. Likewise never written.
    #[serde(skip)]
    pub depth_override: Option<usize>,
    /// Why the file on disk could not be used, when it could not. While this
    /// is set the defaults are in force and `save` refuses: the file is the
    /// only copy of what the user wrote, and one mistyped key must not cost
    /// them the rest of it.
    #[serde(skip)]
    pub load_error: Option<String>,
    /// The file as it was last read or written: its text, or `None` for no
    /// file. `save` looks before it replaces. The file is offered as a way to
    /// configure the app, the app read it once at launch, and the next save of
    /// anything (hiding the detail pane was enough) wrote the launch-time
    /// copy back over whatever had been typed into it since.
    #[serde(skip)]
    on_disk: RefCell<Option<String>>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            // Where Omarchy puts work: its installer makes ~/Work, and its
            // shell keeps `try` under it. It was ~/code, one person's habit,
            // which a fresh Omarchy does not have.
            roots: vec![RootSpec::Simple("~/Work".to_string())],
            extra_repos: Vec::new(),
            max_depth: 3,
            terminal: None,
            editor: None,
            terminal_runs: None,
            follow_omarchy_theme: true,
            commit_detail: true,
            refresh: Refresh::default(),
            roots_override: None,
            depth_override: None,
            load_error: None,
            on_disk: RefCell::new(None),
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

    /// Load the config file. No file is the ordinary first run and means the
    /// defaults. A file that is there but cannot be read or parsed also gets
    /// the defaults — the app is still usable — but it is remembered in
    /// `load_error`, shown in the window, and never saved over.
    pub fn load() -> Self {
        let path = Self::path();
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Self::default(),
            Err(err) => return Self::unusable(&path, &err.to_string()),
        };
        let mut cfg = match toml::from_str::<Config>(&text) {
            Ok(cfg) => cfg.as_read(),
            Err(err) => Self::unusable(&path, &err.to_string()),
        };
        cfg.on_disk = RefCell::new(Some(text));
        cfg
    }

    /// What a file that parsed means, where it leaves something unsaid.
    fn as_read(mut self) -> Self {
        if self.roots.is_empty() {
            self.roots = Self::default().roots;
        }
        // A file from before the fast-forward was taken out may have it on
        // with `fetch` off, and that pair did fetch: the update needed it.
        // So it goes on fetching, which is the half of it that is left.
        if self.refresh._fast_forward == Some(true) {
            self.refresh.fetch = true;
        }
        // Every string here ends up in a label or an entry, and those take no
        // NUL. TOML will carry one (`"x\u0000y"`); it is dropped here, where
        // both a first read and a re-read pass.
        let clean = |text: &mut String| text.retain(|c| c != '\0');
        for text in [&mut self.terminal, &mut self.editor, &mut self.terminal_runs] {
            if let Some(text) = text {
                clean(text);
            }
        }
        self.extra_repos.iter_mut().for_each(clean);
        self.refresh.only_tags.iter_mut().for_each(clean);
        for root in &mut self.roots {
            match root {
                RootSpec::Simple(path) => clean(path),
                RootSpec::Detailed(entry) => {
                    clean(&mut entry.path);
                    if let Some(tag) = &mut entry.tag {
                        clean(tag);
                    }
                }
            }
        }
        self
    }

    /// Read the file again if it is not what was last read or written. The
    /// command line's roots and depth stay as they are: they were never the
    /// file's.
    pub fn reread(&mut self) -> Reread {
        let path = Self::path();
        let disk = std::fs::read_to_string(&path).ok();
        if disk == *self.on_disk.borrow() {
            return Reread::Unchanged;
        }
        let parsed = match &disk {
            // Deleted: the defaults, as on a first run.
            None => Ok(Self::default()),
            Some(text) => toml::from_str::<Config>(text).map_err(|err| err.to_string()),
        };
        match parsed {
            Ok(fresh) => {
                let mut fresh = fresh.as_read();
                fresh.roots_override = self.roots_override.take();
                fresh.depth_override = self.depth_override;
                fresh.on_disk = RefCell::new(disk);
                *self = fresh;
                Reread::Loaded
            }
            Err(why) => {
                // Half-edited, most likely. Falling back to the defaults here
                // would empty the grid under the person typing.
                let why = why_unusable(&path, &why);
                self.load_error = Some(why.clone());
                *self.on_disk.borrow_mut() = disk;
                Reread::Broken(why)
            }
        }
    }

    fn unusable(path: &Path, why: &str) -> Self {
        note!("omarepos: ignoring {}: {why}", path.display());
        Self { load_error: Some(why_unusable(path, why)), ..Self::default() }
    }

    /// The roots in force: the command line's for this run if it gave any,
    /// otherwise the configured ones.
    pub fn roots(&self) -> Vec<Root> {
        self.root_specs().iter().map(RootSpec::resolve).collect()
    }

    fn root_specs(&self) -> &[RootSpec] {
        self.roots_override.as_deref().unwrap_or(&self.roots)
    }

    /// The roots in force, as the user wrote them.
    pub fn root_labels(&self) -> Vec<String> {
        self.root_specs().iter().map(|r| r.path_str().trim().to_string()).collect()
    }

    /// The search depth in force.
    pub fn depth(&self) -> usize {
        self.depth_override.unwrap_or(self.max_depth).max(1)
    }

    /// A run that is looking somewhere other than the configuration says.
    /// Such a run keeps its hands off everything shared: it seeds no tags and
    /// runs no background refresh.
    pub fn one_off(&self) -> bool {
        self.roots_override.is_some() || self.depth_override.is_some()
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
    /// generated one documents itself instead. Refuses when the file on disk
    /// could not be read at launch: what is in memory then is the defaults,
    /// and writing them would replace the only copy of the user's settings.
    pub fn save(&self) -> std::io::Result<()> {
        let path = Self::path();
        refuse_to_replace(&self.load_error)?;
        if std::fs::read_to_string(&path).ok() != *self.on_disk.borrow() {
            return Err(std::io::Error::other(format!(
                "not saved: {} has been changed outside omarepos since it was read. \
                 Rescan (Ctrl+R) to read it again, then make the change once more.",
                tildify(&path)
            )));
        }
        let list = |values: &[String]| -> String {
            values
                .iter()
                .map(|v| quote(v))
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
            Some(term) => out.push_str(&format!("terminal = {}\n", quote(term))),
            None => out.push_str("# terminal = \"alacritty\"   # defaults to $TERMINAL\n"),
        }
        match &self.editor {
            Some(editor) => out.push_str(&format!("editor = {}\n", quote(editor))),
            None => out.push_str("# editor = \"nvim\"          # defaults to $EDITOR\n"),
        }
        match &self.terminal_runs {
            Some(cmd) => out.push_str(&format!("terminal_runs = {}\n", quote(cmd))),
            None => out.push_str(
                "# terminal_runs = \"lazygit\"  # run in the terminal; \"\" for a plain shell\n",
            ),
        }
        out.push_str(&format!(
            "\nfollow_omarchy_theme = {}\n",
            self.follow_omarchy_theme
        ));
        out.push_str(&format!("commit_detail = {}\n", self.commit_detail));

        // A table has to come after every bare key, so this stays last.
        let refresh = &self.refresh;
        out.push_str("\n# Background refresh. `every_minutes = 0` turns it off.\n");
        out.push_str("[refresh]\n");
        out.push_str(&format!("every_minutes = {}\n", refresh.every_minutes));
        out.push_str(&format!("fetch = {}\n", refresh.fetch));
        out.push_str(&format!("only_tags = [{}]\n", list(&refresh.only_tags)));
        out.push_str(&format!("max_parallel = {}\n", refresh.max_parallel));
        out.push_str(&format!("timeout_seconds = {}\n", refresh.timeout_seconds));

        replace_file(&path, &out)?;
        *self.on_disk.borrow_mut() = Some(out);
        Ok(())
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
        for candidate in [
            "alacritty",
            "ghostty",
            "kitty",
            "foot",
            "wezterm",
            "gnome-terminal",
            "ptyxis",
            "konsole",
        ] {
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::testkit::*;

    const WRITTEN_BY_HAND: &str = "roots = [\"~/src\", { path = \"~/Work/client\", nested = true, tag = \"client\" }]\nmax_depth = 4\n";

    #[test]
    fn no_file_is_the_first_run_and_means_the_defaults() {
        let _guard = isolate();
        let _ = std::fs::remove_file(Config::path());
        let cfg = Config::load();
        assert_eq!(cfg.load_error, None);
        assert_eq!(cfg.root_labels(), vec!["~/Work".to_string()]);
        assert!(!cfg.one_off());
    }

    /// One mistyped key, in a file the README invites editing by hand.
    #[test]
    fn a_file_that_does_not_parse_is_remembered_and_never_saved_over() {
        let _guard = isolate();
        let written = WRITTEN_BY_HAND.replace("max_depth", "max_dpeth");
        write(&Config::path(), &written);

        let mut cfg = Config::load();
        let why = cfg.load_error.clone().expect("the failure is remembered");
        assert!(why.contains("config.toml"), "{why}");
        assert!(why.contains("max_dpeth"), "it should say what the parser objected to: {why}");
        assert!(!why.contains('\n'), "one line, for a banner: {why:?}");

        // Hiding the detail pane is all it used to take.
        cfg.commit_detail = false;
        assert!(cfg.save().is_err());
        assert_eq!(read(&Config::path()), written);

        let _ = std::fs::remove_file(Config::path());
    }

    #[test]
    fn what_the_command_line_says_is_used_for_the_run_and_never_written() {
        let _guard = isolate();
        write(&Config::path(), WRITTEN_BY_HAND);
        let mut cfg = Config::load();
        assert_eq!(cfg.load_error, None);

        // omarepos --root /tmp/one-off --depth 1
        cfg.roots_override = Some(vec![RootSpec::Simple("/tmp/one-off".to_string())]);
        cfg.depth_override = Some(1);
        assert!(cfg.one_off());
        assert_eq!(cfg.root_labels(), vec!["/tmp/one-off".to_string()]);
        assert_eq!(cfg.depth(), 1);

        // ...and then any save at all.
        cfg.commit_detail = false;
        cfg.save().unwrap();

        let back = Config::load();
        assert!(!back.one_off());
        assert!(!back.commit_detail, "the edit that was meant is kept");
        assert_eq!(back.root_labels(), vec!["~/src".to_string(), "~/Work/client".to_string()]);
        assert_eq!(back.depth(), 4);
        let client = &back.roots()[1];
        assert!(client.nested && client.tag.as_deref() == Some("client"), "{client:?}");

        let _ = std::fs::remove_file(Config::path());
    }

    /// Every string the app writes, with the characters `{:?}` got wrong: a
    /// variation selector, a zero-width joiner, a combining accent as a Mac
    /// writes one, a no-break space, a soft hyphen, and the control characters.
    #[test]
    fn what_save_writes_load_can_read() {
        let _guard = isolate();
        let path = Config::path();
        let _ = std::fs::remove_file(&path);
        let awkward = [
            "\u{2764}\u{fe0f} favourites",
            "\u{1f468}\u{200d}\u{1f4bb}",
            "Cafe\u{301}",
            "a\u{a0}b",
            "soft\u{ad}hyphen",
            // No NUL here: one is dropped on the way in, by design, and has a
            // test of its own.
            "esc\u{1b}del\u{7f}",
            "quote \" and \\ backslash and 'single'",
            "two\nlines\tand a tab",
        ];
        for text in awkward {
            let mut cfg = Config::default();
            cfg.roots = vec![
                RootSpec::Simple(format!("~/{text}")),
                RootSpec::Detailed(RootEntry {
                    path: format!("~/nested/{text}"),
                    nested: true,
                    tag: Some(text.to_string()),
                    include_self: Some(false),
                }),
            ];
            cfg.extra_repos = vec![format!("/srv/{text}")];
            cfg.terminal = Some(text.to_string());
            cfg.editor = Some(text.to_string());
            cfg.terminal_runs = Some(text.to_string());
            cfg.refresh.only_tags = vec![text.to_string()];
            cfg.save().unwrap();

            let back = Config::load();
            assert_eq!(back.load_error, None, "{text:?} wrote a file that does not parse");
            let roots = back.roots();
            assert_eq!(roots.len(), 2, "{text:?}");
            assert_eq!(roots[1].tag.as_deref(), Some(text), "{text:?}");
            assert!(roots[1].nested && !roots[1].include_self, "{text:?}");
            assert_eq!(back.extra_repos, vec![format!("/srv/{text}")]);
            assert_eq!(back.terminal.as_deref(), Some(text));
            assert_eq!(back.editor.as_deref(), Some(text));
            assert_eq!(back.terminal_runs.as_deref(), Some(text));
            assert_eq!(back.refresh.only_tags, vec![text.to_string()]);
            let _ = std::fs::remove_file(&path);
        }
    }

    #[test]
    fn an_edit_made_outside_is_read_again_and_never_saved_over() {
        let _guard = isolate();
        let path = Config::path();
        write(&path, WRITTEN_BY_HAND);
        let mut cfg = Config::load();
        assert_eq!(cfg.reread(), Reread::Unchanged);

        // In an editor, with the window open.
        let edited = WRITTEN_BY_HAND.replace("max_depth = 4", "max_depth = 6");
        write(&path, &edited);

        // Any save used to put the launch-time copy back.
        cfg.commit_detail = false;
        let refused = cfg.save().expect_err("the file is not what was read");
        assert!(refused.to_string().contains("changed outside"), "{refused}");
        assert_eq!(read(&path), edited);

        assert_eq!(cfg.reread(), Reread::Loaded);
        assert_eq!(cfg.depth(), 6);
        assert!(cfg.commit_detail, "what the file says, not what was in memory");
        cfg.commit_detail = false;
        cfg.save().unwrap();
        assert_eq!(cfg.reread(), Reread::Unchanged, "its own save is not an edit from outside");
        assert_eq!(Config::load().depth(), 6);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_half_edited_file_leaves_what_was_in_force_and_is_not_saved_over() {
        let _guard = isolate();
        let path = Config::path();
        write(&path, WRITTEN_BY_HAND);
        let mut cfg = Config::load();
        cfg.roots_override = Some(vec![RootSpec::Simple("/tmp/one-off".to_string())]);

        let broken = WRITTEN_BY_HAND.replace("max_depth = 4", "max_depth = ");
        write(&path, &broken);
        let Reread::Broken(why) = cfg.reread() else { panic!("it does not parse") };
        assert!(why.contains("config.toml"), "{why}");
        assert_eq!(cfg.max_depth, 4, "not the defaults");
        assert_eq!(cfg.roots.len(), 2, "not the defaults");
        assert!(cfg.save().is_err());
        assert_eq!(read(&path), broken);
        // Said once, not on every rescan until it is mended.
        assert_eq!(cfg.reread(), Reread::Unchanged);

        write(&path, &WRITTEN_BY_HAND.replace("max_depth = 4", "max_depth = 5"));
        assert_eq!(cfg.reread(), Reread::Loaded);
        assert_eq!(cfg.load_error, None);
        assert_eq!(cfg.max_depth, 5);
        assert!(cfg.one_off(), "the command line's roots are kept across a re-read");

        let _ = std::fs::remove_file(&path);
    }

    /// TOML will carry a NUL in a string, and nothing that shows text takes
    /// one.
    #[test]
    fn a_nul_written_into_the_file_does_not_reach_a_label() {
        let _guard = isolate();
        let path = Config::path();
        write(
            &path,
            "roots = [\"~/co\\u0000de\", { path = \"~/work\\u0000\", tag = \"a\\u0000b\" }]\n\
             extra_repos = [\"/srv/x\\u0000y\"]\nterminal = \"foot\\u0000\"\n\
             [refresh]\nonly_tags = [\"w\\u0000ork\"]\n",
        );
        let cfg = Config::load();
        assert_eq!(cfg.load_error, None);
        assert_eq!(cfg.root_labels(), ["~/code", "~/work"]);
        assert_eq!(cfg.roots()[1].tag.as_deref(), Some("ab"));
        assert_eq!(cfg.extra_repos, ["/srv/xy"]);
        assert_eq!(cfg.terminal.as_deref(), Some("foot"));
        assert_eq!(cfg.refresh.only_tags, ["work"]);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_save_replaces_the_file_whole_and_keeps_the_one_before() {
        let _guard = isolate();
        let path = Config::path();
        let _ = std::fs::remove_file(&path);

        let mut cfg = Config::default();
        cfg.save().unwrap();
        let first = read(&path);
        cfg.max_depth = 7;
        cfg.save().unwrap();

        assert!(read(&path).contains("max_depth = 7"));
        assert_eq!(read(&path.with_file_name("config.toml.bak")), first);
        let strays: Vec<String> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .filter(|name| name.ends_with(".tmp"))
            .collect();
        assert!(strays.is_empty(), "half-written files left behind: {strays:?}");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_pass_enters_only_repositories_that_have_been_seen() {
        let _guard = isolate();
        let _ = std::fs::remove_file(known_repos_path());
        let scope = || vec![PathBuf::from("/code/app"), PathBuf::from("/code/unpacked-archive")];

        // Nothing recorded yet: the first run, where everything already on
        // the grid is taken as seen.
        assert_eq!(load_known_repos(), None);
        assert_eq!(only_known(scope(), None), (scope(), 0));

        let mut known = std::collections::BTreeSet::new();
        known.insert("/code/app".to_string());
        save_known_repos(&known);
        let known = load_known_repos().expect("recorded");

        let (enter, left) = only_known(scope(), Some(&known));
        assert_eq!(enter, vec![PathBuf::from("/code/app")]);
        assert_eq!(left, 1, "the directory that turned up since is left for the user");

        let _ = std::fs::remove_file(known_repos_path());
    }

    #[test]
    fn fetch_failures_survive_being_written_and_read_back() {
        let _guard = isolate();
        let mut failures = BTreeMap::new();
        failures.insert(
            "/b/unreachable".to_string(),
            "fatal: could not read from remote repository".to_string(),
        );
        save_fetch_errors(&failures);
        assert_eq!(load_fetch_errors(), failures, "{}", read(&fetch_errors_path()));

        save_fetch_errors(&BTreeMap::new());
        assert!(!fetch_errors_path().exists(), "nothing wrong leaves no file behind");
    }

    /// The file as a version that fast-forwarded left it: a fetch failure, a
    /// plain string, and beside it what an update was refused for, a table.
    #[test]
    fn a_file_that_also_holds_refused_updates_keeps_its_fetch_failures() {
        let _guard = isolate();
        write(
            &fetch_errors_path(),
            "\"/old/repo\" = \"timed out after 60s\"\n\n\
             [\"/a/held-back\"]\nrefused = \"would be overwritten by merge: .env\"\n",
        );
        let read_back = load_fetch_errors();
        assert_eq!(read_back.get("/old/repo").map(String::as_str), Some("timed out after 60s"));
        assert_eq!(read_back.len(), 1, "a refused update is not a fetch failure: {read_back:?}");
        save_fetch_errors(&BTreeMap::new());
    }

    /// Every config file Settings wrote before the fast-forward was taken out
    /// has the key. The file still loads, the key means nothing, and it is
    /// gone from the file the next time it is saved.
    #[test]
    fn a_config_that_still_says_fast_forward_loads_and_loses_the_key_when_saved() {
        let _guard = isolate();
        let path = Config::path();
        write(
            &path,
            "roots = [\"~/Work\"]\n\n[refresh]\nevery_minutes = 15\nfetch = false\n\
             fast_forward = true\nonly_tags = []\nmax_parallel = 4\ntimeout_seconds = 60\n",
        );
        let cfg = Config::load();
        assert_eq!(cfg.load_error, None);
        assert_eq!(cfg.refresh.every_minutes, 15);
        assert!(cfg.refresh.fetch, "it fetched before, to update, and goes on fetching");

        cfg.save().unwrap();
        let saved = read(&path);
        assert!(!saved.contains("fast_forward"), "{saved}");
        assert!(saved.contains("fetch = true"), "{saved}");
    }
}
