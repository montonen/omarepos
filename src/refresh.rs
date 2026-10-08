//! Background refresh: fetch, and fast-forward only where that is unambiguously
//! safe.
//!
//! The network work shells out to `git` rather than going through libgit2.
//! That is deliberate: a user's authentication lives in credential helpers,
//! an SSH agent, `insteadOf` rewrites and `includeIf` identities, none of which
//! libgit2 reads. Driving the same `git` the user drives means their setup
//! applies unchanged, and a repository they cannot fetch by hand is one
//! omarepos does not pretend it can fetch either.
//!
//! What this will never do: merge, rebase, stash, reset, force anything, update
//! under uncommitted changes unless asked for that one repository, or move a
//! branch that has commits of its own. The only write it performs is a
//! fast-forward of the checked-out branch, and `git merge --ff-only` refuses
//! anything else even if the checks here were wrong.
//!
//! Three things about git had to be dealt with for that paragraph to be true,
//! and all three were found the hard way:
//!
//! - `--ff-only` refuses what the command line asks for. What `git merge`
//!   then does is also decided by configuration: with a merge strategy of the
//!   user's, or a branch's own merge options, the same command wrote a merge
//!   commit, or staged a squash, and exited 0. Those settings are overridden
//!   for the one command, and afterwards the branch is looked at instead of
//!   the exit code being believed; see [`FAST_FORWARD`], [`fast_forward`] and
//!   [`landed`].
//! - `git merge` overwrites *ignored* files that are in the way of what it is
//!   checking out, without a word — an ignored `.env` is replaced the day
//!   somebody upstream starts tracking one. It is run with
//!   `--no-overwrite-ignore`, so git refuses instead and the file survives.
//! - A fast-forward runs hooks, and where the hooks live in the tree
//!   (`core.hooksPath`, husky) the hook that runs is the one the fast-forward
//!   just wrote. On a pass across the whole fleet, where nobody chose the
//!   repository, hooks and the other commands a repository's own config can
//!   name are switched off; see [`Pass`].
//!
//! "The only write" is about the working tree and the branch. Three other
//! things in a repository are touched by the two commands this runs, and the
//! review found each of them described as not happening:
//!
//! - A fetch moves the remote-tracking branches, `origin/*`. That is what it
//!   is for, and it is not nothing: `git push --force-with-lease` with no
//!   value compares the remote against exactly those refs, so after a pass it
//!   no longer protects a commit somebody pushed since the user last looked.
//!   The pass has seen it on their behalf. git-push(1) says the same of
//!   anything that fetches in the background, and
//!   `push.useForceIfIncludes = true` restores the check. It is not fixed
//!   here: fetching to somewhere else (`--prefetch`) would leave the cards and
//!   the fast-forward with nothing to read.
//! - A fetch rewrites `FETCH_HEAD`, and goes on into submodules. Both are
//!   switched off; see [`FETCH`].
//! - A fast-forward sets `ORIG_HEAD` to where the branch was, as any `git
//!   merge` does, and so does an attempt git refuses. There is no switch for
//!   it. After an update it is what makes `git reset --hard ORIG_HEAD` undo
//!   the update, so it is left as git writes it rather than put back.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use git2::{BranchType, Repository, RepositoryState, StatusOptions};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Skip {
    /// Uncommitted changes; updating under them is not ours to decide.
    Dirty,
    /// No branch checked out, so there is nothing to fast-forward.
    Detached,
    Unborn,
    Bare,
    /// The branch tracks nothing.
    NoUpstream,
    /// Has commits the upstream does not; a fast-forward would be a lie.
    Diverged,
    /// A merge, rebase, bisect or cherry-pick is in progress.
    Busy,
    /// Its git data points at a work tree or a git directory somewhere else;
    /// see [`crate::git::belongs_here`].
    Elsewhere,
    /// Found since the user last looked, and not yet opened or asked for. A
    /// pass across the fleet does not enter a repository nobody has seen.
    Unseen,
    /// The incoming commits move the pointer of a submodule that is checked
    /// out here. `git merge` moves the pointer and leaves the checkout where
    /// it was, and this module does not update submodules.
    Submodule,
    /// The user moved the branch back along its upstream, and has not moved
    /// it forward since. Only a pass across the fleet gives this: asking for
    /// the repository by name is how it is moved forward again.
    SteppedBack,
}

impl Skip {
    pub fn reason(self) -> &'static str {
        match self {
            Skip::Dirty => "uncommitted changes",
            Skip::Detached => "detached HEAD",
            Skip::Unborn => "no commits",
            Skip::Bare => "bare repository",
            Skip::NoUpstream => "no upstream",
            Skip::Diverged => "has unpushed commits",
            Skip::Busy => "merge or rebase in progress",
            Skip::Elsewhere => "its work tree is not this directory",
            Skip::Unseen => "new here, open it once",
            Skip::Submodule => "update moves a submodule",
            Skip::SteppedBack => "moved back by hand",
        }
    }
}

#[derive(Debug, Clone)]
pub enum Outcome {
    /// Fetched, and there is nothing to bring in: the branch was already
    /// current. On a pass that only fetches, also where there is nothing to
    /// measure: no branch checked out, or one that tracks nothing.
    UpToDate,
    /// Fetched, and the branch is now behind; not fast-forwarded. On a pass
    /// that only fetches this is said whatever else is true of the branch,
    /// commits of its own included.
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
    /// One repository the user pointed at and asked to update. Their whole git
    /// setup applies, hooks included, exactly as if they had typed the pull.
    One,
    /// Everything in scope: the timer, the catch-up at launch, and a fetch of
    /// the whole fleet. Nobody picked these repositories one by one and nobody
    /// is watching them, so nothing a repository carries gets to run: see
    /// [`UNATTENDED`].
    Fleet,
}

/// Put in front of every git command on a [`Pass::Fleet`]. Each entry closes a
/// way for a repository to run a command of its own choosing:
///
/// - hooks, including a `core.hooksPath` pointing into the tree, where a
///   pulled commit can rewrite the hook that is about to run;
/// - the fsmonitor command, which git runs to refresh the index;
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
/// fetch or a fast-forward already running is allowed to finish: stopping git
/// in the middle of either is worse than letting it end.
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
    pub fast_forward: bool,
    pub max_parallel: usize,
    /// How long a fetch may go without anything arriving before it is given
    /// up on. Not how long it may take: see [`FETCH_PATIENCE`].
    pub timeout: Duration,
    pub pass: Pass,
    pub cancel: Cancel,
}

impl Pass {
    /// Run git for this kind of pass.
    fn git(self, path: &Path, args: &[&str], timeout: Duration) -> Result<String, String> {
        match self {
            Pass::One => crate::gitcmd::run(path, args, timeout),
            Pass::Fleet => {
                let mut all = UNATTENDED.to_vec();
                all.extend_from_slice(args);
                crate::gitcmd::run_fenced(path, &all, timeout)
            }
        }
    }

