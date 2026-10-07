//! Repository discovery and inspection via libgit2.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use git2::{BranchType, Repository, StatusOptions};

use crate::lang;

#[derive(Debug, Clone)]
pub struct Commit {
    pub summary: String,
    pub author: String,
    pub when: i64,
    pub short_id: String,
}

#[derive(Debug, Clone)]
pub struct LangShare {
    pub name: String,
    pub color: String,
    pub share: f32,
}

#[derive(Debug, Clone, Default)]
pub struct WorkTree {
    pub staged: usize,
    pub unstaged: usize,
    pub untracked: usize,
    pub conflicted: usize,
}

impl WorkTree {
    pub fn is_clean(&self) -> bool {
        self.staged == 0 && self.unstaged == 0 && self.untracked == 0 && self.conflicted == 0
    }

    pub fn total(&self) -> usize {
        self.staged + self.unstaged + self.untracked + self.conflicted
    }
}

#[derive(Debug, Clone)]
pub struct RepoInfo {
    pub name: String,
    pub path: PathBuf,
    /// Branch name, or a short sha when HEAD is detached.
    pub head: String,
    pub detached: bool,
    pub unborn: bool,
    pub bare: bool,
    pub tree: WorkTree,
    pub ahead: usize,
    pub behind: usize,
    pub upstream: Option<String>,
    pub remote: Option<String>,
    pub local_branches: usize,
    pub stashes: usize,
    pub tracked_files: usize,
    pub langs: Vec<LangShare>,
    pub last_commit: Option<Commit>,
    /// Set when libgit2 could not read the repository at all.
    pub error: Option<String>,
}

impl RepoInfo {
    /// Sort key for "most recent activity first".
    pub fn activity(&self) -> i64 {
        self.last_commit.as_ref().map(|c| c.when).unwrap_or(i64::MIN)
    }

    /// Everything the search box matches against.
    pub fn haystack(&self) -> String {
        let mut s = format!("{} {} {}", self.name, self.path.display(), self.head);
        if let Some(remote) = &self.remote {
            s.push(' ');
            s.push_str(remote);
        }
        for l in &self.langs {
            s.push(' ');
            s.push_str(&l.name);
        }
        s.to_lowercase()
    }
}

fn is_git_dir(path: &Path) -> bool {
    // A worktree or submodule has a `.git` *file* pointing elsewhere.
    path.join(".git").exists()
}

/// Walk `roots` looking for repositories, at most `max_depth` levels down.
/// Once a directory is recognised as a repository we stop descending into it,
/// so vendored checkouts inside `node_modules` or `vendor/` stay out of sight.
pub fn discover(roots: &[PathBuf], max_depth: usize) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for root in roots {
        let root = root.canonicalize().unwrap_or_else(|_| root.clone());
        if is_git_dir(&root) && seen.insert(root.clone()) {
            found.push(root);
            continue;
        }
        walk(&root, 1, max_depth, &mut found, &mut seen);
    }
    found.sort();
    found
}

fn walk(
    dir: &Path,
    depth: usize,
    max_depth: usize,
    found: &mut Vec<PathBuf>,
    seen: &mut std::collections::HashSet<PathBuf>,
) {
    if depth > max_depth {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut children = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') || matches!(name.as_ref(), "node_modules" | "vendor" | "target") {
            continue;
        }
        children.push(path);
    }
    for path in children {
        if is_git_dir(&path) {
            let key = path.canonicalize().unwrap_or_else(|_| path.clone());
            if seen.insert(key) {
                found.push(path);
            }
            continue;
        }
        walk(&path, depth + 1, max_depth, found, seen);
    }
}

