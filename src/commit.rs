//! What a single commit changed.
//!
//! Reading a diff is far more expensive than reading a commit header, and the
//! pane that uses this reloads every time the cursor moves, so this runs off
//! the main thread and can be abandoned.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use git2::{Diff, DiffFindOptions, DiffOptions, Oid, Patch, Repository};

/// A commit touching more files than this is telling you something other than
/// which files it touched.
const MAX_FILES: usize = 400;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    Added,
    Modified,
    Deleted,
    Renamed,
    Copied,
    TypeChange,
    Untracked,
    Other,
}

impl Change {
    /// The single letter git itself uses.
    pub fn letter(self) -> &'static str {
        match self {
            Change::Added => "A",
            Change::Modified => "M",
            Change::Deleted => "D",
            Change::Renamed => "R",
            Change::Copied => "C",
            Change::TypeChange => "T",
            // The same mark git's porcelain uses.
            Change::Untracked => "?",
            Change::Other => "·",
        }
    }

    pub fn css_class(self) -> &'static str {
        match self {
            Change::Added => "added",
            Change::Deleted => "deleted",
            Change::Renamed | Change::Copied => "renamed",
            Change::Untracked => "untracked",
            _ => "modified",
        }
    }

    fn from(delta: git2::Delta) -> Self {
        match delta {
            git2::Delta::Added => Change::Added,
            git2::Delta::Modified => Change::Modified,
            git2::Delta::Deleted => Change::Deleted,
            git2::Delta::Renamed => Change::Renamed,
            git2::Delta::Copied => Change::Copied,
            git2::Delta::Typechange => Change::TypeChange,
            git2::Delta::Untracked => Change::Untracked,
            _ => Change::Other,
        }
    }
}

/// Which part of a repository's state a file change belongs to. For a commit
/// there is only one; for the working tree the distinction is the point —
/// untracked files are the one kind that does not block a fast-forward.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    Committed,
    Staged,
    Unstaged,
    Conflicted,
    Untracked,
}

impl Group {
    pub fn heading(self) -> &'static str {
        match self {
            Group::Committed => "",
            Group::Staged => "Staged",
            Group::Unstaged => "Not staged",
            Group::Conflicted => "Conflicted",
            Group::Untracked => "Untracked",
        }
    }

    /// Whether changes of this kind stop the background refresh from
    /// fast-forwarding. Untracked files do not: git refuses rather than
    /// overwriting one.
    pub fn blocks_update(self) -> bool {
        matches!(self, Group::Staged | Group::Unstaged | Group::Conflicted)
    }
}

#[derive(Debug, Clone)]
pub struct FileChange {
    pub path: String,
    /// Where it came from, for a rename or copy.
    pub from: Option<String>,
    pub change: Change,
    pub insertions: usize,
    pub deletions: usize,
    pub binary: bool,
    pub group: Group,
}

/// What the pane is being asked to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Commit(String),
    /// Everything uncommitted, including untracked files.
    Working,
}

#[derive(Debug, Clone)]
pub struct Detail {
    pub id: String,
    pub short_id: String,
    pub summary: String,
    pub body: String,
    pub author: String,
    pub email: String,
    pub when: i64,
    /// Only kept when it differs from the author, which is the only time it
    /// tells anyone anything.
    pub committer: Option<String>,
    pub parents: Vec<String>,
    pub merge: bool,
    pub files: Vec<FileChange>,
    /// How many files the commit touched, before the cap above.
    pub total_files: usize,
    pub insertions: usize,
    pub deletions: usize,
}

/// A flag the caller can set to abandon a load it no longer wants.
#[derive(Clone, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub fn stop(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    fn stopped(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

