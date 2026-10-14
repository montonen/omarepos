//! Fetching: one repository the user pointed at, or a pass across the fleet;
//! and, for one repository the user asks it of with `p`, a fast-forward.
//!
//! A fetch asks the remote what it has and moves the remote-tracking
//! branches, `origin/*`, to match. No branch of the user's moves, nothing in
//! the working tree changes, and nothing is sent. That is all a pass across
//! the fleet ever does: the timer, the catch-up at launch and `Ctrl+Shift+R`
//! fetch and nothing else.
//!
//! The one thing that writes is a fast-forward of the checked-out branch, and
//! only when the user asks for it, one repository at a time, and only where
//! the repository is clean and its branch is simply behind. It was taken out
//! once, timer and all, and brought back for `p` alone: the timer's version
//! is where nearly all the care below came from, an update nobody had asked
//! for landing under whatever the user was doing.
//!
//! Both shell out to `git` rather than going through libgit2. That is
//! deliberate: a user's authentication lives in credential helpers, an SSH
//! agent and `insteadOf` rewrites, none of which libgit2 reads, and an update
//! should run their hooks and filters as their own pull would. Driving the
//! same `git` the user drives means their setup applies unchanged, and a
//! repository they cannot fetch by hand is one omarepos does not pretend it
//! can fetch either.
//!
//! Two things a plain `git fetch` does besides fetching are switched off,
//! and one thing about it cannot be:
//!
//! - It rewrites `FETCH_HEAD`, and goes on into submodules; see [`FETCH`].
//! - It moves `origin/*`. That is what it is for, and it is not nothing:
//!   `git push --force-with-lease` with no value compares the remote against
//!   exactly those refs, so after a pass it no longer protects a commit
//!   somebody pushed since the user last looked. The pass has seen it on
//!   their behalf. git-push(1) says the same of anything that fetches in the
//!   background, and `push.useForceIfIncludes = true` restores the check. It
//!   is not fixed here: fetching to somewhere else (`--prefetch`) would leave
//!   the cards with nothing to read.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use git2::{BranchType, Repository, RepositoryState, StatusOptions};

/// Why a repository was not fetched, or, fetched, not fast-forwarded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Skip {
    Bare,
    /// It has no remote to ask.
    NoRemote,
    /// Its git data points at a work tree or a git directory somewhere else;
    /// see [`crate::git::belongs_here`].
    Elsewhere,
    /// Found since the user last looked, and not yet opened or asked for. A
    /// pass across the fleet does not enter a repository nobody has seen.
    Unseen,
    /// Staged, modified or conflicted files. Only a clean repository is
    /// fast-forwarded: what to do with work in progress is the user's call.
    Dirty,
    /// No branch checked out, so there is nothing to fast-forward.
    Detached,
    /// No commits yet.
    Unborn,
    /// The branch tracks nothing.
    NoUpstream,
    /// The branch has commits its upstream does not: a fast-forward is not
    /// possible, and a merge or a rebase is the user's to make.
    Diverged,
    /// A merge, rebase, bisect or cherry-pick is in progress.
    Busy,
    /// The incoming commits move the pointer of a submodule that is checked
    /// out here. `git merge` moves the pointer and leaves the checkout where
    /// it was, and this module does not update submodules.
    Submodule,
}

impl Skip {
    pub fn reason(self) -> &'static str {
        match self {
            Skip::Bare => "bare repository",
            Skip::NoRemote => "no remote",
            Skip::Elsewhere => "its work tree is not this directory",
            Skip::Unseen => "new here, open it once",
            Skip::Dirty => "uncommitted changes",
            Skip::Detached => "detached HEAD",
            Skip::Unborn => "no commits",
            Skip::NoUpstream => "no upstream",
            Skip::Diverged => "has commits its upstream does not",
            Skip::Busy => "merge or rebase in progress",
            Skip::Submodule => "update moves a submodule",
        }
    }

    /// Whether this is a reason not to fast-forward, given after a fetch, and
    /// not a reason not to fetch at all.
    pub fn after_fetch(self) -> bool {
        !matches!(self, Skip::Bare | Skip::NoRemote | Skip::Elsewhere | Skip::Unseen)
    }
}

#[derive(Debug, Clone)]
pub enum Outcome {
    /// Fetched, and the checked-out branch has nothing waiting for it. Also
    /// where there is nothing to measure: no branch checked out, or one that
    /// tracks nothing.
    UpToDate,
    /// Fetched, and the checked-out branch is this many commits behind what
    /// it tracks. On a pass that only fetches this is said whatever else is
    /// true of the branch, commits of its own included: it is a count of what
    /// is waiting, and bringing it in was not asked for.
    Behind(usize),
    /// Fetched and fast-forwarded by this many commits.
    FastForwarded(usize),
    Skipped(Skip),
    /// Fetched, but not fast-forwarded, and this is why: what git said when
    /// it refused, or what was found that made asking it pointless. Kept apart
    /// from `Failed`: the remote was reached, and "fetch failed" would send
    /// the user looking in the wrong place. Nothing was changed unless the
    /// text says otherwise.
    Refused(String),
    /// The fetch itself failed.
    Failed(String),
}

#[derive(Debug, Clone)]
pub struct Report {
    pub name: String,
    pub path: PathBuf,
    pub outcome: Outcome,
    /// Whether the fetch happened. A repository skipped before it has proved
    /// nothing about its remote, and must not clear the record of a fetch that
    /// failed last time.
    pub fetched: bool,
}

pub enum Event {
    Repo(Box<Report>),
    Done,
}

/// Who chose the repositories in a pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pass {
    /// One repository the user pointed at and asked to fetch, or with `p` to
    /// fetch and fast-forward. Their whole git setup applies, hooks included,
    /// exactly as if they had typed the commands.
    One,
    /// Everything in scope: the timer, the catch-up at launch, and a fetch of
    /// the whole fleet. Nobody picked these repositories one by one and nobody
    /// is watching them, so nothing a repository carries gets to run (see
    /// [`UNATTENDED`]), and nothing is fast-forwarded.
    Fleet,
}

/// Put in front of every git command on a [`Pass::Fleet`]. Each entry closes a
/// way for a repository to run a command of its own choosing:
///
/// - hooks, which a fetch can run too (`reference-transaction`), including a
///   `core.hooksPath` pointing into the tree;
/// - the fsmonitor command, which git runs when it reads the index. A fetch
///   does not read it, and the housekeeping git starts after one does;
/// - the `ext::` transport, which is a command line with a URL's name.
///
/// This is a list of the known ways, not a proof that there are no others: a
/// repository's own config can name commands in more places than a list of
/// overrides can cover.
const UNATTENDED: [&str; 6] = [
    "-c",
    "core.hooksPath=/dev/null",
    "-c",
    "core.fsmonitor=false",
    "-c",
    "protocol.ext.allow=never",
];

/// Set to abandon a pass that is under way. Repositories not yet started are
/// left alone, and one that has been fetched is not then fast-forwarded. A
/// fetch or a fast-forward already running is allowed to finish.
#[derive(Debug, Clone, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub fn stop(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    pub fn stopped(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

#[derive(Debug, Clone)]
pub struct Options {
    /// Fast-forward the checked-out branch after fetching, where that is
    /// safe. Only on a [`Pass::One`]: a pass across the fleet fetches and
    /// does nothing else, whatever this says.
    pub fast_forward: bool,
    pub max_parallel: usize,
    /// How long a fetch may go without anything arriving before it is given
    /// up on. Not how long it may take: see [`FETCH_PATIENCE`].
    pub timeout: Duration,
    pub pass: Pass,
    pub cancel: Cancel,
}

impl Pass {
    /// Fetch for this kind of pass. `quiet` is how long the fetch may go
    /// without anything arriving, which is what `Options::timeout` means; see
    /// [`FETCH`].
    fn fetch(self, path: &Path, quiet: Duration) -> Result<String, String> {
        match self {
            Pass::One => crate::gitcmd::transfer(path, &FETCH, quiet, FETCH_PATIENCE),
            Pass::Fleet => {
                let mut all = UNATTENDED.to_vec();
                all.extend_from_slice(&FETCH);
                crate::gitcmd::transfer_fenced(path, &all, quiet, FETCH_PATIENCE)
            }
        }
    }
}

/// How long a fetch may take while it is still arriving. `Options::timeout`
/// is a minute by default, and was the limit on the whole fetch: right for
/// giving up on a dead network, and a wall for a repository far behind or a
/// slow link, whose fetch was stopped with data still coming in, on every
/// pass, for ever. That limit is now on silence. This one is the backstop for
/// a remote that goes on saying something and never finishes.
const FETCH_PATIENCE: Duration = Duration::from_secs(60 * 60);

/// The fetch, what it is told to say, and the two things a plain `git fetch`
/// does besides fetching.
///
/// `--progress` where this used to say `--quiet`: progress is how a fetch
/// that is slow is told from one that is dead. git reports what the remote is
/// counting and what has been received as it goes, and the runner gives up
/// when that stops instead of when a clock runs out. None of it is shown; on
/// failure the line that complains is still picked out.
///
/// `--no-write-fetch-head`: a fetch rewrites `.git/FETCH_HEAD`, and that file
/// is the user's. `git fetch origin pull/42/head` followed by `git checkout
/// FETCH_HEAD` is how a pull request gets looked at; a pass landing between
/// the two made the second command check out the tip of the main branch
/// instead, without a word. Nothing here reads the file, so nothing here
/// writes it. git has had the switch since 2.29, for callers like this one.
///
/// `--no-recurse-submodules`: when a fetch brings in a commit that moves a
/// submodule's pointer, git goes on to fetch inside the submodule, from the
/// submodule's own remote. That is a second repository contacted and written
/// to, which the documentation says does not happen, and it made the submodule's
/// remote this repository's problem: one that had moved away failed the fetch
/// of a repository that had itself fetched fine.
const FETCH: [&str; 4] = [
    "fetch",
    "--progress",
    "--no-write-fetch-head",
    "--no-recurse-submodules",
];

/// How long a fast-forward may take. The checkout runs the user's filters, and
/// git-lfs among them fetches as it checks out: a large update over a slow
/// link takes as long as it takes, and stopping git in the middle of a
/// checkout leaves the tree half the old commit and half the new. Half an hour
/// is a backstop against a filter that never returns, not an expectation.
const FAST_FORWARD_PATIENCE: Duration = Duration::from_secs(30 * 60);

/// The fast-forward, and nothing but. [`fast_forward`] puts the branch's own
/// switch in front of this and what to merge behind it.
///
/// `--ff-only` refuses a merge that the *command line* asks for. What
/// `git merge` does is also decided by configuration, read before the command
/// line and able to outvote it, and each of the following made it do
/// something else and exit 0.
///
/// `pull.twohead=recursive`: the key names the strategy for merging one head,
/// and a strategy that cannot fast-forward (`ours`, `subtree`) switches
/// `--ff-only` off without a word. git wrote a merge commit that threw
/// upstream's changes away, and the update was reported as done. It is pinned
/// to a strategy that can, by the one name every git version knows: an empty
/// value is an error, and the default's own name is newer than the oldest git
/// this runs on.
///
/// `--no-squash`, `--no-autostash`: a branch's own merge options, which
/// [`fast_forward`] sets aside, said once more on the command line for the
/// branch where that cannot be done. git reads the command line second.
///
/// `--no-overwrite-ignore`: without it git replaces an ignored file that is in
/// the way of an incoming one, silently, and there is no reflog or stash to
/// get it back from. With it git refuses and names the file.
///
/// `merge.autoStash=false`: a user who has turned autostash on would otherwise
/// have this stash and re-apply their work around the update, and this module
/// promises never to stash.
const FAST_FORWARD: [&str; 10] = [
    "-c",
    "merge.autoStash=false",
    "-c",
    "pull.twohead=recursive",
    "merge",
    "--ff-only",
    "--no-squash",
    "--no-autostash",
    "--no-overwrite-ignore",
    "--quiet",
];

