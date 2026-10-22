//! The last scan, kept, so that a launch can put the whole grid up at once.
//!
//! A scan reads every repository again, and until it has, the window used to
//! fill card by card from nothing. What a scan read is written here when it
//! ends, and read back at launch: the grid starts from it, each card marked
//! as a reading from the past until the scan has read that repository again
//! (ADR-0009).
//!
//! A cache, and nothing more. A file that is missing, that does not parse,
//! that is of another version or that was written for other roots is simply
//! not used, and nothing is said: all that is lost is a moment. It is written
//! by no one-off look (`--root`), whose roots are not the fleet's.

use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use crate::config::Root;
use crate::git::RepoInfo;

/// Bumped whenever what a reading holds changes shape. A file of any other
/// version is not read.
const VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
struct File {
    version: u32,
    /// What was scanned, as [`scope`] spells it. A file written for other
    /// roots is not this grid.
    scope: String,
    /// When the scan that read these ended, in seconds since 1970.
    taken: i64,
    repos: Vec<RepoInfo>,
}

/// The readings of the last scan, and when it ended.
pub struct Snapshot {
    pub taken: i64,
    pub repos: Vec<RepoInfo>,
}

pub fn path() -> PathBuf {
    crate::config::last_refresh_path().with_file_name("last-scan.toml")
}

/// What a scan covers, as text: two scans with the same scope look for the
/// same repositories in the same places.
pub fn scope(roots: &[Root], extras: &[PathBuf], depth: usize) -> String {
    format!("{roots:?} {extras:?} {depth}")
}

/// The last scan of `scope`, if one was kept.
pub fn load(scope: &str) -> Option<Snapshot> {
    let text = std::fs::read_to_string(path()).ok()?;
    let file: File = toml::from_str(&text).ok()?;
    (file.version == VERSION && file.scope == scope)
        .then_some(Snapshot { taken: file.taken, repos: file.repos })
}

/// Every save is numbered as it is asked for, and the one written last is the
/// newest asked for: two scans that end close together write on threads of
/// their own, in whatever order those come round.
static ASKED: AtomicU64 = AtomicU64::new(0);
static WRITTEN: Mutex<u64> = Mutex::new(0);

/// Keep what a scan of `scope` read, ending at `taken`. On a thread of its
/// own: it is a whole file and a sync to disk, and the window waits for
/// neither.
pub fn save(scope: String, taken: i64, repos: Vec<RepoInfo>) {
    let number = ASKED.fetch_add(1, Ordering::SeqCst) + 1;
    std::thread::spawn(move || {
        let mut written = WRITTEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if *written > number {
            return;
        }
        *written = number;
        if let Err(err) = write_now(scope, taken, repos) {
            note!("omarepos: could not keep the last scan: {err}");
        }
    });
}

/// Write the file there and then.
fn write_now(scope: String, taken: i64, repos: Vec<RepoInfo>) -> std::io::Result<()> {
    // A path that is not UTF-8 cannot be written in TOML. Such a repository
    // is left out, and starts the next launch the way every card used to:
    // when the scan reaches it.
    let repos = repos.into_iter().filter(|repo| repo.path.to_str().is_some()).collect();
    let file = File { version: VERSION, scope, taken, repos };
    let body = toml::to_string(&file).map_err(|err| std::io::Error::other(err.to_string()))?;
    crate::config::write_state(&path(), &body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::*;

    fn root(path: &std::path::Path) -> Root {
        Root { path: path.to_path_buf(), nested: false, tag: None, include_self: true }
    }

    /// Everything a card is drawn from comes back as it went in: a repository
    /// with commits, an upstream, uncommitted work and four weeks of activity,
    /// one with no commits at all, and one that could not be read.
    #[test]
    fn a_kept_scan_comes_back_as_it_was_read() {
        let _guard = isolate();
        let f = fleet("snapshot-roundtrip");
        commit_file(&f.work, "mine.txt", "mine\n", "work of its own");
        write(&f.work.join("README.md"), "edited\n");
        f.remote_moves(2);
        git(&f.work, &["fetch", "-q"]);
        let unborn = scratch("snapshot-unborn");
        git(&unborn, &["init", "-q", "-b", "main"]);
        let nothing = scratch("snapshot-not-a-repository");

        let read: Vec<RepoInfo> =
            [&f.work, &unborn, &nothing].iter().map(|path| crate::git::inspect(path)).collect();
        assert!(read[0].activity.days.len() == crate::git::ACTIVITY_DAYS && read[0].behind == 2);
        assert!(read[1].unborn && read[2].error.is_some());

        let scope = scope(&[root(&f.root)], &[], 3);
        write_now(scope.clone(), 1_700_000_000, read.clone()).unwrap();
        let kept = load(&scope).expect("the scan was kept");
        assert_eq!(kept.taken, 1_700_000_000);
        assert_eq!(format!("{:?}", kept.repos), format!("{read:?}"));
    }

    /// A file for other roots, of another version, or that does not parse is
    /// not used, and none of them is an error.
    #[test]
    fn a_kept_scan_of_something_else_is_not_used() {
        let _guard = isolate();
        let here = scope(&[root(std::path::Path::new("/srv/code"))], &[], 3);
        write_now(here.clone(), 1, Vec::new()).unwrap();
        assert!(load(&here).is_some());

        assert!(load(&scope(&[root(std::path::Path::new("/srv/other"))], &[], 3)).is_none());
        assert!(load(&scope(&[root(std::path::Path::new("/srv/code"))], &[], 4)).is_none());

        let text = std::fs::read_to_string(path()).unwrap();
        std::fs::write(path(), text.replace("version = 1", "version = 999")).unwrap();
        assert!(load(&here).is_none());
        std::fs::write(path(), "repos = [").unwrap();
        assert!(load(&here).is_none());
        let _ = std::fs::remove_file(path());
        assert!(load(&here).is_none());
    }

    /// Saves land newest last, whatever order their threads come round in.
    #[test]
    fn the_newest_scan_asked_to_be_kept_is_the_one_kept() {
        let _guard = isolate();
        let here = scope(&[], &[], 3);
        for taken in 1..=20 {
            save(here.clone(), taken, Vec::new());
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while load(&here).map(|kept| kept.taken) != Some(20) {
            assert!(std::time::Instant::now() < deadline, "the last save never landed");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let _ = std::fs::remove_file(path());
    }
}
