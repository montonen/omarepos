//! Fetching: one repository the user pointed at, or a pass across the fleet.
//!
//! It is the one thing omarepos does to a repository. A fetch asks the remote
//! what it has and moves the remote-tracking branches, `origin/*`, to match.
//! No branch of the user's moves, nothing in the working tree changes, and
//! nothing is sent. There was more here once (a fast-forward of the
//! checked-out branch, with the pages of care that took); it was taken out
//! so that what the program may do to every repository on a machine fits in
//! a sentence.
//!
//! The fetch shells out to `git` rather than going through libgit2. That is
//! deliberate: a user's authentication lives in credential helpers, an SSH
//! agent and `insteadOf` rewrites, none of which libgit2 reads. Driving the
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

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use git2::{BranchType, Repository};

/// Why a repository was not fetched at all.
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
}

impl Skip {
    pub fn reason(self) -> &'static str {
        match self {
            Skip::Bare => "bare repository",
            Skip::NoRemote => "no remote",
            Skip::Elsewhere => "its work tree is not this directory",
            Skip::Unseen => "new here, open it once",
        }
    }
}

#[derive(Debug, Clone)]
pub enum Outcome {
    /// Fetched, and the checked-out branch has nothing waiting for it. Also
    /// where there is nothing to measure: no branch checked out, or one that
    /// tracks nothing.
    UpToDate,
    /// Fetched, and the checked-out branch is this many commits behind what
    /// it tracks. Said whatever else is true of the branch, commits of its
    /// own included: it is a count of what is waiting, and bringing it in is
    /// not this program's to do.
    Behind(usize),
    Skipped(Skip),
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
    /// One repository the user pointed at and asked to fetch. Their whole git
    /// setup applies, exactly as if they had typed the fetch.
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
/// left alone. A fetch already running is allowed to finish.
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
    // What is waiting for the checked-out branch, where it tracks something.
    // A detached HEAD or a branch that tracks nothing has been fetched all
    // the same, and "left alone: no upstream" about a repository just fetched
    // is not what happened.
    match behind(&repo) {
        Some(behind) if behind > 0 => report(Outcome::Behind(behind)),
        _ => report(Outcome::UpToDate),
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
/// it disturbs none of them.
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

/// How many commits the checked-out branch is behind what it tracks, as they
/// stand right now. `None` where there is no branch, or it tracks nothing.
fn behind(repo: &Repository) -> Option<usize> {
    let head = repo.head().ok()?;
    let branch = repo.find_branch(head.shorthand().ok()?, BranchType::Local).ok()?;
    let upstream = branch.upstream().ok()?;
    let (local, remote) = (branch.get().target()?, upstream.get().target()?);
    repo.graph_ahead_behind(local, remote).ok().map(|(_, behind)| behind)
}

/// Counts for the summary line after a pass.
#[derive(Debug, Default, Clone)]
pub struct Summary {
    pub fetched: usize,
    pub behind: usize,
    pub skipped: usize,
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
            Outcome::Skipped(skip) => {
                self.skipped += 1;
                *self.skips.entry(skip.reason()).or_insert(0) += 1;
            }
            Outcome::Failed(_) => self.failed += 1,
        }
    }

    pub fn line(&self) -> String {
        let mut parts = vec![format!("{} fetched", self.fetched)];
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
    fn due_for_housekeeping(repo: &Path) {
        git(repo, &["config", "gc.auto", "1"]);
        git(repo, &["config", "gc.autoDetach", "false"]);
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
}