/// The whole fast-forward command for the branch called `branch`, or why it
/// will not be run.
///
/// `branch.<name>.mergeoptions` is read by `git merge` ahead of its command
/// line and can hold anything the command line can. `-s ours` there turns
/// `--ff-only` into a merge commit that discards what upstream did;
/// `--squash` stages upstream's whole diff and leaves the branch where it
/// was; `--autostash` stashes the user's work, updates, and re-applies it
/// with conflict markers where the two overlap. All three exit 0. A strategy
/// cannot be taken back by a later argument, so the key itself is set to
/// nothing for this one command.
///
/// One thing those options can hold is a refusal and not a behaviour, and
/// setting the key aside must not lose it: a branch that insists on signed
/// commits still does.
///
/// `-c key=value` ends the key at the first `=`, so the key cannot be spelled
/// for a branch with one in its name. Such a branch is updated only if it has
/// no merge options to set aside.
fn fast_forward(repo: &Repository, branch: &str) -> Result<Vec<String>, String> {
    let key = format!("branch.{branch}.mergeoptions");
    let own = match repo.config().and_then(|config| config.get_string(&key)) {
        Ok(options) => options,
        Err(err) if err.code() == git2::ErrorCode::NotFound => String::new(),
        // Unreadable is not the same as absent.
        Err(err) => return Err(format!("could not read {key}: {}", err.message())),
    };

    let mut args = Vec::new();
    if !branch.contains('=') {
        args.extend(["-c".to_string(), format!("{key}=")]);
    } else if !own.trim().is_empty() {
        return Err(format!(
            "This branch has merge options of its own ({key} = {}), and with `=` in the \
             branch's name they cannot be set aside for one command. Update it by hand.",
            own.trim()
        ));
    }
    args.extend(FAST_FORWARD.iter().map(|arg| arg.to_string()));
    // The last word on it wins, as it does for git.
    let signed = own
        .split_whitespace()
        .rfind(|option| matches!(*option, "--verify-signatures" | "--no-verify-signatures"));
    if signed == Some("--verify-signatures") {
        args.push("--verify-signatures".to_string());
    }
    args.push("@{u}".to_string());
    Ok(args)
}

pub fn spawn(repos: Vec<PathBuf>, options: Options) -> async_channel::Receiver<Event> {
    let (tx, rx) = async_channel::unbounded();

    std::thread::spawn(move || {
        let workers = options
            .max_parallel
            .clamp(1, 8)
            .min(repos.len().max(1));
        let queue = Arc::new(Mutex::new(repos.into_iter()));
        let mut handles = Vec::with_capacity(workers);

        for _ in 0..workers {
            let queue = Arc::clone(&queue);
            let options = options.clone();
            let tx = tx.clone();
            handles.push(std::thread::spawn(move || {
                loop {
                    if options.cancel.stopped() {
                        return;
                    }
                    let next = {
                        let mut guard = match queue.lock() {
                            Ok(guard) => guard,
                            Err(_) => return,
                        };
                        guard.next()
                    };
                    let Some(path) = next else { return };
                    // From the fetch to the fast-forward: the application
                    // does not exit in the gap between the two.
                    let _hold = crate::gitcmd::Hold::take();
                    let report = refresh_one(&path, &options);
                    if tx.send_blocking(Event::Repo(Box::new(report))).is_err() {
                        return;
                    }
                }
            }));
        }
        for handle in handles {
            let _ = handle.join();
        }
        let _ = tx.send_blocking(Event::Done);
    });

    rx
}

fn refresh_one(path: &Path, options: &Options) -> Report {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string());
    let fetched = std::cell::Cell::new(false);
    let report = |outcome| Report {
        name: name.clone(),
        path: path.to_path_buf(),
        outcome,
        fetched: fetched.get(),
    };
    // The timer never updates anything, whatever it is handed.
    let updating = options.fast_forward && options.pass == Pass::One;

    let repo = match Repository::open(path) {
        Ok(repo) => repo,
        Err(err) => return report(Outcome::Failed(err.message().to_string())),
    };

    if crate::git::belongs_here(&repo, path).is_err() {
        return report(Outcome::Skipped(Skip::Elsewhere));
    }
    if let Some(skip) = disqualify(&repo) {
        return report(Outcome::Skipped(skip));
    }

    let fetch = {
        let gate = gate(&repo);
        let _alone_in_this_ref_store = gate.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        options.pass.fetch(path, options.timeout)
    };
    if let Err(err) = fetch {
        return report(Outcome::Failed(err));
    }
    fetched.set(true);

    // Re-open so the fetched refs are visible; libgit2 caches aggressively.
    let repo = match Repository::open(path) {
        Ok(repo) => repo,
        Err(err) => return report(Outcome::Failed(err.message().to_string())),
    };

    if !updating {
        // What is waiting for the checked-out branch, where it tracks
        // something. A detached HEAD or a branch that tracks nothing has
        // been fetched all the same, and "left alone: no upstream" about a
        // repository just fetched is not what happened.
        let Ok(at) = tracking(&repo) else { return report(Outcome::UpToDate) };
        if at.behind == 0 {
            return report(Outcome::UpToDate);
        }
        // Nothing is updated on this pass, but an update that stopped
        // part-way on an earlier `p` is still lying here, and "fetched, 3
        // behind" would clear the only record of it.
        if at.ahead == 0 && is_dirty(&repo) {
            let left = half_applied(&repo, &at);
            if !left.is_empty() {
                return report(Outcome::Refused(part_way(&left)));
            }
        }
        return report(Outcome::Behind(at.behind));
    }

    // Asked again: the fetch ran this repository's own git, which is long
    // enough for its config to have changed.
    if crate::git::belongs_here(&repo, path).is_err() {
        return report(Outcome::Skipped(Skip::Elsewhere));
    }
    if repo.state() != RepositoryState::Clean {
        return report(Outcome::Skipped(Skip::Busy));
    }
    let at = match tracking(&repo) {
        Ok(at) => at,
        Err(skip) => return report(Outcome::Skipped(skip)),
    };
    if at.behind == 0 {
        return report(Outcome::UpToDate);
    }
    if at.ahead > 0 {
        return report(Outcome::Skipped(Skip::Diverged));
    }
    // Only a clean repository, and looked at after the fetch: it took time,
    // and the user may have started editing during it. Changes that are not
    // the user's but what an earlier update left part-way are said to be that.
    if is_dirty(&repo) {
        let left = half_applied(&repo, &at);
        return report(if left.is_empty() {
            Outcome::Skipped(Skip::Dirty)
        } else {
            Outcome::Refused(part_way(&left))
        });
    }
    // Not a question git answers. It moves the pointer, leaves the checkout,
    // and exits 0.
    if strands_a_submodule(&repo, &at) {
        return report(Outcome::Skipped(Skip::Submodule));
    }
    // Called off while this repository was being fetched: it stays fetched,
    // and where it was.
    if options.cancel.stopped() || crate::gitcmd::winding_down() {
        return report(Outcome::Behind(at.behind));
    }

    let command = match fast_forward(&repo, &at.branch) {
        Ok(command) => command,
        Err(why) => return report(Outcome::Refused(why)),
    };
    let command: Vec<&str> = command.iter().map(String::as_str).collect();
    match crate::gitcmd::run(path, &command, FAST_FORWARD_PATIENCE) {
        // Exit 0 is git's opinion. Where the branch is now is the fact.
        Ok(_) => match landed(path, at.upstream) {
            Ok(()) => report(Outcome::FastForwarded(at.behind)),
            Err(why) => report(Outcome::Refused(why)),
        },
        // Usually git refused before touching anything. Not always: it can
        // fail after writing some of the files, and then the tree has to be
        // looked at, because git's message only names the one it tripped on.
        Err(err) => {
            let left = Repository::open(path)
                .map(|repo| half_applied(&repo, &at))
                .unwrap_or_default();
            report(Outcome::Refused(if left.is_empty() {
                err
            } else {
                format!("{err}\n{}", part_way(&left))
            }))
        }
    }
}

/// The lock to hold while fetching into the ref store `repo` uses.
///
/// A linked worktree is a repository of its own on the grid and to every pass,
/// and it shares one set of remote-tracking refs with the checkout it was made
/// from. Sorted by path the two sit side by side in the queue, so two workers
/// fetched them at the same moment: both read the old value of `origin/main`,
/// the quicker wrote the new one, and the other failed its check with "cannot
/// lock ref". That was one "fetch failed" card on every pass that had
/// something new to bring in.
///
/// One fetch at a time for each shared git directory, then. Each checkout
/// still gets its own fetch, since two worktrees can track different remotes;
/// the second usually finds nothing left to do. The locks are per process and
/// shared by every pass in it, so `f` on one worktree waits for a fleet pass
/// that is fetching the other.
fn gate(repo: &Repository) -> Arc<Mutex<()>> {
    static GATES: OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> = OnceLock::new();
    let common = repo.commondir();
    let key = common.canonicalize().unwrap_or_else(|_| common.to_path_buf());
    let mut gates = GATES
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    // Nobody is holding or waiting on a gate that only this map refers to.
    gates.retain(|_, gate| Arc::strong_count(gate) > 1);
    Arc::clone(gates.entry(key).or_default())
}

/// Reasons to leave a repository alone before touching the network at all.
///
/// A fetch needs a remote and nothing else: it does not care that HEAD is
/// detached, that a rebase is under way or that there are no commits yet, and
/// it disturbs none of them. Those are reasons not to *update*, and are asked
/// after the fetch, so that `p` on such a repository still fetches it.
///
/// A bare repository is left out. A mirror's fetch writes straight into its
/// branches, which is more than "fetch" means anywhere else here.
fn disqualify(repo: &Repository) -> Option<Skip> {
    if repo.is_bare() {
        return Some(Skip::Bare);
    }
    if repo.remotes().map(|r| r.is_empty()).unwrap_or(true) {
        return Some(Skip::NoRemote);
    }
    None
}

/// The checked-out branch and what it tracks, as they stand right now.
struct Tracking {
    /// The branch's short name: `main`.
    branch: String,
    local: git2::Oid,
    upstream: git2::Oid,
    ahead: usize,
    behind: usize,
}

