//! Repository discovery and inspection via libgit2.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use git2::{BranchType, Repository, StatusOptions};

use crate::config::Root;
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
    /// Commits reachable from HEAD that are on no remote at all, counted up to
    /// [`UNPUSHED_CAP`]. Not the same as `ahead`, which measures against one
    /// upstream and is zero when there is none: a branch that was never
    /// published, an upstream since deleted, a detached HEAD and a repository
    /// with no remote all hold work that exists nowhere else, and every one of
    /// them used to read "clean" with nothing to push.
    pub unpushed: usize,
    /// Other local branches that hold commits on no remote.
    pub stray_branches: usize,
    pub upstream: Option<String>,
    /// When the upstream tip was committed. Without it a card can only say how
    /// old the local commit is, which reads as "nothing has happened here"
    /// even when the remote moved an hour ago.
    pub upstream_when: Option<i64>,
    pub remote: Option<String>,
    pub local_branches: usize,
    pub stashes: usize,
    pub tracked_files: usize,
    pub langs: Vec<LangShare>,
    pub last_commit: Option<Commit>,
    /// Who commits here — `includeIf gitdir:` makes this differ per repository,
    /// and a misconfigured rule is otherwise silent until something is pushed
    /// under the wrong name.
    pub identity: Option<String>,
    /// The credential helper that will be used for the remote's host, which is
    /// what decides *which account* a fetch authenticates as.
    pub credentials: Option<String>,
    /// Set when libgit2 could not read the repository at all.
    pub error: Option<String>,
}

impl RepoInfo {
    /// Why an automatic fast-forward would refuse to touch this repository.
    /// Mirrors the checks the refresher makes, so a card can say up front what
    /// the refresher would otherwise only say to the log.
    pub fn update_blocked_by(&self) -> Option<&'static str> {
        if self.bare || self.unborn {
            return Some("nothing to update");
        }
        if self.detached {
            return Some("detached HEAD");
        }
        if self.upstream.is_none() {
            return Some("no upstream");
        }
        if self.ahead > 0 {
            return Some("unpushed commits of its own");
        }
        // Untracked files do not block a fast-forward; git refuses rather than
        // overwriting one, so they are not the user's problem here.
        if self.tree.staged + self.tree.unstaged + self.tree.conflicted > 0 {
            return Some("uncommitted changes");
        }
        None
    }

    /// Sort key for "most recent activity first".
    pub fn activity(&self) -> i64 {
        self.last_commit.as_ref().map(|c| c.when).unwrap_or(i64::MIN)
    }

    /// What free text in the filter box matches against.
    ///
    /// Languages are deliberately not in here. They were, and the result was
    /// that searching for a repository called `asc` returned half the fleet,
    /// because `javascript` contains `asc`. Languages have their own `lang:`
    /// term instead, where a match is something the user asked for.
    pub fn haystack(&self) -> String {
        let mut s = format!("{} {} {}", self.name, self.path.display(), self.head);
        if let Some(remote) = &self.remote {
            s.push(' ');
            s.push_str(remote);
        }
        s.to_lowercase()
    }

    /// Does any language in the mix match `needle`? Anchored at the start of
    /// the name, so `script` does not find JavaScript but `java` does.
    pub fn speaks(&self, needle: &str) -> bool {
        self.langs
            .iter()
            .any(|lang| lang.name.to_lowercase().starts_with(needle))
    }
}

fn is_git_dir(path: &Path) -> bool {
    // A worktree or submodule has a `.git` *file* pointing elsewhere.
    path.join(".git").exists()
}

/// What a root wants tagged on the repositories it finds. `tag` of `None`
/// means it tags nothing, which retires whatever it applied before.
#[derive(Debug, Clone)]
pub struct Seed {
    /// The root's absolute path, as the bookkeeping key.
    pub root: String,
    pub tag: Option<String>,
}

/// A repository found by discovery, and which root it came from.
#[derive(Debug, Clone)]
pub struct Found {
    pub path: PathBuf,
    /// Absent for an individually listed repository, which belongs to no root.
    pub seed: Option<Seed>,
}

/// Past this the count stops: the card says "1000+", and a repository with no
/// remote does not have its whole history walked on every scan.
pub const UNPUSHED_CAP: usize = 1000;

/// How many commits reachable from `from` are on no remote, counted up to
/// `cap`. This is `git rev-list --count <from> --not --remotes`.
fn on_no_remote(repo: &Repository, from: git2::Oid, cap: usize) -> usize {
    let Ok(mut walk) = repo.revwalk() else { return 0 };
    if walk.push(from).is_err() {
        return 0;
    }
    // No remote-tracking refs at all is not an error: then nothing is hidden,
    // and everything counts.
    let _ = walk.hide_glob("refs/remotes/*");
    walk.take(cap).count()
}

