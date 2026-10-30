//! Fixtures for the tests: throwaway repositories and a machine that is not
//! this one.
//!
//! Everything this application does that matters is done to a repository, and
//! the claims worth pinning are about what git does next — so the tests build
//! real repositories and run the real `git`. What they must never do is read
//! the developer's own git config or write to their own config directory;
//! `isolate()` is what stands between the two, and every test that touches a
//! repository starts by calling it.

// A kit is allowed to hold a tool nobody has reached for yet.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, MutexGuard, Once};

static ENV: Once = Once::new();
static LOCK: Mutex<()> = Mutex::new(());

/// Where fixtures live: under `target/`, which is ignored, on disk, and gone
/// with the next `cargo clean`.
pub fn base() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target").join("test-fixtures")
}

/// Cut the test off from the real machine: a fake home, fake XDG directories,
/// a global git config that says only what the fixtures need, and no system
/// config. The environment is process-wide, so this also serialises the tests
/// that call it — hold the guard for the length of the test.
pub fn isolate() -> MutexGuard<'static, ()> {
    let guard = LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    ENV.call_once(|| {
        let home = base().join("home");
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(home.join(".config")).unwrap();
        std::fs::create_dir_all(home.join(".local/state")).unwrap();
        // No housekeeping. A commit or a fetch starts git's maintenance
        // after it, in the background since git 2.47, and that takes a lock
        // in `.git/objects` and lets it go: a test that compares a
        // repository before and after a read saw it written to, now and
        // then, by git tidying up after the fixture.
        let gitconfig = "[user]\n\tname = Fixture\n\temail = fixture@example.invalid\n\
                         [init]\n\tdefaultBranch = main\n\
                         [protocol \"file\"]\n\tallow = always\n\
                         [commit]\n\tgpgsign = false\n\
                         [maintenance]\n\tauto = false\n\
                         [gc]\n\tauto = 0\n";
        std::fs::write(home.join("gitconfig"), gitconfig).unwrap();
        std::fs::write(home.join(".gitconfig"), gitconfig).unwrap();
        // SAFETY: set once, before any test has spawned a thread that reads
        // the environment; every later reader is behind the same lock.
        unsafe {
            std::env::set_var("HOME", &home);
            std::env::set_var("XDG_CONFIG_HOME", home.join(".config"));
            std::env::set_var("XDG_STATE_HOME", home.join(".local/state"));
            std::env::set_var("GIT_CONFIG_GLOBAL", home.join("gitconfig"));
            std::env::set_var("GIT_CONFIG_NOSYSTEM", "1");
            for name in crate::gitcmd::REPOSITORY_ENV.iter().chain(&["GIT_SSH_COMMAND", "SSH_AUTH_SOCK"]) {
                std::env::remove_var(name);
            }
        }
        // libgit2 resolves its config search path once per process. Point it
        // at the same fake home, or what the application reads in-process
        // would disagree with what the git it spawns reads.
        // SAFETY: called once, before any repository has been opened.
        unsafe {
            let _ = git2::opts::set_search_path(git2::ConfigLevel::Global, &home);
            let _ = git2::opts::set_search_path(git2::ConfigLevel::XDG, home.join(".config"));
            let _ = git2::opts::set_search_path(git2::ConfigLevel::System, home.join("no-system"));
        }
    });
    guard
}

/// A fresh, empty directory for one test.
pub fn scratch(name: &str) -> PathBuf {
    let dir = base().join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Run git to build a fixture. Panics on failure: a fixture that did not get
/// built must not be mistaken for a result.
pub fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .expect("git runs");
    if !out.status.success() {
        panic!(
            "fixture git {:?} in {} failed:\n{}\n{}",
            args,
            dir.display(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Like `git`, but hands back whether it worked and everything it printed.
pub fn try_git(dir: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .expect("git runs");
    (
        out.status.success(),
        format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)),
    )
}

pub fn write(path: &Path, body: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, body).unwrap();
}

pub fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|err| format!("<{}: {err}>", path.display()))
}

pub fn commit_file(repo: &Path, file: &str, body: &str, message: &str) {
    write(&repo.join(file), body);
    git(repo, &["add", "--", file]);
    git(repo, &["commit", "-q", "-m", message]);
}

/// Take a loose object away, the way a power cut in the middle of a commit
/// does.
pub fn lose_object(repo: &Path, rev: &str) {
    let id = git(repo, &["rev-parse", rev]);
    let file = repo.join(".git/objects").join(&id[..2]).join(&id[2..]);
    std::fs::remove_file(&file).unwrap_or_else(|err| panic!("{}: {err}", file.display()));
}