pub fn spawn(
    repo: PathBuf,
    target: Target,
) -> (async_channel::Receiver<Result<Box<Detail>, String>>, Cancel) {
    let (tx, rx) = async_channel::bounded(1);
    let cancel = Cancel::default();
    {
        let cancel = cancel.clone();
        std::thread::spawn(move || {
            let result = match &target {
                Target::Commit(id) => load(&repo, id, &cancel),
                Target::Working => load_working(&repo, &cancel),
            };
            if !cancel.stopped() {
                let _ = tx.send_blocking(result.map(Box::new));
            }
        });
    }
    (rx, cancel)
}

fn load(path: &Path, id: &str, cancel: &Cancel) -> Result<Detail, String> {
    let repo = Repository::open(path).map_err(|e| e.message().to_string())?;
    let oid = Oid::from_str(id).map_err(|e| e.message().to_string())?;
    let commit = repo.find_commit(oid).map_err(|e| e.message().to_string())?;

    let author = commit.author();
    let author_name = author.name().unwrap_or("unknown").to_string();
    let committer = commit.committer();
    let committer_name = committer.name().unwrap_or("").to_string();

    let tree = commit.tree().map_err(|e| e.message().to_string())?;
    // A merge is diffed against its first parent: that shows what the merge
    // brought in, which is what someone looking at one means.
    let parent_tree = commit.parent(0).ok().and_then(|p| p.tree().ok());

    let mut opts = DiffOptions::new();
    opts.include_typechange(true).context_lines(0);
    let mut diff: Diff = repo
        .diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), Some(&mut opts))
        .map_err(|e| e.message().to_string())?;

    // Without this a rename reads as a delete plus an add, which is three
    // wrong facts where there was one right one.
    let mut finds = DiffFindOptions::new();
    finds.renames(true).copies(true);
    let _ = diff.find_similar(Some(&mut finds));

    let total_files = diff.deltas().len();
    let mut files = Vec::with_capacity(total_files.min(MAX_FILES));
    let mut insertions = 0usize;
    let mut deletions = 0usize;

    for (index, delta) in diff.deltas().enumerate() {
        if cancel.stopped() {
            return Err("cancelled".into());
        }
        let binary = delta.new_file().is_binary() || delta.old_file().is_binary();
        let (adds, dels) = if binary {
            (0, 0)
        } else {
            match Patch::from_diff(&diff, index) {
                Ok(Some(patch)) => {
                    let (_, adds, dels) = patch.line_stats().unwrap_or((0, 0, 0));
                    (adds, dels)
                }
                _ => (0, 0),
            }
        };
        insertions += adds;
        deletions += dels;

        if files.len() >= MAX_FILES {
            continue;
        }
        let path_of = |f: git2::DiffFile<'_>| {
            f.path()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default()
        };
        let new_path = path_of(delta.new_file());
        let old_path = path_of(delta.old_file());
        let change = Change::from(delta.status());
        files.push(FileChange {
            path: if new_path.is_empty() { old_path.clone() } else { new_path },
            from: (matches!(change, Change::Renamed | Change::Copied) && old_path != *"")
                .then_some(old_path),
            change,
            insertions: adds,
            deletions: dels,
            binary,
            group: Group::Committed,
        });
    }

    Ok(Detail {
        short_id: oid.to_string().chars().take(8).collect(),
        id: oid.to_string(),
        summary: commit
            .summary()
            .ok()
            .flatten()
            .unwrap_or("(no message)")
            .trim()
            .to_string(),
        body: commit
            .message()
            .unwrap_or("")
            .trim()
            .splitn(2, '\n')
            .nth(1)
            .unwrap_or("")
            .trim()
            .to_string(),
        email: author.email().unwrap_or("").to_string(),
        author: author_name.clone(),
        when: commit.time().seconds(),
        committer: (!committer_name.is_empty() && committer_name != author_name)
            .then_some(committer_name),
        parents: commit
            .parent_ids()
            .map(|p| p.to_string().chars().take(8).collect())
            .collect(),
        merge: commit.parent_count() > 1,
        files,
        total_files,
        insertions,
        deletions,
    })
}