/// Walk `roots` looking for repositories, at most `max_depth` levels down.
///
/// A repository normally ends the descent, so vendored checkouts and
/// submodules inside a project stay out of sight. A root marked `nested` lifts
/// that for its own directory only: the repository there is opened and the
/// subdirectories it *ignores* are followed, which is exactly the shape of a
/// workspace full of independent clones. Submodules are tracked rather than
/// ignored, so they are still skipped. Below that first level the normal rule
/// applies again.
pub fn discover(roots: &[Root], max_depth: usize) -> Vec<Found> {
    let mut found = Vec::new();
    let mut seen = HashSet::new();

    for root in roots {
        let base = root.path.canonicalize().unwrap_or_else(|_| root.path.clone());

        if !is_git_dir(&base) {
            walk(&base, 1, max_depth, root, &mut found, &mut seen);
            continue;
        }

        if !root.nested {
            record(base, root, &mut found, &mut seen);
            continue;
        }

        if root.include_self {
            record(base.clone(), root, &mut found, &mut seen);
        }
        descend_workspace(&base, root, max_depth, &mut found, &mut seen);
    }

    found.sort_by(|a, b| a.path.cmp(&b.path));
    found
}

fn record(path: PathBuf, root: &Root, found: &mut Vec<Found>, seen: &mut HashSet<PathBuf>) {
    let key = path.canonicalize().unwrap_or_else(|_| path.clone());
    if seen.insert(key) {
        let seed = Seed {
            root: root.path.display().to_string(),
            tag: root.tag.clone(),
        };
        found.push(Found { path, seed: Some(seed) });
    }
}

/// Directories worth looking at inside `dir`: no dotfiles, and none of the
/// places a checkout is vendored rather than worked on.
fn child_dirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut children = Vec::new();
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') || matches!(name.as_ref(), "node_modules" | "vendor" | "target") {
            continue;
        }
        children.push(entry.path());
    }
    children
}

/// Follow the children a workspace repository disclaims.
fn descend_workspace(
    base: &Path,
    root: &Root,
    max_depth: usize,
    found: &mut Vec<Found>,
    seen: &mut HashSet<PathBuf>,
) {
    let repo = Repository::open(base).ok();
    let submodules = submodule_paths(repo.as_ref());
    for child in child_dirs(base) {
        if !disclaimed(repo.as_ref(), base, &child, &submodules) {
            continue;
        }
        if is_git_dir(&child) {
            record(child, root, found, seen);
        } else {
            // Not a repository itself, but the workspace does not own it
            // either — a grouping folder. Carry on under the normal rule.
            walk(&child, 2, max_depth, root, found, seen);
        }
    }
}

/// Does `repo` ignore `child`? A directory it ignores is not part of it, which
/// is what separates a parked clone from a submodule.
///
/// The path is tested exactly as it is, with no trailing separator: a
/// `.gitignore` of `*` matches `sub/` on the rules alone, and only the
/// slash-free form makes libgit2 consult the index and report a tracked
/// submodule as not ignored. Submodule paths are then excluded outright, so an
/// unusual ignore configuration cannot let one through either.
fn disclaimed(
    repo: Option<&Repository>,
    base: &Path,
    child: &Path,
    submodules: &HashSet<PathBuf>,
) -> bool {
    let Some(repo) = repo else { return false };
    let Ok(relative) = child.strip_prefix(base) else { return false };
    if submodules.contains(relative) {
        return false;
    }
    repo.is_path_ignored(relative).unwrap_or(false)
}

fn submodule_paths(repo: Option<&Repository>) -> HashSet<PathBuf> {
    repo.and_then(|repo| repo.submodules().ok())
        .map(|modules| modules.iter().map(|m| m.path().to_path_buf()).collect())
        .unwrap_or_default()
}

