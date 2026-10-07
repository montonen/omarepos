//! Running `git` itself.
//!
//! Anything that writes goes through here rather than through libgit2. libgit2
//! can build an index entry and a commit object, but it does not run hooks and
//! it does not sign — and it does not read the credential helpers, `insteadOf`
//! rewrites or `includeIf gitdir:` identities a user has configured once and
//! expects everywhere. Driving the same `git` they drive means their setup
//! applies unchanged, and a thing they cannot do by hand is not something
//! omarepos pretends it can do either.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Run git in `dir`, refusing to block on a prompt and giving up after
/// `timeout`. Returns stdout on success, and git's own first line of stderr on
/// failure — a hook that refuses should be quoted, not paraphrased.
pub fn run(dir: &Path, args: &[&str], timeout: Duration) -> Result<String, String> {
    let mut command = Command::new("git");
    command
        .current_dir(dir)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
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
                let read = |pipe: Option<&mut dyn std::io::Read>| -> String {
                    let mut text = String::new();
                    if let Some(pipe) = pipe {
                        let _ = pipe.read_to_string(&mut text);
                    }
                    text
                };
                let mut out = child.stdout.take();
                let mut err = child.stderr.take();
                let stdout = read(out.as_mut().map(|p| p as &mut dyn std::io::Read));
                let stderr = read(err.as_mut().map(|p| p as &mut dyn std::io::Read));
                if status.success() {
                    return Ok(stdout);
                }
                return Err(explain(&stderr, &stdout));
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("timed out after {}s", timeout.as_secs()));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(err) => return Err(format!("git: {err}")),
        }
    }
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
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();

    let complaint = lines.iter().find(|line| {
        let lower = line.to_lowercase();
        lower.starts_with("! ")
            || lower.starts_with("error:")
            || lower.starts_with("fatal:")
            || lower.contains("rejected")
    });

    match complaint.or_else(|| lines.first()) {
        Some(line) => line.trim_start_matches("! ").trim().to_string(),
        None => "git failed".to_string(),
    }
}