    /// Fetch for this kind of pass. `quiet` is how long the fetch may go
    /// without anything arriving, which is what `Options::timeout` now means
    /// for a fetch; see [`FETCH`].
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
/// to, which the README says does not happen, and it made the submodule's
/// remote this repository's problem: one that had moved away failed the fetch
/// of a repository that had itself fetched fine.
const FETCH: [&str; 4] = [
    "fetch",
    "--progress",
    "--no-write-fetch-head",
    "--no-recurse-submodules",
];

/// How long a fast-forward may take. Not `Options::timeout`: that one is tuned
/// to give up on a dead network within a minute, and this is a local write to
/// the working tree, which is worse stopped half-way than waited for. A
/// checkout cut short leaves some files at the new commit, the branch at the
/// old one, and a repository nobody touched looking as if somebody had edited
/// it. Half an hour is a backstop against a filter that never returns, not an
/// expectation.
const FAST_FORWARD_PATIENCE: Duration = Duration::from_secs(30 * 60);

/// The fast-forward, and nothing but. [`fast_forward`] puts the branch's own
/// switch in front of this and what to merge behind it.
///
/// `--ff-only` was described here as the backstop that "refuses anything else
/// even if the checks were wrong". It refuses a merge that the *command line*
/// asks for. What `git merge` does is also decided by configuration, read
/// before the command line and able to outvote it, and each of the following
/// made it do something else and exit 0.
///
/// `pull.twohead=recursive`: the key names the strategy for merging one head,
/// and a strategy that cannot fast-forward (`ours`, `subtree`) switches
/// `--ff-only` off without a word. git wrote a merge commit that threw
/// upstream's changes away, and the pass reported "updated". It is pinned to
/// a strategy that can, by the one name every git version knows: an empty
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

    let repo = match Repository::open(path) {
        Ok(repo) => repo,
        Err(err) => return report(Outcome::Failed(err.message().to_string())),
    };

