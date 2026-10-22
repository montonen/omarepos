//! `omarepos --json` as a script meets it: the program itself, run with a home
//! of its own and nothing else in its environment (no display, no session
//! bus, no `PATH`), and held to what docs/usage.md promises of its standard
//! output and its exit status.
//!
//! The unit tests in src/fleet_json.rs say what a snapshot holds. These say
//! what reaches whoever ran it. A snapshot is JSON, and 0. A failure leaves
//! standard output empty, says why on standard error, and is 1, and so is a
//! standard output that cannot be written to. A reader that went away is 1
//! and is told nothing. A standard output closed before the run is
//! `/dev/null` by the time omarepos starts, and the run is 0. A run writes
//! nothing in the home it is given.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use serde_json::Value;

/// A home of its own for one test, under the target directory: `~/code/app`,
/// a repository with one commit, and `roots = ["~/code"]` in the
/// configuration, where XDG puts it.
struct Home(PathBuf);

impl Home {
    fn new(name: &str) -> Self {
        let home = Home(Path::new(env!("CARGO_TARGET_TMPDIR")).join(name));
        let _ = std::fs::remove_dir_all(&home.0);
        let app = home.app();
        std::fs::create_dir_all(&app).unwrap();
        git(&app, &["init", "-q", "-b", "main"]);
        std::fs::write(app.join("a.txt"), "a\n").unwrap();
        git(&app, &["add", "a.txt"]);
        git(&app, &["commit", "-q", "-m", "one"]);
        home.write(&home.config().join("config.toml"), "roots = [\"~/code\"]\n");
        home
    }

    fn app(&self) -> PathBuf {
        self.0.join("code/app")
    }

    fn config(&self) -> PathBuf {
        self.0.join(".config/omarepos")
    }

    fn state(&self) -> PathBuf {
        self.0.join(".local/state/omarepos")
    }

    fn write(&self, path: &Path, body: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    /// `omarepos` with `args`, in this home and with nothing else in its
    /// environment.
    fn omarepos(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_omarepos"));
        command
            .args(args)
            .env_clear()
            .env("HOME", &self.0)
            .env("XDG_CONFIG_HOME", self.0.join(".config"))
            .env("XDG_STATE_HOME", self.0.join(".local/state"))
            .stdin(Stdio::null());
        command
    }
}

/// Run git to build a fixture, with nobody's configuration, and none of the
/// housekeeping git starts in the background after a commit: it takes a
/// lock in `.git/objects`, and a run that was to write nothing was seen to.
fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .current_dir(dir)
        .args(["-c", "maintenance.auto=false", "-c", "gc.auto=0"])
        .args(args)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "Fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
        .env("GIT_COMMITTER_NAME", "Fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
        .status()
        .expect("git runs");
    assert!(status.success(), "fixture git {args:?} in {} failed", dir.display());
}

/// Every file and directory under `dir`, with what it holds and when it was
/// last changed.
fn every_file(dir: &Path) -> BTreeMap<PathBuf, (Vec<u8>, std::time::SystemTime)> {
    let mut found = BTreeMap::new();
    let mut waiting = vec![dir.to_path_buf()];
    while let Some(dir) = waiting.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let (path, meta) = (entry.path(), entry.metadata().unwrap());
            let held = if meta.is_dir() {
                waiting.push(path.clone());
                Vec::new()
            } else {
                std::fs::read(&path).unwrap_or_default()
            };
            found.insert(path, (held, meta.modified().unwrap()));
        }
    }
    found
}

fn said(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn a_snapshot_is_json_and_its_status_is_0() {
    let home = Home::new("json-cli-snapshot");
    let before = every_file(&home.0);
    let out = home.omarepos(&["--json"]).output().expect("omarepos runs");
    let after = every_file(&home.0);

    assert_eq!(out.status.code(), Some(0), "{}", said(&out));
    assert_eq!(said(&out), "");
    let printed: Value = serde_json::from_slice(&out.stdout).expect("JSON, and nothing else");
    assert_eq!(printed["schema_version"], 1);
    let repos = printed["repos"].as_array().unwrap();
    assert_eq!(repos.len(), 1, "{printed:#}");
    assert_eq!(repos[0]["path"], home.app().canonicalize().unwrap().display().to_string());

    let written: Vec<&PathBuf> =
        before.keys().chain(after.keys()).filter(|path| before.get(*path) != after.get(*path)).collect();
    assert!(written.is_empty(), "written by a snapshot: {written:?}");
}

#[test]
fn a_configuration_that_cannot_be_read_leaves_standard_output_empty() {
    let home = Home::new("json-cli-broken-config");
    home.write(&home.config().join("config.toml"), "roots = [\"~/code\"]\nmax_dpeth = 4\n");
    let out = home.omarepos(&["--json"]).output().expect("omarepos runs");

    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty(), "printed: {}", String::from_utf8_lossy(&out.stdout));
    assert!(said(&out).contains("config.toml") && said(&out).contains("max_dpeth"), "{}", said(&out));
}

