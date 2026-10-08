//! Running `git` itself.
//!
//! Anything that writes goes through here rather than through libgit2. libgit2
//! can build an index entry and a commit object, but it does not run hooks and
//! it does not sign — and it does not read the credential helpers, `insteadOf`
//! rewrites or `includeIf gitdir:` identities a user has configured once and
//! expects everywhere. Driving the same `git` they drive means their setup
//! applies unchanged, and a thing they cannot do by hand is not something
//! omarepos pretends it can do either.
//!
//! Two things are added to their setup, both so that nothing waits on a prompt
//! nobody will answer: `GIT_TERMINAL_PROMPT=0`, and ssh's batch options, which
//! are appended to the ssh command they configured rather than put in its
//! place.
//!
//! That is the terminal. git asks an askpass program before it considers the
//! terminal, and an askpass program is a window: `GIT_ASKPASS`, `core.askPass`
//! and `SSH_ASKPASS` are tried in that order, and `GIT_TERMINAL_PROMPT` gates
//! none of them. For something the user asked for, a window asking for their
//! password is the right answer and is left alone. For a pass nobody is
//! watching ([`run_fenced`]) it is a dialog per repository, every interval,
//! on the screen of somebody who asked for nothing, so there the askpass
//! programs are switched off as well.

use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// How much is under way that the process must not exit in the middle of:
/// every git command, and a refresh of one repository from its fetch to its
/// fast-forward.
static IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);
/// Set while the last window is closing with git still at work.
static WINDING_DOWN: AtomicBool = AtomicBool::new(false);

/// Held for as long as something is running that must be let finish. The
/// application used to exit the moment its window closed, whatever git was in
/// the middle of: a fast-forward cut off between files, a commit whose hook
/// was still running, a push nobody then heard the end of. The window still
/// goes at once. The process now stays until [`busy`] says no.
pub struct Hold(());

impl Hold {
    pub fn take() -> Self {
        IN_FLIGHT.fetch_add(1, Ordering::SeqCst);
        Hold(())
    }
}

impl Drop for Hold {
    fn drop(&mut self) {
        IN_FLIGHT.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Whether git is in the middle of anything.
pub fn busy() -> bool {
    IN_FLIGHT.load(Ordering::SeqCst) > 0
}

/// Say that the application is closing, or has stopped closing. While it is,
/// a fetch under way is stopped and no fast-forward is begun: a fetch is not
/// worth keeping a closed application alive for, up to an hour, and loses
/// nothing by being stopped. Every write is left to finish.
pub fn wind_down(closing: bool) {
    WINDING_DOWN.store(closing, Ordering::SeqCst);
}

pub fn winding_down() -> bool {
    WINDING_DOWN.load(Ordering::SeqCst)
}

/// The variables that tell git which repository to act on, whatever
/// directory it is run in: what `git rev-parse --local-env-vars` lists. One
/// inherited from wherever the application was started (a shell inside a
/// hook, an alias, a `git rebase --exec`) sent every stage, commit and push
/// to that repository and not to the one on screen. No command run from here
/// is given them, and `main` takes them out of the application's own
/// environment at launch, so that a terminal it opens does not have them
/// either.
pub const REPOSITORY_ENV: [&str; 13] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_COMMON_DIR",
    "GIT_IMPLICIT_WORK_TREE",
    "GIT_GRAFT_FILE",
    "GIT_SHALLOW_FILE",
    "GIT_NO_REPLACE_OBJECTS",
    "GIT_REPLACE_REF_BASE",
    "GIT_PREFIX",
    "GIT_CONFIG",
];

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
    exec(dir, args, Limit::Whole(timeout), true)
}

/// As [`run`], for a pass nobody is watching. Two differences: whose word is
/// taken for the ssh command (see [`ssh_command`]), and that nothing is
/// allowed to ask (see [`UNASKED`]).
pub fn run_fenced(dir: &Path, args: &[&str], timeout: Duration) -> Result<String, String> {
    exec(dir, args, Limit::Whole(timeout), false)
}