/// Inspect one repository. Never fails: problems land in `RepoInfo::error`.
pub fn inspect(path: &Path) -> RepoInfo {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string());

    let mut info = RepoInfo {
        name,
        path: path.to_path_buf(),
        head: "-".to_string(),
        detached: false,
        unborn: false,
        bare: false,
        tree: WorkTree::default(),
        ahead: 0,
        behind: 0,
        upstream: None,
        remote: None,
        local_branches: 0,
        stashes: 0,
        tracked_files: 0,
        langs: Vec::new(),
        last_commit: None,
        error: None,
    };

    let mut repo = match Repository::open(path) {
        Ok(repo) => repo,
        Err(err) => {
            info.error = Some(err.message().to_string());
            return info;
        }
    };

    info.bare = repo.is_bare();

    match repo.head() {
        Ok(head) => {
            if repo.head_detached().unwrap_or(false) {
                info.detached = true;
                info.head = head
                    .target()
                    .map(|oid| short(&oid))
                    .unwrap_or_else(|| "detached".into());
            } else {
                info.head = head.shorthand().unwrap_or("HEAD").to_string();
            }
            if let Ok(commit) = head.peel_to_commit() {
                let author = commit.author();
                info.last_commit = Some(Commit {
                    summary: commit
                        .summary()
                        .ok()
                        .flatten()
                        .unwrap_or("")
                        .trim()
                        .to_string(),
                    author: author.name().unwrap_or("unknown").to_string(),
                    when: commit.time().seconds(),
                    short_id: short(&commit.id()),
                });
            }
        }
        Err(err) if err.code() == git2::ErrorCode::UnbornBranch => {
            info.unborn = true;
            info.head = repo
                .find_reference("HEAD")
                .ok()
                .and_then(|r| r.symbolic_target().ok().flatten().map(|t| t.trim_start_matches("refs/heads/").to_string()))
                .unwrap_or_else(|| "main".into());
        }
        Err(err) => {
            info.error = Some(err.message().to_string());
            return info;
        }
    }

    if !info.detached && !info.unborn {
        if let Ok(branch) = repo.find_branch(&info.head, BranchType::Local) {
            if let Ok(upstream) = branch.upstream() {
                info.upstream = upstream.name().ok().flatten().map(|s| s.to_string());
                let local = branch.get().target();
                let remote = upstream.get().target();
                if let (Some(local), Some(remote)) = (local, remote) {
                    if let Ok((ahead, behind)) = repo.graph_ahead_behind(local, remote) {
                        info.ahead = ahead;
                        info.behind = behind;
                    }
                }
            }
        }
    }

    if let Ok(branches) = repo.branches(Some(BranchType::Local)) {
        info.local_branches = branches.count();
    }

    info.remote = repo
        .find_remote("origin")
        .ok()
        .and_then(|r| r.url().ok().map(prettify_remote))
        .or_else(|| {
            repo.remotes().ok().and_then(|names| {
                names
                    .iter()
                    .filter_map(|n| n.ok().flatten())
                    .next()
                    .and_then(|n| repo.find_remote(n).ok())
                    .and_then(|r| r.url().ok().map(prettify_remote))
            })
        });

    if !info.bare {
        let mut opts = StatusOptions::new();
        opts.include_untracked(true)
            .recurse_untracked_dirs(false)
            .include_ignored(false)
            .exclude_submodules(true);
        if let Ok(statuses) = repo.statuses(Some(&mut opts)) {
            for entry in statuses.iter() {
                let s = entry.status();
                if s.is_conflicted() {
                    info.tree.conflicted += 1;
                } else if s.intersects(
                    git2::Status::INDEX_NEW
                        | git2::Status::INDEX_MODIFIED
                        | git2::Status::INDEX_DELETED
                        | git2::Status::INDEX_RENAMED
                        | git2::Status::INDEX_TYPECHANGE,
                ) {
                    info.tree.staged += 1;
                } else if s.contains(git2::Status::WT_NEW) {
                    info.tree.untracked += 1;
                } else if s.intersects(
                    git2::Status::WT_MODIFIED
                        | git2::Status::WT_DELETED
                        | git2::Status::WT_RENAMED
                        | git2::Status::WT_TYPECHANGE,
                ) {
                    info.tree.unstaged += 1;
                }
            }
        }
    }

    let mut stashes = 0usize;
    let _ = repo.stash_foreach(|_, _, _| {
        stashes += 1;
        true
    });
    info.stashes = stashes;

    // Language mix from the index: fast, and it only ever sees tracked files.
    if let Ok(index) = repo.index() {
        let mut counts: HashMap<&'static str, (usize, &'static str)> = HashMap::new();
        let mut total = 0usize;
        for entry in index.iter() {
            let path = String::from_utf8_lossy(&entry.path);
            info.tracked_files += 1;
            if let Some(l) = lang::classify(&path) {
                let slot = counts.entry(l.name).or_insert((0, l.color));
                slot.0 += 1;
                total += 1;
            }
        }
        if total > 0 {
            let mut shares: Vec<LangShare> = counts
                .into_iter()
                .map(|(name, (count, color))| LangShare {
                    name: name.to_string(),
                    color: color.to_string(),
                    share: count as f32 / total as f32,
                })
                .collect();
            shares.sort_by(|a, b| {
                b.share
                    .partial_cmp(&a.share)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| a.name.cmp(&b.name))
            });
            shares.truncate(6);
            info.langs = shares;
        }
    }

    info
}

fn short(oid: &git2::Oid) -> String {
    oid.to_string().chars().take(7).collect()
}

/// `git@github.com:owner/repo.git` and `https://host/owner/repo.git` both
/// collapse to `host/owner/repo`.
fn prettify_remote(url: &str) -> String {
    let url = url.trim().trim_end_matches('/');
    let url = url.strip_suffix(".git").unwrap_or(url);
    if let Some(rest) = url.split_once("://").map(|(_, r)| r) {
        let rest = rest.split_once('@').map(|(_, r)| r).unwrap_or(rest);
        return rest.to_string();
    }
    if let Some((host, path)) = url.split_once(':') {
        let host = host.split_once('@').map(|(_, h)| h).unwrap_or(host);
        return format!("{host}/{path}");
    }
    url.to_string()
}
