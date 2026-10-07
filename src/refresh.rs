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
//! What this will never do: merge, rebase, stash, reset, force anything, touch
//! a dirty working tree, or move a branch that has commits of its own. The only
//! write it performs is a fast-forward of the checked-out branch, and `git
//! merge --ff-only` refuses anything else even if the checks here were wrong.
//!
//! Two things git does by default had to be switched off for that paragraph to
//! be true, and both were found the hard way:
//!
//! - `git merge` overwrites *ignored* files that are in the way of what it is
//!   checking out, without a word — an ignored `.env` is replaced the day
//!   somebody upstream starts tracking one. It is run with
//!   `--no-overwrite-ignore`, so git refuses instead and the file survives.
//! - A fast-forward runs hooks, and where the hooks live in the tree
//!   (`core.hooksPath`, husky) the hook that runs is the one the fast-forward
//!   just wrote. On a pass across the whole fleet, where nobody chose the
//!   repository, hooks and the other commands a repository's own config can
//!   name are switched off; see [`Pass`].

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
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
        }
    }
}

#[derive(Debug, Clone)]
pub enum Outcome {
    /// Fetched, and the branch was already current.
    UpToDate,
    /// Fetched, and the branch is now behind; not fast-forwarded.
    Behind(usize),
    /// Fetched and fast-forwarded by this many commits.
    FastForwarded(usize),
    Skipped(Skip),
    /// Fetched, but git would not fast-forward, and this is what it said.
    /// Nothing was changed. Kept apart from `Failed`: the remote was reached,
    /// and "fetch failed" would send the user looking in the wrong place.
    Refused(String),
    /// The fetch itself failed.
    Failed(String),
}

#[derive(Debug, Clone)]
pub struct Report {
    pub name: String,
    pub path: PathBuf,
    pub outcome: Outcome,
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

#[derive(Debug, Clone)]
pub struct Options {
    pub fast_forward: bool,
    pub max_parallel: usize,
    pub timeout: Duration,
    pub pass: Pass,
}

impl Pass {
    fn git<'a>(self, args: &[&'a str]) -> Vec<&'a str> {
        let mut all: Vec<&'a str> = match self {
            Pass::One => Vec::new(),
            Pass::Fleet => UNATTENDED.to_vec(),
        };
        all.extend_from_slice(args);
        all
    }
}

/// The fast-forward, and nothing but.
///
/// `--no-overwrite-ignore`: without it git replaces an ignored file that is in
/// the way of an incoming one, silently, and there is no reflog or stash to
/// get it back from. With it git refuses and names the file.
///
/// `merge.autoStash=false`: a user who has turned autostash on would otherwise
/// have this stash and re-apply their work around the update, and this module
/// promises never to stash.
/// How long a fast-forward may take. Not `Options::timeout`: that one is tuned
/// to give up on a dead network within a minute, and this is a local write to
/// the working tree, which is worse stopped half-way than waited for. A
/// checkout cut short leaves some files at the new commit, the branch at the
/// old one, and a repository nobody touched looking as if somebody had edited
/// it. Half an hour is a backstop against a filter that never returns, not an
/// expectation.
const FAST_FORWARD_PATIENCE: Duration = Duration::from_secs(30 * 60);

const FAST_FORWARD: [&str; 7] = [
    "-c",
    "merge.autoStash=false",
    "merge",
    "--ff-only",
    "--no-overwrite-ignore",
    "--quiet",
    "@{u}",
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
    let report = |outcome| Report { name: name.clone(), path: path.to_path_buf(), outcome };

    let repo = match Repository::open(path) {
        Ok(repo) => repo,
        Err(err) => return report(Outcome::Failed(err.message().to_string())),
    };

    if let Some(skip) = disqualify(&repo) {
        return report(Outcome::Skipped(skip));
    }

    let fetch = options.pass.git(&["fetch", "--quiet"]);
    if let Err(err) = crate::gitcmd::run(path, &fetch, options.timeout) {
        return report(Outcome::Failed(err));
    }

    // Re-open so the fetched refs are visible; libgit2 caches aggressively.
    let repo = match Repository::open(path) {
        Ok(repo) => repo,
        Err(err) => return report(Outcome::Failed(err.message().to_string())),
    };
    let (ahead, behind) = match divergence(&repo) {
        Ok(counts) => counts,
        Err(skip) => return report(Outcome::Skipped(skip)),
    };

    if behind == 0 {
        return report(Outcome::UpToDate);
    }
    if ahead > 0 {
        return report(Outcome::Skipped(Skip::Diverged));
    }
    if !options.fast_forward {
        return report(Outcome::Behind(behind));
    }

    // Cleanliness is checked again here rather than before the fetch: the fetch
    // took time, and the user may have started editing during it.
    if is_dirty(&repo) {
        return report(Outcome::Skipped(Skip::Dirty));
    }

    let merge = options.pass.git(&FAST_FORWARD);
    match crate::gitcmd::run(path, &merge, FAST_FORWARD_PATIENCE) {
        Ok(_) => report(Outcome::FastForwarded(behind)),
        Err(err) => report(Outcome::Refused(err)),
    }
}

/// Reasons to leave a repository alone before touching the network at all.
fn disqualify(repo: &Repository) -> Option<Skip> {
    if repo.is_bare() {
        return Some(Skip::Bare);
    }
    if repo.state() != RepositoryState::Clean {
        return Some(Skip::Busy);
    }
    match repo.head() {
        Err(err) if err.code() == git2::ErrorCode::UnbornBranch => return Some(Skip::Unborn),
        Err(_) => return Some(Skip::Unborn),
        Ok(_) => {}
    }
    if repo.head_detached().unwrap_or(false) {
        return Some(Skip::Detached);
    }
    if repo.remotes().map(|r| r.is_empty()).unwrap_or(true) {
        return Some(Skip::NoUpstream);
    }
    None
}

fn divergence(repo: &Repository) -> Result<(usize, usize), Skip> {
    let head = repo.head().map_err(|_| Skip::Unborn)?;
    let branch_name = head.shorthand().map_err(|_| Skip::Detached)?.to_string();
    let branch = repo
        .find_branch(&branch_name, BranchType::Local)
        .map_err(|_| Skip::NoUpstream)?;
    let upstream = branch.upstream().map_err(|_| Skip::NoUpstream)?;
    let local = branch.get().target().ok_or(Skip::NoUpstream)?;
    let remote = upstream.get().target().ok_or(Skip::NoUpstream)?;
    repo.graph_ahead_behind(local, remote).map_err(|_| Skip::NoUpstream)
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
        Options { fast_forward, max_parallel: 1, timeout: Duration::from_secs(30), pass }
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

        let hurried = Options {
            fast_forward: true,
            max_parallel: 1,
            timeout: Duration::from_secs(1),
            pass: Pass::Fleet,
        };
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
}