fn walk(
    dir: &Path,
    depth: usize,
    max_depth: usize,
    root: &Root,
    found: &mut Vec<Found>,
    seen: &mut HashSet<PathBuf>,
) {
    if depth > max_depth {
        return;
    }
    for path in child_dirs(dir) {
        if is_git_dir(&path) {
            record(path, root, found, seen);
            continue;
        }
        walk(&path, depth + 1, max_depth, root, found, seen);
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
        unpushed: 0,
        stray_branches: 0,
        upstream: None,
        upstream_when: None,
        remote: None,
        local_branches: 0,
        stashes: 0,
        tracked_files: 0,
        langs: Vec::new(),
        last_commit: None,
        identity: None,
        credentials: None,
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
                info.upstream_when = upstream
                    .get()
                    .peel_to_commit()
                    .ok()
                    .map(|commit| commit.time().seconds());
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

    if !info.bare && !info.unborn {
        let head = repo.head().ok().and_then(|head| head.target());
        if let Some(head) = head {
            info.unpushed = on_no_remote(&repo, head, UNPUSHED_CAP);
        }
        if let Ok(branches) = repo.branches(Some(BranchType::Local)) {
            info.stray_branches = branches
                .flatten()
                .filter_map(|(branch, _)| branch.get().target())
                .filter(|tip| Some(*tip) != head)
                .filter(|tip| on_no_remote(&repo, *tip, 1) > 0)
                .count();
        }
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

    // libgit2 evaluates conditional includes, so this is the same answer
    // `git config` gives in that directory — no subprocess needed.
    if let Ok(config) = repo.config().and_then(|mut c| c.snapshot()) {
        let name = config.get_str("user.name").ok().map(str::to_string);
        let email = config.get_str("user.email").ok().map(str::to_string);
        info.identity = match (name, email) {
            (Some(name), Some(email)) => Some(format!("{name} <{email}>")),
            (Some(name), None) => Some(name),
            (None, Some(email)) => Some(email),
            (None, None) => None,
        };

        let host = info
            .remote
            .as_deref()
            .and_then(|r| r.split('/').next())
            .unwrap_or("github.com");
        for key in [
            format!("credential.https://{host}.helper"),
            "credential.helper".to_string(),
        ] {
            if let Ok(helper) = config.get_str(&key) {
                let helper = helper.trim().trim_start_matches('!').trim();
                if !helper.is_empty() {
                    info.credentials = Some(helper.to_string());
                    break;
                }
            }
        }
    }

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::*;

    /// (ahead of the upstream, on no remote, other branches with work on none)
    fn standing(path: &Path) -> (usize, usize, usize) {
        let info = inspect(path);
        assert_eq!(info.error, None);
        (info.ahead, info.unpushed, info.stray_branches)
    }

    #[test]
    fn work_that_is_on_no_remote_is_counted_with_or_without_an_upstream() {
        let _guard = isolate();

        let f = fleet("git-unpushed-level");
        assert_eq!(standing(&f.work), (0, 0, 0), "level with its upstream");
        commit_file(&f.work, "a.txt", "a\n", "one");
        assert_eq!(standing(&f.work), (1, 1, 0), "ahead already says this one");

        // The ordinary state of work in progress, and it used to read "clean".
        let f = fleet("git-unpushed-new-branch");
        git(&f.work, &["checkout", "-q", "-b", "feature"]);
        for i in 0..3 {
            commit_file(&f.work, &format!("f{i}.txt"), "x\n", &format!("feature {i}"));
        }
        assert_eq!(inspect(&f.work).upstream, None);
        assert_eq!(standing(&f.work), (0, 3, 0), "a branch that was never published");

        let f = fleet("git-unpushed-side-branch");
        git(&f.work, &["checkout", "-q", "-b", "wip"]);
        commit_file(&f.work, "w.txt", "x\n", "wip");
        git(&f.work, &["checkout", "-q", "main"]);
        assert_eq!(standing(&f.work), (0, 0, 1), "work parked on another branch");

        let f = fleet("git-unpushed-upstream-gone");
        git(&f.work, &["checkout", "-q", "-b", "topic"]);
        commit_file(&f.work, "t.txt", "x\n", "published once");
        git(&f.work, &["push", "-q", "-u", "origin", "topic"]);
        commit_file(&f.work, "u.txt", "x\n", "never published");
        git(&f.remote, &["update-ref", "-d", "refs/heads/topic"]);
        git(&f.work, &["fetch", "-q", "--prune"]);
        assert_eq!(standing(&f.work), (0, 2, 0), "the upstream was deleted");

        let f = fleet("git-unpushed-detached");
        git(&f.work, &["checkout", "-q", "--detach"]);
        commit_file(&f.work, "d.txt", "x\n", "on no branch");
        assert_eq!(standing(&f.work), (0, 1, 0), "a detached HEAD");

        let dir = scratch("git-unpushed-no-remote");
        git(&dir, &["init", "-q", "-b", "main"]);
        commit_file(&dir, "a.txt", "a\n", "one");
        commit_file(&dir, "b.txt", "b\n", "two");
        let info = inspect(&dir);
        assert_eq!(info.remote, None);
        assert_eq!((info.ahead, info.unpushed), (0, 2), "no remote at all");
    }
}