/// One line of a unified diff.
#[derive(Debug, Clone)]
pub struct Line {
    /// git's own marker: ' ', '+', '-', or '@' for a hunk header.
    pub origin: char,
    pub text: String,
}

#[derive(Debug, Clone, Default)]
pub struct FileDiff {
    pub lines: Vec<Line>,
    pub truncated: bool,
    pub binary: bool,
}

/// Past this a diff is not being read, it is being scrolled past.
const MAX_LINES: usize = 4_000;

pub fn spawn_patch(
    repo: PathBuf,
    target: Target,
    file: String,
) -> (async_channel::Receiver<Result<Box<FileDiff>, String>>, Cancel) {
    let (tx, rx) = async_channel::bounded(1);
    let cancel = Cancel::default();
    {
        let cancel = cancel.clone();
        std::thread::spawn(move || {
            let result = match &target {
                Target::Commit(id) => patch_for(&repo, id, &file),
                Target::Working => patch_working(&repo, &file),
            };
            if !cancel.stopped() {
                let _ = tx.send_blocking(result.map(Box::new));
            }
        });
    }
    (rx, cancel)
}

/// Everything uncommitted: staged against HEAD, unstaged and untracked against
/// the index. Two diffs rather than one because the two halves answer different
/// questions and only one of them blocks an update.
fn load_working(path: &Path, cancel: &Cancel) -> Result<Detail, String> {
    let repo = Repository::open(path).map_err(|e| e.message().to_string())?;
    let head_tree = repo.head().ok().and_then(|h| h.peel_to_tree().ok());
    let index = repo.index().map_err(|e| e.message().to_string())?;

    let mut files = Vec::new();
    let mut insertions = 0usize;
    let mut deletions = 0usize;

    let mut staged_opts = DiffOptions::new();
    staged_opts.include_typechange(true).context_lines(0);
    if let Ok(diff) = repo.diff_tree_to_index(head_tree.as_ref(), Some(&index), Some(&mut staged_opts))
    {
        collect(&diff, Group::Staged, cancel, &mut files, &mut insertions, &mut deletions);
    }

    let mut work_opts = DiffOptions::new();
    work_opts
        .include_typechange(true)
        .include_untracked(true)
        // One row per untracked file rather than one per untracked directory:
        // "two untracked things" is what the card already says, and this view
        // exists to say which.
        .recurse_untracked_dirs(true)
        .include_ignored(false)
        .context_lines(0);
    if let Ok(diff) = repo.diff_index_to_workdir(Some(&index), Some(&mut work_opts)) {
        collect(&diff, Group::Unstaged, cancel, &mut files, &mut insertions, &mut deletions);
    }

    let total_files = files.len();
    files.truncate(MAX_FILES);

    Ok(Detail {
        id: String::new(),
        short_id: "working".into(),
        summary: "Local changes".into(),
        body: String::new(),
        author: String::new(),
        email: String::new(),
        when: 0,
        committer: None,
        parents: Vec::new(),
        merge: false,
        files,
        total_files,
        insertions,
        deletions,
    })
}

/// Turn a diff's deltas into file changes, assigning each to a group. The group
/// passed in is the default; untracked and conflicted entries say so themselves.
fn collect(
    diff: &Diff<'_>,
    default_group: Group,
    cancel: &Cancel,
    files: &mut Vec<FileChange>,
    insertions: &mut usize,
    deletions: &mut usize,
) {
    for (index, delta) in diff.deltas().enumerate() {
        if cancel.stopped() {
            return;
        }
        let group = match delta.status() {
            git2::Delta::Untracked => Group::Untracked,
            git2::Delta::Conflicted => Group::Conflicted,
            _ => default_group,
        };
        let binary = delta.new_file().is_binary() || delta.old_file().is_binary();
        let (adds, dels) = if binary || group == Group::Untracked {
            // An untracked file has nothing to diff against; counting its whole
            // length as additions would drown the totals.
            (0, 0)
        } else {
            match Patch::from_diff(diff, index) {
                Ok(Some(patch)) => {
                    let (_, adds, dels) = patch.line_stats().unwrap_or((0, 0, 0));
                    (adds, dels)
                }
                _ => (0, 0),
            }
        };
        *insertions += adds;
        *deletions += dels;

        let path_of = |f: git2::DiffFile<'_>| {
            f.path()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default()
        };
        let new_path = path_of(delta.new_file());
        let old_path = path_of(delta.old_file());
        let change = Change::from(delta.status());
        files.push(FileChange {
            path: if new_path.is_empty() { old_path.clone() } else { new_path },
            from: (matches!(change, Change::Renamed | Change::Copied) && !old_path.is_empty())
                .then_some(old_path),
            change,
            insertions: adds,
            deletions: dels,
            binary,
            group,
        });
    }
}