/// As [`run`], for a command that is moving data and says so as it goes: a
/// fetch run with `--progress`. It is given up on when nothing has arrived
/// from it for `quiet`, and otherwise left to finish, up to `ceiling`.
///
/// A fetch used to get one limit on the whole of it, the same minute that is
/// right for noticing a dead network. A fetch that needed longer than that
/// could then never finish: far behind, or on a slow link, it was stopped at
/// the minute with data still arriving, started again from nothing on the
/// next pass, and left what it had received in `.git/objects/pack` each time,
/// where git keeps it for two weeks. Silence is the sign of a dead link. Time
/// taken is not.
pub fn transfer(
    dir: &Path,
    args: &[&str],
    quiet: Duration,
    ceiling: Duration,
) -> Result<String, String> {
    exec(dir, args, Limit::WhileTalking { quiet, ceiling }, true)
}

/// As [`transfer`], with the fence of [`run_fenced`].
pub fn transfer_fenced(
    dir: &Path,
    args: &[&str],
    quiet: Duration,
    ceiling: Duration,
) -> Result<String, String> {
    exec(dir, args, Limit::WhileTalking { quiet, ceiling }, false)
}

/// How long a command is given.
#[derive(Clone, Copy)]
enum Limit {
    /// This long, start to finish, whatever it is doing.
    Whole(Duration),
    /// For as long as it keeps saying something, with no gap longer than
    /// `quiet`; and however talkative, no longer than `ceiling`.
    WhileTalking { quiet: Duration, ceiling: Duration },
}

impl Limit {
    /// Why to give up now, if it is time to: on a command started at `started`
    /// that last wrote to either pipe at `heard`.
    fn passed(self, started: Instant, heard: Instant) -> Option<String> {
        let timed_out = |limit: Duration| {
            (started.elapsed() >= limit).then(|| format!("timed out after {}s", limit.as_secs()))
        };
        match self {
            Limit::Whole(timeout) => timed_out(timeout),
            Limit::WhileTalking { quiet, ceiling } => {
                if heard.elapsed() >= quiet {
                    return Some(format!("nothing arrived for {}s, gave up", quiet.as_secs()));
                }
                timed_out(ceiling)
            }
        }
    }
}

/// The environment of a pass nobody is watching, on top of what every command
/// gets. `GIT_TERMINAL_PROMPT=0` was believed to cover this and does not: with
/// an askpass program exported by the desktop session, or named by a
/// repository's own `core.askPass`, the timer's fetches put a password dialog
/// on screen for every repository whose stored credential had expired.
///
/// - `GIT_ASKPASS` set and empty is the one value that outranks the other two
///   places git looks, and runs nothing. Unset, git falls through to them.
/// - `SSH_ASKPASS_REQUIRE=never` is the same for ssh's own questions, a key's
///   passphrase or a host key, which reach it whatever the ssh command is.
const UNASKED: [(&str, &str); 2] = [("GIT_ASKPASS", ""), ("SSH_ASKPASS_REQUIRE", "never")];

/// Put in front of the arguments on a pass nobody is watching: git's own word
/// for "do not ask", which a credential helper that would otherwise open a
/// window or a browser is expected to honour as well.
const UNASKED_CONFIG: [&str; 2] = ["-c", "credential.interactive=false"];

/// What ssh has to be told so that it fails instead of waiting on a prompt
/// nobody will answer.
const BATCH: &str = "-o BatchMode=yes -o ConnectTimeout=10";

