//! Running `git` itself.
//!
//! Anything that writes goes through here rather than through libgit2. libgit2
//! can build an index entry and a commit object, but it does not run hooks and
//! it does not sign — and it does not read the credential helpers, `insteadOf`
//! rewrites or `includeIf gitdir:` identities a user has configured once and
//! expects everywhere. Driving the same `git` they drive means their setup
//! applies unchanged, and a thing they cannot do by hand is not something
//! omarepos pretends it can do either.

use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// How long git gets to stop after being asked to, before it is made to.
const GRACE: Duration = Duration::from_secs(5);
/// How long to go on waiting for the last of the output once git has exited.
/// A hook that leaves something running in the background leaves it holding
/// the pipe, and the answer must not wait for that to finish.
const LINGER: Duration = Duration::from_millis(200);
/// More output than this from one command is not something anybody will read.
/// It is still drained — a full pipe stops git — just not kept.
const KEEP: usize = 1 << 20;

/// Run git in `dir`, refusing to block on a prompt and giving up after
/// `timeout`. Returns stdout on success, and the line of git's output that
/// carries the complaint on failure — a hook that refuses should be quoted, not
/// paraphrased.
///
/// Both pipes are read while git runs. Read only afterwards, a hook that
/// printed more than a pipe holds (64 KiB) blocked git on its own output until
/// the timeout, and a commit that had succeeded was reported as one that timed
/// out.
///
/// Giving up is done politely. git is asked to stop first, with SIGTERM, which
/// it answers by removing its lock files; killed outright it leaves an
/// `index.lock` behind and every later command in that repository fails on it.
/// The signal goes to git's whole process group, so a hook or filter it
/// started stops with it.
pub fn run(dir: &Path, args: &[&str], timeout: Duration) -> Result<String, String> {
    let mut command = Command::new("git");
    command
        .current_dir(dir)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Its own group, so that giving up can reach its children too.
        .process_group(0)
        // Without this a repository needing credentials parks on a prompt that
        // nobody can answer, and the worker thread is gone for good.
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0");
    if std::env::var_os("GIT_SSH_COMMAND").is_none() {
        command.env("GIT_SSH_COMMAND", "ssh -o BatchMode=yes -o ConnectTimeout=10");
    }

    let mut child = command.spawn().map_err(|err| format!("git: {err}"))?;
    let out = child.stdout.take().map(drain);
    let err = child.stderr.take().map(drain);

    // None when the timeout is too long to be a point in time: no deadline.
    let deadline = Instant::now().checked_add(timeout);
    let mut nap = Duration::from_millis(1);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                    stop(&mut child);
                    return Err(format!("timed out after {}s", timeout.as_secs()));
                }
                // Short commands are the common case; do not make each one
                // wait out a fixed nap.
                std::thread::sleep(nap);
                nap = (nap * 2).min(Duration::from_millis(50));
            }
            Err(err) => return Err(format!("git: {err}")),
        }
    };

    let until = Instant::now() + LINGER;
    let stdout = out.map(|reader| collect(reader, until)).unwrap_or_default();
    let stderr = err.map(|reader| collect(reader, until)).unwrap_or_default();
    if status.success() {
        return Ok(stdout);
    }
    Err(explain(&stderr, &stdout))
}

type Reader = (Arc<Mutex<Vec<u8>>>, JoinHandle<()>);

/// Read a pipe to its end on a thread of its own, into a buffer that can be
/// looked at before the end arrives.
fn drain(mut pipe: impl Read + Send + 'static) -> Reader {
    let kept = Arc::new(Mutex::new(Vec::new()));
    let into = Arc::clone(&kept);
    let handle = std::thread::spawn(move || {
        let mut chunk = [0u8; 8192];
        loop {
            match pipe.read(&mut chunk) {
                Ok(0) | Err(_) => return,
                Ok(n) => {
                    let Ok(mut kept) = into.lock() else { return };
                    let room = KEEP.saturating_sub(kept.len());
                    kept.extend_from_slice(&chunk[..n.min(room)]);
                }
            }
        }
    });
    (kept, handle)
}

