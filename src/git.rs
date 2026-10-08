//! Repository discovery and inspection via libgit2.

use std::collections::{HashMap, HashSet};
use std::os::unix::ffi::OsStrExt;
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
    ///
    /// The path is the one the card shows, `~/code/thing`, for the same
    /// reason. It was the absolute one, so the home directory was in every
    /// repository's text and on no card: `home`, or any piece of the login
    /// name, matched the whole fleet, and `~/code`, read off a card, matched
    /// nothing.
    pub fn haystack(&self) -> String {
        let path = crate::config::tildify(&self.path);
        let mut s = format!("{} {} {}", self.name, path, self.head);
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

/// What became of one configured root.
///
/// Discovery used to return a list of repositories and nothing else, so a
/// root that contributed none was indistinguishable from one that was not
/// configured: a folder that had been moved, a workspace whose own repository
/// stopped opening, an ignore rule that did not cover the clones. Each of
/// those takes repositories off the grid, and out of every fetch, without a
/// word anywhere. This is the word; [`RootReport::says`] puts it in a phrase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootReport {
    /// The root's path as configured: the key a [`Seed`] carries.
    pub root: String,
    /// Repositories this root put on the grid.
    pub found: usize,
    /// Repositories it reached that another root had already put there: a
    /// narrower root inside it, or the same folder listed twice. A root with
    /// none of its own and some of these has not come up empty.
    pub shared: usize,
    /// What went wrong, as a phrase to follow the root's name. `None` when
    /// nothing did, which is not the same as having found something.
    pub problem: Option<String>,
    /// Repositories inside a workspace that its own repository neither
    /// ignores nor holds as a submodule. The rule for a workspace follows
    /// only what is ignored, so these are not on the grid.
    pub left_out: Vec<PathBuf>,
}

impl RootReport {
    fn new(root: &Root) -> Self {
        RootReport {
            root: root.path.display().to_string(),
            found: 0,
            shared: 0,
            problem: None,
            left_out: Vec::new(),
        }
    }

    /// What there is to tell the user about this root, as a phrase to follow
    /// its name, or `None` when it found repositories and nothing is amiss.
    //
    // Nothing outside the tests calls it yet: the status bar and the Settings
    // row that are to show it are in other files.
    #[allow(dead_code)]
    pub fn says(&self) -> Option<String> {
        if let Some(problem) = &self.problem {
            return Some(problem.clone());
        }
        let nothing = self.found == 0 && self.shared == 0;
        if self.left_out.is_empty() {
            return nothing.then(|| "no repositories found".to_string());
        }
        const NAMED: usize = 3;
        let mut names: Vec<String> = self
            .left_out
            .iter()
            .take(NAMED)
            .map(|path| match path.file_name() {
                Some(name) => name.to_string_lossy().to_string(),
                None => path.display().to_string(),
            })
            .collect();
        if self.left_out.len() > NAMED {
            names.push(format!("{} more", self.left_out.len() - NAMED));
        }
        let count = self.left_out.len();
        let (repositories, are) = if count == 1 { ("repository", "is") } else { ("repositories", "are") };
        Some(format!(
            "{}{count} {repositories} in it {are} not ignored by its own repository, and so \
             {are} left out ({})",
            if nothing { "no repositories found: " } else { "" },
            names.join(", ")
        ))
    }
}

/// Everything discovery has to say: the repositories, and a report for each
/// root in the order the roots were given.
pub struct Survey {
    pub found: Vec<Found>,
    pub roots: Vec<RootReport>,
}

/// Walk `roots` looking for repositories, at most `max_depth` levels down.
/// See [`survey`], which this is without the reports.
//
// The scan asks for the reports as well, so outside the tests nothing calls
// this any more. It stays for a caller that only wants the list.
#[cfg_attr(not(test), allow(dead_code))]
pub fn discover(roots: &[Root], max_depth: usize) -> Vec<Found> {
    survey(roots, max_depth).found
}