/// Rust's runtime opens `/dev/null` in the place of a standard output that
/// is closed before the program starts (`>&-`). The run is then the same as
/// one sent to `/dev/null`, which is 0 and says nothing, as docs/usage.md
/// says. So is `/dev/null` opened as a script's "throw this away" opens it,
/// for reading and writing (Python's `subprocess.DEVNULL`).
#[test]
fn a_closed_standard_output_is_dev_null() {
    use std::os::unix::process::CommandExt;
    let home = Home::new("json-cli-closed");
    let mut command = home.omarepos(&["--json"]);
    // SAFETY: close(2) in the child, between fork and exec, of a descriptor
    // that is the child's own. Nothing is allocated.
    unsafe {
        command.pre_exec(|| {
            libc::close(libc::STDOUT_FILENO);
            Ok(())
        });
    }
    let out = command.output().expect("omarepos runs");
    assert_eq!(out.status.code(), Some(0), "{}", said(&out));
    assert_eq!(said(&out), "");

    let null = std::fs::OpenOptions::new().read(true).write(true).open("/dev/null").unwrap();
    let out = home.omarepos(&["--json"]).stdout(null).output().expect("omarepos runs");
    assert_eq!(out.status.code(), Some(0), "{}", said(&out));
    assert_eq!(said(&out), "");
}

/// A standard output that refuses what is written to it, as a full disk
/// does, is a failure, and says so.
#[test]
fn a_standard_output_that_cannot_be_written_to_is_a_failure() {
    let home = Home::new("json-cli-full");
    let full = std::fs::OpenOptions::new().write(true).open("/dev/full").unwrap();
    let out = home.omarepos(&["--json"]).stdout(full).output().expect("omarepos runs");

    assert_eq!(out.status.code(), Some(1));
    assert!(said(&out).contains("the snapshot could not be written"), "{}", said(&out));
}

#[test]
fn a_reader_that_went_away_is_a_failure_and_is_told_nothing() {
    let home = Home::new("json-cli-gone");
    let (reader, writer) = std::io::pipe().unwrap();
    drop(reader);
    let out = home.omarepos(&["--json"]).stdout(writer).output().expect("omarepos runs");

    assert_eq!(out.status.code(), Some(1));
    assert_eq!(said(&out), "");
}

/// A remote's login is not printed, written in a way git itself cannot leave
/// out of what it says, a password with a `/` and a quote in it, which no
/// look for addresses finds either: neither as the remote nor out of the
/// record of a failed fetch in the state directory.
#[test]
fn no_login_is_printed() {
    let home = Home::new("json-cli-login");
    let app = home.app();
    let url = "https://deploy:it's/SECRET@example.invalid/x.git";
    git(&app, &["remote", "add", "origin", url]);
    let key = app.canonicalize().unwrap().display().to_string();
    let said_then = format!("fatal: unable to access '{url}/': URL rejected");
    home.write(&home.state().join("fetch-errors.toml"), &format!("{key:?} = {said_then:?}\n"));

    let out = home.omarepos(&["--json"]).output().expect("omarepos runs");
    assert_eq!(out.status.code(), Some(0), "{}", said(&out));
    let text = String::from_utf8_lossy(&out.stdout);
    for secret in ["SECRET", "it's", "deploy"] {
        assert!(!text.contains(secret), "{secret} in {text}");
    }
    let printed: Value = serde_json::from_str(&text).expect("JSON, and nothing else");
    let repo = &printed["repos"][0];
    assert_eq!(repo["remote"], "example.invalid/x");
    assert!(repo["web"].is_null(), "{repo:#}");
    assert_eq!(repo["fetch_failed"], "fatal: unable to access 'https://•••@example.invalid/x.git/': URL rejected");
}

#[test]
fn a_record_of_failed_fetches_that_cannot_be_read_is_said_and_the_snapshot_printed() {
    let home = Home::new("json-cli-record");
    home.write(&home.state().join("fetch-errors.toml"), "\"/x\" = \"timed ou");
    let out = home.omarepos(&["--json"]).output().expect("omarepos runs");

    assert_eq!(out.status.code(), Some(0), "{}", said(&out));
    assert!(said(&out).contains("fetch-errors.toml"), "{}", said(&out));
    let printed: Value = serde_json::from_slice(&out.stdout).expect("JSON, and nothing else");
    assert!(printed["fetch_failures_unreadable"].is_string(), "{printed:#}");
}
