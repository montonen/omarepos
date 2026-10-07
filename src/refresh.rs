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

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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

#[derive(Debug, Clone)]
pub struct Options {
    pub fast_forward: bool,
    pub max_parallel: usize,
    pub timeout: Duration,
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

    if let Err(err) = run_git(path, &["fetch", "--quiet"], options.timeout) {
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

    match run_git(path, &["merge", "--ff-only", "--quiet", "@{u}"], options.timeout) {
        Ok(()) => report(Outcome::FastForwarded(behind)),
        Err(err) => report(Outcome::Failed(err)),
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

/// Staged, modified or conflicted. Untracked files are left out: they cannot be
/// overwritten by a fast-forward without git refusing on its own.
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

/// Run git in `dir`, refusing to block on a prompt and giving up after
/// `timeout`. Returns git's own stderr on failure.
fn run_git(dir: &Path, args: &[&str], timeout: Duration) -> Result<(), String> {
    let mut command = Command::new("git");
    command
        .current_dir(dir)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        // Without this a repository needing credentials parks on a prompt that
        // nobody can answer, and the worker thread is gone for good.
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0");
    if std::env::var_os("GIT_SSH_COMMAND").is_none() {
        command.env("GIT_SSH_COMMAND", "ssh -o BatchMode=yes -o ConnectTimeout=10");
    }

    let mut child = command.spawn().map_err(|err| format!("git: {err}"))?;
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if status.success() {
                    return Ok(());
                }
                let mut stderr = String::new();
                if let Some(mut pipe) = child.stderr.take() {
                    use std::io::Read;
                    let _ = pipe.read_to_string(&mut stderr);
                }
                let first = stderr
                    .lines()
                    .find(|line| !line.trim().is_empty())
                    .unwrap_or("git failed")
                    .trim()
                    .to_string();
                return Err(first);
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("timed out after {}s", timeout.as_secs()));
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(err) => return Err(format!("git: {err}")),
        }
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
        if self.failed > 0 {
            parts.push(format!("{} failed", self.failed));
        }
        parts.join(" · ")
    }
}