/// Walk `roots` looking for repositories, at most `max_depth` levels down, and
/// say what each root came to.
///
/// A repository normally ends the descent, so vendored checkouts and
/// submodules inside a project stay out of sight. A root marked `nested` lifts
/// that for the repository at the root itself: it is opened, and the
/// directories it *ignores* are followed, which is exactly the shape of a
/// workspace full of independent clones. Its submodules are never followed.
/// Inside a clone found that way the normal rule applies again.
///
/// A repository under two roots belongs to the narrower one. The roots used
/// to be walked in the order they were listed, and a repository went to
/// whichever reached it first: add `~/code/clients` with a tag to a
/// configuration that already had `~/code`, which is the only order Settings
/// can produce, and the wider root had claimed every repository before the
/// tagged one was looked at. The tag was shown on the root's row and applied
/// to nothing.
pub fn survey(roots: &[Root], max_depth: usize) -> Survey {
    let mut found = Vec::new();
    let mut seen = HashSet::new();
    let mut reports: Vec<RootReport> = roots.iter().map(RootReport::new).collect();

    let bases: Vec<PathBuf> = roots
        .iter()
        .map(|root| root.path.canonicalize().unwrap_or_else(|_| root.path.clone()))
        .collect();
    // Deepest first. Two roots that hold the same repository lie on one line
    // of ancestors, so the one with more components is the narrower; the sort
    // is stable, so roots that do not overlap keep the order they were given.
    let mut order: Vec<usize> = (0..roots.len()).collect();
    order.sort_by_key(|&at| std::cmp::Reverse(bases[at].components().count()));

    for at in order {
        let (root, base) = (&roots[at], &bases[at]);
        let mut search = Search {
            root,
            max_depth,
            found: &mut found,
            seen: &mut seen,
            report: &mut reports[at],
            owned: HashSet::new(),
        };

        if !is_git_dir(base) {
            // Only here, for the root itself. A folder further down that
            // cannot be read is an ordinary thing to come across, and not
            // what the user pointed at.
            match std::fs::read_dir(base) {
                Ok(_) => search.walk(base, 1),
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                    search.report.problem = Some("does not exist".to_string());
                }
                Err(err) => search.report.problem = Some(format!("cannot be read ({err})")),
            }
            continue;
        }

        if !root.nested {
            search.record(base.clone());
            continue;
        }

        if root.include_self {
            search.record(base.clone());
        }
        match Repository::open(base) {
            Ok(repo) => search.workspace(&repo, base),
            // Without its repository there are no ignore rules to follow and
            // no list of submodules to leave alone. This used to find nothing
            // at all, silently, which took a whole workspace off the grid the
            // day its wrapper stopped opening. So it is searched as the plain
            // folder it has become, and the wrapper gets a card whether or not
            // one was asked for, because a card is where the reason is shown.
            Err(err) => {
                search.report.problem = Some(format!(
                    "its own repository cannot be opened ({}), so it is searched as a plain \
                     folder: a submodule of it would be shown as a clone",
                    err.message()
                ));
                if search.seen.insert(base.clone()) {
                    search.found.push(Found { path: base.clone(), seed: None });
                }
                search.walk(base, 1);
            }
        }
    }

    found.sort_by(|a, b| a.path.cmp(&b.path));
    Survey { found, roots: reports }
}

/// The search of one root.
struct Search<'a> {
    root: &'a Root,
    max_depth: usize,
    found: &'a mut Vec<Found>,
    seen: &'a mut HashSet<PathBuf>,
    report: &'a mut RootReport,
    /// The submodules of a workspace, as absolute paths. Empty for any other
    /// root.
    owned: HashSet<PathBuf>,
}