fn patch_for(path: &Path, id: &str, file: &str) -> Result<FileDiff, String> {
    let repo = Repository::open(path).map_err(|e| e.message().to_string())?;
    let oid = Oid::from_str(id).map_err(|e| e.message().to_string())?;
    let commit = repo.find_commit(oid).map_err(|e| e.message().to_string())?;
    let tree = commit.tree().map_err(|e| e.message().to_string())?;
    let parent_tree = commit.parent(0).ok().and_then(|p| p.tree().ok());

    // Narrowed to the one file, and with real context this time — the commit
    // summary asks for counts, this asks to be read.
    let mut opts = DiffOptions::new();
    opts.context_lines(3).include_typechange(true).pathspec(file);
    let diff = repo
        .diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), Some(&mut opts))
        .map_err(|e| e.message().to_string())?;

    let mut out = FileDiff::default();
    for index in 0..diff.deltas().len() {
        let Ok(Some(patch)) = Patch::from_diff(&diff, index) else { continue };
        if patch.num_hunks() == 0 {
            out.binary = diff
                .get_delta(index)
                .map(|d| d.new_file().is_binary() || d.old_file().is_binary())
                .unwrap_or(false);
        }
        for hunk_index in 0..patch.num_hunks() {
            let Ok((hunk, lines_in_hunk)) = patch.hunk(hunk_index) else { continue };
            out.lines.push(Line {
                origin: '@',
                text: String::from_utf8_lossy(hunk.header()).trim_end().to_string(),
            });
            for line_index in 0..lines_in_hunk {
                if out.lines.len() >= MAX_LINES {
                    out.truncated = true;
                    return Ok(out);
                }
                let Ok(line) = patch.line_in_hunk(hunk_index, line_index) else { continue };
                out.lines.push(Line {
                    origin: line.origin(),
                    text: String::from_utf8_lossy(line.content())
                        .trim_end_matches('\n')
                        .trim_end_matches('\r')
                        .to_string(),
                });
            }
        }
    }
    Ok(out)
}

/// The uncommitted diff of one file, including an untracked one — which has no
/// blob to compare against, so git is asked to show its content as added.
fn patch_working(path: &Path, file: &str) -> Result<FileDiff, String> {
    let repo = Repository::open(path).map_err(|e| e.message().to_string())?;
    let index = repo.index().map_err(|e| e.message().to_string())?;

    let mut out = FileDiff::default();
    let mut opts = DiffOptions::new();
    opts.context_lines(3)
        .include_typechange(true)
        .include_untracked(true)
        .recurse_untracked_dirs(true)
        .show_untracked_content(true)
        .pathspec(file);

    // Staged first, so a file that is both staged and modified still shows
    // something rather than nothing.
    let head_tree = repo.head().ok().and_then(|h| h.peel_to_tree().ok());
    let mut staged_opts = DiffOptions::new();
    staged_opts.context_lines(3).include_typechange(true).pathspec(file);
    if let Ok(diff) = repo.diff_tree_to_index(head_tree.as_ref(), Some(&index), Some(&mut staged_opts)) {
        append_patch(&diff, &mut out);
    }
    if let Ok(diff) = repo.diff_index_to_workdir(Some(&index), Some(&mut opts)) {
        append_patch(&diff, &mut out);
    }
    Ok(out)
}