/// Like `git`, with both of a commit's clocks set to `when`, in seconds since
/// 1970: for a fixture whose history has to have happened on chosen days.
pub fn git_at(dir: &Path, when: i64, args: &[&str]) -> String {
    let stamp = format!("@{when} +0000");
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_AUTHOR_DATE", &stamp)
        .env("GIT_COMMITTER_DATE", &stamp)
        .output()
        .expect("git runs");
    if !out.status.success() {
        panic!(
            "fixture git {:?} at {when} in {} failed:\n{}\n{}",
            args,
            dir.display(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A commit of nothing by `who`, a name and an address, at `when`. A test of
/// history needs a commit to be there, to be somebody's and to be so old, and
/// no file has to change for that.
pub fn commit_as(repo: &Path, who: (&str, &str), when: i64, message: &str) {
    let (name, email) = (format!("user.name={}", who.0), format!("user.email={}", who.1));
    let commit = ["-c", &name, "-c", &email, "commit", "-q", "--allow-empty", "-m", message];
    git_at(repo, when, &commit);
}

/// A commit of nothing for each of `times`, oldest first, on top of whatever
/// `branch` holds and all by `who`. They go in through `git fast-import`, so
/// that a history of hundreds costs about what one `git commit` does.
pub fn commit_run(repo: &Path, branch: &str, who: (&str, &str), times: &[i64]) {
    let branch = format!("refs/heads/{branch}");
    let (grown, _) = try_git(repo, &["rev-parse", "--verify", "-q", &branch]);
    let mut stream = String::new();
    for (i, when) in times.iter().enumerate() {
        let message = format!("run {i}");
        let who = format!("{} <{}> {when} +0000", who.0, who.1);
        stream.push_str(&format!("commit {branch}\nauthor {who}\ncommitter {who}\n"));
        stream.push_str(&format!("data {}\n{message}\n", message.len()));
        // Only the first says what it stands on, and only when the branch is
        // there already. The ones after it follow the one before, and a
        // branch fast-import is told no start for is begun from nothing.
        if i == 0 && grown {
            stream.push_str(&format!("from {branch}^0\n"));
        }
        stream.push('\n');
    }

    use std::io::Write;
    let mut import = Command::new("git")
        .current_dir(repo)
        .args(["fast-import", "--quiet"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("git runs");
    // Written whole before anything is read back. `--quiet` leaves it nothing
    // to say unless it stops, and a pipe closed early shows below as a failure.
    let _ = import.stdin.take().expect("a pipe to write to").write_all(stream.as_bytes());
    let out = import.wait_with_output().expect("git runs");
    if !out.status.success() {
        panic!(
            "fixture git fast-import in {} failed:\n{}\n{}",
            repo.display(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// A remote and two clones of it: `remote.git` (bare), `seed` (somebody else,
/// who pushes) and `work` (the repository under test). One commit on `main`,
/// upstream set.
pub struct Fleet {
    pub root: PathBuf,
    pub remote: PathBuf,
    pub seed: PathBuf,
    pub work: PathBuf,
}

pub fn fleet(name: &str) -> Fleet {
    let root = scratch(name);
    let remote = root.join("remote.git");
    let seed = root.join("seed");
    let work = root.join("work");
    git(&root, &["init", "-q", "--bare", "-b", "main", "remote.git"]);
    git(&root, &["clone", "-q", remote.to_str().unwrap(), "seed"]);
    commit_file(&seed, "README.md", "one\n", "first");
    git(&seed, &["push", "-q", "-u", "origin", "main"]);
    git(&root, &["clone", "-q", remote.to_str().unwrap(), "work"]);
    Fleet { root, remote, seed, work }
}

impl Fleet {
    /// Somebody else pushes `n` commits to the remote's `main`, each adding a
    /// file named `upstream-<i>.txt`.
    pub fn remote_moves(&self, n: usize) {
        git(&self.seed, &["pull", "-q", "--ff-only"]);
        for i in 0..n {
            commit_file(
                &self.seed,
                &format!("upstream-{i}.txt"),
                &format!("upstream {i}\n"),
                &format!("upstream {i}"),
            );
        }
        git(&self.seed, &["push", "-q", "origin", "main"]);
    }

    /// Somebody else commits whatever `change` leaves in their clone, and
    /// pushes it.
    pub fn remote_commits(&self, message: &str, change: impl FnOnce(&Path)) {
        git(&self.seed, &["pull", "-q", "--ff-only"]);
        change(&self.seed);
        git(&self.seed, &["add", "-A", "-f", "."]);
        git(&self.seed, &["commit", "-q", "-m", message]);
        git(&self.seed, &["push", "-q", "origin", "main"]);
    }

    pub fn head(&self) -> String {
        git(&self.work, &["rev-parse", "HEAD"])
    }

    /// What the remote has under `name`, or nothing if it has no such ref.
    pub fn remote_ref(&self, name: &str) -> String {
        let (ok, out) = try_git(&self.remote, &["rev-parse", "--verify", "-q", name]);
        if ok { out.trim().to_string() } else { String::new() }
    }
}

/// Wait for a worker's answer.
pub fn recv<T>(rx: async_channel::Receiver<T>) -> T {
    rx.recv_blocking().expect("the worker reported")
}
