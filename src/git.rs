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
    /// Why the counts above are not counts at all: libgit2 could not read the
    /// index or the status, and said this. It refuses an index written with
    /// `core.splitIndex` or as a sparse index, both of which git itself reads
    /// without a word, and one cut short by a crash. The counts stay at zero,
    /// and zero used to be drawn as a green "clean" on a repository with
    /// edits in it.
    ///
    /// Kept apart from [`RepoInfo::error`] on purpose. That one means the
    /// repository could not be read at all: it takes the card's other pills
    /// away, refuses to open the commit graph and drops the repository from
    /// every fetch, and none of that is true of a repository whose history
    /// and remotes read perfectly well.
    pub unreadable: Option<String>,
}

impl WorkTree {
    /// Known to be clean. A tree that could not be read is not.
    pub fn is_clean(&self) -> bool {
        self.unreadable.is_none()
            && self.staged == 0
            && self.unstaged == 0
            && self.untracked == 0
            && self.conflicted == 0
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
    /// Why `ahead`, `behind`, `unpushed` and `stray_branches` are not counts
    /// at all: the commits between here and the remotes could not be walked,
    /// and libgit2 said this. A zero that means "could not count" is not to be
    /// shown as nothing to push.
    pub history_unreadable: Option<String>,
    pub upstream: Option<String>,
    /// When the upstream tip was committed. Without it a card can only say how
    /// old the local commit is, which reads as "nothing has happened here"
    /// even when the remote moved an hour ago.
    pub upstream_when: Option<i64>,
    pub remote: Option<String>,
    pub local_branches: usize,
    pub stashes: usize,
    pub tracked_files: usize,
    /// Every language among the tracked files, largest share first. The card
    /// draws the first few; the `lang:` filter has to see all of them.
    pub langs: Vec<LangShare>,
    pub last_commit: Option<Commit>,
    /// `user.name` and `user.email` as libgit2 reads the configuration for
    /// this directory. `includeIf gitdir:` makes this differ per repository,
    /// and a misconfigured rule is otherwise silent until something is pushed
    /// under the wrong name.
    ///
    /// It is what is configured, not who git will record, and the card words
    /// it that way. Only git knows the second, and nothing runs git during a
    /// scan; the commit box asks it ([`crate::commit::identity`]).
    pub identity: Option<String>,
    /// What is in plain sight that makes git likely to record somebody other
    /// than `identity`; see [`identity_doubt`].
    pub identity_doubt: Option<String>,
    /// Set when libgit2 could not read the repository at all: it does not
    /// open, it is not this directory's, or HEAD or the commit it names cannot
    /// be read. A narrower failure has a field of its own, so that it does not
    /// cost the repository everything else the card can still say:
    /// [`WorkTree::unreadable`] and `history_unreadable`.
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
        // The refresher takes a status it cannot read as a reason not to
        // proceed, so the card must not promise that it will.
        if self.tree.unreadable.is_some() {
            return Some("its working tree could not be read");
        }
        // Untracked files do not hold a fast-forward back. The exception, an
        // incoming file of the same name, is one only git can see coming: it
        // refuses, and the refusal is shown on the card when it happens.
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

/// Whether `repo`, opened at `found`, really is the repository *of* that
/// directory.
///
/// Discovery takes any directory with a `.git` in it, and a `.git` can say
/// that its files live somewhere else. `core.worktree` in its config makes
/// both libgit2 and git treat another directory as the work tree: status is
/// read from there, `git add` stages from there, and a fast-forward writes
/// there, into a place the user never pointed this application at. A `.git`
/// that is a file can likewise name another repository's git directory. A
/// directory that arrived under a scan root from somewhere else (an archive, a
/// synced folder) only has to contain one of these to turn "refresh the
/// fleet" into "write files anywhere this user can".
///
/// So a repository is used only when its work tree is the directory it was
/// found in, and its git directory is either inside that directory or one of
/// the arrangements git itself makes: a linked worktree, a submodule, or a git
/// directory kept apart with `--separate-git-dir`.
pub fn belongs_here(repo: &Repository, found: &Path) -> Result<(), String> {
    if repo.is_bare() {
        return Ok(());
    }
    let Ok(here) = found.canonicalize() else {
        return Err(format!("{} cannot be resolved", found.display()));
    };
    let tree = repo.workdir().and_then(|tree| tree.canonicalize().ok());
    if tree.as_deref() != Some(here.as_path()) {
        return Err(match tree {
            Some(tree) => format!(
                "Its git data says the work tree is somewhere else ({}). omarepos leaves a \
                 repository like that alone.",
                tree.display()
            ),
            None => "Its work tree cannot be found.".to_string(),
        });
    }

    let Ok(gitdir) = repo.path().canonicalize() else {
        return Err("Its git directory cannot be resolved.".to_string());
    };
    if gitdir.starts_with(&here) {
        return Ok(());
    }
    // The `.git` here is a file naming a git directory somewhere else. git
    // builds three things that way, and each can be told from a directory that
    // has merely helped itself to another repository's git data.
    //
    // A linked worktree's git directory records which `.git` file leads to it.
    if let Ok(back) = std::fs::read_to_string(gitdir.join("gitdir")) {
        let ours = Path::new(back.trim()).canonicalize().ok() == Some(here.join(".git"));
        return if ours {
            Ok(())
        } else {
            Err(format!(
                "Its .git points at a worktree's git directory ({}) that belongs to another \
                 directory. omarepos leaves a repository like that alone.",
                gitdir.display()
            ))
        };
    }
    // A submodule's names its work tree in `core.worktree`, which the
    // comparison above has already held to this directory. That leaves a git
    // directory kept apart on purpose (`--separate-git-dir`), which is fine,
    // and a `.git` belonging to some other work tree, which is not.
    if gitdir.file_name().is_some_and(|name| name == ".git") {
        return Err(format!(
            "Its .git points at another repository's git directory ({}). omarepos leaves a \
             repository like that alone.",
            gitdir.display()
        ));
    }
    Ok(())
}

/// Open the repository found at `path`, refusing one that [`belongs_here`]
/// does not vouch for. Everything that reads a working tree or writes anything
/// opens through this.
pub fn open_here(path: &Path) -> Result<Repository, String> {
    let repo = Repository::open(path).map_err(|err| err.message().to_string())?;
    belongs_here(&repo, path)?;
    Ok(repo)
}

/// Past this the count stops: the card says "1000+", and a repository with no
/// remote does not have its whole history walked on every scan.
pub const UNPUSHED_CAP: usize = 1000;

/// How many commits reachable from `from` are on no remote, counted up to
/// `cap`. This is `git rev-list --count <from> --not --remotes`.
///
/// A walk that fails is an error and not a count. It used to be a count both
/// ways: a walk that could not start was zero, and one that broke off at a
/// commit it could not read counted the failure itself as a commit.
fn on_no_remote(repo: &Repository, from: git2::Oid, cap: usize) -> Result<usize, git2::Error> {
    let mut walk = repo.revwalk()?;
    walk.push(from)?;
    // No remote-tracking refs at all is not an error: then nothing is hidden,
    // and everything counts.
    let _ = walk.hide_glob("refs/remotes/*");
    let mut count = 0;
    for step in walk.take(cap) {
        step?;
        count += 1;
    }
    Ok(count)
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

/// Inspect one repository. Never fails, and never passes a read that failed
/// off as a reading of zero: a repository that cannot be read at all says so in
/// [`RepoInfo::error`], a working tree that cannot in [`WorkTree::unreadable`],
/// and commits that cannot be counted in [`RepoInfo::history_unreadable`].
///
/// It used to promise the first and do none of the three for anything past
/// opening the repository. An index libgit2 refused came out as a clean tree
/// with no files in it, and a newest commit lost to a power cut as "no commits
/// yet", on the one screen meant to show which repositories need looking at.
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
        history_unreadable: None,
        upstream: None,
        upstream_when: None,
        remote: None,
        local_branches: 0,
        stashes: 0,
        tracked_files: 0,
        langs: Vec::new(),
        last_commit: None,
        identity: None,
        identity_doubt: None,
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
    // Before anything is read from a working tree.
    if let Err(why) = belongs_here(&repo, path) {
        info.error = Some(why);
        return info;
    }

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
            match head.peel_to_commit() {
                Ok(commit) => {
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
                // HEAD names a commit and the commit is not there, or is not
                // one: git calls this a bad object and stops. The card went on
                // to say "clean" and "no commits yet".
                Err(err) => {
                    info.error = Some(format!("Its newest commit cannot be read: {}", err.message()));
                    return info;
                }
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
                    match repo.graph_ahead_behind(local, remote) {
                        Ok((ahead, behind)) => {
                            info.ahead = ahead;
                            info.behind = behind;
                        }
                        Err(err) => info.history_unreadable = Some(err.message().to_string()),
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
            match on_no_remote(&repo, head, UNPUSHED_CAP) {
                Ok(count) => info.unpushed = count,
                Err(err) => {
                    info.history_unreadable.get_or_insert_with(|| err.message().to_string());
                }
            }
        }
        if let Ok(branches) = repo.branches(Some(BranchType::Local)) {
            for tip in branches.flatten().filter_map(|(branch, _)| branch.get().target()) {
                if Some(tip) == head {
                    continue;
                }
                match on_no_remote(&repo, tip, 1) {
                    Ok(0) => {}
                    Ok(_) => info.stray_branches += 1,
                    Err(err) => {
                        info.history_unreadable.get_or_insert_with(|| err.message().to_string());
                    }
                }
            }
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

    // libgit2's reading of `user.*`, which is git's own for the plain case and
    // for `includeIf gitdir:` and `onbranch:`. This comment used to say it was
    // "the same answer `git config` gives", and the card printed it as "Commits
    // as". It is not the same answer where the identity is chosen by remote
    // URL (`hasconfig:`, which libgit2 skips), by `author.*`, or by the
    // environment, and there the card named the wrong person with full
    // confidence. Asking git would settle it, and nothing runs git in a
    // repository nobody has looked at yet; so the card says what this is, and
    // `identity_doubt` says when not to lean on it.
    if let Ok(config) = repo.config().and_then(|mut c| c.snapshot()) {
        let name = config.get_str("user.name").ok().map(str::to_string);
        let email = config.get_str("user.email").ok().map(str::to_string);
        info.identity = match (name, email) {
            (Some(name), Some(email)) => Some(format!("{name} <{email}>")),
            (Some(name), None) => Some(name),
            (None, Some(email)) => Some(email),
            (None, None) => None,
        };
        info.identity_doubt = identity_doubt(&config, &|name| std::env::var_os(name).is_some());
    }
    // Nothing here says which credential helper a fetch will use. The card
    // used to, from two exact config keys and a host cut out of the tidied-up
    // remote, with github.com standing in when there was no remote. That named
    // a helper for a repository with no remote and for one fetched over SSH,
    // which asks none, and missed any helper set for a path, a user or a
    // wildcard host. git asks every helper that matches, in order, and offers
    // no way to list them short of running it.

    if !info.bare {
        // No `update_index`, on purpose. Without it libgit2 never writes back
        // the stat data it has just refreshed, so a repository whose files
        // were copied, restored or touched is hashed in full on every scan,
        // until git rewrites its own index (a commit, a fast-forward, a plain
        // `git status`). With it, libgit2 writes `.git/index` back from the
        // copy it read when it started: a `git add` that landed while it was
        // hashing is gone, the extensions git keeps there are dropped, and a
        // held `index.lock` fails the whole read. Slow is the better failure.
        // The cure, where one is wanted, is git's own `update-index --refresh`
        // on a repository the user has asked to have updated.
        let mut opts = StatusOptions::new();
        opts.include_untracked(true)
            .recurse_untracked_dirs(false)
            .include_ignored(false)
            .exclude_submodules(true);
        match repo.statuses(Some(&mut opts)) {
            Ok(statuses) => {
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
            // There was no arm for this, so the counts stayed at zero and
            // zero is what "clean" is drawn from.
            Err(err) => info.tree.unreadable = Some(err.message().to_string()),
        }
    }

    let mut stashes = 0usize;
    let _ = repo.stash_foreach(|_, _, _| {
        stashes += 1;
        true
    });
    info.stashes = stashes;

    // Language mix from the index: fast, and it only ever sees tracked files.
    let index = match repo.index() {
        Ok(index) => Some(index),
        // The index the status was read from, and so nearly always the same
        // refusal. Where the status somehow got by without it, "0 files" and
        // an empty language bar are still not a reading.
        Err(err) => {
            if !info.bare {
                info.tree.unreadable.get_or_insert_with(|| err.message().to_string());
            }
            None
        }
    };
    if let Some(index) = index {
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
            // All of them. The list was cut to six here, for the card's bar,
            // and `speaks` reads the same list: `lang:docker` could not find a
            // repository whose Dockerfile was its seventh language.
            info.langs = shares;
        }
    }

    info
}

/// The first thing in sight that outranks `user.name` and `user.email`, or
/// that libgit2 passed over on the way to them, worded to follow "git may
/// record someone else:". `None` is not a guarantee: it means only that none
/// of the usual ways was seen.
///
/// `in_environment` says whether a variable is set, and is a parameter so that
/// a test can ask without changing the environment of every other test.
fn identity_doubt(config: &git2::Config, in_environment: &dyn Fn(&str) -> bool) -> Option<String> {
    // For the author and the committer, git takes these ahead of `user.*`.
    for key in ["author.name", "author.email", "committer.name", "committer.email"] {
        if config.get_str(key).is_ok() {
            return Some(format!("{key} is set, and outranks user.*"));
        }
    }
    for name in ["GIT_AUTHOR_NAME", "GIT_AUTHOR_EMAIL", "GIT_COMMITTER_NAME", "GIT_COMMITTER_EMAIL"] {
        if in_environment(name) {
            return Some(format!("{name} is set in the environment, and outranks the configuration"));
        }
    }
    // libgit2 follows three kinds of conditional include and reads straight
    // past any other, without a word. The rule itself is still listed as an
    // ordinary entry, which is how one that was skipped can be told.
    let mut entries = config.entries(Some("^includeif\\.")).ok()?;
    while let Some(entry) = entries.next() {
        let Ok(entry) = entry else { continue };
        let Ok(name) = entry.name() else { continue };
        let Some(condition) = name
            .strip_prefix("includeif.")
            .and_then(|rest| rest.strip_suffix(".path"))
        else {
            continue;
        };
        let followed = ["gitdir:", "gitdir/i:", "onbranch:"]
            .iter()
            .any(|kind| condition.starts_with(kind));
        if !followed {
            let kind = condition.split(':').next().unwrap_or(condition);
            return Some(format!(
                "the configuration has an includeIf \"{kind}:\" rule, which only git itself evaluates"
            ));
        }
    }
    None
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

    // ---- a repository is used only if it is this directory's ---------------

    #[test]
    fn a_repository_that_says_its_files_are_elsewhere_is_left_alone() {
        let _guard = isolate();
        let f = fleet("git-redirect");
        let elsewhere = scratch("git-redirect-victim");
        write(&elsewhere.join("private.txt"), "not this repository's\n");
        git(&f.work, &["config", "core.worktree", elsewhere.to_str().unwrap()]);

        let info = inspect(&f.work);
        let why = info.error.expect("it has to be refused, and say why");
        assert!(why.contains("somewhere else"), "{why}");
        assert_eq!(info.tree.untracked, 0, "the other directory was read");
        assert!(open_here(&f.work).is_err());
    }

    /// A directory whose `.git` is a file naming somebody else's git directory.
    #[test]
    fn a_directory_that_borrows_another_repositorys_git_data_is_left_alone() {
        let _guard = isolate();
        let f = fleet("git-borrowed");
        let squatter = scratch("git-borrowed-squatter");
        write(&squatter.join(".git"), &format!("gitdir: {}\n", f.work.join(".git").display()));

        assert!(inspect(&squatter).error.is_some());
        assert!(open_here(&squatter).is_err());
        assert_eq!(inspect(&f.work).error, None, "the repository it borrowed from is fine");
    }

    #[test]
    fn the_ways_git_itself_keeps_a_work_tree_apart_from_its_git_data_still_work() {
        let _guard = isolate();

        let f = fleet("git-linked-worktree");
        let linked = f.root.join("linked");
        git(&f.work, &["worktree", "add", "-q", "-b", "side", linked.to_str().unwrap()]);
        assert_eq!(inspect(&linked).error, None, "a linked worktree");

        let root = scratch("git-separate-git-dir");
        git(&root, &["init", "-q", "-b", "main", "--separate-git-dir", "store.git", "tree"]);
        commit_file(&root.join("tree"), "a.txt", "a\n", "one");
        assert_eq!(inspect(&root.join("tree")).error, None, "--separate-git-dir");

        let f = fleet("git-submodule-super");
        let lib = fleet("git-submodule-lib");
        git(&f.work, &["submodule", "add", "-q", lib.remote.to_str().unwrap(), "lib"]);
        assert_eq!(inspect(&f.work.join("lib")).error, None, "a submodule");
        assert_eq!(inspect(&f.work).error, None, "and the repository it lives in");
    }

    // ---- a read that fails is not a reading of zero ------------------------

    /// Take a loose object away, the way a power cut in the middle of a commit
    /// does.
    fn lose_object(repo: &Path, rev: &str) {
        let id = git(repo, &["rev-parse", rev]);
        let file = repo.join(".git/objects").join(&id[..2]).join(&id[2..]);
        std::fs::remove_file(&file).unwrap_or_else(|err| panic!("{}: {err}", file.display()));
    }

    #[test]
    fn an_index_that_cannot_be_read_is_not_a_clean_repository() {
        let _guard = isolate();
        let f = fleet("inspect-split-index");
        // What `core.splitIndex` does on the next write. git reads it without
        // a word; libgit2 refuses the whole index.
        git(&f.work, &["update-index", "--split-index"]);
        write(&f.work.join("README.md"), "one\ntwo\n");
        assert_eq!(git(&f.work, &["status", "--short"]), "M README.md");

        let info = inspect(&f.work);
        assert!(!info.tree.is_clean(), "an edited file, and the card says clean");
        let why = info.tree.unreadable.clone().expect("it has to say what stopped it");
        assert!(why.contains("link"), "{why}");
        // Not through `error`: that takes the commit graph and the fetch away
        // from a repository whose history reads perfectly well.
        assert_eq!(info.error, None);
        assert_eq!(info.head, "main");
        assert_eq!(info.history_unreadable, None);
        // The refresher will not fast-forward over a status it cannot read,
        // and the card has to agree with it.
        assert_eq!(info.update_blocked_by(), Some("its working tree could not be read"));

        // An index cut short, which is what a crash leaves.
        let f = fleet("inspect-cut-index");
        let index = f.work.join(".git/index");
        let whole = std::fs::read(&index).unwrap();
        std::fs::write(&index, &whole[..40]).unwrap();
        let info = inspect(&f.work);
        assert!(info.tree.unreadable.is_some() && !info.tree.is_clean());
        assert_eq!(info.error, None);
    }

    #[test]
    fn a_repository_whose_newest_commit_cannot_be_read_says_so() {
        let _guard = isolate();
        let f = fleet("inspect-lost-head");
        commit_file(&f.work, "a.txt", "a\n", "one");
        commit_file(&f.work, "b.txt", "b\n", "two");
        lose_object(&f.work, "HEAD");

        let info = inspect(&f.work);
        assert!(info.error.is_some(), "it read as: {:?} {:?}", info.head, info.last_commit);
    }

    #[test]
    fn commits_that_cannot_be_counted_are_not_counted_as_none() {
        let _guard = isolate();
        let f = fleet("inspect-lost-history");
        for name in ["a", "b", "c"] {
            commit_file(&f.work, &format!("{name}.txt"), "x\n", name);
        }
        assert_eq!(standing(&f.work), (3, 3, 0));
        lose_object(&f.work, "HEAD~1");

        // The newest commit, the index and the files are all still there.
        let info = inspect(&f.work);
        assert_eq!(info.error, None);
        assert_eq!(info.tree.unreadable, None);
        assert!(info.history_unreadable.is_some(), "{} ahead, {} unpushed", info.ahead, info.unpushed);

        // A shallow clone stops short on purpose, and that is not damage.
        let f = fleet("inspect-shallow");
        f.remote_moves(3);
        let url = format!("file://{}", f.remote.display());
        git(&f.root, &["clone", "-q", "--depth", "1", &url, "shallow"]);
        let shallow = f.root.join("shallow");
        commit_file(&shallow, "mine.txt", "x\n", "mine");
        let info = inspect(&shallow);
        assert_eq!((info.error.clone(), info.history_unreadable.clone()), (None, None));
        assert_eq!((info.ahead, info.unpushed), (1, 1));
    }

    #[test]
    fn a_repository_with_no_commits_still_has_a_working_tree() {
        let _guard = isolate();
        let dir = scratch("inspect-unborn");
        git(&dir, &["init", "-q", "-b", "main"]);
        write(&dir.join("README.md"), "one\n");
        write(&dir.join("main.rs"), "fn main() {}\n");
        git(&dir, &["add", "--", "README.md"]);

        let info = inspect(&dir);
        assert_eq!(info.error, None);
        assert!(info.unborn);
        assert_eq!((info.tree.staged, info.tree.untracked), (1, 1));
    }

    #[test]
    fn the_language_filter_finds_a_language_the_repository_has_little_of() {
        let _guard = isolate();
        let dir = scratch("inspect-many-languages");
        git(&dir, &["init", "-q", "-b", "main"]);
        let files = [
            ("ts", 9),
            ("tsx", 8),
            ("json", 7),
            ("md", 6),
            ("yaml", 5),
            ("css", 4),
            ("js", 3),
            ("html", 2),
            ("sh", 1),
        ];
        for (extension, count) in files {
            for n in 0..count {
                write(&dir.join(format!("src/file{n}.{extension}")), "x\n");
            }
        }
        write(&dir.join("Dockerfile"), "FROM scratch\n");
        git(&dir, &["add", "-A"]);
        git(&dir, &["commit", "-q", "-m", "one"]);

        let info = inspect(&dir);
        assert_eq!(info.tracked_files, 46);
        // The seventh language and on. Six is how many the card's bar draws,
        // and the filter used to see no further than the bar.
        for wanted in ["javascript", "html", "shell", "docker"] {
            assert!(info.speaks(wanted), "lang:{wanted} does not find it");
        }
        assert!(!info.speaks("rust"));
        // Largest first still, which is the order the bar and its legend take.
        let names: Vec<&str> = info.langs.iter().map(|lang| lang.name.as_str()).collect();
        assert_eq!(names[..6], ["TypeScript", "TSX", "JSON", "Markdown", "YAML", "CSS"]);
        assert_eq!(names.len(), 10);
    }

    // ---- who commits here, as far as can be read without asking git --------

    /// Who git itself says a commit made here would be recorded as.
    fn recorded_as(repo: &Path) -> String {
        crate::commit::identity(repo).unwrap()
    }

    #[test]
    fn the_card_does_not_vouch_for_an_identity_only_git_can_decide() {
        let _guard = isolate();

        // The plain case, and a rule of the kind libgit2 does follow: its
        // reading and git's agree, and nothing is in doubt.
        let f = fleet("inspect-identity-gitdir");
        let info = inspect(&f.work);
        assert_eq!(info.identity.as_deref(), Some("Fixture <fixture@example.invalid>"));
        assert_eq!(info.identity_doubt, None);
        let client = f.root.join("client.inc");
        write(&client, "[user]\n\temail = me@client.example\n");
        let rule = format!("includeIf.gitdir:{}/.path", f.work.display());
        git(&f.work, &["config", &rule, client.to_str().unwrap()]);
        let info = inspect(&f.work);
        assert_eq!(info.identity.as_deref(), Some("Fixture <me@client.example>"));
        assert_eq!(info.identity.as_deref(), Some(recorded_as(&f.work).as_str()));
        assert_eq!(info.identity_doubt, None);

        // Chosen by remote URL, which libgit2 reads straight past: the card
        // used to name the wrong person here, as "Commits as".
        let f = fleet("inspect-identity-hasconfig");
        let client = f.root.join("client.inc");
        write(&client, "[user]\n\temail = me@client.example\n");
        let rule = "includeIf.hasconfig:remote.*.url:**/remote.git.path";
        git(&f.work, &["config", rule, client.to_str().unwrap()]);
        assert_eq!(recorded_as(&f.work), "Fixture <me@client.example>");
        let info = inspect(&f.work);
        assert_eq!(info.identity.as_deref(), Some("Fixture <fixture@example.invalid>"));
        let doubt = info.identity_doubt.expect("a rule it cannot evaluate is a reason to doubt");
        assert!(doubt.contains("hasconfig"), "{doubt}");

        // `author.*` outranks `user.*`, and libgit2 was never asked for it.
        let f = fleet("inspect-identity-author");
        git(&f.work, &["config", "author.email", "author@elsewhere.example"]);
        assert_eq!(recorded_as(&f.work), "Fixture <author@elsewhere.example>");
        let info = inspect(&f.work);
        assert_eq!(info.identity.as_deref(), Some("Fixture <fixture@example.invalid>"));
        assert!(info.identity_doubt.is_some_and(|doubt| doubt.contains("author.email")));

        // And so does the environment git will be started in.
        let repo = Repository::open(&f.seed).unwrap();
        let config = repo.config().and_then(|mut c| c.snapshot()).unwrap();
        assert_eq!(identity_doubt(&config, &|_| false), None);
        let doubt = identity_doubt(&config, &|name| name == "GIT_COMMITTER_EMAIL").unwrap();
        assert!(doubt.contains("GIT_COMMITTER_EMAIL"), "{doubt}");
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
