//! Repository discovery and inspection via libgit2.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use git2::{BranchType, Repository, StatusOptions};
use serde::{Deserialize, Serialize};

use crate::config::Root;
use crate::lang;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Commit {
    pub summary: String,
    pub author: String,
    pub when: i64,
    pub short_id: String,
    /// Written under the identity this repository is configured to commit
    /// as, where that can be told: see [`Activity::split`].
    pub mine: bool,
    /// On no remote: nothing a remote-tracking branch points at reaches it.
    /// [`RepoInfo::unpushed`] says how many such commits there are, and a
    /// number cannot say whether the commit a card names is one of them.
    ///
    /// It comes from the walk that makes that count, so it is false wherever
    /// the count was not made either: in a bare repository, and where the
    /// walk failed, which `history_unreadable` says.
    pub unpushed: bool,
}

impl Commit {
    /// What a card says of `commit`. Whose it is and whether it is anywhere
    /// else are not things a commit knows of itself.
    fn of(commit: &git2::Commit<'_>, mine: bool, unpushed: bool) -> Commit {
        Commit {
            summary: commit.summary().ok().flatten().unwrap_or("").trim().to_string(),
            author: commit.author().name().unwrap_or("unknown").to_string(),
            when: commit.time().seconds(),
            short_id: short(&commit.id()),
            mine,
            unpushed,
        }
    }
}

/// How many days of history a card draws.
pub const ACTIVITY_DAYS: usize = 28;