/// The value to give `GIT_SSH_COMMAND`, or `None` to leave git to its own
/// devices.
///
/// The variable used to be set to plain `ssh` with the batch options, always.
/// But it outranks `core.sshCommand`, which is how a per-client key is usually
/// chosen (often through the same `includeIf` that sets the identity), so a
/// fetch or a push from here went out with the default key: failing where a
/// hand-typed one works, or worse, succeeding as the wrong account. The batch
/// options are now added to whatever the user configured.
///
/// `attended` says whether somebody asked for this command, and with that
/// whether the repository's own config is believed. For a repository the user
/// pointed at, it is, as it would be for a command they typed. On a pass
/// across the fleet it is not: an ssh command is a command, and a repository
/// does not get to name one for a run nobody is watching.
fn ssh_command(dir: &Path, args: &[&str], attended: bool) -> Option<String> {
    let is_ssh = |command: &str| {
        // A wrapper script may not take ssh's options; only ssh itself gets
        // them.
        let program = command.split_whitespace().next().unwrap_or_default();
        Path::new(program).file_name().is_some_and(|name| name == "ssh")
    };
    // Set by the user for this session: theirs. Acting for them it is left
    // exactly as it is, prompts included. On a pass nobody is watching it is
    // still what runs, but the variable used to mean the batch options were
    // left off altogether, and an exported `ssh -i …` then sat on a passphrase
    // prompt in whatever terminal the application was started from.
    if let Some(exported) = std::env::var_os("GIT_SSH_COMMAND") {
        return match exported.to_str() {
            Some(exported) if !attended && is_ssh(exported) => Some(format!("{exported} {BATCH}")),
            _ => None,
        };
    }
    // A program, not a command line: there is nowhere to put an option.
    if std::env::var_os("GIT_SSH").is_some() {
        return None;
    }
    let own_config = attended;
    let plain = || Some(format!("ssh {BATCH}"));
    if !talks_to_a_remote(args) {
        return plain();
    }
    let mut ask = Command::new("git");
    for name in REPOSITORY_ENV {
        ask.env_remove(name);
    }
    let asked = ask
        .current_dir(dir)
        .args(["config", "--show-scope", "--get", "core.sshCommand"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string());
    let Some((scope, configured)) = asked.as_deref().and_then(|line| line.split_once('\t')) else {
        return plain();
    };
    if !own_config && matches!(scope, "local" | "worktree") {
        return plain();
    }
    if is_ssh(configured) {
        Some(format!("{configured} {BATCH}"))
    } else if own_config || !matches!(scope, "local" | "worktree") {
        None
    } else {
        plain()
    }
}

/// The git command being run, looking past any leading `-c key=value`.
fn verb<'a>(args: &[&'a str]) -> &'a str {
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        if *arg == "-c" {
            rest.next();
            continue;
        }
        return arg;
    }
    ""
}

/// Whether this is `fetch` or `push`.
fn talks_to_a_remote(args: &[&str]) -> bool {
    matches!(verb(args), "fetch" | "push")
}