fn tracking(repo: &Repository) -> Result<Tracking, Skip> {
    let head = repo.head().map_err(|_| Skip::Unborn)?;
    if repo.head_detached().unwrap_or(false) {
        return Err(Skip::Detached);
    }
    let branch_name = head.shorthand().map_err(|_| Skip::Detached)?.to_string();
    let branch = repo
        .find_branch(&branch_name, BranchType::Local)
        .map_err(|_| Skip::NoUpstream)?;
    let upstream = branch.upstream().map_err(|_| Skip::NoUpstream)?;
    let local = branch.get().target().ok_or(Skip::NoUpstream)?;
    let remote = upstream.get().target().ok_or(Skip::NoUpstream)?;
    let (ahead, behind) = repo
        .graph_ahead_behind(local, remote)
        .map_err(|_| Skip::NoUpstream)?;
    Ok(Tracking { branch: branch_name, local, upstream: remote, ahead, behind })
}

/// Whether the branch is where a fast-forward to `meant` puts it.
///
/// git exiting 0 used to be the whole of the evidence, and the count reported
/// was one worked out before the command ran. With a merge strategy of the
/// user's in force the same exit code came back from a merge commit, and from
/// a squash that staged upstream's diff and left the branch where it was.
/// Those two are closed off in [`fast_forward`]. This is for the one nobody
/// has thought of.
///
/// A branch level with its upstream as it stands now also counts: a fetch from
/// a terminal may have moved the upstream on between the measuring and the
/// merge, and git then fast-forwarded to the newer commit.
fn landed(path: &Path, meant: git2::Oid) -> Result<(), String> {
    let short = |oid: git2::Oid| oid.to_string()[..7].to_string();
    let now = Repository::open(path).ok().and_then(|repo| tracking(&repo).ok());
    match now {
        Some(at) if at.local == meant || at.local == at.upstream => Ok(()),
        Some(at) => Err(format!(
            "git reported the update as done, but the branch is at {} and its upstream at {}, \
             which is not what a fast-forward leaves. Look at `git status` and `git log -3` \
             before anything else.",
            short(at.local),
            short(at.upstream)
        )),
        None => Err("git reported the update as done, but the branch cannot be read back \
                     to check: look at `git status` before anything else."
            .to_string()),
    }
}

/// Staged, modified or conflicted. Untracked files are left out: git refuses
/// on its own to overwrite one. It does *not* refuse for an ignored file unless
/// told to, which is what `--no-overwrite-ignore` in [`FAST_FORWARD`] is for.
fn is_dirty(repo: &Repository) -> bool {
    let mut opts = StatusOptions::new();
    opts.include_untracked(false)
        .include_ignored(false)
        .exclude_submodules(true);
    match repo.statuses(Some(&mut opts)) {
        Ok(statuses) => statuses.iter().any(|entry| {
            entry.status().intersects(
                git2::Status::INDEX_NEW
                    | git2::Status::INDEX_MODIFIED
                    | git2::Status::INDEX_DELETED
                    | git2::Status::INDEX_RENAMED
                    | git2::Status::INDEX_TYPECHANGE
                    | git2::Status::WT_MODIFIED
                    | git2::Status::WT_DELETED
                    | git2::Status::WT_RENAMED
                    | git2::Status::WT_TYPECHANGE
                    | git2::Status::CONFLICTED,
            )
        }),
        // Unreadable status is not a licence to proceed.
        Err(_) => true,
    }
}

/// Whether fast-forwarding would move the pointer of a submodule that is
/// checked out here.
///
/// `git merge` does not update submodules, with or without
/// `submodule.recurse`. It records the new pointer and leaves the checkout on
/// the old commit, so the repository's code is new and the library's is not.
/// That was reported as "updated", the card showed "clean" because its status
/// leaves submodules out, and a build that broke had no visible cause.
/// Bringing the submodule along is `git submodule update`, which this module
/// does not run; so the update is left for the user to pull, and the reason
/// says why.
///
/// A submodule that was never checked out has nothing to be left behind, and
/// does not hold anything back.
fn strands_a_submodule(repo: &Repository, at: &Tracking) -> bool {
    let tree = |commit| repo.find_commit(commit).and_then(|commit| commit.tree());
    let incoming = match (tree(at.local), tree(at.upstream)) {
        (Ok(ours), Ok(theirs)) => repo.diff_tree_to_tree(Some(&ours), Some(&theirs), None),
        (Err(err), _) | (_, Err(err)) => Err(err),
    };
    // Unreadable is not a licence to proceed.
    let Ok(incoming) = incoming else { return true };
    let Some(workdir) = repo.workdir() else { return false };
    incoming.deltas().any(|delta| {
        [delta.old_file(), delta.new_file()].iter().any(|file| {
            file.mode() == git2::FileMode::Commit
                && file.path().is_some_and(|path| workdir.join(path).join(".git").exists())
        })
    })
}

/// The paths where the working tree already holds what the upstream commit
/// has, while the branch is still on a commit that has something else.
///
/// git's checkout is not a transaction. When one incoming path cannot be
/// written (a directory somebody else owns, a full disk, a filter that fails)
/// it has already written others, and it exits without moving the branch. The
/// repository is then dirty with upstream's own content: `git status` lists
/// the files as modified, and one the update adds as untracked.
///
/// Told apart from an edit by content, not by remembering: a file counts only
/// if it is byte for byte what the update brings. So this needs no record of
/// the failure, finds the same thing after a restart, and can never claim a
/// file the user has changed. A file filtered on the way in (git-lfs) does not
/// compare equal and is left out, which is the safe side to be wrong on.
fn half_applied(repo: &Repository, at: &Tracking) -> Vec<String> {
    let tree = |commit| repo.find_commit(commit).and_then(|commit| commit.tree());
    let (Ok(ours), Ok(theirs)) = (tree(at.local), tree(at.upstream)) else {
        return Vec::new();
    };
    let Ok(incoming) = repo.diff_tree_to_tree(Some(&ours), Some(&theirs), None) else {
        return Vec::new();
    };
    // A submodule's pointer is compared with a checkout, not with content.
    let link = |file: &git2::DiffFile| file.mode() == git2::FileMode::Commit;
    let changed: Vec<PathBuf> = incoming
        .deltas()
        .filter(|delta| !link(&delta.old_file()) && !link(&delta.new_file()))
        .filter_map(|delta| delta.new_file().path().or(delta.old_file().path()))
        .map(Path::to_path_buf)
        .collect();
    if changed.is_empty() {
        return Vec::new();
    }

    // Of the paths the update touches, the ones where the working tree is not
    // yet what upstream has. Ignored and untracked files are asked for: a path
    // the update deletes is "not yet" exactly when a file is still there.
    let mut only = git2::DiffOptions::new();
    only.disable_pathspec_match(true)
        .include_untracked(true)
        .recurse_untracked_dirs(true)
        .include_ignored(true)
        .recurse_ignored_dirs(true)
        .ignore_submodules(true);
    for path in &changed {
        only.pathspec(path);
    }
    let Ok(pending) = repo.diff_tree_to_workdir(Some(&theirs), Some(&mut only)) else {
        return Vec::new();
    };
    let pending: HashSet<PathBuf> = pending
        .deltas()
        .flat_map(|delta| [delta.old_file().path(), delta.new_file().path()])
        .flatten()
        .map(Path::to_path_buf)
        .collect();
    changed
        .iter()
        .filter(|path| !pending.contains(*path))
        .map(|path| path.display().to_string())
        .collect()
}

/// What to say about a tree [`half_applied`] found something in. The same
/// words for the same files, so that a pass repeating it is not new trouble.
fn part_way(left: &[String]) -> String {
    let mut named = left.iter().take(3).cloned().collect::<Vec<_>>().join(", ");
    if left.len() > 3 {
        named.push_str(&format!(" and {} more", left.len() - 3));
    }
    format!(
        "Part of this update is already in the working tree: {named} {} the incoming version \
         while the branch is still on the old commit. That is what an update leaves when \
         something stops it part-way. git lists them as changes, but they are exactly what the \
         update brings, so nothing is lost by putting them back (`git restore <file>`, or \
         deleting one the update adds) and updating again.",
        if left.len() == 1 { "holds" } else { "hold" }
    )
}

/// Counts for the summary line after a pass.
#[derive(Debug, Default, Clone)]
pub struct Summary {
    pub fetched: usize,
    pub behind: usize,
    pub updated: usize,
    pub skipped: usize,
    /// Fetched, but not fast-forwarded where that was asked for, or found
    /// holding what an earlier update left part-way.
    pub refused: usize,
    pub failed: usize,
    /// Why repositories were passed over, so the summary can say rather than
    /// leave the user to guess which of their repos was left alone and why.
    pub skips: std::collections::BTreeMap<&'static str, usize>,
}

impl Summary {
    pub fn add(&mut self, outcome: &Outcome) {
        match outcome {
            Outcome::UpToDate => self.fetched += 1,
            Outcome::Behind(_) => {
                self.fetched += 1;
                self.behind += 1;
            }
            Outcome::FastForwarded(_) => {
                self.fetched += 1;
                self.updated += 1;
            }
            Outcome::Skipped(skip) => {
                if skip.after_fetch() {
                    self.fetched += 1;
                }
                self.skipped += 1;
                *self.skips.entry(skip.reason()).or_insert(0) += 1;
            }
            Outcome::Refused(_) => {
                self.fetched += 1;
                self.refused += 1;
            }
            Outcome::Failed(_) => self.failed += 1,
        }
    }

    pub fn line(&self) -> String {
        let mut parts = vec![format!("{} fetched", self.fetched)];
        if self.updated > 0 {
            parts.push(format!("{} updated", self.updated));
        }
        if self.behind > 0 {
            parts.push(format!("{} behind", self.behind));
        }
        if self.skipped > 0 {
            let reasons = self
                .skips
                .iter()
                .map(|(reason, count)| format!("{count} {reason}"))
                .collect::<Vec<_>>()
                .join(", ");
            parts.push(format!("{} skipped ({reasons})", self.skipped));
        }
        if self.refused > 0 {
            parts.push(format!("{} with an update left part-way", self.refused));
        }
        if self.failed > 0 {
            parts.push(format!("{} failed", self.failed));
        }
        parts.join(" · ")
    }
}

#[cfg(test)]
mod tests {
    use std::hash::{DefaultHasher, Hash, Hasher};
    use std::os::unix::fs::PermissionsExt;

    use super::*;
    use crate::testkit::*;

    fn options(pass: Pass) -> Options {
        Options {
            fast_forward: false,
            max_parallel: 1,
            timeout: Duration::from_secs(30),
            pass,
            cancel: Cancel::default(),
        }
    }

    fn fetch(dir: &Path, pass: Pass) -> Report {
        refresh_one(dir, &options(pass))
    }

    /// Where the clone under test thinks the remote's `main` is.
    fn origin_main(dir: &Path) -> String {
        let (ok, out) = try_git(dir, &["rev-parse", "--verify", "-q", "origin/main"]);
        if ok { out.trim().to_string() } else { String::new() }
    }