    if crate::git::belongs_here(&repo, path).is_err() {
        return report(Outcome::Skipped(Skip::Elsewhere));
    }
    if let Some(skip) = disqualify(&repo, options.fast_forward) {
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
    // Asked again: the fetch ran this repository's own git, which is long
    // enough for its config to have changed.
    if crate::git::belongs_here(&repo, path).is_err() {
        return report(Outcome::Skipped(Skip::Elsewhere));
    }
    let at = match tracking(&repo) {
        Ok(at) => at,
        // Asked only to fetch, that has been done, and "left alone: no
        // upstream" about a repository just fetched is not what happened.
        // There is nothing here to count commits against, so nothing is.
        Err(_) if !options.fast_forward => return report(Outcome::UpToDate),
        Err(skip) => return report(Outcome::Skipped(skip)),
    };
    let behind = at.behind;

    // Before "up to date" or "has commits of its own" is said about a branch
    // that is ahead: whose commits are they?
    if at.ahead > 0 {
        if let Some(why) = withdrawn(&repo) {
            return report(Outcome::Refused(why));
        }
    }

    // A pass that only fetches reports the fetch. The reasons below are why a
    // branch would not be *updated*, and were given for one that nobody had
    // asked to update: "left alone: has unpushed commits" after a fetch that
    // had worked, counted as skipped and not as fetched.
    if !options.fast_forward {
        if behind == 0 {
            return report(Outcome::UpToDate);
        }
        // Nothing is updated on this pass, but an update that stopped
        // part-way on an earlier one is still lying here, and "fetched, 3
        // behind" would clear the only record of it.
        if at.ahead == 0 && is_dirty(&repo) {
            let left = half_applied(&repo, &at);
            if !left.is_empty() {
                return report(Outcome::Refused(part_way(&left)));
            }
        }
        return report(Outcome::Behind(behind));
    }

    if behind == 0 {
        return report(Outcome::UpToDate);
    }
    if at.ahead > 0 {
        return report(Outcome::Skipped(Skip::Diverged));
    }
    // Behind because the remote moved, or because the user moved back? The
    // second is theirs to undo, by asking for this repository.
    if options.pass == Pass::Fleet && stepped_back(&repo, &at) {
        return report(Outcome::Skipped(Skip::SteppedBack));
    }

    // Cleanliness is checked again here rather than before the fetch: the fetch
    // took time, and the user may have started editing during it.
    //
    // Only a fleet pass holds a dirty repository back outright. Asked for one
    // repository, the question goes to git, which answers it better: a
    // fast-forward leaves uncommitted changes alone when the incoming commits
    // do not touch the same files, and refuses, changing nothing, when they
    // do. Holding back here too left "behind, with an edit somewhere" with no
    // way forward but a commit, and that commit diverged the branch.
    if options.pass == Pass::Fleet && is_dirty(&repo) {
        // The changes may not be the user's. A fast-forward that stopped
        // part-way leaves upstream's own files behind as "modified", and the
        // pass after it used to report "uncommitted changes" about a
        // repository nobody had touched, which also wiped the record of what
        // had gone wrong.
        let left = half_applied(&repo, &at);
        return report(if left.is_empty() {
            Outcome::Skipped(Skip::Dirty)
        } else {
            Outcome::Refused(part_way(&left))
        });
    }

    // On either kind of pass: this is not a question git answers. It moves
    // the pointer, leaves the checkout, and exits 0.
    if strands_a_submodule(&repo, &at) {
        return report(Outcome::Skipped(Skip::Submodule));
    }

    // Called off while this repository was being fetched: it stays fetched,
    // and where it was.
    if options.cancel.stopped() || crate::gitcmd::winding_down() {
        return report(Outcome::Behind(behind));
    }

    let command = match fast_forward(&repo, &at.branch) {
        Ok(command) => command,
        Err(why) => return report(Outcome::Refused(why)),
    };
    let command: Vec<&str> = command.iter().map(String::as_str).collect();
    match options.pass.git(path, &command, FAST_FORWARD_PATIENCE) {
        // Exit 0 is git's opinion. Where the branch is now is the fact.
        Ok(_) => match landed(path, at.upstream) {
            Ok(()) => report(Outcome::FastForwarded(behind)),
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
/// lock ref". That was one "fetch failed" card and one update put off, on
/// every pass that had something new to bring in.
///
/// One fetch at a time for each shared git directory, then. Each checkout
/// still gets its own fetch, since two worktrees can track different remotes;
/// the second usually finds nothing left to do. The locks are per process and
/// shared by every pass in it, so `p` on one worktree waits for a fleet pass
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
/// Which reasons depends on what the pass is for. A fetch needs a remote and
/// nothing else: it does not care that HEAD is detached, that a rebase is
/// under way or that there are no commits yet, and it disturbs none of them.
/// Those are reasons not to *update*. They used to be applied to every pass,
/// so the pass called "fetch only" never fetched such a repository at all,
/// and its `origin/*` went stale for as long as the rebase took.
///
/// A bare repository is left out of both. A mirror's fetch writes straight
/// into its branches, which is more than "fetch" means anywhere else here.
fn disqualify(repo: &Repository, updating: bool) -> Option<Skip> {
    if repo.is_bare() {
        return Some(Skip::Bare);
    }
    if updating {
        if repo.state() != RepositoryState::Clean {
            return Some(Skip::Busy);
        }
        if repo.head().is_err() {
            return Some(Skip::Unborn);
        }
        if repo.head_detached().unwrap_or(false) {
            return Some(Skip::Detached);
        }
    }
    if repo.remotes().map(|r| r.is_empty()).unwrap_or(true) {
        return Some(Skip::NoUpstream);
    }
    None
}

/// The checked-out branch and what it tracks, as they stand right now.
struct Tracking {
    /// The branch's short name: `main`.
    branch: String,
    /// The upstream's full name: `refs/remotes/origin/main`.
    upstream_ref: String,
    local: git2::Oid,
    upstream: git2::Oid,
    ahead: usize,
    behind: usize,
}

fn tracking(repo: &Repository) -> Result<Tracking, Skip> {
    let head = repo.head().map_err(|_| Skip::Unborn)?;
    let branch_name = head.shorthand().map_err(|_| Skip::Detached)?.to_string();
    let branch = repo
        .find_branch(&branch_name, BranchType::Local)
        .map_err(|_| Skip::NoUpstream)?;
    let upstream = branch.upstream().map_err(|_| Skip::NoUpstream)?;
    let upstream_ref = upstream.get().name().map_err(|_| Skip::NoUpstream)?.to_string();
    let local = branch.get().target().ok_or(Skip::NoUpstream)?;
    let remote = upstream.get().target().ok_or(Skip::NoUpstream)?;
    let (ahead, behind) = repo
        .graph_ahead_behind(local, remote)
        .map_err(|_| Skip::NoUpstream)?;
    Ok(Tracking { branch: branch_name, upstream_ref, local, upstream: remote, ahead, behind })
}

pub(crate) fn divergence(repo: &Repository) -> Result<(usize, usize), Skip> {
    tracking(repo).map(|at| (at.ahead, at.behind))
}

/// What to say when the checked-out branch still holds commits that its
/// upstream had and no longer has: the remote's branch was moved back, or
/// rewritten, since this clone was level with it. `None` when it does not.
///
/// Counting cannot see this. A branch moved back on the remote leaves a clone
/// that had kept up "1 ahead, 0 behind", exactly like a clone with a commit of
/// its owner's waiting to be pushed, and it was reported the same way: "up to
/// date", then "Push 1" on the graph, one click from putting back what a
/// teammate had just taken off the remote (a leaked key, in the review's
/// example). A commit replaced instead of removed left the clone "1 ahead, 1
/// behind", and its owner, who had committed nothing, was told it "has
/// unpushed commits".
///
/// git keeps what is needed to tell the two apart: the log of the upstream
/// ref records every value it has had. A commit by which the branch is ahead
/// that the upstream once *pointed at* was on the remote and has been taken
/// off it. A commit of the user's own was never there. Nothing is remembered
/// here, so a rewrite fetched from a terminal is seen as well, and it goes on
/// being seen for as long as the state lasts and the log reaches back.
///
/// Public within the crate because the place this matters most is the push,
/// which is not this module's.
pub(crate) fn withdrawn(repo: &Repository) -> Option<String> {
    let at = tracking(repo).ok()?;
    // A branch that tracks another local branch has that branch's own log to
    // answer to, and its owner rewrites it at will.
    let upstream = at.upstream_ref.strip_prefix("refs/remotes/")?;
    if at.ahead == 0 {
        return None;
    }
    let mut walk = repo.revwalk().ok()?;
    walk.push(at.local).ok()?;
    walk.hide(at.upstream).ok()?;
    let ahead: HashSet<git2::Oid> = walk.filter_map(Result::ok).collect();

    // Newest first: the most recent tip the remote gave up.
    let log = repo.reflog(&at.upstream_ref).ok()?;
    let was = log.iter().map(|entry| entry.id_old()).find(|old| ahead.contains(old))?;
    let (gone, _) = repo.graph_ahead_behind(was, at.upstream).ok()?;
    let tip = repo.find_commit(was).ok()?;
    let short = &was.to_string()[..7];
    let summary = tip.summary().ok().flatten().unwrap_or("no message");
    let (them, were) = if gone == 1 { ("it", "it was") } else { ("them", "they were") };
    Some(format!(
        "{upstream} was moved back or rewritten on the remote. It used to end at {short} \
         (\"{summary}\"); that is no longer on the remote and is still in this branch, {} taken \
         off it in all. Pushing this branch would put {them} back: find out why {were} removed \
         before pushing or building on {them}.",
        if gone == 1 { "1 commit".to_string() } else { format!("{gone} commits") }
    ))
}

/// Whether the branch is where a fast-forward to `meant` puts it.
///
/// git exiting 0 used to be the whole of the evidence, and the count reported
/// was one worked out before the command ran. With a merge strategy of the
/// user's in force the same exit code came back from a merge commit, and from
/// a squash that staged upstream's diff and left the branch where it was; the
/// card then offered "Push 1" for a commit nobody had written. Those two are
/// closed off in [`fast_forward`]. This is for the one nobody has thought of.
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

/// Whether the last thing to move the checked-out branch was the user stepping
/// it back along its upstream.
///
/// "Clean and strictly behind" is what a branch is when the remote has moved,
/// and it is what a branch is after `git reset --hard` to an older commit. The
/// pass could not tell them apart, so stepping back lasted one interval: to
/// get away from an update that broke the build (`git reset --hard ORIG_HEAD`,
/// which is how git-reset(1) says to undo a pull), to reproduce a bug on last
/// week's code, the tree was moved forward again under whatever was running
/// in it, and again after every further reset.
///
/// The branch's own log says which it is. Its newest entry is the last thing
/// that moved it, and a step back has two marks: the branch went to an
/// ancestor of where it was, and where it was is a commit the upstream has.
/// The second is what keeps `git reset --hard HEAD~1` to throw away a commit
/// of one's own from freezing the branch: that is stepping back onto the
/// upstream, not along it.
///
/// It holds until the branch is moved again, by anything: `p` here, a pull in
/// a terminal. Only a pass across the fleet asks.
fn stepped_back(repo: &Repository, at: &Tracking) -> bool {
    let Ok(log) = repo.reflog(&format!("refs/heads/{}", at.branch)) else {
        return false;
    };
    let Some(last) = log.get(0) else { return false };
    let (from, to) = (last.id_old(), last.id_new());
    // A log that does not end where the branch is has missed something, and
    // is not evidence of anything.
    if from.is_zero() || to != at.local {
        return false;
    }
    let behind_where_it_was = repo.graph_descendant_of(from, to).unwrap_or(false);
    let upstream_has_it =
        from == at.upstream || repo.graph_descendant_of(at.upstream, from).unwrap_or(false);
    behind_where_it_was && upstream_has_it
}

/// Whether fast-forwarding would move the pointer of a submodule that is
/// checked out here.
///
/// `git merge` does not update submodules, with or without
/// `submodule.recurse`. It records the new pointer and leaves the checkout on
/// the old commit, so the repository's code is new and the library's is not.
/// The pass reported that as "updated", the card showed "clean" because its
/// status leaves submodules out, and a build that broke overnight had no
/// visible cause. Bringing the submodule along is `git submodule update`,
/// which this module does not run; so the update is left for the user to
/// pull, and the reason says why.
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
    pub updated: usize,
    pub commits: usize,
    pub behind: usize,
    pub skipped: usize,
    /// Fetched, but git would not fast-forward.
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
            Outcome::Behind(n) => {
                self.fetched += 1;
                self.behind += 1;
                self.commits += n;
            }
            Outcome::FastForwarded(n) => {
                self.fetched += 1;
                self.updated += 1;
                self.commits += n;
            }
            Outcome::Skipped(skip) => {
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
            parts.push(format!("{} updated (+{} commits)", self.updated, self.commits));
        } else if self.behind > 0 {
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
            parts.push(format!("{} could not update", self.refused));
        }
        if self.failed > 0 {
            parts.push(format!("{} failed", self.failed));
        }
        parts.join(" · ")
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;
    use crate::testkit::*;

    fn options(fast_forward: bool, pass: Pass) -> Options {
        Options {
            fast_forward,
            max_parallel: 1,
            timeout: Duration::from_secs(30),
            pass,
            cancel: Cancel::default(),
        }
    }

    /// What a pass across the whole fleet does to the repository under test.
    fn fleet_pass(f: &Fleet) -> Outcome {
        refresh_one(&f.work, &options(true, Pass::Fleet)).outcome
    }

    /// What happens when the user asks for this one repository to be updated.
    fn asked_for(f: &Fleet) -> Outcome {
        refresh_one(&f.work, &options(true, Pass::One)).outcome
    }

    // ---- what the README says the refresher will and will not do ----------

    #[test]
    fn clean_and_behind_is_fast_forwarded() {
        let _guard = isolate();
        let f = fleet("refresh-ff");
        f.remote_moves(3);
        assert!(matches!(fleet_pass(&f), Outcome::FastForwarded(3)));
        assert_eq!(f.head(), f.remote_ref("refs/heads/main"));
    }

    #[test]
    fn fetch_only_never_moves_the_branch() {
        let _guard = isolate();
        let f = fleet("refresh-fetch-only");
        let before = f.head();
        f.remote_moves(2);
        let got = refresh_one(&f.work, &options(false, Pass::Fleet)).outcome;
        assert!(matches!(got, Outcome::Behind(2)), "{got:?}");
        assert_eq!(f.head(), before);
    }

    #[test]
    fn an_unstaged_edit_holds_the_repository_back_and_survives() {
        let _guard = isolate();
        let f = fleet("refresh-dirty-unstaged");
        let before = f.head();
        write(&f.work.join("README.md"), "my edit\n");
        f.remote_moves(1);
        let got = fleet_pass(&f);
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
        let got = fleet_pass(&f);
        assert!(matches!(got, Outcome::Skipped(Skip::Dirty)), "{got:?}");
        assert_eq!(f.head(), before);
    }

    #[test]
    fn untracked_files_do_not_hold_it_back() {
        let _guard = isolate();
        let f = fleet("refresh-untracked");
        write(&f.work.join("notes.txt"), "mine\n");
        f.remote_moves(1);
        assert!(matches!(fleet_pass(&f), Outcome::FastForwarded(1)));
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
        let got = fleet_pass(&f);
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
        let got = fleet_pass(&f);
        assert!(matches!(got, Outcome::Skipped(Skip::Diverged)), "{got:?}");
        assert_eq!(f.head(), before);
    }

    #[test]
    fn detached_no_upstream_and_mid_merge_are_left_alone() {
        let _guard = isolate();

        let f = fleet("refresh-detached");
        git(&f.work, &["checkout", "-q", "--detach"]);
        f.remote_moves(1);
        let got = fleet_pass(&f);
        assert!(matches!(got, Outcome::Skipped(Skip::Detached)), "{got:?}");

        let f = fleet("refresh-no-upstream");
        git(&f.work, &["checkout", "-q", "-b", "local-only"]);
        f.remote_moves(1);
        let got = fleet_pass(&f);
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
        let got = fleet_pass(&f);
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
        assert!(matches!(fleet_pass(&f), Outcome::FastForwarded(1)));
        write(&f.work.join(".env"), "KEY=the only copy\n");
        write(&f.work.join("local/notes.md"), "mine\n");
        let before = f.head();

        // Then somebody starts tracking a .env, and puts a file where the
        // ignored directory is.
        f.remote_commits("track .env", |seed| {
            write(&seed.join(".env"), "KEY=upstream\n");
            write(&seed.join("local"), "a file now\n");
        });

        let got = fleet_pass(&f);
        assert_eq!(read(&f.work.join(".env")), "KEY=the only copy\n", "{got:?}");
        assert_eq!(read(&f.work.join("local/notes.md")), "mine\n", "{got:?}");
        assert_eq!(f.head(), before);
        let Outcome::Refused(why) = &got else { panic!("{got:?}") };
        assert!(
            why.contains(".env") || why.contains("local"),
            "git's refusal should name what is in the way: {why}"
        );

        // Asking for this one repository is no licence to overwrite either.
        let got = asked_for(&f);
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

        let got = fleet_pass(&f);
        assert_eq!(read(&f.work.join(".vscode/settings.json")), "{ \"mine\": true }\n", "{got:?}");
        assert!(matches!(got, Outcome::Refused(_)), "{got:?}");
        assert_eq!(f.head(), before);
    }

    #[test]
    fn an_ignored_file_out_of_the_way_does_not_stop_the_update() {
        let _guard = isolate();
        let f = fleet("refresh-ignored-bystander");
        f.remote_commits("ignore .env", |seed| write(&seed.join(".gitignore"), ".env\n"));
        assert!(matches!(fleet_pass(&f), Outcome::FastForwarded(1)));
        write(&f.work.join(".env"), "KEY=mine\n");
        f.remote_moves(2);
        let got = fleet_pass(&f);
        assert!(matches!(got, Outcome::FastForwarded(2)), "{got:?}");
        assert_eq!(read(&f.work.join(".env")), "KEY=mine\n");
    }

    // ---- a pass can be called off -----------------------------------------

    #[test]
    fn a_pass_that_is_called_off_starts_nothing_more_and_updates_nothing() {
        let _guard = isolate();
        let f = fleet("refresh-called-off");
        let before = f.head();
        f.remote_moves(1);
        let called_off = options(true, Pass::Fleet);
        called_off.cancel.stop();

        // A repository already being fetched when the pass is called off stays
        // fetched, and where it was.
        let got = refresh_one(&f.work, &called_off).outcome;
        assert!(matches!(got, Outcome::Behind(1)), "{got:?}");
        assert_eq!(f.head(), before);

        // And nothing that had not started is started.
        let rx = spawn(vec![f.work.clone(), f.seed.clone()], called_off);
        let mut entered = 0;
        while let Ok(Event::Repo(_)) = rx.recv_blocking() {
            entered += 1;
        }
        assert_eq!(entered, 0);
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

        for pass in [Pass::Fleet, Pass::One] {
            let got = refresh_one(&f.work, &options(true, pass)).outcome;
            assert!(matches!(got, Outcome::Skipped(Skip::Elsewhere)), "{pass:?}: {got:?}");
        }
        assert_eq!(read(&elsewhere.join("README.md")), "the other directory's own file\n");
        assert!(!elsewhere.join(".config").exists(), "files were written outside the repository");
    }

    // ---- asked for one repository, git decides about uncommitted work -----

    #[test]
    fn asked_for_one_repository_an_edit_elsewhere_does_not_stop_the_update() {
        let _guard = isolate();
        let f = fleet("refresh-asked-dirty-elsewhere");
        write(&f.work.join("README.md"), "my edit\n");
        write(&f.work.join("staged.txt"), "staged\n");
        git(&f.work, &["add", "staged.txt"]);
        f.remote_moves(2);

        let got = asked_for(&f);

        assert!(matches!(got, Outcome::FastForwarded(2)), "{got:?}");
        assert_eq!(f.head(), f.remote_ref("refs/heads/main"));
        assert_eq!(read(&f.work.join("README.md")), "my edit\n");
        assert_eq!(git(&f.work, &["status", "--porcelain"]), "M README.md\nA  staged.txt");
    }

    #[test]
    fn asked_for_one_repository_an_edit_in_the_way_is_refused_and_kept() {
        let _guard = isolate();
        let f = fleet("refresh-asked-dirty-overlap");
        let before = f.head();
        write(&f.work.join("README.md"), "my edit\n");
        f.remote_commits("their edit to the same file", |seed| {
            write(&seed.join("README.md"), "their edit\n");
        });

        let got = asked_for(&f);

        let Outcome::Refused(why) = &got else { panic!("{got:?}") };
        assert!(why.contains("README.md"), "git's refusal should name the file: {why}");
        assert_eq!(f.head(), before);
        assert_eq!(read(&f.work.join("README.md")), "my edit\n");
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

        let hurried = Options { timeout: Duration::from_secs(1), ..options(true, Pass::Fleet) };
        let got = refresh_one(&f.work, &hurried).outcome;

        assert!(matches!(got, Outcome::FastForwarded(1)), "{got:?}");
        assert_eq!(f.head(), f.remote_ref("refs/heads/main"));
        assert!(!f.work.join(".git/index.lock").exists());
        assert_eq!(git(&f.work, &["status", "--porcelain"]), "", "a half-updated tree");
    }

    // ---- nothing a repository carries runs on a pass nobody is watching ---

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
    fn a_hook_that_arrives_in_the_pulled_commit_does_not_run_on_a_fleet_pass() {
        let _guard = isolate();
        let (f, marker) = hook_arrives("refresh-hook-fleet");
        let got = fleet_pass(&f);
        assert!(matches!(got, Outcome::FastForwarded(1)), "{got:?}");
        assert!(f.work.join(".githooks/post-merge").exists(), "the hook did arrive");
        assert!(!marker.exists(), "a hook ran on a pass nobody was watching");
    }

    #[test]
    fn hooks_still_run_when_the_user_asks_for_one_repository() {
        let _guard = isolate();
        let (f, marker) = hook_arrives("refresh-hook-asked");
        let got = asked_for(&f);
        assert!(matches!(got, Outcome::FastForwarded(1)), "{got:?}");
        assert!(marker.exists(), "their own pull would have run it, so this does too");
    }

    #[test]
    fn a_fleet_pass_does_not_run_the_fsmonitor_command() {
        let _guard = isolate();
        for (pass, runs) in [(Pass::Fleet, false), (Pass::One, true)] {
            let f = fleet(&format!("refresh-fsmonitor-{pass:?}"));
            let marker = f.root.join("FSMONITOR-RAN");
            let command = format!("touch '{}'; false", marker.display());
            git(&f.work, &["config", "core.fsmonitor", &command]);
            f.remote_moves(1);
            let got = refresh_one(&f.work, &options(true, pass)).outcome;
            assert!(matches!(got, Outcome::FastForwarded(1)), "{pass:?}: {got:?}");
            assert_eq!(marker.exists(), runs, "{pass:?}");
        }
    }

    #[test]
    fn a_fleet_pass_refuses_the_ext_transport_even_where_the_repository_allows_it() {
        let _guard = isolate();
        for (pass, runs) in [(Pass::Fleet, false), (Pass::One, true)] {
            let f = fleet(&format!("refresh-ext-{pass:?}"));
            let marker = f.root.join("EXT-RAN");
            // In an ext:: address "% " is a space inside one argument.
            let url = format!("ext::sh -c touch% {};% exit% 1", marker.display());
            git(&f.work, &["config", "protocol.ext.allow", "always"]);
            git(&f.work, &["remote", "set-url", "origin", &url]);
            let got = refresh_one(&f.work, &options(true, pass)).outcome;
            assert!(matches!(got, Outcome::Failed(_)), "{pass:?}: {got:?}");
            assert_eq!(marker.exists(), runs, "{pass:?}");
        }
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
            for pass in [Pass::Fleet, Pass::One] {
                let f = fleet(&format!("rf-merge-config-{name}-{pass:?}"));
                git(&f.work, &["config", key, value]);
                f.remote_commits("an edit and a new file", |seed| {
                    write(&seed.join("README.md"), "two\n");
                    write(&seed.join("added.txt"), "new\n");
                });

                let got = refresh_one(&f.work, &options(true, pass)).outcome;

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

        let got = asked_for(&f);

        assert_eq!(read(&f.work.join("README.md")), "my edit\n", "{got:?}");
        assert_eq!(git(&f.work, &["stash", "list"]), "", "{got:?}");
        assert_eq!(f.head(), before, "{got:?}");
        assert!(matches!(got, Outcome::Refused(_)), "{got:?}");
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

        let got = fleet_pass(&f);

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

        for pass in [Pass::Fleet, Pass::One] {
            let got = refresh_one(&f.work, &options(true, pass)).outcome;
            let Outcome::Refused(why) = &got else { panic!("{pass:?}: {got:?}") };
            assert!(why.contains("mergeoptions"), "{pass:?}: {why}");
            assert_eq!(f.head(), before, "{pass:?}");
        }

        // Without options of its own, a branch named like that is updated
        // like any other.
        git(&f.work, &["config", "--unset", "branch.rel=1.mergeoptions"]);
        let got = fleet_pass(&f);
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
        let hurried = Options { timeout: Duration::from_secs(1), ..options(false, Pass::Fleet) };

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
        let hurried = Options { timeout: Duration::from_secs(1), ..options(false, Pass::Fleet) };

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
            let report = fetch_only(&f.work, pass);
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

    // ---- fetching is not updating ----------------------------------------

    fn fetch_only(dir: &Path, pass: Pass) -> Report {
        refresh_one(dir, &options(false, pass))
    }

    /// Where the clone under test thinks the remote's `main` is.
    fn origin_main(dir: &Path) -> String {
        let (ok, out) = try_git(dir, &["rev-parse", "--verify", "-q", "origin/main"]);
        if ok { out.trim().to_string() } else { String::new() }
    }

    /// Fetch-only is `f`, and the timer with updates off. It went through the
    /// fast-forward's list of reasons to leave a repository alone before it
    /// fetched, so a detached HEAD or a rebase in progress, neither of which
    /// a fetch touches, was never fetched at all.
    #[test]
    fn fetch_only_fetches_where_an_update_would_have_nothing_to_move() {
        let _guard = isolate();
        for pass in [Pass::Fleet, Pass::One] {
            let f = fleet(&format!("rf-fetch-only-detached-{pass:?}"));
            git(&f.work, &["checkout", "-q", "--detach"]);
            let before = f.head();
            f.remote_moves(1);
            let report = fetch_only(&f.work, pass);
            assert_eq!(origin_main(&f.work), f.remote_ref("refs/heads/main"), "{pass:?}");
            assert!(report.fetched, "{pass:?}");
            assert!(matches!(report.outcome, Outcome::UpToDate), "{pass:?}: {:?}", report.outcome);
            assert_eq!(f.head(), before, "{pass:?}");

            let f = fleet(&format!("rf-fetch-only-mid-merge-{pass:?}"));
            git(&f.work, &["checkout", "-q", "-b", "side"]);
            commit_file(&f.work, "README.md", "side\n", "side");
            git(&f.work, &["checkout", "-q", "main"]);
            commit_file(&f.work, "README.md", "main\n", "main");
            let (merged, _) = try_git(&f.work, &["merge", "side"]);
            assert!(!merged, "the fixture needs a conflicted merge in progress");
            let before = f.head();
            f.remote_moves(1);
            let report = fetch_only(&f.work, pass);
            assert_eq!(origin_main(&f.work), f.remote_ref("refs/heads/main"), "{pass:?}");
            assert!(report.fetched, "{pass:?}");
            assert!(matches!(report.outcome, Outcome::Behind(1)), "{pass:?}: {:?}", report.outcome);
            assert_eq!(f.head(), before, "{pass:?}");
            assert!(f.work.join(".git/MERGE_HEAD").exists(), "{pass:?}: the merge was disturbed");

            // No commits yet, and a remote to fetch from.
            let f = fleet(&format!("rf-fetch-only-unborn-{pass:?}"));
            let empty = f.root.join("empty");
            std::fs::create_dir_all(&empty).unwrap();
            git(&empty, &["init", "-q", "-b", "main"]);
            git(&empty, &["remote", "add", "origin", f.remote.to_str().unwrap()]);
            let report = fetch_only(&empty, pass);
            assert_eq!(origin_main(&empty), f.remote_ref("refs/heads/main"), "{pass:?}");
            assert!(report.fetched, "{pass:?}");
            assert!(matches!(report.outcome, Outcome::UpToDate), "{pass:?}: {:?}", report.outcome);
        }
    }

    /// And after the fetch it reported the fast-forward's refusals: "left
    /// alone: has unpushed commits" about a repository it had just fetched.
    #[test]
    fn fetch_only_reports_the_fetch_and_not_why_an_update_would_not_happen() {
        let _guard = isolate();
        for pass in [Pass::Fleet, Pass::One] {
            let f = fleet(&format!("rf-fetch-only-diverged-{pass:?}"));
            commit_file(&f.work, "mine.txt", "mine\n", "local work");
            let before = f.head();
            f.remote_moves(2);
            let report = fetch_only(&f.work, pass);
            assert!(matches!(report.outcome, Outcome::Behind(2)), "{pass:?}: {:?}", report.outcome);
            assert!(report.fetched, "{pass:?}");
            assert_eq!(f.head(), before, "{pass:?}");

            // A branch that tracks nothing: fetched, and nothing to count.
            let f = fleet(&format!("rf-fetch-only-no-upstream-{pass:?}"));
            git(&f.work, &["checkout", "-q", "-b", "local-only"]);
            f.remote_moves(1);
            let report = fetch_only(&f.work, pass);
            assert_eq!(origin_main(&f.work), f.remote_ref("refs/heads/main"), "{pass:?}");
            assert!(matches!(report.outcome, Outcome::UpToDate), "{pass:?}: {:?}", report.outcome);
            assert!(report.fetched, "{pass:?}");
        }
    }

    /// What cannot be fetched is still not fetched, and with updates on
    /// nothing has changed: the same repositories are left alone, before the
    /// network is touched.
    #[test]
    fn what_cannot_be_fetched_is_left_alone_and_an_updating_pass_is_as_it_was() {
        let _guard = isolate();

        let f = fleet("rf-fetch-only-impossible");
        let report = fetch_only(&f.remote, Pass::Fleet);
        assert!(matches!(report.outcome, Outcome::Skipped(Skip::Bare)), "{:?}", report.outcome);
        assert!(!report.fetched);
        let alone = f.root.join("alone");
        std::fs::create_dir_all(&alone).unwrap();
        git(&alone, &["init", "-q", "-b", "main"]);
        commit_file(&alone, "README.md", "one\n", "first");
        let report = fetch_only(&alone, Pass::Fleet);
        assert!(matches!(report.outcome, Outcome::Skipped(Skip::NoUpstream)), "{report:?}");
        assert!(!report.fetched);

        for pass in [Pass::Fleet, Pass::One] {
            let f = fleet(&format!("rf-updating-detached-{pass:?}"));
            git(&f.work, &["checkout", "-q", "--detach"]);
            let seen = origin_main(&f.work);
            f.remote_moves(1);
            let report = refresh_one(&f.work, &options(true, pass));
            assert!(
                matches!(report.outcome, Outcome::Skipped(Skip::Detached)),
                "{pass:?}: {:?}",
                report.outcome
            );
            assert!(!report.fetched, "{pass:?}");
            assert_eq!(origin_main(&f.work), seen, "{pass:?}");

            let f = fleet(&format!("rf-updating-diverged-{pass:?}"));
            commit_file(&f.work, "mine.txt", "mine\n", "local work");
            f.remote_moves(1);
            let report = refresh_one(&f.work, &options(true, pass));
            assert!(
                matches!(report.outcome, Outcome::Skipped(Skip::Diverged)),
                "{pass:?}: {:?}",
                report.outcome
            );
            assert!(report.fetched, "{pass:?}");
        }
    }

    // ---- a branch the user moved back ------------------------------------

    /// "Clean and strictly behind" is what a branch is when the remote has
    /// moved. It is also what a branch is after `git reset --hard` to an older
    /// commit, and the timer could not tell: undoing its own update, the way
    /// git-reset(1) says to undo a pull, lasted one interval.
    #[test]
    fn a_branch_stepped_back_by_hand_is_not_moved_forward_again_unasked() {
        let _guard = isolate();
        let f = fleet("rf-stepped-back");
        f.remote_moves(2);
        assert!(matches!(fleet_pass(&f), Outcome::FastForwarded(2)));
        git(&f.work, &["reset", "-q", "--hard", "ORIG_HEAD"]);
        let chosen = f.head();

        let got = fleet_pass(&f);
        assert_eq!(f.head(), chosen, "{got:?}");
        assert!(matches!(got, Outcome::Skipped(Skip::SteppedBack)), "{got:?}");

        // More arriving upstream is no reason either.
        f.remote_commits("more", |seed| write(&seed.join("more.txt"), "more\n"));
        let report = refresh_one(&f.work, &options(true, Pass::Fleet));
        assert!(matches!(report.outcome, Outcome::Skipped(Skip::SteppedBack)), "{report:?}");
        assert!(report.fetched);
        assert_eq!(f.head(), chosen);

        // Asking for it by name is, and after that it is kept current again.
        let got = asked_for(&f);
        assert!(matches!(got, Outcome::FastForwarded(3)), "{got:?}");
        f.remote_commits("and more", |seed| write(&seed.join("yet-more.txt"), "more\n"));
        let got = fleet_pass(&f);
        assert!(matches!(got, Outcome::FastForwarded(1)), "{got:?}");
        assert_eq!(f.head(), f.remote_ref("refs/heads/main"));
    }

    /// Only a step back along the upstream counts. Throwing away a commit of
    /// one's own with the same command leaves a branch that should simply be
    /// kept current.
    #[test]
    fn dropping_a_commit_of_ones_own_does_not_hold_the_branch_back() {
        let _guard = isolate();
        let f = fleet("rf-stepped-back-not");
        commit_file(&f.work, "mine.txt", "mine\n", "an experiment");
        git(&f.work, &["reset", "-q", "--hard", "HEAD~1"]);
        f.remote_moves(1);
        let got = fleet_pass(&f);
        assert!(matches!(got, Outcome::FastForwarded(1)), "{got:?}");
    }

    // ---- an upstream that was rewritten ----------------------------------

    /// Somebody pushes a commit that should not have been pushed, and a
    /// teammate takes it off the remote again. A clone that had been level
    /// with it is now "1 ahead", with a commit its owner never wrote; the pass
    /// called that up to date, and the graph offered to push it back.
    #[test]
    fn a_commit_taken_off_the_remote_is_not_reported_as_up_to_date() {
        let _guard = isolate();
        for (fast_forward, pass) in
            [(true, Pass::Fleet), (false, Pass::Fleet), (true, Pass::One), (false, Pass::One)]
        {
            let which = format!("{fast_forward}-{pass:?}");
            let f = fleet(&format!("rf-rewound-{which}"));
            f.remote_commits("add the deploy key", |seed| {
                write(&seed.join("deploy.env"), "KEY=should not be here\n");
            });
            assert!(matches!(fleet_pass(&f), Outcome::FastForwarded(1)));
            let level = f.head();
            git(&f.seed, &["reset", "-q", "--hard", "HEAD~1"]);
            git(&f.seed, &["push", "-q", "--force", "origin", "main"]);

            let report = refresh_one(&f.work, &options(fast_forward, pass));

            let Outcome::Refused(why) = &report.outcome else { panic!("{which}: {report:?}") };
            assert!(why.contains(&level[..7]), "{which}: it should name the commit: {why}");
            assert!(why.contains("add the deploy key"), "{which}: {why}");
            assert!(why.contains("origin/main"), "{which}: {why}");
            assert!(report.fetched, "{which}");
            assert_eq!(f.head(), level, "{which}");

            // Not only on the pass that fetched the rewrite: the state is the
            // same on the next one, and so is what is said about it.
            let again = refresh_one(&f.work, &options(fast_forward, pass)).outcome;
            let Outcome::Refused(still) = &again else { panic!("{which}: {again:?}") };
            assert_eq!(still, why, "{which}");
        }
    }

    /// Replaced instead of removed: the last commit amended and force-pushed.
    /// A follower who had never committed was told it "has unpushed commits".
    #[test]
    fn a_commit_replaced_on_the_remote_is_not_called_the_users_own() {
        let _guard = isolate();
        let f = fleet("rf-rewritten");
        f.remote_moves(1);
        assert!(matches!(fleet_pass(&f), Outcome::FastForwarded(1)));
        let level = f.head();
        git(&f.seed, &["commit", "-q", "--amend", "-m", "upstream 0, reworded"]);
        git(&f.seed, &["push", "-q", "--force", "origin", "main"]);

        let got = fleet_pass(&f);

        let Outcome::Refused(why) = &got else { panic!("{got:?}") };
        assert!(why.contains(&level[..7]), "{why}");
        assert_eq!(f.head(), level);
    }

    /// Commits the user made are theirs, however they got ahead, and a remote
    /// they rewrote themselves is not news to them.
    #[test]
    fn commits_of_the_users_own_are_still_only_ahead() {
        let _guard = isolate();
        let f = fleet("rf-rewound-not");
        commit_file(&f.work, "mine.txt", "mine\n", "local work");
        let got = fleet_pass(&f);
        assert!(matches!(got, Outcome::UpToDate), "{got:?}");
        f.remote_moves(1);
        let got = fleet_pass(&f);
        assert!(matches!(got, Outcome::Skipped(Skip::Diverged)), "{got:?}");

        // Pushed, amended, force-pushed, and then one more commit on top.
        let f = fleet("rf-rewound-own-push");
        commit_file(&f.work, "mine.txt", "mine\n", "local work");
        git(&f.work, &["push", "-q", "origin", "main"]);
        git(&f.work, &["commit", "-q", "--amend", "-m", "local work, reworded"]);
        git(&f.work, &["push", "-q", "--force", "origin", "main"]);
        commit_file(&f.work, "more.txt", "more\n", "more local work");
        let got = fleet_pass(&f);
        assert!(matches!(got, Outcome::UpToDate), "{got:?}");
        assert!(withdrawn(&Repository::open(&f.work).unwrap()).is_none());
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
        assert!(matches!(fleet_pass(&f), Outcome::FastForwarded(1)));
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
        for pass in [Pass::Fleet, Pass::One] {
            let f = four_files_incoming(&format!("rf-part-way-{pass:?}"));
            let before = f.head();
            let locked = ReadOnly::new(f.work.join("docs"));

            let got = refresh_one(&f.work, &options(true, pass)).outcome;

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
        let f = four_files_incoming("rf-part-way-after");
        let before = f.head();
        let locked = ReadOnly::new(f.work.join("docs"));
        let first = fleet_pass(&f);
        drop(locked);
        assert!(matches!(first, Outcome::Refused(_)), "{first:?}");

        // On the timer with updates on, on the timer with only fetching on,
        // and when the user fetches it by hand.
        let mut said = Vec::new();
        let later = [(true, Pass::Fleet), (false, Pass::Fleet), (false, Pass::One)];
        for (fast_forward, pass) in later {
            let got = refresh_one(&f.work, &options(fast_forward, pass)).outcome;
            let Outcome::Refused(why) = got else { panic!("{fast_forward} {pass:?}: {got:?}") };
            assert!(names_what_was_left(&why), "{fast_forward} {pass:?}: {why}");
            said.push(why);
        }
        // The same words every time: only new trouble raises a toast.
        assert!(said.iter().all(|why| *why == said[0]), "{said:?}");

        // Asked to update it, git refuses for the files in the way, and the
        // answer still says whose they are.
        let got = asked_for(&f);
        let Outcome::Refused(why) = &got else { panic!("{got:?}") };
        assert!(why.contains("would be overwritten"), "{why}");
        assert!(names_what_was_left(why), "{why}");
        assert_eq!(f.head(), before);

        // The way out the message gives is the one that works.
        git(&f.work, &["restore", "a/one.txt", "z/last.txt"]);
        std::fs::remove_file(f.work.join("a/added.txt")).unwrap();
        let got = fleet_pass(&f);
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
        let f = fleet("rf-part-way-added");
        f.remote_commits("somewhere for the docs", |seed| {
            write(&seed.join("docs/index.md"), "index\n");
        });
        assert!(matches!(fleet_pass(&f), Outcome::FastForwarded(1)));
        let before = f.head();
        f.remote_commits("two new files", |seed| {
            write(&seed.join("a/new.txt"), "new\n");
            write(&seed.join("docs/new.md"), "new\n");
        });
        let locked = ReadOnly::new(f.work.join("docs"));
        let first = fleet_pass(&f);
        drop(locked);
        let Outcome::Refused(why) = &first else { panic!("{first:?}") };
        assert!(why.contains("docs/new.md"), "git's own reason: {why}");
        assert!(why.contains("a/new.txt holds the incoming version"), "{why}");

        let got = fleet_pass(&f);
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

        let got = fleet_pass(&f);
        assert!(matches!(got, Outcome::Skipped(Skip::Dirty)), "{got:?}");
        let got = refresh_one(&f.work, &options(false, Pass::Fleet)).outcome;
        assert!(matches!(got, Outcome::Behind(1)), "{got:?}");

        let got = asked_for(&f);
        let Outcome::Refused(why) = &got else { panic!("{got:?}") };
        assert!(why.contains("a/one.txt"), "{why}");
        assert!(!why.contains("incoming version"), "{why}");
        assert_eq!(read(&f.work.join("a/one.txt")), "one, mine\n");

        // An update that takes files away, one of them tracked although it is
        // ignored: a file still there is not "already what upstream has".
        let f = fleet("rf-part-way-not-deleted");
        f.remote_commits("a log that got committed", |seed| {
            write(&seed.join(".gitignore"), "*.log\n");
            write(&seed.join("build.log"), "noise\n");
            write(&seed.join("old.txt"), "old\n");
        });
        assert!(matches!(fleet_pass(&f), Outcome::FastForwarded(1)));
        f.remote_commits("take them out", |seed| {
            std::fs::remove_file(seed.join("build.log")).unwrap();
            std::fs::remove_file(seed.join("old.txt")).unwrap();
        });
        write(&f.work.join("README.md"), "my edit\n");
        let got = fleet_pass(&f);
        assert!(matches!(got, Outcome::Skipped(Skip::Dirty)), "{got:?}");
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

            let got = refresh_one(&f.work, &options(false, pass)).outcome;

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
            // Both branches move upstream, so each fetch has both refs to write.
            on_feat(&format!("feat-{round}.txt"));
            f.remote_commits("main moves", |seed| {
                write(&seed.join(format!("main-{round}.txt")), "main\n");
            });

            let both = vec![f.work.clone(), linked.clone()];
            let rx = spawn(both, Options { max_parallel: 4, ..options(true, Pass::Fleet) });
            while let Ok(Event::Repo(report)) = rx.recv_blocking() {
                if !matches!(report.outcome, Outcome::FastForwarded(1)) {
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
            let got = refresh_one(&v.f.work, &options(false, pass)).outcome;
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

        let report = refresh_one(&v.f.work, &options(false, Pass::Fleet));

        assert!(matches!(report.outcome, Outcome::Behind(1)), "{:?}", report.outcome);
        assert!(report.fetched);
    }

    /// `git merge` moves a submodule's pointer and leaves its checkout where
    /// it was. The pass said "updated", the card said "clean", `git status`
    /// said modified, and the build ran new code against the old library.
    #[test]
    fn an_update_that_moves_a_checked_out_submodule_is_left_to_the_user() {
        let _guard = isolate();
        for pass in [Pass::Fleet, Pass::One] {
            let v = vendored(&format!("rf-submodule-update-{pass:?}"));
            let before = v.f.head();
            v.pointer_moves();

            let got = refresh_one(&v.f.work, &options(true, pass)).outcome;

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
        let got = fleet_pass(&v.f);
        assert!(matches!(got, Outcome::FastForwarded(1)), "{got:?}");

        let v = vendored("rf-submodule-absent");
        git(&v.f.work, &["submodule", "--quiet", "deinit", "-f", "vendor/lib"]);
        v.pointer_moves();
        let got = fleet_pass(&v.f);
        assert!(matches!(got, Outcome::FastForwarded(1)), "{got:?}");
        assert_eq!(v.f.head(), v.f.remote_ref("refs/heads/main"));
        assert_eq!(git(&v.f.work, &["status", "--porcelain"]), "");
    }
}