/// What has been committed lately, for the card to draw.
///
/// A card had the newest commit and nothing behind it, and that cannot tell
/// the last commit of a busy week from the only one this year, or the user's
/// own work from a colleague's.
///
/// It is counted over HEAD and the tip of its upstream together, each commit
/// once. A checkout that is behind has not gone quiet, which is what
/// [`RepoInfo::upstream_when`] is there to say as well.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Activity {
    /// Commits per day as `[mine, others]`, oldest first, the last 24 hours
    /// last. [`ACTIVITY_DAYS`] long, or empty when history could not be read:
    /// a walk that broke off has not found the rest of the month quiet, and
    /// an empty list cannot be drawn as if it had. `reached`, `week` and
    /// `people` are not readings then either.
    ///
    /// A day is 24 hours counted back from the moment of the scan and not a
    /// date on a calendar, so that no time zone comes into it. Which day a
    /// commit falls in is said by its author's clock: work that was rebased
    /// this morning was not written this morning. A commit dated after the
    /// scan, by a clock that runs ahead, is counted in the last day.
    pub days: Vec<[u32; 2]>,
    /// How many of those days, counting back from now, the walk reached
    /// before it hit its limit of commits, or the edge of a shallow clone,
    /// which is as far as its commits go. [`ACTIVITY_DAYS`] when it reached
    /// them all. Days it did not reach are unknown, which is not the same as
    /// quiet: what is counted on one of them is what the walk happened to
    /// read of it, so at least that many were committed, and a zero there is
    /// only nothing read.
    pub reached: usize,
    /// Commits in the last seven days, and how many different people made
    /// them, going by the address each was written under. Both are lower
    /// bounds where `reached` is less than seven.
    pub week: u32,
    pub people: usize,
    /// Whether "mine" could be told from "others" at all. Mine is what was
    /// written under the `user.email` configured here, and that is worth
    /// going by only when it is who git would record. With no address
    /// configured, or with [`RepoInfo::identity_doubt`] set, every commit is
    /// counted under others and no [`Commit`] is `mine`: one colour for
    /// everybody, rather than a colleague's month drawn as the user's own.
    pub split: bool,
    /// The newest commits on HEAD, newest first, at most three: the last
    /// commit and the two before it, in the order `git log` lists them. What
    /// the upstream has on top of HEAD is counted in `days` and is not named
    /// here, because it is not what is checked out.
    pub recent: Vec<Commit>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LangShare {
    pub name: String,
    pub share: f32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// The page that remote has on the web, when its URL says where one is;
    /// see [`web_url`].
    pub web: Option<String>,
    pub local_branches: usize,
    pub stashes: usize,
    pub tracked_files: usize,
    /// Every language among the tracked files, largest share first. The card
    /// draws the first few; the `lang:` filter has to see all of them.
    pub langs: Vec<LangShare>,
    pub last_commit: Option<Commit>,
    /// What has been committed lately. The first of the commits it names is
    /// `last_commit`. In a repository that could not be read (`error`) it is
    /// left as empty as the rest, and [`Activity::days`] says what that means.
    pub activity: Activity,
    /// `user.name` and `user.email` as libgit2 reads the configuration for
    /// this directory. `includeIf gitdir:` makes this differ per repository,
    /// and a misconfigured rule is otherwise silent until something is pushed
    /// under the wrong name.
    ///
    /// It is what is configured, not who git will record, and the card words
    /// it that way. Only git knows the second, and nothing here runs git to
    /// ask.
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
    /// Sort key for "most recent activity first". Not `activity`: that is
    /// the field with the last four weeks in it.
    pub fn last_active(&self) -> i64 {
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
/// both libgit2 and git treat another directory as the work tree: status and
/// the working-tree view are read from there, a place the user never pointed
/// this application at. A `.git` that is a file can likewise name another
/// repository's git directory, and a fetch would be written into that. A
/// directory that arrived under a scan root from somewhere else (an archive, a
/// synced folder) only has to contain one of these to turn "refresh the
/// fleet" into reading, and fetching into, anywhere this user can.
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

/// The commits reachable from `from` that are on no remote, up to `cap` of
/// them. This is `git rev-list <from> --not --remotes`, and how many it finds
/// is the count a card shows.
///
/// They are handed back and not only counted so that the commits a card names
/// can be marked from this walk, without a second one for the same answer.
///
/// A walk that fails is an error and not a count. It used to be a count both
/// ways: a walk that could not start was zero, and one that broke off at a
/// commit it could not read counted the failure itself as a commit.
fn on_no_remote(
    repo: &Repository,
    from: git2::Oid,
    cap: usize,
) -> Result<Vec<git2::Oid>, git2::Error> {
    let mut walk = repo.revwalk()?;
    walk.push(from)?;
    // No remote-tracking refs at all is not an error: then nothing is hidden,
    // and everything counts.
    let _ = walk.hide_glob("refs/remotes/*");
    walk.take(cap).collect()
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
    // By name. A directory is listed in whatever order its filesystem keeps
    // it in, and what a root is said to have left out names the first few in
    // the order they were come across: "(y, x)" on one disk and "(x, y)" on
    // the next.
    children.sort();
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
/// and commits that cannot be counted in [`RepoInfo::history_unreadable`]. A
/// month of history that cannot be walked leaves [`Activity::days`] empty,
/// which no card can draw as a quiet one.
///
/// It used to promise the first and do none of the three for anything past
/// opening the repository. An index libgit2 refused came out as a clean tree
/// with no files in it, and a newest commit lost to a power cut as "no commits
/// yet", on the one screen meant to show which repositories need looking at.
pub fn inspect(path: &Path) -> RepoInfo {
    inspect_at(path, now())
}

/// [`inspect`], told what time it is. The days of an [`Activity`] are counted
/// back from the moment of the scan, and a test of them has to be able to say
/// when that was.
fn inspect_at(path: &Path, now: i64) -> RepoInfo {
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
        web: None,
        local_branches: 0,
        stashes: 0,
        tracked_files: 0,
        langs: Vec::new(),
        last_commit: None,
        activity: Activity::default(),
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

    // The commits HEAD and its upstream are on, which is where the count of
    // what has been committed lately starts from.
    let mut head_commit = None;
    let mut upstream_commit = None;

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
                    // Whose it is and whether it is on a remote are not known
                    // yet. Both are filled in where `activity` is.
                    info.last_commit = Some(Commit::of(&commit, false, false));
                    head_commit = Some(commit.id());
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
                upstream_commit = remote;
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

    // The commits `unpushed` counts, kept so that the ones a card names can
    // be told from the ones a remote has.
    let mut unpushed = Vec::new();
    if !info.bare && !info.unborn {
        let head = repo.head().ok().and_then(|head| head.target());
        if let Some(head) = head {
            match on_no_remote(&repo, head, UNPUSHED_CAP) {
                Ok(found) => {
                    info.unpushed = found.len();
                    unpushed = found;
                }
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
                    Ok(found) if found.is_empty() => {}
                    Ok(_) => info.stray_branches += 1,
                    Err(err) => {
                        info.history_unreadable.get_or_insert_with(|| err.message().to_string());
                    }
                }
            }
        }
    }

    // One remote for both: the page `o` opens is the page of the remote the
    // card names. Origin, or the first there is when nothing is called that.
    let remote_url = repo
        .find_remote("origin")
        .ok()
        .and_then(|r| r.url().ok().map(str::to_string))
        .or_else(|| {
            repo.remotes().ok().and_then(|names| {
                names
                    .iter()
                    .filter_map(|n| n.ok().flatten())
                    .next()
                    .and_then(|n| repo.find_remote(n).ok())
                    .and_then(|r| r.url().ok().map(str::to_string))
            })
        });
    info.remote = remote_url.as_deref().map(prettify_remote);
    info.web = remote_url.as_deref().and_then(web_url);

    // The address that makes a commit the user's own, where there is one to
    // go by: see [`Activity::split`].
    let mut me = None;

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
        info.identity = match (name, email.clone()) {
            (Some(name), Some(email)) => Some(format!("{name} <{email}>")),
            (Some(name), None) => Some(name),
            (None, Some(email)) => Some(email),
            (None, None) => None,
        };
        info.identity_doubt = identity_doubt(&config, &|name| std::env::var_os(name).is_some());
        // Only where it is the address git would record, and an empty one is
        // nobody's.
        if info.identity_doubt.is_none() {
            me = email.filter(|email| !email.is_empty());
        }
    }
    // Nothing here says which credential helper a fetch will use. The card
    // used to, from two exact config keys and a host cut out of the tidied-up
    // remote, with github.com standing in when there was no remote. That named
    // a helper for a repository with no remote and for one fetched over SSH,
    // which asks none, and missed any helper set for a path, a user or a
    // wildcard host. git asks every helper that matches, in order, and offers
    // no way to list them short of running it.

    info.activity = activity_at(&repo, head_commit, upstream_commit, me.as_deref(), &unpushed, now);
    // `last_commit` is the first of the commits named there. By now it is
    // known whose it is and whether a remote has it.
    if let (Some(last), Some(newest)) = (info.last_commit.as_mut(), info.activity.recent.first()) {
        last.mine = newest.mine;
        last.unpushed = newest.unpushed;
    }

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
        let mut counts: HashMap<&'static str, usize> = HashMap::new();
        let mut total = 0usize;
        for entry in index.iter() {
            let path = String::from_utf8_lossy(&entry.path);
            info.tracked_files += 1;
            if let Some(language) = lang::classify(&path) {
                *counts.entry(language).or_insert(0) += 1;
                total += 1;
            }
        }
        if total > 0 {
            let mut shares: Vec<LangShare> = counts
                .into_iter()
                .map(|(name, count)| LangShare {
                    name: name.to_string(),
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

/// Seconds since 1970, which is how a commit is dated.
fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_secs() as i64)
        .unwrap_or(0)
}

const DAY: i64 = 86_400;

/// The days at the end of [`Activity::days`] that `week` and `people` are
/// counted over.
const WEEK: usize = 7;

/// The most commits a scan counts in one repository. Every repository is
/// walked at every scan, and four weeks of a busy one is thousands of commits
/// read to draw twenty-eight bars. A walk that stops here says how far it
/// got, in [`Activity::reached`], and the days it never read are not passed
/// off as days on which nothing was committed.
const ACTIVITY_COMMITS: usize = 200;

/// How many commits a card names: the last one and the two before it.
const RECENT_COMMITS: usize = 3;

/// How many whole days before `now` it was at `when`. A moment after `now` is
/// today's: a commit dated by a clock that runs ahead is recent work, and
/// there is no day after the last for it to go in.
fn days_ago(now: i64, when: i64) -> usize {
    usize::try_from(now.saturating_sub(when) / DAY).unwrap_or(0)
}

/// What has been committed lately, as of `now`: see [`Activity`] for what
/// each figure means. `head` and `upstream` are the commits the two are on,
/// `me` is the address that makes a commit the user's own, where there is one
/// to go by, and `unpushed` is what [`on_no_remote`] found from HEAD.
///
/// The time is handed in because the days are counted back from it, and a
/// test of them has to be able to say when that was.
fn activity_at(
    repo: &Repository,
    head: Option<git2::Oid>,
    upstream: Option<git2::Oid>,
    me: Option<&str>,
    unpushed: &[git2::Oid],
    now: i64,
) -> Activity {
    let mine = |commit: &git2::Commit<'_>| {
        me.is_some_and(|me| commit.author().email_bytes().eq_ignore_ascii_case(me.as_bytes()))
    };
    let mut activity = Activity { split: me.is_some(), ..Activity::default() };

    // A walk of its own, from HEAD alone: what the upstream has on top is not
    // checked out, and a card names what is. However far it gets is kept. A
    // commit that could be read is worth naming when the one under it could
    // not, and when the count below came to nothing.
    let _ = newest_first(repo, head, |commit| {
        let unpushed = unpushed.contains(&commit.id());
        activity.recent.push(Commit::of(commit, mine(commit), unpushed));
        activity.recent.len() < RECENT_COMMITS
    });

    let mut days = vec![[0u32; 2]; ACTIVITY_DAYS];
    let mut people = HashSet::new();
    let mut counted = 0;
    let mut reached = ACTIVITY_DAYS;
    let walked = newest_first(repo, head.into_iter().chain(upstream), |commit| {
        // The first commit that is not counted, either for being older than
        // anything a card draws or for being one more than a scan counts.
        // Whatever the walk has not handed over by now was committed no later
        // than this one, and written no later than it was committed, so
        // every day after this one's has been read in full.
        //
        // That takes a commit to be no older than what it stands on. One
        // that is, made by a clock that was set back or given an old date on
        // purpose, hides what is under it from this walk exactly as it does
        // from `git log --since`.
        let committed = days_ago(now, commit.time().seconds());
        if committed >= ACTIVITY_DAYS || counted == ACTIVITY_COMMITS {
            reached = reached.min(committed);
            return false;
        }
        counted += 1;
        // The edge of a shallow clone, or of a graft: the commit names a
        // parent and libgit2 lists none, because the history was cut there
        // on purpose. What lies under it was not read, any more than what
        // lies past the limit, and its own day may hold more than this one
        // commit.
        if commit.parent_count() == 0 && commit.header_field_bytes("parent").is_ok() {
            reached = reached.min(committed);
        }

        let author = commit.author();
        let written = days_ago(now, author.when().seconds());
        if written < ACTIVITY_DAYS {
            let whose = if mine(commit) { 0 } else { 1 };
            days[ACTIVITY_DAYS - 1 - written][whose] += 1;
        }
        if written < WEEK {
            // The address, however it is capitalised, and the name where a
            // commit was written under no address at all.
            let email = author.email_bytes();
            let who = if email.is_empty() { author.name_bytes() } else { email };
            people.insert(String::from_utf8_lossy(who).to_lowercase());
        }
        true
    });
    // A walk that broke off has counted some of these days and has not found
    // the rest of them quiet, so nothing is said about any.
    if walked.is_ok() {
        activity.week = days[ACTIVITY_DAYS - WEEK..].iter().map(|day| day[0] + day[1]).sum();
        activity.people = people.len();
        activity.days = days;
        activity.reached = reached;
    }
    activity
}

/// Hand `each` the commits reachable from `tips`, newest first by the
/// committer's clock and each of them once, for as long as it answers that it
/// wants more. This is the order `git log` lists them in: of two committed in
/// the same second, the one that was found first goes first.
///
/// libgit2 has a walk in this order, and this is not it. It marks every
/// sorted walk limited, which is its word for reading all of the history
/// before it hands over the first commit. Over the 48 repositories of one
/// machine that came to 479 ms a scan, against 33 ms for a walk of this kind,
/// and over a fixture of 100,000 commits it is 300 ms against half a
/// millisecond: what this reads does not grow with what lies behind it. A
/// commit is read when one of its children has been handed over and more was
/// asked for, so a walk that is stopped has read the tips, what it handed
/// over, the parents of all of that but the last, and nothing behind them.
/// And a commit is all that is read: never a tree, never a diff.
fn newest_first<'repo>(
    repo: &'repo Repository,
    tips: impl IntoIterator<Item = git2::Oid>,
    mut each: impl FnMut(&git2::Commit<'repo>) -> bool,
) -> Result<(), git2::Error> {
    let mut seen = HashSet::new();
    // Every commit read, in the order it was read, and the ones not handed
    // over yet as (when committed, place in `read`). A heap gives up its
    // largest, so the place is reversed for the earlier one to win a tie.
    let mut read: Vec<git2::Commit<'repo>> = Vec::new();
    let mut waiting = BinaryHeap::new();
    let mut found: Vec<git2::Oid> = tips.into_iter().collect();
    loop {
        for id in found.drain(..) {
            if seen.insert(id) {
                let commit = repo.find_commit(id)?;
                waiting.push((commit.time().seconds(), Reverse(read.len())));
                read.push(commit);
            }
        }
        let Some((_, Reverse(at))) = waiting.pop() else { return Ok(()) };
        let commit = &read[at];
        if !each(commit) {
            return Ok(());
        }
        found.extend(commit.parent_ids());
    }
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

/// Hosts that take git over ssh under one name and serve their pages under
/// another.
const SSH_ONLY_HOSTS: &[(&str, &str)] = &[
    ("ssh.github.com", "github.com"),
    ("altssh.gitlab.com", "gitlab.com"),
    ("altssh.bitbucket.org", "bitbucket.org"),
];

/// Where a remote's repository is on the web, for a browser to be sent to, or
/// nothing when the remote does not say.
///
/// A remote's URL is an address for git. That the page for people is at the
/// same host and path is only a convention, but it is one GitHub, GitLab,
/// Bitbucket, Codeberg, Gitea and sourcehut all keep, so that is what this
/// builds: `https://`, the host, the path, without `.git`.
///
/// What it leaves out matters as much:
///
/// - Whoever the URL logs in as, and above all a token written into an
///   `https://` remote. A browser keeps every address it is handed.
/// - The port of an ssh remote, which no web server is listening on. An
///   `http://` remote keeps its port and its `http`: there the remote is a
///   web address already, and the server's own.
///
/// And it answers nothing at all for a remote that is a directory or a
/// `file://` URL, for one that names no repository, and for an ssh remote
/// whose host has no dot in it. That last is a name out of `~/.ssh/config`:
/// which host it stands for is ssh's to know, and a guess would open somebody
/// else's page.
pub fn web_url(remote: &str) -> Option<String> {
    let url = remote.trim();
    // Nothing a browser, or whatever starts it, could read as two things.
    if url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return None;
    }
    let url = url.trim_end_matches('/');
    let url = url.strip_suffix(".git").unwrap_or(url);

    // The scheme, where it is the web's already; who@host:port; the path.
    let (scheme, authority, path) = match url.split_once("://") {
        Some((scheme, rest)) => {
            let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
            match scheme.to_ascii_lowercase().as_str() {
                "https" => (Some("https"), authority, path),
                "http" => (Some("http"), authority, path),
                "ssh" | "git" | "git+ssh" | "ssh+git" => (None, authority, path),
                // `file://`, and anything else nobody browses.
                _ => return None,
            }
        }
        // `who@host:path`, the way scp writes it. Git takes a colon that
        // comes after a slash to be part of a directory's name, and so does
        // this.
        None => {
            let (authority, path) = url.split_once(':')?;
            if authority.contains('/') {
                return None;
            }
            (None, authority, path)
        }
    };
    let host = authority.rsplit_once('@').map_or(authority, |(_, host)| host);
    let host = host.to_ascii_lowercase();
    let path = path.trim_matches('/');
    if host.is_empty() || path.is_empty() || host.starts_with("git-codecommit.") {
        return None;
    }

    let Some(scheme) = scheme else {
        let host = host.split_once(':').map_or(host.as_str(), |(host, _port)| host);
        let named = host.contains('.')
            && host.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-');
        if !named {
            return None;
        }
        // Azure DevOps writes an ssh remote as `v3/organisation/project/
        // repository` and the page as something else again.
        if host == "ssh.dev.azure.com" || host == "vs-ssh.visualstudio.com" {
            let parts: Vec<&str> = path.split('/').collect();
            return match parts.as_slice() {
                ["v3", organisation, project, repository] => Some(format!(
                    "https://dev.azure.com/{organisation}/{project}/_git/{repository}"
                )),
                _ => None,
            };
        }
        let host = SSH_ONLY_HOSTS
            .iter()
            .find(|(ssh, _)| *ssh == host)
            .map_or(host, |(_, web)| web);
        return Some(format!("https://{host}/{path}"));
    };
    Some(format!("{scheme}://{host}/{path}"))
}

/// What to call the site a page from [`web_url`] is on: the name it goes by
/// where everybody knows it by one, and its host otherwise. A company's own
/// GitHub or GitLab gets its host, because "Open on GitHub" would name the
/// wrong place.
pub fn web_site(page: &str) -> &str {
    let rest = page.split_once("://").map_or(page, |(_, rest)| rest);
    let host = rest.split('/').next().unwrap_or(rest);
    let host = host
        .rsplit_once(':')
        .filter(|(_, port)| !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()))
        .map_or(host, |(host, _port)| host);
    match host {
        "github.com" | "gist.github.com" => "GitHub",
        "gitlab.com" => "GitLab",
        "bitbucket.org" => "Bitbucket",
        "codeberg.org" => "Codeberg",
        "dev.azure.com" => "Azure DevOps",
        _ => host,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::*;

    #[test]
    fn a_remote_becomes_the_page_a_browser_can_open() {
        let cases: &[(&str, Option<&str>)] = &[
            // The everyday spellings of one repository.
            ("https://github.com/acme/widgets.git", Some("https://github.com/acme/widgets")),
            ("https://github.com/acme/widgets", Some("https://github.com/acme/widgets")),
            ("https://github.com/acme/widgets.git/", Some("https://github.com/acme/widgets")),
            ("git@github.com:acme/widgets.git", Some("https://github.com/acme/widgets")),
            ("ssh://git@github.com/acme/widgets.git", Some("https://github.com/acme/widgets")),
            ("git+ssh://git@github.com/acme/widgets.git", Some("https://github.com/acme/widgets")),
            ("git://git.example.org/acme/widgets.git", Some("https://git.example.org/acme/widgets")),
            ("  git@github.com:acme/widgets.git\n", Some("https://github.com/acme/widgets")),
            ("HTTPS://github.com/acme/widgets.git", Some("https://github.com/acme/widgets")),
            // A host is the same host however it is capitalised. A path is not.
            ("git@GitHub.com:Acme/Widgets.git", Some("https://github.com/Acme/Widgets")),
            // An ssh port is ssh's, and no web server is listening on it. An
            // http remote's port is the web server's own, and so is its
            // choice of http.
            (
                "ssh://git@git.example.org:2222/acme/widgets.git",
                Some("https://git.example.org/acme/widgets"),
            ),
            (
                "http://git.example.org:3000/acme/widgets.git",
                Some("http://git.example.org:3000/acme/widgets"),
            ),
            ("http://localhost:3000/acme/widgets.git", Some("http://localhost:3000/acme/widgets")),
            // Whoever the remote logs in as is not the browser's to be told,
            // a token least of all: a browser keeps what it is handed.
            (
                "https://jm:ghp_0123456789abcdef@github.com/acme/widgets.git",
                Some("https://github.com/acme/widgets"),
            ),
            (
                "https://jm:p@ss@github.com/acme/widgets.git",
                Some("https://github.com/acme/widgets"),
            ),
            (
                "https://acme@dev.azure.com/acme/Shop/_git/widgets",
                Some("https://dev.azure.com/acme/Shop/_git/widgets"),
            ),
            // Subgroups, a namespace with a tilde, a path from the root.
            (
                "git@gitlab.com:acme/platform/widgets.git",
                Some("https://gitlab.com/acme/platform/widgets"),
            ),
            ("git@git.sr.ht:~jm/widgets", Some("https://git.sr.ht/~jm/widgets")),
            ("git@git.example.org:/acme/widgets.git", Some("https://git.example.org/acme/widgets")),
            // Hosts that take ssh under one name and serve pages under another.
            (
                "git@ssh.dev.azure.com:v3/acme/Shop/widgets",
                Some("https://dev.azure.com/acme/Shop/_git/widgets"),
            ),
            (
                "acme@vs-ssh.visualstudio.com:v3/acme/Shop/widgets",
                Some("https://dev.azure.com/acme/Shop/_git/widgets"),
            ),
            ("git@ssh.dev.azure.com:acme/widgets", None),
            (
                "ssh://git@ssh.github.com:443/acme/widgets.git",
                Some("https://github.com/acme/widgets"),
            ),
            (
                "ssh://git@altssh.gitlab.com:443/acme/widgets.git",
                Some("https://gitlab.com/acme/widgets"),
            ),
            // A directory is not on the web, whatever it is called.
            ("/srv/git/widgets.git", None),
            ("../widgets", None),
            ("./with:colon/widgets", None),
            ("file:///srv/git/widgets.git", None),
            // A name out of ~/.ssh/config. Which host it stands for is ssh's
            // to know, and a guess would open somebody else's page.
            ("work:acme/widgets.git", None),
            ("git@github-work:acme/widgets.git", None),
            ("ssh://git@forge/acme/widgets.git", None),
            // Git is served here and nothing else is.
            ("https://git-codecommit.eu-north-1.amazonaws.com/v1/repos/widgets", None),
            ("ssh://git-codecommit.eu-north-1.amazonaws.com/v1/repos/widgets", None),
            // No repository named, no host named, a transport nobody browses.
            ("", None),
            ("https://github.com", None),
            ("https://github.com/", None),
            ("git@github.com:", None),
            ("https:///acme/widgets", None),
            ("ftp://git.example.org/acme/widgets.git", None),
            // Nothing a browser would read as more than one thing.
            ("https://github.com/acme/wid gets", None),
            ("https://github.com/acme/wid\tgets", None),
            ("git@git hub.com:acme/widgets", None),
        ];
        for (remote, page) in cases {
            assert_eq!(web_url(remote).as_deref(), *page, "{remote:?}");
        }
    }

    #[test]
    fn a_page_is_called_by_the_name_its_site_goes_by() {
        for (page, site) in [
            ("https://github.com/acme/widgets", "GitHub"),
            ("https://gist.github.com/0123abcd", "GitHub"),
            ("https://gitlab.com/acme/platform/widgets", "GitLab"),
            ("https://bitbucket.org/acme/widgets", "Bitbucket"),
            ("https://codeberg.org/acme/widgets", "Codeberg"),
            ("https://dev.azure.com/acme/Shop/_git/widgets", "Azure DevOps"),
            // A company's own GitHub is not GitHub the site: say which.
            ("https://github.example.org/acme/widgets", "github.example.org"),
            ("http://git.example.org:3000/acme/widgets", "git.example.org"),
            ("http://localhost:3000/acme/widgets", "localhost"),
        ] {
            assert_eq!(web_site(page), site, "{page}");
        }
    }

    #[test]
    fn a_repository_knows_the_page_of_the_remote_its_card_names() {
        let _guard = isolate();
        let dir = scratch("git-web-page");
        git(&dir, &["init", "-q", "-b", "main"]);
        commit_file(&dir, "a.txt", "a\n", "one");
        assert_eq!(inspect(&dir).web, None, "no remote, no page");

        // Not origin, but the only one there is.
        git(&dir, &["remote", "add", "upstream", "git@git.example.org:acme/widgets.git"]);
        let info = inspect(&dir);
        assert_eq!(info.remote.as_deref(), Some("git.example.org/acme/widgets"));
        assert_eq!(info.web.as_deref(), Some("https://git.example.org/acme/widgets"));

        // Origin wins once there is one, for the page as for the name.
        git(&dir, &["remote", "add", "origin", "https://jm:secret@forge.example.org/jm/widgets.git"]);
        let info = inspect(&dir);
        assert_eq!(info.remote.as_deref(), Some("forge.example.org/jm/widgets"));
        assert_eq!(info.web.as_deref(), Some("https://forge.example.org/jm/widgets"));

        // Shorthand that git rewrites is followed to where git goes.
        git(&dir, &["config", "url.git@git.example.org:.insteadOf", "eg:"]);
        git(&dir, &["remote", "set-url", "origin", "eg:acme/gadgets.git"]);
        let info = inspect(&dir);
        assert_eq!(info.remote.as_deref(), Some("git.example.org/acme/gadgets"));
        assert_eq!(info.web.as_deref(), Some("https://git.example.org/acme/gadgets"));

        // A clone of a directory has a remote and no page.
        let f = fleet("git-web-page-local");
        let info = inspect(&f.work);
        assert!(info.remote.is_some());
        assert_eq!(info.web, None);
    }

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

    /// Who git itself says a commit made here would be recorded as: its
    /// answer is "Name <mail> 1700000000 +0000", and this is the part before
    /// the time.
    fn recorded_as(repo: &Path) -> String {
        let said = git(repo, &["var", "GIT_AUTHOR_IDENT"]);
        match said.split_once('>') {
            Some((who, _)) => format!("{who}>"),
            None => said,
        }
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

    // ---- what has been committed lately ------------------------------------

    const ANN: (&str, &str) = ("Ann Other", "ann@example.invalid");
    const BEN: (&str, &str) = ("Ben Elsewhere", "ben@example.invalid");
    /// Who the kit's git config commits as, and so whose commits are "mine"
    /// in a fixture that configures nobody of its own.
    const ME: (&str, &str) = ("Fixture", "fixture@example.invalid");

    /// The moment these tests scan at, a long way ahead of the real clock.
    /// Every commit that is meant to be counted is given its date by hand,
    /// back from here. What a fixture commits without one, as `fleet()` does
    /// its first, is dated by the real clock and so is older than anything a
    /// card draws.
    fn scan_time() -> i64 {
        now() + 400 * DAY
    }

    /// A scan at `now`, held to what every scan promises: the commit a card
    /// has always shown is the first of the ones it names.
    fn scan(path: &Path, now: i64) -> RepoInfo {
        let info = inspect_at(path, now);
        assert_eq!(
            format!("{:?}", info.last_commit.as_ref()),
            format!("{:?}", info.activity.recent.first())
        );
        info
    }

    /// [`ACTIVITY_DAYS`] quiet days, but for `busy`: (days back from the scan,
    /// `[mine, others]`).
    fn days(busy: &[(usize, [u32; 2])]) -> Vec<[u32; 2]> {
        let mut days = vec![[0, 0]; ACTIVITY_DAYS];
        for (ago, count) in busy {
            days[ACTIVITY_DAYS - 1 - ago] = *count;
        }
        days
    }

    /// What the commits a card names are called, newest first.
    fn named(info: &RepoInfo) -> Vec<&str> {
        info.activity.recent.iter().map(|commit| commit.summary.as_str()).collect()
    }

    /// The same, with whether each is on no remote.
    fn marked(info: &RepoInfo) -> Vec<(&str, bool)> {
        info.activity
            .recent
            .iter()
            .map(|commit| (commit.summary.as_str(), commit.unpushed))
            .collect()
    }

    #[test]
    fn a_commit_is_counted_on_the_day_it_was_written_in_days_back_from_the_scan() {
        let _guard = isolate();
        let now = scan_time();
        let dir = scratch("activity-days");
        git(&dir, &["init", "-q", "-b", "main"]);
        // Seconds before the scan, oldest first, which is the order they have
        // to be made in.
        let made = [
            40 * DAY,
            // Twenty-eight days to the second is the twenty-ninth day back,
            // and no card draws that one.
            28 * DAY,
            28 * DAY - 1,
            27 * DAY,
            27 * DAY - 1,
            2 * DAY,
            DAY + 1,
            // Twenty-four hours to the second is yesterday's.
            DAY,
            DAY - 1,
            60,
            0,
            // Dated after the scan, by an hour and by three days: a clock
            // that runs ahead, or one that was set wrong. It is recent work
            // all the same.
            -3600,
            -3 * DAY,
        ];
        for ago in made {
            commit_as(&dir, ME, now - ago, &format!("{ago} s before the scan"));
        }

        let activity = scan(&dir, now).activity;
        assert_eq!(
            activity.days,
            days(&[(27, [2, 0]), (26, [1, 0]), (2, [1, 0]), (1, [2, 0]), (0, [5, 0])])
        );
        assert_eq!(activity.reached, ACTIVITY_DAYS);
        assert_eq!((activity.week, activity.people), (8, 1));

        // And a scan that is not told the time takes the real one.
        let dir = scratch("activity-days-now");
        git(&dir, &["init", "-q", "-b", "main"]);
        commit_as(&dir, ME, super::now() - 3 * DAY - 60, "three days ago by the real clock");
        assert_eq!(inspect(&dir).activity.days, days(&[(3, [1, 0])]));
    }

    #[test]
    fn work_that_was_made_again_today_is_counted_on_the_day_it_was_written() {
        let _guard = isolate();
        let now = scan_time();
        let dir = scratch("activity-rebased");
        git(&dir, &["init", "-q", "-b", "main"]);
        commit_as(&dir, ME, now - 3 * DAY, "under the branch");
        // Ann's branch, rebased here an hour ago. The committer is the user
        // and the committer's clock says today for all three of its commits.
        // The author is still Ann, and her clock says forty days ago, ten
        // and two.
        let ann = format!("{} <{}>", ANN.0, ANN.1);
        for (i, written) in [40 * DAY, 10 * DAY, 2 * DAY].into_iter().enumerate() {
            let date = format!("@{} +0000", now - written);
            let message = format!("written {} days ago", written / DAY);
            let commit =
                ["commit", "-q", "--allow-empty", "--author", &ann, "--date", &date, "-m", &message];
            git_at(&dir, now - 3600 + i as i64, &commit);
        }

        let info = scan(&dir, now);
        // The one written forty days ago is on no day a card draws. The walk
        // goes by when a commit was committed, so it goes on past that one
        // to the commit underneath. And whose a commit is goes by who wrote
        // it: these are Ann's, whoever committed them, and she is the second
        // of the week's two people.
        assert_eq!(info.activity.days, days(&[(10, [0, 1]), (3, [1, 0]), (2, [0, 1])]));
        assert_eq!((info.activity.week, info.activity.people), (2, 2));
        assert_eq!(
            named(&info),
            ["written 2 days ago", "written 10 days ago", "written 40 days ago"]
        );
        // The name on a card has always been the author's, and how old the
        // card says a commit is has always gone by when it was committed.
        // Neither has changed.
        let last = info.last_commit.expect("there are commits");
        assert_eq!((last.author.as_str(), last.when, last.mine), ("Ann Other", now - 3598, false));
    }

    #[test]
    fn the_users_own_commits_are_told_from_everybody_elses() {
        let _guard = isolate();
        let now = scan_time();
        let dir = scratch("activity-mine");
        git(&dir, &["init", "-q", "-b", "main"]);
        commit_as(&dir, ANN, now - 2 * DAY - 30, "ann, two days ago");
        commit_as(&dir, ME, now - 2 * DAY - 20, "mine, two days ago");
        // An address is the same address however it is capitalised.
        let capitals = ("Fixture", "Fixture@Example.INVALID");
        commit_as(&dir, capitals, now - 2 * DAY - 10, "mine, in capitals");
        commit_as(&dir, BEN, now - DAY - 10, "ben, yesterday");
        commit_as(&dir, ME, now - 60, "mine, just now");

        let info = scan(&dir, now);
        assert_eq!(info.identity.as_deref(), Some("Fixture <fixture@example.invalid>"));
        assert_eq!(info.identity_doubt, None);
        assert!(info.activity.split);
        assert_eq!(info.activity.days, days(&[(2, [2, 1]), (1, [0, 1]), (0, [1, 0])]));
        let whose: Vec<(&str, bool)> = info
            .activity
            .recent
            .iter()
            .map(|commit| (commit.summary.as_str(), commit.mine))
            .collect();
        assert_eq!(
            whose,
            [("mine, just now", true), ("ben, yesterday", false), ("mine, in capitals", true)]
        );
        assert!(info.last_commit.is_some_and(|commit| commit.mine));
    }

    #[test]
    fn nobodys_commits_are_called_the_users_own_when_who_that_is_is_in_doubt() {
        let _guard = isolate();
        let now = scan_time();
        let f = fleet("activity-doubt");
        commit_as(&f.work, ME, now - DAY, "mine, by user.email");
        commit_as(&f.work, ANN, now - 3600, "ann's");
        let info = scan(&f.work, now);
        assert!(info.activity.split);
        assert_eq!(info.activity.days, days(&[(1, [1, 0]), (0, [0, 1])]));

        // `author.email` outranks `user.email`: the address libgit2 read is
        // not the one git would record, and a card that went by it would draw
        // the user's commits as somebody else's and the other way round.
        git(&f.work, &["config", "author.email", "author@elsewhere.example"]);
        let info = scan(&f.work, now);
        assert!(info.identity_doubt.is_some());
        assert!(!info.activity.split);
        assert_eq!(info.activity.days, days(&[(1, [0, 1]), (0, [0, 1])]));
        assert!(info.activity.recent.iter().all(|commit| !commit.mine));
        // Counted all the same, and still by two people.
        assert_eq!((info.activity.week, info.activity.people), (2, 2));

        // An address that is empty is nobody's, a commit written under none
        // included.
        git(&f.work, &["config", "--unset", "author.email"]);
        git(&f.work, &["config", "user.email", ""]);
        commit_as(&f.work, ("Nobody Inparticular", ""), now - 60, "no address");
        let info = scan(&f.work, now);
        assert_eq!(info.identity_doubt, None);
        assert!(!info.activity.split);
        assert_eq!(info.activity.days, days(&[(1, [0, 1]), (0, [0, 2])]));
        assert!(info.activity.recent.iter().all(|commit| !commit.mine));

        // And no `user.email` at all, which no fixture can be without: the
        // kit's own configuration has one.
        let repo = Repository::open(&f.work).unwrap();
        let head = repo.head().unwrap().target();
        let activity = activity_at(&repo, head, None, None, &[], now);
        assert!(!activity.split);
        assert_eq!(activity.days, days(&[(1, [0, 1]), (0, [0, 2])]));
        assert!(activity.recent.iter().all(|commit| !commit.mine));
    }

    #[test]
    fn a_week_is_its_commits_and_the_people_who_wrote_them() {
        let _guard = isolate();
        let now = scan_time();
        let dir = scratch("activity-week");
        git(&dir, &["init", "-q", "-b", "main"]);
        // Before the week, and seven days to the second is the eighth day
        // back: both are counted on their days, and Ben is not one of the
        // week's people.
        commit_as(&dir, BEN, now - 9 * DAY, "ben, nine days ago");
        commit_as(&dir, BEN, now - 7 * DAY, "ben, a second too early");
        commit_as(&dir, ANN, now - 7 * DAY + 1, "ann, inside the week");
        // One person, however her address is capitalised, in letters ASCII
        // has no capitals for as well.
        let (small, capitals) = ("åsa@example.invalid", "ÅSA@Example.Invalid");
        commit_as(&dir, ("Åsa Påhittad", small), now - 4 * DAY - 120, "åsa");
        commit_as(&dir, ("Åsa Påhittad", capitals), now - 4 * DAY - 60, "åsa, in capitals");
        commit_as(&dir, ("Ann Other", "ANN@Example.Invalid"), now - 3 * DAY, "ann, in capitals");
        // No address at all, and then the name has to do.
        commit_as(&dir, ("Nobody Inparticular", ""), now - 2 * DAY, "no address");
        commit_as(&dir, ("Nobody Inparticular", ""), now - 2 * DAY + 60, "no address, again");
        commit_as(&dir, ("Somebody Else", ""), now - DAY, "no address, somebody else");
        commit_as(&dir, ME, now - 60, "mine");

        let activity = scan(&dir, now).activity;
        assert_eq!(
            activity.days,
            days(&[
                (9, [0, 1]),
                (7, [0, 1]),
                (6, [0, 1]),
                (4, [0, 2]),
                (3, [0, 1]),
                (2, [0, 1]),
                (1, [0, 2]),
                (0, [1, 0]),
            ])
        );
        assert_eq!((activity.week, activity.people), (8, 5));
    }

    #[test]
    fn a_checkout_that_is_behind_has_not_gone_quiet() {
        let _guard = isolate();
        let now = scan_time();
        let f = fleet("activity-behind");
        // Somebody pushes, and this clone takes it.
        commit_as(&f.seed, ANN, now - 5 * DAY, "shared");
        git(&f.seed, &["push", "-q", "origin", "main"]);
        git(&f.work, &["pull", "-q", "--ff-only"]);
        // They push twice more, and this clone only fetches.
        commit_as(&f.seed, ANN, now - 2 * DAY, "theirs, two days ago");
        commit_as(&f.seed, BEN, now - DAY, "theirs, yesterday");
        git(&f.seed, &["push", "-q", "origin", "main"]);
        git(&f.work, &["fetch", "-q"]);

        let info = scan(&f.work, now);
        assert_eq!((info.ahead, info.behind), (0, 2));
        // What both sides hold is there once.
        assert_eq!(info.activity.days, days(&[(5, [0, 1]), (2, [0, 1]), (1, [0, 1])]));
        assert_eq!((info.activity.week, info.activity.people), (3, 2));
        // The commits a card names are the ones that are checked out.
        assert_eq!(named(&info), ["shared", "first"]);

        // Work of its own on top. Neither side holds the other now, and each
        // commit is still counted once.
        commit_as(&f.work, ME, now - 3600, "mine, an hour ago");
        let info = scan(&f.work, now);
        assert_eq!((info.ahead, info.behind), (1, 2));
        assert_eq!(
            info.activity.days,
            days(&[(5, [0, 1]), (2, [0, 1]), (1, [0, 1]), (0, [1, 0])])
        );
        assert_eq!(named(&info), ["mine, an hour ago", "shared", "first"]);

        // On no branch there is no upstream to be behind.
        git(&f.work, &["checkout", "-q", "--detach"]);
        let info = scan(&f.work, now);
        assert_eq!(info.activity.days, days(&[(5, [0, 1]), (0, [1, 0])]));
    }

    #[test]
    fn a_walk_that_stops_at_its_limit_says_how_far_it_got() {
        let _guard = isolate();
        let now = scan_time();
        // A commit an hour for `count` hours, oldest first, the newest a
        // minute before the scan.
        let hourly = |count: usize| -> Vec<i64> {
            (0..count as i64).rev().map(|hours| now - hours * 3600 - 60).collect()
        };
        // Commits on each day, the last 24 hours first.
        fn per_day(activity: &Activity) -> Vec<u32> {
            activity.days.iter().rev().map(|day| day[0] + day[1]).collect()
        }
        let history = |name: &str, times: &[i64]| -> Activity {
            let dir = scratch(name);
            git(&dir, &["init", "-q", "-b", "main"]);
            commit_run(&dir, "main", ANN, times);
            scan(&dir, now).activity
        };

        // Fifty more than a scan counts. What it counted is eight whole days
        // and the first eight hours of the ninth.
        let activity = history("activity-limit", &hourly(ACTIVITY_COMMITS + 50));
        let counted = per_day(&activity);
        assert_eq!(counted[..8], [24; 8]);
        assert_eq!(counted[8], 8, "the day it stopped in, as far as that was read");
        assert_eq!(counted[9..], [0; ACTIVITY_DAYS - 9]);
        assert_eq!(activity.reached, 8);
        assert_eq!((activity.week, activity.people), (7 * 24, 1));
        assert!(activity.days.iter().all(|day| day[0] == 0), "none of them is the user's");

        // Exactly as many as a scan counts, and nothing under them that a
        // card would draw: the limit was met and cut nothing off, so every
        // day was reached.
        let activity = history("activity-limit-met", &hourly(ACTIVITY_COMMITS));
        assert_eq!(per_day(&activity).iter().sum::<u32>(), ACTIVITY_COMMITS as u32);
        assert_eq!(activity.reached, ACTIVITY_DAYS);
        let mut times = vec![now - 60 * DAY];
        times.extend(hourly(ACTIVITY_COMMITS));
        let activity = history("activity-limit-met-old", &times);
        assert_eq!(per_day(&activity).iter().sum::<u32>(), ACTIVITY_COMMITS as u32);
        assert_eq!(activity.reached, ACTIVITY_DAYS);

        // One commit more, twelve days back. It is the newest that was not
        // counted, and anything else that was not is older still, so the
        // days between it and the rest are known to be quiet.
        let mut times = vec![now - 12 * DAY - 60];
        times.extend(hourly(ACTIVITY_COMMITS));
        let activity = history("activity-limit-gap", &times);
        assert_eq!(per_day(&activity).iter().sum::<u32>(), ACTIVITY_COMMITS as u32);
        assert_eq!(activity.reached, 12);
    }

    #[test]
    fn the_newest_commits_are_named_in_the_order_git_log_lists_them() {
        let _guard = isolate();
        let now = scan_time();
        // The first three of `git log`, as the short ids a card shows.
        fn git_log(dir: &Path) -> Vec<String> {
            git(dir, &["log", "-3", "--format=%H"]).lines().map(|id| id[..7].to_string()).collect()
        }
        fn ids(info: &RepoInfo) -> Vec<String> {
            info.activity.recent.iter().map(|commit| commit.short_id.clone()).collect()
        }

        // A branch that was worked on after the main line was last touched,
        // and then merged: the two commits before the merge are both on its
        // second parent's side.
        let dir = scratch("activity-recent-merge");
        git(&dir, &["init", "-q", "-b", "main"]);
        commit_as(&dir, ME, now - 10 * DAY, "root");
        git(&dir, &["checkout", "-q", "-b", "side"]);
        commit_as(&dir, ANN, now - 4 * DAY, "side, one");
        commit_as(&dir, ANN, now - 2 * DAY, "side, two");
        git(&dir, &["checkout", "-q", "main"]);
        commit_as(&dir, ME, now - 5 * DAY, "main line");
        git_at(&dir, now - DAY, &["merge", "-q", "--no-ff", "-m", "merge side", "side"]);
        let info = scan(&dir, now);
        assert_eq!(named(&info), ["merge side", "side, two", "side, one"]);
        assert_eq!(ids(&info), git_log(&dir));
        // A merge is a commit like any other: on its day, and its author's.
        assert_eq!(
            info.activity.days,
            days(&[(10, [1, 0]), (5, [1, 0]), (4, [0, 1]), (2, [0, 1]), (1, [1, 0])])
        );

        // Everything in one second, which is how a script commits. Git lists
        // a merge's first parent ahead of its second then, and what was found
        // first ahead of what was found later.
        let dir = scratch("activity-recent-ties");
        git(&dir, &["init", "-q", "-b", "main"]);
        let second = now - DAY;
        commit_as(&dir, ME, second, "root");
        git(&dir, &["checkout", "-q", "-b", "side"]);
        commit_as(&dir, ANN, second, "side");
        git(&dir, &["checkout", "-q", "main"]);
        commit_as(&dir, ME, second, "main line");
        git_at(&dir, second, &["merge", "-q", "--no-ff", "-m", "merge side", "side"]);
        let info = scan(&dir, now);
        assert_eq!(named(&info), ["merge side", "main line", "side"]);
        assert_eq!(ids(&info), git_log(&dir));
        commit_as(&dir, ME, second, "on top");
        let info = scan(&dir, now);
        assert_eq!(named(&info), ["on top", "merge side", "main line"]);
        assert_eq!(ids(&info), git_log(&dir));

        // It is the committer's clock that git goes by. The main line here
        // was written before the side branch and committed after it, as a
        // cherry-pick is, and is listed ahead of it.
        let dir = scratch("activity-recent-committed");
        git(&dir, &["init", "-q", "-b", "main"]);
        commit_as(&dir, ME, now - 10 * DAY, "root");
        git(&dir, &["checkout", "-q", "-b", "side"]);
        commit_as(&dir, ANN, now - 2 * DAY, "side");
        git(&dir, &["checkout", "-q", "main"]);
        let written = format!("@{} +0000", now - 9 * DAY);
        let commit = ["commit", "-q", "--allow-empty", "--date", &written, "-m", "main line"];
        git_at(&dir, now - DAY, &commit);
        git_at(&dir, now - 3600, &["merge", "-q", "--no-ff", "-m", "merge side", "side"]);
        let info = scan(&dir, now);
        assert_eq!(named(&info), ["merge side", "main line", "side"]);
        assert_eq!(ids(&info), git_log(&dir));

        // Fewer than three commits, and then that is all of them.
        let dir = scratch("activity-recent-two");
        git(&dir, &["init", "-q", "-b", "main"]);
        commit_as(&dir, ME, now - 2 * DAY, "one");
        commit_as(&dir, ME, now - DAY, "two");
        assert_eq!(named(&scan(&dir, now)), ["two", "one"]);
    }

    #[test]
    fn a_commit_that_is_on_no_remote_is_marked_as_one() {
        let _guard = isolate();
        let now = scan_time();
        let f = fleet("activity-unpushed");
        let info = scan(&f.work, now);
        assert_eq!(marked(&info), [("first", false)], "level with its upstream");

        commit_as(&f.work, ME, now - 2 * DAY, "one");
        commit_as(&f.work, ME, now - DAY, "two");
        let info = scan(&f.work, now);
        assert_eq!((info.ahead, info.unpushed), (2, 2));
        assert_eq!(marked(&info), [("two", true), ("one", true), ("first", false)]);
        assert!(info.last_commit.is_some_and(|commit| commit.unpushed));

        git(&f.work, &["push", "-q", "origin", "main"]);
        let info = scan(&f.work, now);
        assert_eq!(marked(&info), [("two", false), ("one", false), ("first", false)]);
        assert!(info.last_commit.is_some_and(|commit| !commit.unpushed));

        // A branch that was never published, off one that was: no upstream to
        // be ahead of, and its commit is on no remote all the same.
        git(&f.work, &["checkout", "-q", "-b", "topic"]);
        commit_as(&f.work, ME, now - 3600, "three");
        let info = scan(&f.work, now);
        assert_eq!((info.ahead, info.unpushed), (0, 1));
        assert_eq!(marked(&info), [("three", true), ("two", false), ("one", false)]);

        // The remote itself is not a clone with work to push. Nothing is
        // counted there, and nothing is marked.
        let info = scan(&f.remote, now);
        assert!(info.bare);
        assert_eq!(info.unpushed, 0);
        assert_eq!(marked(&info), [("two", false), ("one", false), ("first", false)]);
        assert_eq!(info.activity.days, days(&[(2, [1, 0]), (1, [1, 0])]));
    }

    #[test]
    fn with_no_remote_every_commit_is_on_none() {
        let _guard = isolate();
        let now = scan_time();
        let dir = scratch("activity-no-remote");
        git(&dir, &["init", "-q", "-b", "main"]);
        for ago in [4, 3, 2, 1] {
            commit_as(&dir, ME, now - ago * DAY, &format!("{ago}d ago"));
        }

        let info = scan(&dir, now);
        assert_eq!((info.remote.as_deref(), info.unpushed), (None, 4));
        assert_eq!(marked(&info), [("1d ago", true), ("2d ago", true), ("3d ago", true)]);
        // Nothing else about it needs a remote.
        assert_eq!(
            info.activity.days,
            days(&[(4, [1, 0]), (3, [1, 0]), (2, [1, 0]), (1, [1, 0])])
        );
        assert_eq!(info.activity.reached, ACTIVITY_DAYS);

        // More of them than are counted. The count stops at its cap, and the
        // newest commits are among the ones it got to: the marks are taken
        // from that walk, and it has to have started where a card does.
        let dir = scratch("activity-no-remote-many");
        git(&dir, &["init", "-q", "-b", "main"]);
        let minutes = (0..UNPUSHED_CAP as i64 + 100).rev();
        let times: Vec<i64> = minutes.map(|ago| now - 40 * DAY - ago * 60).collect();
        commit_run(&dir, "main", ME, &times);
        let info = scan(&dir, now);
        assert_eq!(info.unpushed, UNPUSHED_CAP);
        assert_eq!(marked(&info), [("run 1099", true), ("run 1098", true), ("run 1097", true)]);
    }

    #[test]
    fn a_repository_with_no_commits_has_had_a_quiet_month() {
        let _guard = isolate();
        let dir = scratch("activity-unborn");
        git(&dir, &["init", "-q", "-b", "main"]);

        let info = scan(&dir, scan_time());
        assert!(info.unborn);
        assert_eq!(info.error, None);
        assert!(info.last_commit.is_none() && info.activity.recent.is_empty());
        // Nothing was committed, and that is known: quiet, not unread.
        assert_eq!(info.activity.days, days(&[]));
        assert_eq!(info.activity.reached, ACTIVITY_DAYS);
        assert_eq!((info.activity.week, info.activity.people), (0, 0));
    }

    #[test]
    fn a_repository_nobody_has_touched_lately_is_quiet_and_still_names_its_commits() {
        let _guard = isolate();
        let now = scan_time();
        let dir = scratch("activity-old");
        git(&dir, &["init", "-q", "-b", "main"]);
        for ago in [90, 60, 45, 29] {
            commit_as(&dir, ME, now - ago * DAY, &format!("{ago}d ago"));
        }

        let info = scan(&dir, now);
        assert_eq!(info.activity.days, days(&[]));
        assert_eq!(info.activity.reached, ACTIVITY_DAYS);
        assert_eq!((info.activity.week, info.activity.people), (0, 0));
        assert_eq!(named(&info), ["29d ago", "45d ago", "60d ago"]);
    }

    #[test]
    fn history_that_cannot_be_read_is_not_drawn_as_quiet() {
        let _guard = isolate();
        let now = scan_time();
        let dir = scratch("activity-lost");
        git(&dir, &["init", "-q", "-b", "main"]);
        for ago in [5, 4, 3, 2, 1] {
            commit_as(&dir, ME, now - ago * DAY, &format!("{ago}d ago"));
        }
        assert_eq!(scan(&dir, now).activity.days.len(), ACTIVITY_DAYS);
        lose_object(&dir, "HEAD~3");

        let info = scan(&dir, now);
        assert_eq!(info.error, None);
        // Nothing, rather than three days with a commit on them and
        // twenty-five that are called quiet because the walk broke off.
        assert_eq!(info.activity.days, Vec::<[u32; 2]>::new());
        assert_eq!((info.activity.reached, info.activity.week, info.activity.people), (0, 0, 0));
        // The commits it names were all read before the one that is gone.
        assert_eq!(named(&info), ["1d ago", "2d ago", "3d ago"]);

        // Gone from right under the newest, and that one is still named.
        lose_object(&dir, "HEAD~1");
        let info = scan(&dir, now);
        assert_eq!(info.error, None);
        assert_eq!(info.activity.days, Vec::<[u32; 2]>::new());
        assert_eq!(named(&info), ["1d ago"]);

        // A shallow clone stops short on purpose, and that is not damage. But
        // what was committed before its one commit is not known to be
        // nothing: the walk got as far as that commit's day and no further.
        let f = fleet("activity-shallow");
        commit_as(&f.seed, ANN, now - 3 * DAY, "three days ago");
        commit_as(&f.seed, ANN, now - 2 * DAY, "two days ago");
        git(&f.seed, &["push", "-q", "origin", "main"]);
        let url = format!("file://{}", f.remote.display());
        git(&f.root, &["clone", "-q", "--depth", "1", &url, "shallow"]);
        let info = scan(&f.root.join("shallow"), now);
        assert_eq!(info.activity.days, days(&[(2, [0, 1])]));
        assert_eq!(info.activity.reached, 2);
        assert_eq!(named(&info), ["two days ago"]);
        // A history that really begins two days ago is another matter.
        // Nothing is missing under a commit that names no parent.
        let dir = scratch("activity-root");
        git(&dir, &["init", "-q", "-b", "main"]);
        commit_as(&dir, ANN, now - 2 * DAY, "the first there is");
        let info = scan(&dir, now);
        assert_eq!(info.activity.days, days(&[(2, [0, 1])]));
        assert_eq!(info.activity.reached, ACTIVITY_DAYS);
    }

    #[test]
    fn the_days_are_counted_without_reading_what_lies_behind_them() {
        let _guard = isolate();
        let now = scan_time();
        let dir = scratch("activity-no-further");
        git(&dir, &["init", "-q", "-b", "main"]);
        for ago in [50, 28, 2, 1] {
            commit_as(&dir, ME, now - ago * DAY, &format!("{ago}d ago"));
        }
        // The oldest commit, which is nothing a card has any use for. A walk
        // that read all of the history before it handed over any of it would
        // stop here, and it is the reading of all of it that a count made in
        // every repository at every scan cannot afford. This one stops at the
        // commit above it, which is 28 days old to the second, and never asks
        // what that stands on.
        lose_object(&dir, "HEAD~3");

        let info = scan(&dir, now);
        assert_eq!(info.activity.days, days(&[(2, [1, 0]), (1, [1, 0])]));
        assert_eq!(info.activity.reached, ACTIVITY_DAYS);
        assert_eq!(named(&info), ["1d ago", "2d ago", "28d ago"]);
        // The count of what is on no remote is another walk. With no remote
        // to stop it short it does go that far back, and says that it could
        // not.
        assert!(info.history_unreadable.is_some());
    }
}