    /// Everything in a repository that is the user's and not the remote's:
    /// where HEAD is, every branch, tag and stash, what is staged, the index
    /// byte for byte, and every file in the working tree. A fetch is the one
    /// thing done to a repository, and this is what it has to leave as found.
    ///
    /// Nothing here asks git for a status: that refreshes the index, and the
    /// looking would be what changed it.
    fn theirs(dir: &Path) -> Vec<(String, String)> {
        let mut found = Vec::new();
        for args in [
            &["symbolic-ref", "-q", "HEAD"][..],
            &["rev-parse", "-q", "--verify", "HEAD"],
            &["for-each-ref", "refs/heads", "refs/tags", "refs/stash"],
            &["ls-files", "--stage"],
        ] {
            found.push((format!("git {}", args.join(" ")), try_git(dir, args).1));
        }
        let mut index = DefaultHasher::new();
        std::fs::read(dir.join(".git/index")).unwrap_or_default().hash(&mut index);
        found.push((".git/index".to_string(), format!("{:x}", index.finish())));
        files_under(dir, dir, &mut found);
        found.sort();
        found
    }

    fn files_under(root: &Path, dir: &Path, found: &mut Vec<(String, String)>) {
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if entry.file_name() == ".git" {
                continue;
            }
            if path.is_dir() {
                files_under(root, &path, found);
            } else {
                let name = path.strip_prefix(root).unwrap().display().to_string();
                found.push((name, read(&path)));
            }
        }
    }

    // ---- what the README says a fetch does, and does not ------------------

    #[test]
    fn a_fetch_brings_in_what_is_new_and_moves_nothing_of_the_users() {
        let _guard = isolate();
        for pass in [Pass::Fleet, Pass::One] {
            let f = fleet(&format!("rf-fetch-{pass:?}"));
            let before = theirs(&f.work);
            f.remote_moves(3);

            let report = fetch(&f.work, pass);

            assert!(matches!(report.outcome, Outcome::Behind(3)), "{pass:?}: {:?}", report.outcome);
            assert!(report.fetched, "{pass:?}");
            assert_eq!(origin_main(&f.work), f.remote_ref("refs/heads/main"), "{pass:?}");
            assert_eq!(theirs(&f.work), before, "{pass:?}");
        }
    }

    #[test]
    fn a_remote_with_nothing_new_is_said_to_have_nothing_new() {
        let _guard = isolate();
        let f = fleet("rf-nothing-new");
        let report = fetch(&f.work, Pass::Fleet);
        assert!(matches!(report.outcome, Outcome::UpToDate), "{:?}", report.outcome);
        assert!(report.fetched);
    }

    /// Uncommitted work is nothing to a fetch, and a fetch is nothing to it.
    /// That goes for a file the remote is about to add under the same name
    /// too, and for one only this clone ignores.
    #[test]
    fn uncommitted_work_is_not_in_the_way_and_is_not_touched() {
        let _guard = isolate();
        for pass in [Pass::Fleet, Pass::One] {
            let f = fleet(&format!("rf-uncommitted-{pass:?}"));
            write(&f.work.join("README.md"), "my edit\n");
            write(&f.work.join("staged.txt"), "staged\n");
            git(&f.work, &["add", "staged.txt"]);
            write(&f.work.join("upstream-0.txt"), "MY UNTRACKED WORK\n");
            write(&f.work.join(".git/info/exclude"), ".env\n");
            write(&f.work.join(".env"), "KEY=the only copy\n");
            let before = theirs(&f.work);
            f.remote_moves(1);

            let report = fetch(&f.work, pass);

            assert!(matches!(report.outcome, Outcome::Behind(1)), "{pass:?}: {:?}", report.outcome);
            assert_eq!(theirs(&f.work), before, "{pass:?}");
        }
    }

    /// What is waiting is counted whatever else is true of the branch. It
    /// said "left alone: has unpushed commits" once, about a repository it
    /// had just fetched.
    #[test]
    fn commits_of_its_own_are_left_and_what_is_waiting_is_still_counted() {
        let _guard = isolate();
        for pass in [Pass::Fleet, Pass::One] {
            let f = fleet(&format!("rf-diverged-{pass:?}"));
            commit_file(&f.work, "mine.txt", "mine\n", "local work");
            let before = theirs(&f.work);
            f.remote_moves(2);
            let report = fetch(&f.work, pass);
            assert!(matches!(report.outcome, Outcome::Behind(2)), "{pass:?}: {:?}", report.outcome);
            assert!(report.fetched, "{pass:?}");
            assert_eq!(theirs(&f.work), before, "{pass:?}");

            // A branch that tracks nothing: fetched, and nothing to count.
            let f = fleet(&format!("rf-no-upstream-{pass:?}"));
            git(&f.work, &["checkout", "-q", "-b", "local-only"]);
            f.remote_moves(1);
            let report = fetch(&f.work, pass);
            assert_eq!(origin_main(&f.work), f.remote_ref("refs/heads/main"), "{pass:?}");
            assert!(matches!(report.outcome, Outcome::UpToDate), "{pass:?}: {:?}", report.outcome);
            assert!(report.fetched, "{pass:?}");
        }
    }

    /// A fetch needs a remote and nothing else. A detached HEAD, a merge
    /// under way and a repository with no commits yet are fetched like any
    /// other, and none of them is disturbed by it.
    #[test]
    fn a_repository_is_fetched_whatever_state_it_is_in() {
        let _guard = isolate();
        for pass in [Pass::Fleet, Pass::One] {
            let f = fleet(&format!("rf-detached-{pass:?}"));
            git(&f.work, &["checkout", "-q", "--detach"]);
            let before = theirs(&f.work);
            f.remote_moves(1);
            let report = fetch(&f.work, pass);
            assert_eq!(origin_main(&f.work), f.remote_ref("refs/heads/main"), "{pass:?}");
            assert!(report.fetched, "{pass:?}");
            assert!(matches!(report.outcome, Outcome::UpToDate), "{pass:?}: {:?}", report.outcome);
            assert_eq!(theirs(&f.work), before, "{pass:?}");

            let f = fleet(&format!("rf-mid-merge-{pass:?}"));
            git(&f.work, &["checkout", "-q", "-b", "side"]);
            commit_file(&f.work, "README.md", "side\n", "side");
            git(&f.work, &["checkout", "-q", "main"]);
            commit_file(&f.work, "README.md", "main\n", "main");
            let (merged, _) = try_git(&f.work, &["merge", "side"]);
            assert!(!merged, "the fixture needs a conflicted merge in progress");
            let before = theirs(&f.work);
            f.remote_moves(1);
            let report = fetch(&f.work, pass);
            assert_eq!(origin_main(&f.work), f.remote_ref("refs/heads/main"), "{pass:?}");
            assert!(report.fetched, "{pass:?}");
            assert!(matches!(report.outcome, Outcome::Behind(1)), "{pass:?}: {:?}", report.outcome);
            assert_eq!(theirs(&f.work), before, "{pass:?}");
            assert!(f.work.join(".git/MERGE_HEAD").exists(), "{pass:?}: the merge was disturbed");

            // No commits yet, and a remote to fetch from.
            let f = fleet(&format!("rf-unborn-{pass:?}"));
            let empty = f.root.join("empty");
            std::fs::create_dir_all(&empty).unwrap();
            git(&empty, &["init", "-q", "-b", "main"]);
            git(&empty, &["remote", "add", "origin", f.remote.to_str().unwrap()]);
            let report = fetch(&empty, pass);
            assert_eq!(origin_main(&empty), f.remote_ref("refs/heads/main"), "{pass:?}");
            assert!(report.fetched, "{pass:?}");
            assert!(matches!(report.outcome, Outcome::UpToDate), "{pass:?}: {:?}", report.outcome);
        }
    }

    /// What cannot be fetched is left alone, before the network is touched.
    #[test]
    fn what_cannot_be_fetched_is_left_alone() {
        let _guard = isolate();
        let f = fleet("rf-impossible");
        let report = fetch(&f.remote, Pass::Fleet);
        assert!(matches!(report.outcome, Outcome::Skipped(Skip::Bare)), "{:?}", report.outcome);
        assert!(!report.fetched);

        let alone = f.root.join("alone");
        std::fs::create_dir_all(&alone).unwrap();
        git(&alone, &["init", "-q", "-b", "main"]);
        commit_file(&alone, "README.md", "one\n", "first");
        for pass in [Pass::Fleet, Pass::One] {
            let report = fetch(&alone, pass);
            assert!(
                matches!(report.outcome, Outcome::Skipped(Skip::NoRemote)),
                "{pass:?}: {:?}",
                report.outcome
            );
            assert!(!report.fetched, "{pass:?}");
        }
    }

    /// The one thing a fetch adds outside `origin/*`, and it is git's own
    /// default: a tag the remote has on a commit that was fetched comes with
    /// it. A tag of the user's own by the same name stays where they put it.
    #[test]
    fn a_tag_on_what_was_fetched_arrives_and_replaces_none_of_the_users() {
        let _guard = isolate();
        for pass in [Pass::Fleet, Pass::One] {
            let f = fleet(&format!("rf-tags-{pass:?}"));
            git(&f.work, &["tag", "mine"]);
            let mine = git(&f.work, &["rev-parse", "refs/tags/mine"]);
            f.remote_moves(1);
            git(&f.seed, &["tag", "v1"]);
            git(&f.seed, &["tag", "mine"]);
            git(&f.seed, &["push", "-q", "origin", "v1", "mine"]);
            assert_ne!(f.remote_ref("refs/tags/mine"), mine, "the fixture needs the two to differ");

            let report = fetch(&f.work, pass);

            assert!(matches!(report.outcome, Outcome::Behind(1)), "{pass:?}: {:?}", report.outcome);
            let arrived = git(&f.work, &["rev-parse", "refs/tags/v1"]);
            assert_eq!(arrived, f.remote_ref("refs/tags/v1"), "{pass:?}");
            assert_eq!(git(&f.work, &["rev-parse", "refs/tags/mine"]), mine, "{pass:?}");
        }
    }

    // ---- a pass can be called off -----------------------------------------

    #[test]
    fn a_pass_that_is_called_off_starts_nothing_more() {
        let _guard = isolate();
        let f = fleet("rf-called-off");
        f.remote_moves(1);
        let called_off = options(Pass::Fleet);
        called_off.cancel.stop();

        // A fetch already running when the pass is called off is let finish.
        let got = refresh_one(&f.work, &called_off).outcome;
        assert!(matches!(got, Outcome::Behind(1)), "{got:?}");

        // And nothing that had not started is started.
        let rx = spawn(vec![f.work.clone(), f.seed.clone()], called_off);
        let mut entered = 0;
        while let Ok(Event::Repo(_)) = rx.recv_blocking() {
            entered += 1;
        }
        assert_eq!(entered, 0);
    }

    // ---- a fetch goes into the repository's own git data or nowhere -------

    #[test]
    fn a_directory_whose_git_data_points_somewhere_else_is_not_fetched() {
        let _guard = isolate();

        // A work tree that is another directory.
        let f = fleet("rf-redirect");
        let elsewhere = scratch("rf-redirect-victim");
        write(&elsewhere.join("README.md"), "the other directory's own file\n");
        git(&f.work, &["config", "core.worktree", elsewhere.to_str().unwrap()]);
        let seen = origin_main(&f.work);
        f.remote_moves(1);
        for pass in [Pass::Fleet, Pass::One] {
            let report = fetch(&f.work, pass);
            assert!(
                matches!(report.outcome, Outcome::Skipped(Skip::Elsewhere)),
                "{pass:?}: {:?}",
                report.outcome
            );
            assert!(!report.fetched, "{pass:?}");
        }
        assert_eq!(origin_main(&f.work), seen);
        assert_eq!(read(&elsewhere.join("README.md")), "the other directory's own file\n");

        // A `.git` that is a file naming another repository's git data: a
        // fetch there would be written into that repository.
        let f = fleet("rf-redirect-gitfile");
        let decoy = f.root.join("decoy");
        write(&decoy.join(".git"), &format!("gitdir: {}\n", f.work.join(".git").display()));
        let seen = origin_main(&f.work);
        f.remote_moves(1);
        for pass in [Pass::Fleet, Pass::One] {
            let report = fetch(&decoy, pass);
            assert!(
                matches!(report.outcome, Outcome::Skipped(Skip::Elsewhere)),
                "{pass:?}: {:?}",
                report.outcome
            );
            assert!(!report.fetched, "{pass:?}");
        }
        assert_eq!(origin_main(&f.work), seen);
    }

    // ---- nothing a repository carries runs on a pass nobody is watching ---

    /// A clone with a `reference-transaction` hook, which is the hook a fetch
    /// runs: once for each batch of refs it is about to move. This one leaves
    /// a marker behind. `in_the_tree` keeps it under `core.hooksPath` in the
    /// working tree, where a commit can carry it, and not in `.git/hooks`.
    fn hooked(name: &str, in_the_tree: bool) -> (Fleet, PathBuf) {
        let f = fleet(name);
        let marker = f.root.join("HOOK-RAN");
        let script = format!("#!/bin/sh\ntouch '{}'\n", marker.display());
        if in_the_tree {
            git(&f.work, &["config", "core.hooksPath", ".githooks"]);
            executable(&f.work.join(".githooks/reference-transaction"), &script);
        } else {
            executable(&f.work.join(".git/hooks/reference-transaction"), &script);
        }
        f.remote_moves(1);
        (f, marker)
    }

    #[test]
    fn a_hook_does_not_run_on_a_fleet_pass() {
        let _guard = isolate();
        for in_the_tree in [false, true] {
            let (f, marker) = hooked(&format!("rf-hook-fleet-{in_the_tree}"), in_the_tree);
            let got = fetch(&f.work, Pass::Fleet).outcome;
            assert!(matches!(got, Outcome::Behind(1)), "{in_the_tree}: {got:?}");
            assert!(!marker.exists(), "{in_the_tree}: a hook ran on a pass nobody was watching");
        }
    }

    #[test]
    fn hooks_still_run_when_the_user_asks_for_one_repository() {
        let _guard = isolate();
        for in_the_tree in [false, true] {
            let (f, marker) = hooked(&format!("rf-hook-asked-{in_the_tree}"), in_the_tree);
            let got = fetch(&f.work, Pass::One).outcome;
            assert!(matches!(got, Outcome::Behind(1)), "{in_the_tree}: {got:?}");
            assert!(marker.exists(), "{in_the_tree}: their own fetch would have run it, so this does too");
        }
    }

    /// Leave `repo` with enough loose objects that the housekeeping git starts
    /// after a fetch (`gc --auto`) has something to do, and have it done
    /// before the fetch returns instead of in the background. git guesses the
    /// number of loose objects from one directory of the 256, so that is the
    /// one to fill.
    ///
    /// The `gc` task is asked for by name. It was what the housekeeping came
    /// to by default up to git 2.55; 2.56 goes about it another way unless
    /// told, one that two loose objects do not set off, and a fetch there
    /// read no index and ran nothing, which left this with nothing to test.
    fn due_for_housekeeping(repo: &Path) {
        git(repo, &["config", "gc.auto", "1"]);
        git(repo, &["config", "gc.autoDetach", "false"]);
        git(repo, &["config", "maintenance.gc.enabled", "true"]);
        let counted = repo.join(".git/objects/17");
        let store = Repository::open(repo).unwrap();
        for i in 0..20_000 {
            if std::fs::read_dir(&counted).map(|dir| dir.count()).unwrap_or(0) >= 2 {
                return;
            }
            store.blob(format!("filler {i}\n").as_bytes()).unwrap();
        }
        panic!("no two objects landed in {}", counted.display());
    }

    /// A fetch does not look at the working tree and has no reason to ask the
    /// fsmonitor. The housekeeping that follows one does: it repacks, that
    /// reads the index, and reading the index is what runs the command.
    #[test]
    fn a_fleet_pass_does_not_run_the_fsmonitor_command() {
        let _guard = isolate();
        for (pass, runs) in [(Pass::Fleet, false), (Pass::One, true)] {
            let f = fleet(&format!("rf-fsmonitor-{pass:?}"));
            let marker = f.root.join("FSMONITOR-RAN");
            let command = format!("touch '{}'; false", marker.display());
            git(&f.work, &["config", "core.fsmonitor", &command]);
            due_for_housekeeping(&f.work);
            f.remote_moves(1);
            let got = fetch(&f.work, pass).outcome;
            assert!(matches!(got, Outcome::Behind(1)), "{pass:?}: {got:?}");
            assert_eq!(marker.exists(), runs, "{pass:?}");
        }
    }

    #[test]
    fn a_fleet_pass_refuses_the_ext_transport_even_where_the_repository_allows_it() {
        let _guard = isolate();
        for (pass, runs) in [(Pass::Fleet, false), (Pass::One, true)] {
            let f = fleet(&format!("rf-ext-{pass:?}"));
            let marker = f.root.join("EXT-RAN");
            // In an ext:: address "% " is a space inside one argument.
            let url = format!("ext::sh -c touch% {};% exit% 1", marker.display());
            git(&f.work, &["config", "protocol.ext.allow", "always"]);
            git(&f.work, &["remote", "set-url", "origin", &url]);
            let got = fetch(&f.work, pass).outcome;
            assert!(matches!(got, Outcome::Failed(_)), "{pass:?}: {got:?}");
            assert_eq!(marker.exists(), runs, "{pass:?}");
        }
    }

    // ---- a fetch is given up on for silence, not for taking long ----------

    fn executable(path: &Path, script: &str) {
        write(path, script);
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// A remote with something under a megabyte to send that does not
    /// compress, and a link that lets it through 8 KiB at a time: about three
    /// seconds, with git reporting progress all the way.
    fn on_a_slow_link(name: &str) -> Fleet {
        let f = fleet(name);
        f.remote_commits("a hundred files of noise", |seed| {
            let mut state = 0x9E37_79B9_7F4A_7C15_u64;
            for i in 0..100 {
                let noise: Vec<u8> = (0..8192)
                    .map(|_| {
                        state ^= state << 13;
                        state ^= state >> 7;
                        state ^= state << 17;
                        (state >> 32) as u8
                    })
                    .collect();
                std::fs::write(seed.join(format!("noise-{i}.bin")), noise).unwrap();
            }
        });
        let slow = f.root.join("slow-upload-pack");
        executable(
            &slow,
            "#!/bin/sh\n\
             git upload-pack \"$@\" | {\n\
             \twhile :; do\n\
             \t\tsaid=$(LC_ALL=C dd bs=8192 count=1 2>&1 >&3)\n\
             \t\tcase \"$said\" in \"0+0 records in\"*) break ;; esac\n\
             \t\tsleep 0.03\n\
             \tdone\n\
             } 3>&1\n",
        );
        git(&f.work, &["config", "remote.origin.uploadpack", slow.to_str().unwrap()]);
        f
    }

    fn partial_packs(repo: &Path) -> Vec<String> {
        std::fs::read_dir(repo.join(".git/objects/pack"))
            .map(|entries| {
                entries
                    .flatten()
                    .map(|entry| entry.file_name().to_string_lossy().to_string())
                    .filter(|name| name.starts_with("tmp_"))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The limit on a fetch was on the whole of it. A fetch that needed
    /// longer could never finish: it was stopped at the limit with data still
    /// arriving, started again from nothing on the next pass, and left what
    /// it had received so far in `.git/objects/pack` every time.
    #[test]
    fn a_fetch_that_is_still_arriving_is_not_cut_off() {
        let _guard = isolate();
        let f = on_a_slow_link("rf-slow-link");
        let hurried = Options { timeout: Duration::from_secs(1), ..options(Pass::Fleet) };

        let started = std::time::Instant::now();
        let got = refresh_one(&f.work, &hurried).outcome;
        let took = started.elapsed();

        assert!(matches!(got, Outcome::Behind(1)), "{got:?} after {took:?}");
        assert!(took > Duration::from_secs(2), "over in {took:?}, which shows nothing");
        assert_eq!(origin_main(&f.work), f.remote_ref("refs/heads/main"));
        assert_eq!(partial_packs(&f.work), Vec::<String>::new());
    }

    /// A link that has gone dead is still given up on, after `timeout` of
    /// nothing, and said to be that.
    #[test]
    fn a_fetch_that_has_gone_quiet_is_given_up_on() {
        let _guard = isolate();
        let f = fleet("rf-dead-link");
        f.remote_moves(1);
        let dead = f.root.join("dead-upload-pack");
        executable(&dead, "#!/bin/sh\nsleep 30\n");
        git(&f.work, &["config", "remote.origin.uploadpack", dead.to_str().unwrap()]);
        let hurried = Options { timeout: Duration::from_secs(1), ..options(Pass::Fleet) };

        let started = std::time::Instant::now();
        let report = refresh_one(&f.work, &hurried);

        let Outcome::Failed(why) = &report.outcome else { panic!("{report:?}") };
        assert!(why.contains("nothing arrived for 1s"), "{why}");
        assert!(!report.fetched);
        assert!(started.elapsed() < Duration::from_secs(5), "took {:?}", started.elapsed());
    }

    /// What git says of a repository the server will not show, with the
    /// real git saying it: a server here answers every request "404", which
    /// is what GitHub answers for a private repository the account may not
    /// see. The words are git's, and what is added to them depends on them.
    #[test]
    fn a_remote_that_answers_not_found_is_said_to_be_that_or_out_of_sight() {
        let _guard = isolate();
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = server.local_addr().unwrap().port();
        std::thread::spawn(move || {
            use std::io::{Read, Write};
            for stream in server.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut request = [0u8; 4096];
                let _ = stream.read(&mut request);
                let _ = stream.write_all(
                    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                );
            }
        });
        let f = fleet("rf-not-found");
        let url = format!("http://127.0.0.1:{port}/acme/billing.git");
        git(&f.work, &["remote", "set-url", "origin", &url]);

        for pass in [Pass::One, Pass::Fleet] {
            let report = fetch(&f.work, pass);
            let Outcome::Failed(why) = &report.outcome else { panic!("{pass:?}: {report:?}") };
            assert_eq!(
                why,
                &format!(
                    "repository not found, or the account git signs in with has no access to it\n\
                     git said: fatal: repository '{url}/' not found"
                ),
                "{pass:?}"
            );
            assert!(!report.fetched, "{pass:?}");
        }
    }

    // ---- what a fetch leaves alone ----------------------------------------

    /// `git fetch origin pull/42/head`, then `git checkout FETCH_HEAD`, is how
    /// a pull request is looked at. A pass that lands between the two must not
    /// change what the second one checks out.
    #[test]
    fn a_fetch_leaves_fetch_head_naming_what_the_user_fetched() {
        let _guard = isolate();
        for pass in [Pass::Fleet, Pass::One] {
            let f = fleet(&format!("rf-fetch-head-{pass:?}"));
            git(&f.seed, &["checkout", "-q", "-b", "pr-42"]);
            commit_file(&f.seed, "pr.txt", "under review\n", "PR 42");
            git(&f.seed, &["push", "-q", "origin", "pr-42"]);
            git(&f.seed, &["checkout", "-q", "main"]);
            git(&f.work, &["fetch", "-q", "origin", "refs/heads/pr-42"]);
            let under_review = git(&f.work, &["rev-parse", "FETCH_HEAD"]);
            assert_eq!(under_review, f.remote_ref("refs/heads/pr-42"));
            f.remote_moves(1);

            let got = fetch(&f.work, pass).outcome;

            assert!(matches!(got, Outcome::Behind(1)), "{pass:?}: {got:?}");
            assert_eq!(git(&f.work, &["rev-parse", "FETCH_HEAD"]), under_review, "{pass:?}");
        }
    }

    /// A linked worktree is a repository of its own on the grid, and shares
    /// one set of `origin/*` with the checkout it was made from. Fetched at
    /// the same moment, both read the old values, the quicker one writes the
    /// new ones, and the other fails: "cannot lock ref", for a repository that
    /// fetches fine by hand.
    #[test]
    fn a_repository_and_its_linked_worktree_do_not_fetch_over_each_other() {
        let _guard = isolate();
        let f = fleet("rf-worktree-pair");
        let on_feat = |file: &str| {
            git(&f.seed, &["checkout", "-q", "feat"]);
            commit_file(&f.seed, file, "feat\n", file);
            git(&f.seed, &["push", "-q", "-u", "origin", "feat"]);
            git(&f.seed, &["checkout", "-q", "main"]);
        };
        git(&f.seed, &["branch", "feat"]);
        on_feat("feat.txt");
        git(&f.work, &["fetch", "-q"]);
        let linked = f.root.join("work-feat");
        git(&f.work, &["worktree", "add", "-q", linked.to_str().unwrap(), "feat"]);

        let mut wrong = Vec::new();
        for round in 0..6 {
            // Both branches move upstream, so each fetch has both refs to write,
            // and since nothing here brings them in, each is one further behind.
            on_feat(&format!("feat-{round}.txt"));
            f.remote_commits("main moves", |seed| {
                write(&seed.join(format!("main-{round}.txt")), "main\n");
            });

            let both = vec![f.work.clone(), linked.clone()];
            let rx = spawn(both, Options { max_parallel: 4, ..options(Pass::Fleet) });
            while let Ok(Event::Repo(report)) = rx.recv_blocking() {
                if !matches!(report.outcome, Outcome::Behind(waiting) if waiting == round + 1) {
                    wrong.push(format!("round {round}, {}: {:?}", report.name, report.outcome));
                }
            }
        }
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    }

    /// A repository with a library vendored as a submodule at `vendor/lib`,
    /// checked out in the clone under test.
    struct Vendored {
        f: Fleet,
        lib_seed: PathBuf,
    }

    fn vendored(name: &str) -> Vendored {
        let f = fleet(name);
        let lib = f.root.join("lib.git");
        let lib_seed = f.root.join("lib-seed");
        git(&f.root, &["init", "-q", "--bare", "-b", "main", "lib.git"]);
        git(&f.root, &["clone", "-q", lib.to_str().unwrap(), "lib-seed"]);
        commit_file(&lib_seed, "lib.txt", "v1\n", "library v1");
        git(&lib_seed, &["push", "-q", "-u", "origin", "main"]);
        git(&f.seed, &["submodule", "--quiet", "add", lib.to_str().unwrap(), "vendor/lib"]);
        git(&f.seed, &["commit", "-q", "-m", "vendor the library"]);
        git(&f.seed, &["push", "-q", "origin", "main"]);
        git(&f.work, &["pull", "-q", "--ff-only"]);
        git(&f.work, &["submodule", "--quiet", "update", "--init"]);
        Vendored { f, lib_seed }
    }

    impl Vendored {
        /// The library gets a commit and upstream moves its pointer to it.
        /// Returns the library's new commit.
        fn pointer_moves(&self) -> String {
            commit_file(&self.lib_seed, "lib.txt", "v2\n", "library v2");
            git(&self.lib_seed, &["push", "-q", "origin", "main"]);
            self.f.remote_commits("bump the library", |seed| {
                git(&seed.join("vendor/lib"), &["pull", "-q", "--ff-only"]);
            });
            git(&self.lib_seed, &["rev-parse", "HEAD"])
        }

        fn module(&self) -> PathBuf {
            self.f.work.join("vendor/lib")
        }
    }

    /// Left to its default, a fetch that brings in a commit moving a submodule
    /// pointer goes on to fetch inside the submodule, from the submodule's own
    /// remote.
    #[test]
    fn a_fetch_does_not_go_on_into_a_submodule() {
        let _guard = isolate();
        for pass in [Pass::Fleet, Pass::One] {
            let v = vendored(&format!("rf-submodule-fetch-{pass:?}"));
            let arrived = v.pointer_moves();
            let got = fetch(&v.f.work, pass).outcome;
            assert!(matches!(got, Outcome::Behind(1)), "{pass:?}: {got:?}");
            let (has, _) = try_git(&v.module(), &["cat-file", "-e", &arrived]);
            assert!(!has, "{pass:?}: the fetch went on into the submodule");
        }
    }

    /// Going on into the submodule also made its remote this repository's
    /// problem: one that had moved or gone away failed the fetch of a
    /// repository that had itself fetched without trouble.
    #[test]
    fn a_submodule_that_cannot_be_fetched_does_not_fail_the_repository() {
        let _guard = isolate();
        let v = vendored("rf-submodule-unreachable");
        let gone = v.f.root.join("moved-away.git");
        git(&v.module(), &["remote", "set-url", "origin", gone.to_str().unwrap()]);
        v.pointer_moves();

        let report = fetch(&v.f.work, Pass::Fleet);

        assert!(matches!(report.outcome, Outcome::Behind(1)), "{:?}", report.outcome);
        assert!(report.fetched);
    }

    // ---- `p`, and only `p`, fast-forwards --------------------------------

    /// What `p` on this repository's card does: fetch, and fast-forward.
    fn update_report(dir: &Path) -> Report {
        refresh_one(dir, &Options { fast_forward: true, ..options(Pass::One) })
    }

    fn update(f: &Fleet) -> Outcome {
        update_report(&f.work).outcome
    }

    /// Whether this runs as root, as the tests do in CI's container. Root can
    /// write into a directory that is not writable, and the tests of an update
    /// that stops part-way need one that stops it.
    fn as_root() -> bool {
        // SAFETY: geteuid has no preconditions and cannot fail.
        unsafe { libc::geteuid() == 0 }
    }

    /// The timer and `Ctrl+Shift+R` fetch, and nothing else, even handed the
    /// option to do more.
    #[test]
    fn a_pass_across_the_fleet_never_fast_forwards() {
        let _guard = isolate();
        let f = fleet("refresh-fleet-never-updates");
        let before = f.head();
        f.remote_moves(2);
        let asked = Options { fast_forward: true, ..options(Pass::Fleet) };
        let got = refresh_one(&f.work, &asked).outcome;
        assert!(matches!(got, Outcome::Behind(2)), "{got:?}");
        assert_eq!(f.head(), before);
    }

    /// Where `p` will not update, it has still fetched, and "behind" is true.
    #[test]
    fn p_fetches_even_where_it_does_not_update() {
        let _guard = isolate();
        let f = fleet("refresh-p-fetches");
        write(&f.work.join("README.md"), "my edit\n");
        f.remote_moves(1);
        let report = update_report(&f.work);
        assert!(matches!(report.outcome, Outcome::Skipped(Skip::Dirty)), "{:?}", report.outcome);
        assert!(report.fetched);
        assert_eq!(origin_main(&f.work), f.remote_ref("refs/heads/main"));
    }

    #[test]
    fn a_clean_branch_that_is_behind_is_fast_forwarded() {
        let _guard = isolate();
        let f = fleet("refresh-ff");
        f.remote_moves(3);
        assert!(matches!(update(&f), Outcome::FastForwarded(3)));
        assert_eq!(f.head(), f.remote_ref("refs/heads/main"));
    }



    #[test]
    fn an_unstaged_edit_holds_the_repository_back_and_survives() {
        let _guard = isolate();
        let f = fleet("refresh-dirty-unstaged");
        let before = f.head();
        write(&f.work.join("README.md"), "my edit\n");
        f.remote_moves(1);
        let got = update(&f);
        assert!(matches!(got, Outcome::Skipped(Skip::Dirty)), "{got:?}");
        assert_eq!(f.head(), before);
        assert_eq!(read(&f.work.join("README.md")), "my edit\n");
    }

    #[test]
    fn a_staged_edit_holds_the_repository_back() {
        let _guard = isolate();
        let f = fleet("refresh-dirty-staged");
        let before = f.head();
        write(&f.work.join("README.md"), "my edit\n");
        git(&f.work, &["add", "README.md"]);
        f.remote_moves(1);
        let got = update(&f);
        assert!(matches!(got, Outcome::Skipped(Skip::Dirty)), "{got:?}");
        assert_eq!(f.head(), before);
    }

    #[test]
    fn untracked_files_do_not_hold_it_back() {
        let _guard = isolate();
        let f = fleet("refresh-untracked");
        write(&f.work.join("notes.txt"), "mine\n");
        f.remote_moves(1);
        assert!(matches!(update(&f), Outcome::FastForwarded(1)));
        assert_eq!(read(&f.work.join("notes.txt")), "mine\n");
    }

    #[test]
    fn an_untracked_file_in_the_way_is_never_overwritten() {
        let _guard = isolate();
        let f = fleet("refresh-untracked-collision");
        let before = f.head();
        // The remote is about to add a file of this name.
        write(&f.work.join("upstream-0.txt"), "MY UNTRACKED WORK\n");
        f.remote_moves(1);
        let got = update(&f);
        let Outcome::Refused(why) = &got else { panic!("{got:?}") };
        assert!(why.contains("upstream-0.txt"), "git's refusal should name the file: {why}");
        assert_eq!(f.head(), before);
        assert_eq!(read(&f.work.join("upstream-0.txt")), "MY UNTRACKED WORK\n");
    }

    #[test]
    fn commits_of_its_own_are_never_merged() {
        let _guard = isolate();
        let f = fleet("refresh-diverged");
        commit_file(&f.work, "mine.txt", "mine\n", "local work");
        let before = f.head();
        f.remote_moves(1);
        let got = update(&f);
        assert!(matches!(got, Outcome::Skipped(Skip::Diverged)), "{got:?}");
        assert_eq!(f.head(), before);
    }

    /// Reasons not to update, and so asked after the fetch: `p` on such a
    /// repository fetches it all the same.
    #[test]
    fn detached_no_upstream_and_mid_merge_are_left_alone() {
        let _guard = isolate();

        let f = fleet("refresh-detached");
        git(&f.work, &["checkout", "-q", "--detach"]);
        f.remote_moves(1);
        let got = update(&f);
        assert!(matches!(got, Outcome::Skipped(Skip::Detached)), "{got:?}");

        let f = fleet("refresh-no-upstream");
        git(&f.work, &["checkout", "-q", "-b", "local-only"]);
        f.remote_moves(1);
        let got = update(&f);
        assert!(matches!(got, Outcome::Skipped(Skip::NoUpstream)), "{got:?}");

        let f = fleet("refresh-mid-merge");
        git(&f.work, &["checkout", "-q", "-b", "side"]);
        commit_file(&f.work, "README.md", "side\n", "side");
        git(&f.work, &["checkout", "-q", "main"]);
        commit_file(&f.work, "README.md", "main\n", "main");
        let (merged, _) = try_git(&f.work, &["merge", "side"]);
        assert!(!merged, "the fixture needs a conflicted merge in progress");
        let before = f.head();
        f.remote_moves(1);
        let got = update(&f);
        assert!(matches!(got, Outcome::Skipped(Skip::Busy)), "{got:?}");
        assert_eq!(f.head(), before);
    }


    // ---- ignored files: git overwrites them unless told not to ------------

    #[test]
    fn an_ignored_file_in_the_way_is_never_overwritten() {
        let _guard = isolate();
        let f = fleet("refresh-ignored-collision");
        // Everybody ignores .env and local/, as a tracked .gitignore says.
        f.remote_commits("ignore what is local", |seed| {
            write(&seed.join(".gitignore"), ".env\nlocal/\n");
        });
        assert!(matches!(update(&f), Outcome::FastForwarded(1)));
        write(&f.work.join(".env"), "KEY=the only copy\n");
        write(&f.work.join("local/notes.md"), "mine\n");
        let before = f.head();

        // Then somebody starts tracking a .env, and puts a file where the
        // ignored directory is.
        f.remote_commits("track .env", |seed| {
            write(&seed.join(".env"), "KEY=upstream\n");
            write(&seed.join("local"), "a file now\n");
        });

        let got = update(&f);
        assert_eq!(read(&f.work.join(".env")), "KEY=the only copy\n", "{got:?}");
        assert_eq!(read(&f.work.join("local/notes.md")), "mine\n", "{got:?}");
        assert_eq!(f.head(), before);
        let Outcome::Refused(why) = &got else { panic!("{got:?}") };
        assert!(
            why.contains(".env") || why.contains("local"),
            "git's refusal should name what is in the way: {why}"
        );

        // And not the second time either.
        let got = update(&f);
        assert!(matches!(got, Outcome::Refused(_)), "{got:?}");
        assert_eq!(read(&f.work.join(".env")), "KEY=the only copy\n");
    }

    #[test]
    fn a_path_ignored_only_in_this_clone_is_not_overwritten_either() {
        let _guard = isolate();
        let f = fleet("refresh-excluded-collision");
        write(&f.work.join(".git/info/exclude"), ".vscode/\n");
        write(&f.work.join(".vscode/settings.json"), "{ \"mine\": true }\n");
        let before = f.head();
        f.remote_commits("share editor settings", |seed| {
            write(&seed.join(".vscode/settings.json"), "{ \"team\": true }\n");
        });

        let got = update(&f);
        assert_eq!(read(&f.work.join(".vscode/settings.json")), "{ \"mine\": true }\n", "{got:?}");
        assert!(matches!(got, Outcome::Refused(_)), "{got:?}");
        assert_eq!(f.head(), before);
    }

    #[test]
    fn an_ignored_file_out_of_the_way_does_not_stop_the_update() {
        let _guard = isolate();
        let f = fleet("refresh-ignored-bystander");
        f.remote_commits("ignore .env", |seed| write(&seed.join(".gitignore"), ".env\n"));
        assert!(matches!(update(&f), Outcome::FastForwarded(1)));
        write(&f.work.join(".env"), "KEY=mine\n");
        f.remote_moves(2);
        let got = update(&f);
        assert!(matches!(got, Outcome::FastForwarded(2)), "{got:?}");
        assert_eq!(read(&f.work.join(".env")), "KEY=mine\n");
    }


    // ---- an update lands in the repository's own directory or nowhere ------

    #[test]
    fn a_fast_forward_never_lands_in_another_directory() {
        let _guard = isolate();
        let f = fleet("refresh-redirect");
        let elsewhere = scratch("refresh-redirect-victim");
        write(&elsewhere.join("README.md"), "the other directory's own file\n");
        git(&f.work, &["config", "core.worktree", elsewhere.to_str().unwrap()]);
        f.remote_commits("a payload", |seed| {
            write(&seed.join("README.md"), "overwritten\n");
            write(&seed.join(".config/autostart/payload.desktop"), "[Desktop Entry]\n");
        });

        for pass in [Pass::One] {
            let got = refresh_one(&f.work, &Options { fast_forward: true, ..options(pass) }).outcome;
            assert!(matches!(got, Outcome::Skipped(Skip::Elsewhere)), "{pass:?}: {got:?}");
        }
        assert_eq!(read(&elsewhere.join("README.md")), "the other directory's own file\n");
        assert!(!elsewhere.join(".config").exists(), "files were written outside the repository");
    }


    // ---- a fast-forward is not stopped half-way ---------------------------

    /// Upstream adds files that pass through a slow filter on checkout: the
    /// stand-in for git-lfs fetching objects, or a cold disk. The pass's
    /// timeout is shorter than the checkout takes.
    #[test]
    fn a_slow_checkout_is_waited_for_not_cut_off_by_the_fetch_timeout() {
        let _guard = isolate();
        let f = fleet("refresh-slow-checkout");
        f.remote_commits("binaries", |seed| {
            write(&seed.join(".gitattributes"), "*.bin filter=slow\n");
            for i in 0..3 {
                write(&seed.join(format!("big-{i}.bin")), "payload\n");
            }
        });
        git(&f.work, &["config", "filter.slow.smudge", "sh -c 'sleep 1; cat'"]);
        git(&f.work, &["config", "filter.slow.clean", "cat"]);

        let hurried = Options { fast_forward: true, timeout: Duration::from_secs(1), ..options(Pass::One) };
        let got = refresh_one(&f.work, &hurried).outcome;

        assert!(matches!(got, Outcome::FastForwarded(1)), "{got:?}");
        assert_eq!(f.head(), f.remote_ref("refs/heads/main"));
        assert!(!f.work.join(".git/index.lock").exists());
        assert_eq!(git(&f.work, &["status", "--porcelain"]), "", "a half-updated tree");
    }


    // ---- an update the user asked for runs what their own pull would ------

    /// A clone whose hooks live in the tree, and an upstream commit that adds
    /// a post-merge hook leaving a marker behind.
    fn hook_arrives(name: &str) -> (Fleet, PathBuf) {
        let f = fleet(name);
        git(&f.work, &["config", "core.hooksPath", ".githooks"]);
        let marker = f.root.join("HOOK-RAN");
        let script = format!("#!/bin/sh\ntouch '{}'\n", marker.display());
        f.remote_commits("add a post-merge hook", |seed| {
            let hook = seed.join(".githooks/post-merge");
            write(&hook, &script);
            std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        });
        (f, marker)
    }



    #[test]
    fn a_hook_runs_when_the_user_asks_for_the_update() {
        let _guard = isolate();
        let (f, marker) = hook_arrives("refresh-hook-asked");
        let got = update(&f);
        assert!(matches!(got, Outcome::FastForwarded(1)), "{got:?}");
        assert!(marker.exists(), "their own pull would have run it, so this does too");
    }


    // ---- a fast-forward is one, whatever the merge settings say -----------

    /// Each of these makes `git merge --ff-only` do something that is not a
    /// fast-forward and exit 0: a merge commit that throws upstream's changes
    /// away, or upstream's whole diff staged with the branch left where it
    /// was. Each was reported as "updated".
    #[test]
    fn merge_settings_cannot_turn_the_fast_forward_into_something_else() {
        let _guard = isolate();
        let mut wrong = Vec::new();
        for (name, key, value) in [
            ("ours", "branch.main.mergeoptions", "-s ours"),
            ("subtree", "branch.main.mergeoptions", "-s subtree"),
            ("squash", "branch.main.mergeoptions", "--squash"),
            ("twohead", "pull.twohead", "ours"),
        ] {
            for pass in [Pass::One] {
                let f = fleet(&format!("rf-merge-config-{name}-{pass:?}"));
                git(&f.work, &["config", key, value]);
                f.remote_commits("an edit and a new file", |seed| {
                    write(&seed.join("README.md"), "two\n");
                    write(&seed.join("added.txt"), "new\n");
                });

                let got = refresh_one(&f.work, &Options { fast_forward: true, ..options(pass) }).outcome;

                let fast_forwarded = matches!(got, Outcome::FastForwarded(1))
                    && f.head() == f.remote_ref("refs/heads/main")
                    && git(&f.work, &["status", "--porcelain"]).is_empty()
                    && read(&f.work.join("README.md")) == "two\n";
                if !fast_forwarded {
                    wrong.push(format!(
                        "{name}, {pass:?}: {got:?}, then `{}` and status `{}`",
                        git(&f.work, &["log", "--format=%s", "-1"]),
                        git(&f.work, &["status", "--porcelain"])
                    ));
                }
            }
        }
        assert!(wrong.is_empty(), "not a fast-forward:\n{}", wrong.join("\n"));
    }

    /// The module promises never to stash. `merge.autoStash` was switched off
    /// for that; the same request in a branch's own merge options was not.
    /// And `p` leaves a repository with work in it alone before git is asked.
    #[test]
    fn a_branch_that_asks_for_autostash_is_not_stashed_around() {
        let _guard = isolate();
        let f = fleet("rf-merge-config-autostash");
        git(&f.work, &["config", "branch.main.mergeoptions", "--autostash"]);
        let before = f.head();
        write(&f.work.join("README.md"), "my edit\n");
        f.remote_commits("their edit to the same file", |seed| {
            write(&seed.join("README.md"), "their edit\n");
        });

        let got = update(&f);

        assert_eq!(read(&f.work.join("README.md")), "my edit\n", "{got:?}");
        assert_eq!(git(&f.work, &["stash", "list"]), "", "{got:?}");
        assert_eq!(f.head(), before, "{got:?}");
        assert!(matches!(got, Outcome::Skipped(Skip::Dirty)), "{got:?}");
    }

    /// Setting a branch's merge options aside must not set aside a refusal
    /// the user asked for.
    #[test]
    fn a_branch_that_insists_on_signed_commits_still_does() {
        let _guard = isolate();
        let f = fleet("rf-merge-config-signed");
        git(&f.work, &["config", "branch.main.mergeoptions", "--verify-signatures"]);
        let before = f.head();
        f.remote_moves(1);

        let got = update(&f);

        let Outcome::Refused(why) = &got else { panic!("{got:?}") };
        assert!(why.contains("signature"), "{why}");
        assert_eq!(f.head(), before);
    }

    /// `-c branch.<name>.mergeoptions=` is how the options are set aside, and
    /// it cannot be spelled for a branch with `=` in its name: git reads the
    /// key as ending at the first one.
    #[test]
    fn merge_options_that_cannot_be_set_aside_stop_the_update() {
        let _guard = isolate();
        let f = fleet("rf-merge-config-equals");
        git(&f.work, &["branch", "-m", "main", "rel=1"]);
        git(&f.work, &["config", "branch.rel=1.mergeoptions", "-s ours"]);
        let before = f.head();
        f.remote_moves(1);

        for pass in [Pass::One] {
            let got = refresh_one(&f.work, &Options { fast_forward: true, ..options(pass) }).outcome;
            let Outcome::Refused(why) = &got else { panic!("{pass:?}: {got:?}") };
            assert!(why.contains("mergeoptions"), "{pass:?}: {why}");
            assert_eq!(f.head(), before, "{pass:?}");
        }

        // Without options of its own, a branch named like that is updated
        // like any other.
        git(&f.work, &["config", "--unset", "branch.rel=1.mergeoptions"]);
        let got = update(&f);
        assert!(matches!(got, Outcome::FastForwarded(1)), "{got:?}");
        assert_eq!(f.head(), f.remote_ref("refs/heads/main"));
    }

    /// The last line of defence, for whatever the switches above do not
    /// cover: exit 0 is not what "fast-forwarded" means.
    #[test]
    fn success_is_believed_only_when_the_branch_is_at_its_upstream() {
        let _guard = isolate();
        let f = fleet("rf-landed");
        f.remote_moves(2);
        git(&f.work, &["fetch", "-q"]);
        let upstream = git2::Oid::from_str(&f.remote_ref("refs/heads/main")).unwrap();

        let said = landed(&f.work, upstream).unwrap_err();
        assert!(said.contains(&f.head()[..7]), "it should say where the branch is: {said}");

        git(&f.work, &["merge", "-q", "--ff-only", "@{u}"]);
        assert_eq!(landed(&f.work, upstream), Ok(()));
    }


    // ---- an update that stops part-way -----------------------------------

    /// A directory nothing can be written into, the way a container running
    /// as root or a code generator leaves one, for as long as this lives.
    struct ReadOnly(PathBuf);

    impl ReadOnly {
        fn new(dir: PathBuf) -> Self {
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
            ReadOnly(dir)
        }
    }

    impl Drop for ReadOnly {
        fn drop(&mut self) {
            let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
        }
    }

    /// A clone whose next update changes a file in each of three directories
    /// and adds a fourth file.
    fn four_files_incoming(name: &str) -> Fleet {
        let f = fleet(name);
        f.remote_commits("lay the tree out", |seed| {
            write(&seed.join("a/one.txt"), "one\n");
            write(&seed.join("docs/guide.md"), "guide\n");
            write(&seed.join("z/last.txt"), "last\n");
        });
        assert!(matches!(update(&f), Outcome::FastForwarded(1)));
        f.remote_commits("change all three and add one", |seed| {
            write(&seed.join("a/one.txt"), "one, upstream\n");
            write(&seed.join("a/added.txt"), "added upstream\n");
            write(&seed.join("docs/guide.md"), "guide, upstream\n");
            write(&seed.join("z/last.txt"), "last, upstream\n");
        });
        f
    }

    fn names_what_was_left(why: &str) -> bool {
        ["a/one.txt", "a/added.txt", "z/last.txt"].iter().all(|file| why.contains(file))
    }

    /// git's checkout is not a transaction. When one path cannot be written it
    /// has already written others, and it exits without moving the branch.
    #[test]
    fn an_update_that_stops_part_way_says_what_it_left_behind() {
        let _guard = isolate();
        if as_root() {
            return;
        }
        for pass in [Pass::One] {
            let f = four_files_incoming(&format!("rf-part-way-{pass:?}"));
            let before = f.head();
            let locked = ReadOnly::new(f.work.join("docs"));

            let got = refresh_one(&f.work, &Options { fast_forward: true, ..options(pass) }).outcome;

            drop(locked);
            assert_eq!(f.head(), before, "{pass:?}: {got:?}");
            assert_eq!(read(&f.work.join("a/one.txt")), "one, upstream\n", "the fixture: {got:?}");
            let Outcome::Refused(why) = &got else { panic!("{pass:?}: {got:?}") };
            assert!(why.contains("docs/guide.md"), "{pass:?}: git's own reason: {why}");
            assert!(names_what_was_left(why), "{pass:?}: and what it left: {why}");
        }
    }

    /// The pass after that finds a dirty tree, and used to say so: "left
    /// alone: uncommitted changes", about a repository nobody had edited. That
    /// report also cleared the record of the one before it.
    #[test]
    fn what_an_update_left_behind_is_not_then_called_the_users_changes() {
        let _guard = isolate();
        if as_root() {
            return;
        }
        let f = four_files_incoming("rf-part-way-after");
        let before = f.head();
        let locked = ReadOnly::new(f.work.join("docs"));
        let first = update(&f);
        drop(locked);
        assert!(matches!(first, Outcome::Refused(_)), "{first:?}");

        // On a pass across the fleet, asked to update or not (it does not
        // update), and when the user fetches it by hand.
        let mut said = Vec::new();
        let later = [(true, Pass::Fleet), (false, Pass::Fleet), (false, Pass::One)];
        for (fast_forward, pass) in later {
            let got = refresh_one(&f.work, &Options { fast_forward, ..options(pass) }).outcome;
            let Outcome::Refused(why) = got else { panic!("{fast_forward} {pass:?}: {got:?}") };
            assert!(names_what_was_left(&why), "{fast_forward} {pass:?}: {why}");
            said.push(why);
        }
        // The same words every time: only new trouble raises a toast.
        assert!(said.iter().all(|why| *why == said[0]), "{said:?}");

        // Asked to update it, it is left alone, since it is not clean, and
        // the answer says whose the changes are.
        let got = update(&f);
        let Outcome::Refused(why) = &got else { panic!("{got:?}") };
        assert!(names_what_was_left(why), "{why}");
        assert_eq!(f.head(), before);

        // The way out the message gives is the one that works.
        git(&f.work, &["restore", "a/one.txt", "z/last.txt"]);
        std::fs::remove_file(f.work.join("a/added.txt")).unwrap();
        let got = update(&f);
        assert!(matches!(got, Outcome::FastForwarded(1)), "{got:?}");
        assert_eq!(f.head(), f.remote_ref("refs/heads/main"));
    }

    /// An update that only adds files leaves nothing "modified" behind it,
    /// only files git calls untracked, and the tree does not even look dirty.
    /// The next attempt is then refused over a file "in the way" that the
    /// attempt before it put there.
    #[test]
    fn a_file_an_update_added_before_it_stopped_is_recognised_as_its_own() {
        let _guard = isolate();
        if as_root() {
            return;
        }
        let f = fleet("rf-part-way-added");
        f.remote_commits("somewhere for the docs", |seed| {
            write(&seed.join("docs/index.md"), "index\n");
        });
        assert!(matches!(update(&f), Outcome::FastForwarded(1)));
        let before = f.head();
        f.remote_commits("two new files", |seed| {
            write(&seed.join("a/new.txt"), "new\n");
            write(&seed.join("docs/new.md"), "new\n");
        });
        let locked = ReadOnly::new(f.work.join("docs"));
        let first = update(&f);
        drop(locked);
        let Outcome::Refused(why) = &first else { panic!("{first:?}") };
        assert!(why.contains("docs/new.md"), "git's own reason: {why}");
        assert!(why.contains("a/new.txt holds the incoming version"), "{why}");

        let got = update(&f);
        let Outcome::Refused(why) = &got else { panic!("{got:?}") };
        assert!(why.contains("would be overwritten"), "git's own reason: {why}");
        assert!(why.contains("a/new.txt holds the incoming version"), "{why}");
        assert_eq!(f.head(), before);
    }

    /// Changes that are the user's own are still theirs, and still reported
    /// as such, including beside something an update left.
    #[test]
    fn an_edit_of_the_users_own_is_never_mistaken_for_a_leftover() {
        let _guard = isolate();
        let f = four_files_incoming("rf-part-way-not");
        write(&f.work.join("a/one.txt"), "one, mine\n");
        write(&f.work.join("notes.txt"), "mine\n");

        let got = update(&f);
        assert!(matches!(got, Outcome::Skipped(Skip::Dirty)), "{got:?}");
        let got = refresh_one(&f.work, &Options { fast_forward: false, ..options(Pass::Fleet) }).outcome;
        assert!(matches!(got, Outcome::Behind(1)), "{got:?}");

        let got = update(&f);
        assert!(matches!(got, Outcome::Skipped(Skip::Dirty)), "{got:?}");
        assert_eq!(read(&f.work.join("a/one.txt")), "one, mine\n");

        // An update that takes files away, one of them tracked although it is
        // ignored: a file still there is not "already what upstream has".
        let f = fleet("rf-part-way-not-deleted");
        f.remote_commits("a log that got committed", |seed| {
            write(&seed.join(".gitignore"), "*.log\n");
            write(&seed.join("build.log"), "noise\n");
            write(&seed.join("old.txt"), "old\n");
        });
        assert!(matches!(update(&f), Outcome::FastForwarded(1)));
        f.remote_commits("take them out", |seed| {
            std::fs::remove_file(seed.join("build.log")).unwrap();
            std::fs::remove_file(seed.join("old.txt")).unwrap();
        });
        write(&f.work.join("README.md"), "my edit\n");
        let got = update(&f);
        assert!(matches!(got, Outcome::Skipped(Skip::Dirty)), "{got:?}");
    }


    /// `git merge` moves a submodule's pointer and leaves its checkout where
    /// it was. The pass said "updated", the card said "clean", `git status`
    /// said modified, and the build ran new code against the old library.
    #[test]
    fn an_update_that_moves_a_checked_out_submodule_is_left_to_the_user() {
        let _guard = isolate();
        for pass in [Pass::One] {
            let v = vendored(&format!("rf-submodule-update-{pass:?}"));
            let before = v.f.head();
            v.pointer_moves();

            let got = refresh_one(&v.f.work, &Options { fast_forward: true, ..options(pass) }).outcome;

            assert_eq!(git(&v.f.work, &["status", "--porcelain"]), "", "{pass:?}: {got:?}");
            assert!(matches!(got, Outcome::Skipped(Skip::Submodule)), "{pass:?}: {got:?}");
            assert_eq!(v.f.head(), before, "{pass:?}");
        }
    }

    /// Only that. An update that leaves the pointer alone goes through, and
    /// so does one that moves it where the submodule was never checked out:
    /// there is no checkout to leave behind.
    #[test]
    fn a_submodule_holds_back_only_the_update_that_would_strand_it() {
        let _guard = isolate();
        let v = vendored("rf-submodule-bystander");
        v.f.remote_moves(1);
        let got = update(&v.f);
        assert!(matches!(got, Outcome::FastForwarded(1)), "{got:?}");

        let v = vendored("rf-submodule-absent");
        git(&v.f.work, &["submodule", "--quiet", "deinit", "-f", "vendor/lib"]);
        v.pointer_moves();
        let got = update(&v.f);
        assert!(matches!(got, Outcome::FastForwarded(1)), "{got:?}");
        assert_eq!(v.f.head(), v.f.remote_ref("refs/heads/main"));
        assert_eq!(git(&v.f.work, &["status", "--porcelain"]), "");
    }
}
