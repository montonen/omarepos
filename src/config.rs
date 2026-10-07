//! User configuration, read from `~/.config/omarepos/config.toml`.
//!
//! Every field is optional; a missing file just means defaults.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

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

/// What the last pass could not do for a repository.
///
/// The two are kept apart because they send the user to different places: a
/// fetch that failed is about the network or the credentials, and an update
/// git refused is about something in the working tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Trouble {
    /// The fetch failed. A bare string on disk, which is all this file held
    /// before refusals were recorded too.
    Fetch(String),
    /// Fetched, but git would not fast-forward.
    Update { refused: String },
}

/// Repositories the last pass could not fetch or could not update. Kept with
/// the refresh time, in the state directory: the failure outlives the session
/// that discovered it, and a card that cannot say why it is stuck is the thing
/// this exists to fix.
pub fn fetch_errors_path() -> PathBuf {
    last_refresh_path().with_file_name("fetch-errors.toml")
}

pub fn load_fetch_errors() -> std::collections::BTreeMap<String, Trouble> {
    std::fs::read_to_string(fetch_errors_path())
        .ok()
        .and_then(|text| toml::from_str(&text).ok())
        .unwrap_or_default()
}

pub fn save_fetch_errors(errors: &std::collections::BTreeMap<String, Trouble>) {
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
                eprintln!("omarepos: could not record fetch failures: {err}");
            }
        }
        Err(err) => eprintln!("omarepos: could not encode fetch failures: {err}"),
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
        eprintln!("omarepos: could not record which repositories have been seen: {err}");
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
        eprintln!("omarepos: could not record the refresh time: {err}");
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
            commit_detail: true,
            refresh: Refresh::default(),
            roots_override: None,
            depth_override: None,
            load_error: None,
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
        match toml::from_str::<Config>(&text) {
            Ok(mut cfg) => {
                if cfg.roots.is_empty() {
                    cfg.roots = Self::default().roots;
                }
                cfg
            }
            Err(err) => Self::unusable(&path, &err.to_string()),
        }
    }

    fn unusable(path: &Path, why: &str) -> Self {
        eprintln!("omarepos: ignoring {}: {why}", path.display());
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
        out.push_str(&format!("commit_detail = {}\n", self.commit_detail));

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

        replace_file(&path, &out)
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
        assert_eq!(cfg.root_labels(), vec!["~/code".to_string()]);
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
    fn troubles_of_both_kinds_survive_being_written_and_read_back() {
        let _guard = isolate();
        let mut troubles = BTreeMap::new();
        // A refusal sorts before a fetch failure here, which is the order a
        // TOML writer has to be careful with: a table ahead of a plain value.
        troubles.insert(
            "/a/held-back".to_string(),
            Trouble::Update { refused: "would be overwritten by merge: .env".to_string() },
        );
        troubles.insert(
            "/b/unreachable".to_string(),
            Trouble::Fetch("fatal: could not read from remote repository".to_string()),
        );
        save_fetch_errors(&troubles);
        assert_eq!(load_fetch_errors(), troubles, "{}", read(&fetch_errors_path()));

        save_fetch_errors(&BTreeMap::new());
        assert!(!fetch_errors_path().exists(), "nothing wrong leaves no file behind");
    }

    #[test]
    fn a_file_from_before_refusals_were_recorded_still_reads() {
        let _guard = isolate();
        write(&fetch_errors_path(), "\"/old/repo\" = \"timed out after 60s\"\n");
        let read_back = load_fetch_errors();
        assert_eq!(
            read_back.get("/old/repo"),
            Some(&Trouble::Fetch("timed out after 60s".to_string()))
        );
        save_fetch_errors(&BTreeMap::new());
    }
}