impl Search<'_> {
    fn record(&mut self, path: PathBuf) {
        let key = path.canonicalize().unwrap_or_else(|_| path.clone());
        if self.seen.insert(key) {
            let seed = Seed {
                root: self.root.path.display().to_string(),
                tag: self.root.tag.clone(),
            };
            self.found.push(Found { path, seed: Some(seed) });
            self.report.found += 1;
        } else {
            self.report.shared += 1;
        }
    }

    /// The normal rule: look in `dir`, whose children are `depth` levels below
    /// the root, and stop at the first repository on the way down.
    fn walk(&mut self, dir: &Path, depth: usize) {
        if depth > self.max_depth {
            return;
        }
        for path in child_dirs(dir) {
            // At every depth. The list was only held against the first level
            // of a workspace, so a submodule at `libs/shared` came through a
            // grouping folder and onto the grid as a clone: tagged, and
            // fast-forwarded off the commit its workspace pins it to.
            if self.owned.contains(&path) {
                continue;
            }
            if is_git_dir(&path) {
                self.record(path);
                continue;
            }
            self.walk(&path, depth + 1);
        }
    }

    /// A workspace: follow what `repo`, the repository at `base`, ignores.
    fn workspace(&mut self, repo: &Repository, base: &Path) {
        match submodule_paths(repo) {
            Ok(paths) => self.owned = paths.into_iter().map(|path| base.join(path)).collect(),
            Err(err) => {
                self.report.problem = Some(format!(
                    "its submodules cannot be listed ({}), so one may be shown as a clone",
                    err.message()
                ));
            }
        }
        self.own_folder(repo, base, base, 1);
    }

    /// One of the workspace repository's own directories: `base` itself, or a
    /// folder below it that it does not ignore. Its children are `depth`
    /// levels below the root.
    ///
    /// Only `base` was ever looked in. A rule of `repos/*` ignores everything
    /// in `repos` and not `repos` itself, so the folder was passed over as the
    /// workspace's own and the clones in it were never asked about: an empty
    /// grid from a root set up the way its switch in Settings describes.
    fn own_folder(&mut self, repo: &Repository, base: &Path, dir: &Path, depth: usize) {
        if depth > self.max_depth {
            return;
        }
        for child in child_dirs(dir) {
            if self.owned.contains(&child) {
                continue;
            }
            let ignored = match ignores(repo, base, &child) {
                Ok(ignored) => ignored,
                Err(err) => {
                    self.report.problem.get_or_insert_with(|| {
                        format!("its ignore rules cannot be read ({})", err.message())
                    });
                    false
                }
            };
            match (ignored, is_git_dir(&child)) {
                (true, true) => self.record(child),
                // Not a repository itself, but the workspace does not own it
                // either: a grouping folder. Carry on under the normal rule.
                (true, false) => self.walk(&child, depth + 1),
                // A clone the workspace has no rule for, or a repository
                // somebody put inside it. Not followed, and said.
                (false, true) => self.report.left_out.push(child),
                (false, false) => self.own_folder(repo, base, &child, depth + 1),
            }
        }
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

/// Do `repo`'s ignore rules match `child`? A directory it ignores is not part
/// of it, which is what makes a parked clone something to follow.
///
/// That is all this says. libgit2 answers from the ignore rules alone: it
/// never looks in the index, with or without a trailing separator on the
/// path, so under a blanket rule such as `*` every directory is "ignored",
/// the workspace's own tracked folders and its submodules included. The
/// comment here used to say that the slash-free form made libgit2 consult the
/// index and report a tracked submodule as not ignored. It does not, and the
/// list of submodules, which that comment called a second safeguard, is the
/// only thing that keeps one out. Hence [`submodule_paths`], and why
/// [`Search::walk`] holds every directory against it.
fn ignores(repo: &Repository, base: &Path, child: &Path) -> Result<bool, git2::Error> {
    let Ok(relative) = child.strip_prefix(base) else { return Ok(false) };
    repo.is_path_ignored(relative)
}

/// Every path at which `repo` holds another repository as its own, relative
/// to its work tree: what `.gitmodules` and HEAD list, and every gitlink in
/// the index. The second covers a submodule added by hand with no entry in
/// `.gitmodules`, and a `.gitmodules` that does not parse, which used to
/// empty the list and let every submodule through. An error only when
/// neither could be read.
fn submodule_paths(repo: &Repository) -> Result<HashSet<PathBuf>, git2::Error> {
    const GITLINK: u32 = 0o160000;
    let listed = repo
        .submodules()
        .map(|modules| modules.iter().map(|m| m.path().to_path_buf()).collect::<Vec<_>>());
    let linked = repo.index().map(|index| {
        index
            .iter()
            .filter(|entry| entry.mode == GITLINK)
            .map(|entry| PathBuf::from(std::ffi::OsStr::from_bytes(&entry.path)))
            .collect::<Vec<_>>()
    });
    match (listed, linked) {
        (Err(err), Err(_)) => Err(err),
        (listed, linked) => Ok(listed
            .into_iter()
            .flatten()
            .chain(linked.into_iter().flatten())
            .collect()),
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

    // ---- where repositories come from --------------------------------------

    fn plain(path: &Path) -> Root {
        Root { path: path.to_path_buf(), nested: false, tag: None, include_self: true }
    }

    /// A workspace as Settings makes one: followed into, its own card hidden.
    fn workspace(path: &Path) -> Root {
        Root { nested: true, include_self: false, ..plain(path) }
    }

    fn tagged(root: Root, tag: &str) -> Root {
        Root { tag: Some(tag.to_string()), ..root }
    }

    /// An empty repository at `dir`: all discovery looks for is the `.git`.
    fn init(dir: &Path) {
        std::fs::create_dir_all(dir).unwrap();
        git(dir, &["init", "-q", "-b", "main"]);
    }

    /// A workspace repository at `dir` with these ignore rules committed.
    fn wrapper(dir: &Path, ignore: &str) {
        init(dir);
        write(&dir.join(".gitignore"), ignore);
        git(dir, &["add", "-f", ".gitignore"]);
        git(dir, &["commit", "-q", "-m", "a workspace"]);
    }

    /// What was found, as paths below `base`.
    fn below(found: &[Found], base: &Path) -> Vec<String> {
        let base = base.canonicalize().unwrap();
        found
            .iter()
            .map(|found| match found.path.strip_prefix(&base) {
                Ok(rest) if rest.as_os_str().is_empty() => ".".to_string(),
                Ok(rest) => rest.display().to_string(),
                Err(_) => found.path.display().to_string(),
            })
            .collect()
    }

    #[test]
    fn a_narrower_root_tags_what_is_under_it_wherever_it_is_listed() {
        let _guard = isolate();
        let code = scratch("disc-narrower-root");
        for repo in ["app", "clients/api", "clients/web"] {
            init(&code.join(repo));
        }
        // The order Settings leaves them in: the root that was there first,
        // then the folder inside it that was added to be tagged.
        let roots = [plain(&code), tagged(plain(&code.join("clients")), "acme")];

        let found = discover(&roots, 3);
        assert_eq!(below(&found, &code), ["app", "clients/api", "clients/web"]);
        let tags: Vec<Option<&str>> = found
            .iter()
            .map(|found| found.seed.as_ref().and_then(|seed| seed.tag.as_deref()))
            .collect();
        assert_eq!(tags, [None, Some("acme"), Some("acme")]);

        // Each root is told apart by what it put on the grid, and the wider
        // one is not said to have missed what the narrower one took.
        let reports = survey(&roots, 3).roots;
        assert_eq!((reports[0].found, reports[0].shared), (1, 2));
        assert_eq!((reports[1].found, reports[1].shared), (2, 0));
        // A root with nothing left to itself has still not come up empty.
        let roots = [plain(&code.join("clients")), plain(&code.join("clients"))];
        let reports = survey(&roots, 3).roots;
        assert_eq!((reports[1].found, reports[1].shared), (0, 2));
        assert_eq!(reports[1].says(), None);
    }

    #[test]
    fn a_root_that_finds_nothing_says_so_and_why() {
        let _guard = isolate();
        let base = scratch("disc-reports");
        init(&base.join("code/app"));
        std::fs::create_dir_all(base.join("empty")).unwrap();
        write(&base.join("file"), "not a folder\n");
        // Clones named one by one, and one that was cloned since.
        let named = base.join("named");
        wrapper(&named, "/a/\n/b/\n");
        for clone in ["a", "b", "c-new"] {
            init(&named.join(clone));
        }
        // No rule for any of them.
        let bare_rules = base.join("no-rules");
        wrapper(&bare_rules, "*.log\n");
        for clone in ["x", "y"] {
            init(&bare_rules.join(clone));
        }

        let roots = [
            plain(&base.join("code")),
            plain(&base.join("empty")),
            plain(&base.join("gone")),
            plain(&base.join("file")),
            workspace(&named),
            workspace(&bare_rules),
        ];
        let survey = survey(&roots, 3);
        assert_eq!(below(&survey.found, &base), ["code/app", "named/a", "named/b"]);

        // One report for each root, in the order they were given, whatever
        // order they were searched in.
        let reports = survey.roots;
        let keys: Vec<&str> = reports.iter().map(|report| report.root.as_str()).collect();
        let given: Vec<String> = roots.iter().map(|root| root.path.display().to_string()).collect();
        assert_eq!(keys, given);

        assert_eq!((reports[0].found, reports[0].says()), (1, None));
        assert_eq!(reports[1].says().as_deref(), Some("no repositories found"));
        assert_eq!(reports[2].says().as_deref(), Some("does not exist"));
        let file = reports[3].says().unwrap();
        assert!(file.starts_with("cannot be read ("), "{file}");

        assert_eq!(reports[4].found, 2);
        assert_eq!(reports[4].left_out, [named.canonicalize().unwrap().join("c-new")]);
        assert_eq!(
            reports[4].says().as_deref(),
            Some(
                "1 repository in it is not ignored by its own repository, and so is left out \
                 (c-new)"
            )
        );
        assert_eq!(
            reports[5].says().as_deref(),
            Some(
                "no repositories found: 2 repositories in it are not ignored by its own \
                 repository, and so are left out (x, y)"
            )
        );
    }

    #[test]
    fn a_workspace_that_ignores_everything_shows_its_clones_and_not_its_submodules() {
        let _guard = isolate();
        let lib = fleet("disc-star-lib");
        let ws = scratch("disc-star-workspace").join("ws");
        // The ignore rule the README gives for a workspace.
        wrapper(&ws, "*\n!.gitignore\n");
        let url = lib.remote.to_str().unwrap();
        // git wants `-f` to add anything under a blanket ignore.
        git(&ws, &["submodule", "add", "-q", "-f", url, "tools"]);
        git(&ws, &["submodule", "add", "-q", "-f", url, "libs/shared"]);
        git(&ws, &["commit", "-q", "-m", "two submodules"]);
        for clone in ["api", "team/jobs", "libs/parked"] {
            init(&ws.join(clone));
        }

        // `libs/shared` used to be found: under `*` libgit2 calls every child
        // ignored, tracked or not, and the list of submodules was only held
        // against the first level.
        let found = discover(&[workspace(&ws)], 3);
        assert_eq!(below(&found, &ws), ["api", "libs/parked", "team/jobs"]);

        // A `.gitmodules` that does not parse used to empty the list, and
        // then the one at the first level came through as well. The index
        // still says which paths are another repository's.
        write(&ws.join(".gitmodules"), "[submodule \"tools\n\tpath = \n");
        assert!(Repository::open(&ws).unwrap().submodules().is_err());
        let found = discover(&[workspace(&ws)], 3);
        assert_eq!(below(&found, &ws), ["api", "libs/parked", "team/jobs"]);
    }

    #[test]
    fn a_workspace_follows_what_it_ignores_below_its_first_level_too() {
        let _guard = isolate();
        let ws = scratch("disc-ignored-below").join("ws");
        // Common, and git itself reports `repos/` as ignored for it.
        wrapper(&ws, "repos/*\n");
        for clone in ["repos/a", "repos/b"] {
            init(&ws.join(clone));
        }
        let found = discover(&[workspace(&ws)], 3);
        assert_eq!(below(&found, &ws), ["repos/a", "repos/b"]);
    }

    #[test]
    fn a_workspace_whose_own_repository_will_not_open_keeps_its_clones() {
        let _guard = isolate();
        let ws = scratch("disc-broken-wrapper").join("ws");
        wrapper(&ws, "*\n!.gitignore\n");
        for clone in ["a", "b"] {
            init(&ws.join(clone));
        }
        std::fs::remove_file(ws.join(".git/HEAD")).unwrap();
        assert!(Repository::open(&ws).is_err());

        // Thirty repositories used to leave the grid here in one scan, with
        // nothing anywhere to say why. The wrapper is shown as well, hidden
        // or not: its card is where the reason can be read.
        let root = tagged(workspace(&ws), "acme");
        let survey = survey(std::slice::from_ref(&root), 3);
        let found = survey.found;
        assert_eq!(below(&found, &ws), [".", "a", "b"]);
        assert!(inspect(&found[0].path).error.is_some());
        assert!(found[0].seed.is_none(), "a card nobody asked for takes no tag");
        assert!(found[1].seed.is_some());
        let said = survey.roots[0].says().expect("and the root says what happened to it");
        assert!(said.starts_with("its own repository cannot be opened ("), "{said}");
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

    #[test]
    fn the_filter_matches_the_path_the_card_shows_and_not_the_home_directory() {
        let _guard = isolate();
        let home = crate::config::home();
        let dir = home.join("code/inspect-haystack");
        let _ = std::fs::remove_dir_all(&dir);
        init(&dir);

        let hay = inspect(&dir).haystack();
        // What the card prints, so what a user would type.
        assert!(hay.contains("~/code/inspect-haystack"), "{hay}");
        // The home directory's own path was in every haystack and on no card:
        // typing any piece of it, the login name included, kept the whole
        // fleet on screen.
        assert!(!hay.contains(&home.display().to_string().to_lowercase()), "{hay}");
        assert!(!hay.contains("test-fixtures"), "{hay}");

        // Outside the home directory the whole path is what the card shows.
        let dir = scratch("inspect-haystack-outside");
        init(&dir);
        let hay = inspect(&dir).haystack();
        assert!(hay.contains(&dir.display().to_string().to_lowercase()), "{hay}");
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