/// What a reader has, waiting until `until` for it to finish. Lossy on
/// purpose: a hook that prints something that is not UTF-8 has still said
/// something.
fn collect((kept, handle): Reader, until: Instant) -> String {
    while !handle.is_finished() && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(2));
    }
    let kept = kept.lock().map(|kept| kept.clone()).unwrap_or_default();
    String::from_utf8_lossy(&kept).into_owned()
}

/// Ask git and everything it started to stop, and insist only if they do not.
fn stop(child: &mut Child) {
    let group = -(child.id() as libc::pid_t);
    // SAFETY: kill(2) with a process group this function's caller created and
    // has not yet reaped; it touches no memory.
    unsafe { libc::kill(group, libc::SIGTERM) };
    let until = Instant::now() + GRACE;
    while Instant::now() < until {
        if matches!(child.try_wait(), Ok(Some(_)) | Err(_)) {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    // SAFETY: as above.
    unsafe { libc::kill(group, libc::SIGKILL) };
    let _ = child.wait();
}

/// Pick the line worth showing out of git's output.
///
/// The first line is not it. A rejected push opens with `To <url>`, which says
/// nothing; the reason is further down in `! [rejected]` or `error:`. Prefer a
/// line that carries a complaint, and fall back to the first only when none
/// does.
fn explain(stderr: &str, stdout: &str) -> String {
    let lines: Vec<&str> = stderr
        .lines()
        .chain(stdout.lines())
        .filter(|line| !line.trim().is_empty())
        .collect();

    let complaint = lines.iter().position(|line| {
        let lower = line.trim().to_lowercase();
        lower.starts_with("! ")
            || lower.starts_with("error:")
            || lower.starts_with("fatal:")
            || lower.contains("rejected")
    });

    let Some(at) = complaint.or(if lines.is_empty() { None } else { Some(0) }) else {
        return "git failed".to_string();
    };
    let mut said = lines[at].trim().trim_start_matches("! ").trim().to_string();

    // "The following untracked working tree files would be overwritten by
    // merge:" names them on the lines underneath, one to a line behind a tab.
    // Without the names the sentence only says that something is in the way.
    if said.ends_with(':') {
        let named: Vec<&str> = lines[at + 1..]
            .iter()
            .take_while(|line| line.starts_with('\t'))
            .map(|line| line.trim())
            .collect();
        if !named.is_empty() {
            said.push(' ');
            said.push_str(&named.iter().take(3).copied().collect::<Vec<_>>().join(", "));
            if named.len() > 3 {
                said.push_str(&format!(" and {} more", named.len() - 3));
            }
        }
    }
    said
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    use super::*;
    use crate::testkit::*;

    /// Long enough for anything here that works; short enough that a test of
    /// something that hangs does not take the afternoon.
    const PATIENT: Duration = Duration::from_secs(8);
    const PROMPT: Duration = Duration::from_secs(4);

    /// About 300 KB on stderr: several times what a pipe holds.
    const NOISE: &str = "i=0; while [ $i -lt 3000 ]; do \
        echo 'lint: a line of output about a hundred characters long, more or less, to fill the pipe up quickly' >&2; \
        i=$((i+1)); done";

    fn repo(name: &str) -> PathBuf {
        let dir = scratch(name);
        git(&dir, &["init", "-q", "-b", "main"]);
        commit_file(&dir, "README.md", "one\n", "first");
        dir
    }

    fn hook(repo: &Path, name: &str, script: &str) {
        let path = repo.join(".git/hooks").join(name);
        write(&path, &format!("#!/bin/sh\n{script}\n"));
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn stage_something(repo: &Path) {
        write(&repo.join("new.txt"), "new\n");
        git(repo, &["add", "new.txt"]);
    }

    #[test]
    fn a_noisy_passing_hook_does_not_stall_the_commit() {
        let _guard = isolate();
        let dir = repo("gitcmd-noisy-pass");
        hook(&dir, "pre-commit", NOISE);
        stage_something(&dir);

        let started = Instant::now();
        let got = run(&dir, &["commit", "-m", "second"], PATIENT);

        assert!(got.is_ok(), "{got:?}");
        assert!(started.elapsed() < PROMPT, "took {:?}", started.elapsed());
        assert_eq!(git(&dir, &["log", "--format=%s", "-1"]), "second");
    }

    #[test]
    fn a_noisy_failing_hook_is_quoted_rather_than_timed_out() {
        let _guard = isolate();
        let dir = repo("gitcmd-noisy-fail");
        hook(&dir, "pre-commit", &format!("{NOISE}\nexit 1"));
        stage_something(&dir);

        let started = Instant::now();
        let got = run(&dir, &["commit", "-m", "second"], PATIENT);

        let said = got.unwrap_err();
        assert!(said.starts_with("lint:"), "the hook's own words, not: {said}");
        assert!(started.elapsed() < PROMPT, "took {:?}", started.elapsed());
        assert_eq!(git(&dir, &["log", "--format=%s", "-1"]), "first");
    }

    #[test]
    fn output_that_is_not_utf8_is_still_quoted() {
        let _guard = isolate();
        let dir = repo("gitcmd-not-utf8");
        hook(&dir, "pre-commit", "printf 'refused: \\377\\376 is not text\\n' >&2\nexit 1");
        stage_something(&dir);

        let said = run(&dir, &["commit", "-m", "second"], PATIENT).unwrap_err();
        assert!(said.starts_with("refused:"), "{said}");
    }

    /// A hook that starts something in the background leaves it holding the
    /// pipe. The commit is done; the answer must not wait for the straggler.
    #[test]
    fn a_hook_that_leaves_something_running_does_not_hold_up_the_answer() {
        let _guard = isolate();
        let dir = repo("gitcmd-straggler");
        hook(&dir, "post-commit", "sleep 12 &");
        stage_something(&dir);

        let started = Instant::now();
        let got = run(&dir, &["commit", "-m", "second"], PATIENT);

        assert!(got.is_ok(), "{got:?}");
        assert!(started.elapsed() < PROMPT, "took {:?}", started.elapsed());
    }

    /// The slow filter stands in for git-lfs on a dead link. git is stopped
    /// part-way through writing the working tree.
    #[test]
    fn giving_up_on_a_write_leaves_no_lock_behind() {
        let _guard = isolate();
        let f = fleet("gitcmd-timeout-lock");
        f.remote_commits("binaries", |seed| {
            write(&seed.join(".gitattributes"), "*.bin filter=slow\n");
            for i in 0..3 {
                write(&seed.join(format!("big-{i}.bin")), "payload\n");
            }
        });
        git(&f.work, &["config", "filter.slow.smudge", "sh -c 'sleep 3; cat'"]);
        git(&f.work, &["config", "filter.slow.clean", "cat"]);
        git(&f.work, &["fetch", "-q"]);

        let got = run(&f.work, &["merge", "--ff-only", "--quiet", "@{u}"], Duration::from_secs(1));

        assert_eq!(got, Err("timed out after 1s".to_string()));
        assert!(
            !f.work.join(".git/index.lock").exists(),
            "git was killed holding its lock; every later command here fails on it"
        );
        // And the repository still takes a command.
        let (ok, said) = try_git(&f.work, &["add", "-A"]);
        assert!(ok, "{said}");
    }

    #[test]
    fn the_complaint_is_the_line_that_complains() {
        assert_eq!(
            explain("To /tmp/remote.git\n ! [rejected]        main -> main (fetch first)\n", ""),
            "[rejected]        main -> main (fetch first)"
        );
        assert_eq!(
            explain(
                "error: The following untracked working tree files would be overwritten by merge:\n\
                 \t.env\n\tlocal/notes.md\nPlease move or remove them before you merge.\nAborting\n",
                ""
            ),
            "error: The following untracked working tree files would be overwritten by merge: \
             .env, local/notes.md"
        );
        assert_eq!(explain("", ""), "git failed");
    }
}