fn append_patch(diff: &Diff<'_>, out: &mut FileDiff) {
    for index in 0..diff.deltas().len() {
        let Ok(Some(patch)) = Patch::from_diff(diff, index) else { continue };
        if patch.num_hunks() == 0 {
            out.binary = diff
                .get_delta(index)
                .map(|d| d.new_file().is_binary() || d.old_file().is_binary())
                .unwrap_or(out.binary);
        }
        for hunk_index in 0..patch.num_hunks() {
            let Ok((hunk, lines_in_hunk)) = patch.hunk(hunk_index) else { continue };
            out.lines.push(Line {
                origin: '@',
                text: String::from_utf8_lossy(hunk.header()).trim_end().to_string(),
            });
            for line_index in 0..lines_in_hunk {
                if out.lines.len() >= MAX_LINES {
                    out.truncated = true;
                    return;
                }
                let Ok(line) = patch.line_in_hunk(hunk_index, line_index) else { continue };
                out.lines.push(Line {
                    origin: line.origin(),
                    text: String::from_utf8_lossy(line.content())
                        .trim_end_matches('\n')
                        .trim_end_matches('\r')
                        .to_string(),
                });
            }
        }
    }
}

/// Why a push cannot go ahead, as a sentence. Checked before invoking git so
/// the user gets an explanation rather than a rejection from the far end.
pub fn why_not_push(
    ahead: usize,
    behind: usize,
    upstream: Option<&str>,
    only_remote: Option<&str>,
) -> Option<String> {
    if upstream.is_none() {
        return match only_remote {
            // Publishing a branch is a different act from pushing one, and it
            // is named differently in the UI.
            Some(_) => None,
            None => Some(
                "This branch has no upstream, and there is no single remote to publish it to."
                    .into(),
            ),
        };
    }
    if behind > 0 {
        return Some(format!(
            "{behind} commits are waiting on the upstream. Fast-forward first — a push would be rejected."
        ));
    }
    if ahead == 0 {
        return Some("Nothing to push.".into());
    }
    None
}

/// Push the checked-out branch to its upstream. Never forced, never `--all`,
/// never tags. `publish` names the remote for a branch that has never been
/// pushed, which is the only time an upstream is set.
pub fn spawn_push(
    repo: PathBuf,
    branch: String,
    publish: Option<String>,
) -> async_channel::Receiver<Result<String, String>> {
    let (tx, rx) = async_channel::bounded(1);
    std::thread::spawn(move || {
        let mut args: Vec<String> = vec!["push".into()];
        if let Some(remote) = publish {
            args.push("--set-upstream".into());
            args.push(remote);
            args.push(branch);
        }
        let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
        // A pre-push hook can be a test suite, so the same patience as a commit.
        let result = crate::gitcmd::run(&repo, &borrowed, WRITE_TIMEOUT).map(|out| {
            out.lines()
                .find(|line| !line.trim().is_empty())
                .unwrap_or("Pushed")
                .trim()
                .to_string()
        });
        let _ = tx.send_blocking(result);
    });
    rx
}

/// A node in the tree the pane draws.
pub enum Node {
    /// A directory, with the path segment(s) it stands for. Single-child chains
    /// are collapsed into one node, so `src/Api/One/Interfaces` is one row
    /// rather than four.
    Dir { name: String, depth: usize },
    /// `file.path` stays the full path, since that is what a diff needs;
    /// `display` is the part worth putting on screen under its directory.
    File {
        file: FileChange,
        display: String,
        depth: usize,
    },
}