fn exec(dir: &Path, args: &[&str], limit: Limit, attended: bool) -> Result<String, String> {
    let _hold = Hold::take();
    let mut command = Command::new("git");
    command.current_dir(dir);
    if !attended {
        command.args(UNASKED_CONFIG).envs(UNASKED);
    }
    command
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
    for name in REPOSITORY_ENV {
        command.env_remove(name);
    }
    if let Some(ssh) = ssh_command(dir, args, attended) {
        command.env("GIT_SSH_COMMAND", ssh);
    }

    let mut child = command.spawn().map_err(|err| format!("git: {err}"))?;
    let started = Instant::now();
    // When either pipe last had something on it.
    let heard = Arc::new(Mutex::new(started));
    let out = child.stdout.take().map(|pipe| drain(pipe, Arc::clone(&heard)));
    let err = child.stderr.take().map(|pipe| drain(pipe, Arc::clone(&heard)));

    let mut nap = Duration::from_millis(1);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                let heard = heard.lock().map(|heard| *heard).unwrap_or(started);
                if let Some(why) = limit.passed(started, heard) {
                    stop(&mut child);
                    return Err(why);
                }
                if winding_down() && verb(args) == "fetch" {
                    stop(&mut child);
                    return Err("stopped: omarepos was closing".to_string());
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
/// looked at before the end arrives, noting in `heard` when anything came.
fn drain(mut pipe: impl Read + Send + 'static, heard: Arc<Mutex<Instant>>) -> Reader {
    let kept = Arc::new(Mutex::new(Vec::new()));
    let into = Arc::clone(&kept);
    let handle = std::thread::spawn(move || {
        let mut chunk = [0u8; 8192];
        loop {
            match pipe.read(&mut chunk) {
                Ok(0) | Err(_) => return,
                Ok(n) => {
                    if let Ok(mut heard) = heard.lock() {
                        *heard = Instant::now();
                    }
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
    // NULs are made visible. A hook that lists files with `-z` prints them,
    // and what git said goes into a label, which takes no NUL: the message
    // was cut short at the first one.
    String::from_utf8_lossy(&kept).replace('\0', "\u{2400}")
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

/// How much of a hook's output is kept when it is all there is to show.
const SAID_LINES: usize = 40;

/// Pick what is worth showing out of git's output.
///
/// The first line is not it. A rejected push opens with `To <url>`, which says
/// nothing; the reason is further down in `! [rejected]` or `error:`. Prefer a
/// line of git's own that carries a complaint.
///
/// When there is none, what is left is somebody else talking: a hook that
/// refused, in its own words, which is the thing the caller most needs to
/// see. All of it is returned, not its first line. A linter's first line is
/// the name of the script it ran, and its findings come after.
fn explain(stderr: &str, stdout: &str) -> String {
    // A hook's standard output arrives on git's; put it ahead of what git
    // adds on the way out.
    //
    // A carriage return ends a line as well. Progress is written over itself
    // with them, and an error that interrupts it arrives on the end of the
    // progress line: read as one line it does not start with "fatal:", and
    // what was shown was forty lines of percentages.
    let ends = ['\n', '\r'];
    let lines: Vec<&str> = stdout
        .split(ends)
        .chain(stderr.split(ends))
        .filter(|line| !line.trim().is_empty())
        // git's closing remark after a hook or the remote has already said
        // why. On its own it sends the reader to look for a reason that was
        // on the line above.
        .filter(|line| !line.trim().to_lowercase().starts_with("error: failed to push some refs"))
        .collect();

    let complaint = lines.iter().position(|line| {
        let lower = line.trim().to_lowercase();
        lower.starts_with("! ")
            || lower.starts_with("error:")
            || lower.starts_with("fatal:")
            || lower.contains("rejected")
    });

    let Some(at) = complaint else {
        if lines.is_empty() {
            return "git failed".to_string();
        }
        let from = lines.len().saturating_sub(SAID_LINES);
        return lines[from..].iter().map(|line| line.trim_end()).collect::<Vec<_>>().join("\n");
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
        assert_eq!(said.lines().count(), SAID_LINES, "its last lines, and no more than fit");
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

    // ---- given up on for silence, not for taking long ----------------------

    /// Something that takes two seconds and says so five times a second.
    const BUSY: &str =
        "i=0; while [ $i -lt 10 ]; do echo 'still going' >&2; sleep 0.2; i=$((i+1)); done";

    #[test]
    fn a_command_that_keeps_talking_outlasts_the_limit_on_silence() {
        let _guard = isolate();
        let dir = repo("rf-gitcmd-talking");
        hook(&dir, "pre-commit", BUSY);
        stage_something(&dir);
        let second = Duration::from_secs(1);

        // The limit `run` applies is on the whole command, whatever it is
        // doing, and this is what it did to a fetch that was still arriving.
        let cut_off = run(&dir, &["commit", "-m", "second"], second);
        assert_eq!(cut_off, Err("timed out after 1s".into()));
        assert_eq!(git(&dir, &["log", "--format=%s", "-1"]), "first");

        let got = transfer(&dir, &["commit", "-m", "second"], second, PATIENT);
        assert!(got.is_ok(), "{got:?}");
        assert_eq!(git(&dir, &["log", "--format=%s", "-1"]), "second");
    }

    #[test]
    fn a_command_that_goes_quiet_is_given_up_on() {
        let _guard = isolate();
        let dir = repo("rf-gitcmd-quiet");
        hook(&dir, "pre-commit", "echo 'starting' >&2\nsleep 12");
        stage_something(&dir);

        let started = Instant::now();
        let got = transfer(&dir, &["commit", "-m", "second"], Duration::from_secs(1), PATIENT);

        assert_eq!(got, Err("nothing arrived for 1s, gave up".into()));
        assert!(started.elapsed() < PROMPT, "took {:?}", started.elapsed());
        assert!(!dir.join(".git/index.lock").exists());
    }

    /// Talking is not a licence to go on for ever.
    #[test]
    fn a_command_that_keeps_talking_still_has_a_ceiling() {
        let _guard = isolate();
        let dir = repo("rf-gitcmd-ceiling");
        hook(&dir, "pre-commit", BUSY);
        stage_something(&dir);

        let ceiling = Duration::from_secs(1);
        let got = transfer_fenced(&dir, &["commit", "-m", "second"], PATIENT, ceiling);

        assert_eq!(got, Err("timed out after 1s".into()));
        assert_eq!(git(&dir, &["log", "--format=%s", "-1"]), "first");
    }

    // ---- whose ssh --------------------------------------------------------

    /// Something to stand in for ssh: it writes down how it was called, and
    /// fails, so nothing waits on a network.
    fn recording(dir: &Path, name: &str) -> (PathBuf, PathBuf) {
        let log = dir.join(format!("{name}.log"));
        let script = dir.join("bin").join(name);
        write(&script, &format!("#!/bin/sh\necho \"$@\" >> '{}'\nexit 1\n", log.display()));
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        (script, log)
    }

    fn ssh_repo(name: &str) -> PathBuf {
        let dir = repo(name);
        git(&dir, &["remote", "add", "origin", "git@host.invalid:me/repo.git"]);
        dir
    }

    /// `core.sshCommand` is how a per-client key is usually chosen. The
    /// variable this module sets outranks it, and used to replace it.
    #[test]
    fn a_configured_ssh_is_kept_and_only_given_the_batch_options() {
        let _guard = isolate();
        let dir = ssh_repo("gitcmd-ssh-configured");
        let (ssh, log) = recording(&dir, "ssh");
        git(&dir, &["config", "core.sshCommand", &format!("{} -i id_work", ssh.display())]);

        let _ = run(&dir, &["fetch", "--quiet"], PATIENT);

        let called = read(&log);
        assert!(called.contains("-i id_work"), "the configured key was dropped: {called}");
        assert!(called.contains("BatchMode=yes"), "and it must still not wait on a prompt: {called}");
    }

    #[test]
    fn a_wrapper_that_is_not_ssh_is_left_exactly_as_configured() {
        let _guard = isolate();
        let dir = ssh_repo("gitcmd-ssh-wrapper");
        let (wrapper, log) = recording(&dir, "corp-ssh-wrapper");
        git(&dir, &["config", "core.sshCommand", wrapper.to_str().unwrap()]);

        let _ = run(&dir, &["fetch", "--quiet"], PATIENT);

        let called = read(&log);
        assert!(called.contains("host.invalid"), "the wrapper was not used: {called}");
        assert!(!called.contains("BatchMode"), "it may not take ssh's options: {called}");
    }

    /// On a pass nobody is watching, a repository's own config does not get to
    /// name the command; the user's config still does.
    #[test]
    fn a_fleet_pass_takes_the_ssh_command_from_the_user_not_from_the_repository() {
        let _guard = isolate();
        let dir = ssh_repo("gitcmd-ssh-fenced");
        let (theirs, their_log) = recording(&dir, "repo-says-run-this");
        git(&dir, &["config", "core.sshCommand", theirs.to_str().unwrap()]);

        let _ = run_fenced(&dir, &["fetch", "--quiet"], PATIENT);
        assert!(!their_log.exists(), "the repository's own command ran unattended");

        // The same setting coming from outside the repository is the user's.
        git(&dir, &["config", "--unset", "core.sshCommand"]);
        let (mine, my_log) = recording(&dir, "ssh");
        // SAFETY: under the isolation lock, and taken out again below.
        unsafe {
            std::env::set_var("GIT_CONFIG_COUNT", "1");
            std::env::set_var("GIT_CONFIG_KEY_0", "core.sshCommand");
            std::env::set_var("GIT_CONFIG_VALUE_0", format!("{} -i id_work", mine.display()));
        }
        let _ = run_fenced(&dir, &["-c", "core.hooksPath=/dev/null", "fetch", "--quiet"], PATIENT);
        unsafe {
            for name in ["GIT_CONFIG_COUNT", "GIT_CONFIG_KEY_0", "GIT_CONFIG_VALUE_0"] {
                std::env::remove_var(name);
            }
        }
        assert!(read(&my_log).contains("-i id_work"), "{}", read(&my_log));
    }

    // ---- whose prompt -----------------------------------------------------

    /// A repository whose fetch needs a password only a prompt can supply. The
    /// proxy names a user and no password, and git asks for that before it
    /// connects to anything; the askpass program writes down that it was
    /// called and fails, so git stops there and nothing leaves the machine.
    fn needs_a_password(name: &str) -> (PathBuf, PathBuf, PathBuf) {
        let dir = repo(name);
        git(&dir, &["remote", "add", "origin", "http://git.example.invalid/me/repo.git"]);
        git(&dir, &["config", "http.proxy", "http://someone@proxy.example.invalid:8080"]);
        let (askpass, log) = recording(&dir, "askpass");
        (dir, askpass, log)
    }

    /// `GIT_TERMINAL_PROMPT=0` stops git asking on the terminal. It asks an
    /// askpass program first, and that is a window on somebody's screen.
    #[test]
    fn a_pass_nobody_is_watching_opens_no_password_dialog() {
        let _guard = isolate();
        let (dir, askpass, log) = needs_a_password("rf-gitcmd-askpass-fenced");

        // Named by the repository itself, which needs no environment at all.
        git(&dir, &["config", "core.askPass", askpass.to_str().unwrap()]);
        let got = run_fenced(&dir, &["fetch", "--quiet"], PATIENT);
        assert!(got.is_err(), "{got:?}");
        assert!(!log.exists(), "the repository's askpass ran unattended: {}", read(&log));
        git(&dir, &["config", "--unset", "core.askPass"]);

        // Exported by the desktop session, or by the editor's terminal.
        for name in ["SSH_ASKPASS", "GIT_ASKPASS"] {
            // SAFETY: under the isolation lock, and taken out again below.
            unsafe { std::env::set_var(name, &askpass) };
            let got = run_fenced(&dir, &["fetch", "--quiet"], PATIENT);
            unsafe { std::env::remove_var(name) };
            assert!(got.is_err(), "{name}: {got:?}");
            assert!(!log.exists(), "{name} was run unattended: {}", read(&log));
        }
    }

    /// The other half: a fetch the user asked for may still ask them.
    #[test]
    fn asked_for_one_repository_the_users_askpass_is_still_theirs() {
        let _guard = isolate();
        let (dir, askpass, log) = needs_a_password("rf-gitcmd-askpass-attended");
        // SAFETY: under the isolation lock, and taken out again below.
        unsafe { std::env::set_var("GIT_ASKPASS", &askpass) };
        let got = run(&dir, &["fetch", "--quiet"], PATIENT);
        unsafe { std::env::remove_var("GIT_ASKPASS") };
        assert!(got.is_err(), "{got:?}");
        assert!(read(&log).contains("Password for"), "{}", read(&log));
    }

    /// An ssh command exported for the session is the user's, and used. On a
    /// pass nobody is watching it is still told not to wait on a prompt.
    #[test]
    fn an_exported_ssh_command_is_kept_and_batched_only_when_unattended() {
        let _guard = isolate();
        let dir = ssh_repo("rf-gitcmd-ssh-exported");
        let (ssh, log) = recording(&dir, "ssh");
        let exported = format!("{} -i id_session", ssh.display());

        // SAFETY: under the isolation lock, and taken out again below.
        unsafe { std::env::set_var("GIT_SSH_COMMAND", &exported) };
        let _ = run(&dir, &["fetch", "--quiet"], PATIENT);
        let attended = read(&log);
        std::fs::remove_file(&log).unwrap();
        let _ = run_fenced(&dir, &["fetch", "--quiet"], PATIENT);
        unsafe { std::env::remove_var("GIT_SSH_COMMAND") };
        let unattended = read(&log);

        assert!(attended.contains("-i id_session"), "{attended}");
        assert!(!attended.contains("BatchMode"), "theirs, untouched: {attended}");
        assert!(unattended.contains("-i id_session"), "the exported key was dropped: {unattended}");
        assert!(unattended.contains("BatchMode=yes"), "it could wait on a prompt: {unattended}");
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
        // Progress is written over itself with carriage returns, and what
        // went wrong arrives on the end of the line it interrupted.
        assert_eq!(
            explain(
                "remote: Counting objects: 100% (9/9), done.\n\
                 Receiving objects:  33% (3/9)\rReceiving objects:  44% (4/9)\rfatal: early EOF\n",
                ""
            ),
            "fatal: early EOF"
        );
    }

    /// The README promises a hook's refusal "quoted back as git wrote it".
    #[test]
    fn a_hook_that_refuses_is_quoted_in_full_and_not_talked_over() {
        let _guard = isolate();

        // A linter: its first line is the script's name, the findings follow.
        let dir = repo("gitcmd-hook-lint");
        hook(
            &dir,
            "pre-commit",
            "echo '> lint-staged'\necho 'src/app.ts'\necho '  12:3  error  no-unused-vars'\necho '1 problem'\nexit 1",
        );
        stage_something(&dir);
        let said = run(&dir, &["commit", "-m", "second"], PATIENT).unwrap_err();
        assert_eq!(said, "> lint-staged\nsrc/app.ts\n  12:3  error  no-unused-vars\n1 problem");

        // A pre-push hook: git follows it with "error: failed to push some
        // refs", which used to be what was shown.
        let f = fleet("gitcmd-hook-pre-push");
        commit_file(&f.work, "mine.txt", "mine\n", "ready");
        hook(&f.work, "pre-push", "echo 'pre-push: 3 tests failing, fix them before pushing'\nexit 1");
        let said = run(&f.work, &["push", "origin", "main"], PATIENT).unwrap_err();
        assert_eq!(said, "pre-push: 3 tests failing, fix them before pushing");
    }
    /// What the window's close handler relies on: `busy` is true for exactly
    /// as long as a command runs, a commit with a slow hook is let finish
    /// while the application winds down, and a fetch is not.
    #[test]
    fn closing_waits_for_a_write_and_stops_a_fetch() {
        let _guard = isolate();
        let dir = repo("gitcmd-closing");
        hook(&dir, "pre-commit", "sleep 1");
        write(&dir.join("README.md"), "two\n");
        git(&dir, &["add", "README.md"]);
        assert!(!busy());

        let worker = {
            let dir = dir.clone();
            std::thread::spawn(move || run(&dir, &["commit", "-m", "second"], PATIENT))
        };
        std::thread::sleep(Duration::from_millis(300));
        assert!(busy(), "the commit's hook is still running");
        wind_down(true);
        let committed = worker.join().unwrap();
        wind_down(false);
        assert!(committed.is_ok(), "a write is let finish: {committed:?}");
        assert!(!busy());
        assert_eq!(git(&dir, &["log", "-1", "--format=%s"]), "second");

        // A fetch from a remote that takes its time answering.
        let remote = scratch("gitcmd-closing-remote");
        git(&remote, &["init", "-q", "--bare", "."]);
        let slow = dir.join("slow-upload-pack");
        write(&slow, "#!/bin/sh\nsleep 6\nexec git upload-pack \"$@\"\n");
        std::fs::set_permissions(&slow, std::fs::Permissions::from_mode(0o755)).unwrap();
        git(&dir, &["remote", "add", "origin", remote.to_str().unwrap()]);
        git(&dir, &["config", "remote.origin.uploadpack", slow.to_str().unwrap()]);

        let started = Instant::now();
        let worker = {
            let dir = dir.clone();
            std::thread::spawn(move || run(&dir, &["fetch", "--quiet"], PATIENT))
        };
        std::thread::sleep(Duration::from_millis(300));
        wind_down(true);
        let fetched = worker.join().unwrap();
        wind_down(false);
        assert_eq!(fetched, Err("stopped: omarepos was closing".to_string()));
        assert!(started.elapsed() < Duration::from_secs(4), "it was stopped, not waited for");
        assert!(!busy());
    }
    /// A hook that lists what it objects to with `-z`.
    #[test]
    fn a_nul_in_what_a_hook_printed_is_made_visible() {
        let _guard = isolate();
        let dir = repo("gitcmd-nul");
        hook(&dir, "pre-commit", "printf 'refused: a.txt\\0b.txt\\n' >&2; exit 1");
        write(&dir.join("README.md"), "two\n");
        git(&dir, &["add", "README.md"]);
        let said = run(&dir, &["commit", "-m", "second"], PATIENT).unwrap_err();
        assert!(!said.contains('\0'), "{said:?}");
        assert!(said.contains("a.txt\u{2400}b.txt"), "{said:?}");
    }
}