/// Arrange changed files into an indented tree, flattened for rendering.
///
/// A flat list of paths like
/// `src/Orders.Api.Application/Checkout/Interfaces/IFoo.cs` is a
/// wall of identical prefixes; the shape of a change is what the list is for.
pub fn tree(files: &[FileChange]) -> Vec<Node> {
    #[derive(Default)]
    struct Dir {
        dirs: std::collections::BTreeMap<String, Dir>,
        files: Vec<FileChange>,
    }

    let mut root = Dir::default();
    for file in files {
        let mut parts: Vec<&str> = file.path.split('/').collect();
        let _name = parts.pop();
        let mut at = &mut root;
        for part in parts {
            at = at.dirs.entry(part.to_string()).or_default();
        }
        at.files.push(file.clone());
    }

    fn walk(dir: &Dir, prefix: String, depth: usize, out: &mut Vec<Node>) {
        for (name, child) in &dir.dirs {
            // Collapse a chain of directories that each hold only one thing.
            if child.files.is_empty() && child.dirs.len() == 1 {
                let joined = if prefix.is_empty() {
                    name.clone()
                } else {
                    format!("{prefix}/{name}")
                };
                walk(child, joined, depth, out);
                continue;
            }
            let label = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            out.push(Node::Dir { name: label, depth });
            walk(child, String::new(), depth + 1, out);
        }
        for file in &dir.files {
            let display = file.path.rsplit('/').next().unwrap_or(&file.path).to_string();
            out.push(Node::File { file: file.clone(), display, depth });
        }
    }

    let mut out = Vec::new();
    walk(&root, String::new(), 0, &mut out);
    out
}

/// How long a `git` write is given. Generous, because a `pre-commit` hook can
/// be a test suite.
const WRITE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// What a commit was refused for, as a sentence rather than a subprocess error.
pub fn why_not_commit(path: &Path, staged: usize, message: &str) -> Option<String> {
    if message.trim().is_empty() {
        return Some("Write a commit message first.".into());
    }
    if staged == 0 {
        return Some("Nothing is staged. Stage a file to commit it.".into());
    }
    let state = Repository::open(path).map(|r| r.state()).ok()?;
    if state != git2::RepositoryState::Clean {
        // omarepos has no conflict story and must not pretend otherwise.
        return Some(format!(
            "This repository is in the middle of something ({state:?}). Finish or abort it in a terminal."
        ));
    }
    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Write {
    Stage,
    Unstage,
}

/// Stage or unstage whole files. Never a hunk: see ADR-0004 — hunk staging is
/// the heart of a committing tool, and this is not one.
pub fn spawn_write(
    repo: PathBuf,
    what: Write,
    files: Vec<String>,
) -> async_channel::Receiver<Result<(), String>> {
    let (tx, rx) = async_channel::bounded(1);
    std::thread::spawn(move || {
        let mut args: Vec<&str> = match what {
            Write::Stage => vec!["add", "--"],
            // `restore --staged` leaves the working tree alone; `reset` would
            // be the older spelling of the same thing.
            Write::Unstage => vec!["restore", "--staged", "--"],
        };
        args.extend(files.iter().map(String::as_str));
        let _ = tx.send_blocking(crate::gitcmd::run(&repo, &args, WRITE_TIMEOUT).map(|_| ()));
    });
    rx
}

/// Commit what is staged. `-m` only: no `-a`, no `--amend`, nothing that could
/// take in work the user has not looked at or rewrite what is already there.
pub fn spawn_commit(
    repo: PathBuf,
    message: String,
) -> async_channel::Receiver<Result<String, String>> {
    let (tx, rx) = async_channel::bounded(1);
    std::thread::spawn(move || {
        let result = crate::gitcmd::run(&repo, &["commit", "-m", &message], WRITE_TIMEOUT)
            .and_then(|_| {
                crate::gitcmd::run(&repo, &["rev-parse", "--short=8", "HEAD"], WRITE_TIMEOUT)
            })
            .map(|sha| sha.trim().to_string());
        let _ = tx.send_blocking(result);
    });
    rx
}
