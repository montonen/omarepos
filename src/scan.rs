//! Background scanning.
//!
//! Discovery and `git status` are both blocking I/O, so they run on worker
//! threads and stream results back to the GTK main loop over an async channel.
//! Cards appear as they are inspected rather than after the whole fleet is done.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::config::Root;
use crate::git::{self, Found, RepoInfo, RootReport, Seed};

pub enum Event {
    /// Discovery finished: this many repositories are on their way.
    Discovered(usize),
    Repo {
        info: RepoInfo,
        /// What the root it was found under wants tagged on it.
        seed: Option<Seed>,
    },
    Done,
}

/// Kick off a scan. The first receiver yields one `Discovered`, then one
/// `Repo` per repository, then `Done`. `extras` are individually added
/// repositories, included whether or not they sit under a root.
///
/// The second receiver has what each root came to: one report per root, in
/// the order the roots were given. The events can only speak of repositories
/// that were found, so a root that found none had no way to say so: its
/// repositories were simply not on the grid. The reports are sent once,
/// before `Discovered`, so by the time that event is handled a `try_recv` on
/// the second receiver has them.
pub fn spawn(
    roots: Vec<Root>,
    extras: Vec<PathBuf>,
    max_depth: usize,
) -> (async_channel::Receiver<Event>, async_channel::Receiver<Vec<RootReport>>) {
    let (tx, rx) = async_channel::unbounded();
    let (reports_tx, reports_rx) = async_channel::bounded(1);

    std::thread::spawn(move || {
        let survey = git::survey(&roots, max_depth);
        // Nobody listening is not a reason to stop: `spawn` does not listen.
        let _ = reports_tx.send_blocking(survey.roots);
        let mut paths = survey.found;
        let known: std::collections::HashSet<PathBuf> = paths
            .iter()
            .map(|found| {
                found
                    .path
                    .canonicalize()
                    .unwrap_or_else(|_| found.path.clone())
            })
            .collect();
        for extra in extras {
            let canonical = extra.canonicalize().unwrap_or_else(|_| extra.clone());
            if !known.contains(&canonical) && canonical.join(".git").exists() {
                paths.push(Found { path: canonical, seed: None });
            }
        }
        paths.sort_by(|a, b| a.path.cmp(&b.path));
        if tx.send_blocking(Event::Discovered(paths.len())).is_err() {
            return;
        }

        let workers = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
            .clamp(1, 8)
            .min(paths.len().max(1));

        let queue = Arc::new(Mutex::new(paths.into_iter()));
        let mut handles = Vec::with_capacity(workers);
        for _ in 0..workers {
            let queue = Arc::clone(&queue);
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
                    let Some(found) = next else { return };
                    let event = Event::Repo {
                        info: git::inspect(&found.path),
                        seed: found.seed,
                    };
                    if tx.send_blocking(event).is_err() {
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

    (rx, reports_rx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::*;

    fn root(path: &std::path::Path) -> Root {
        Root { path: path.to_path_buf(), nested: false, tag: None, include_self: true }
    }

    /// Read a scan to its end: how many repositories it announced, and the
    /// names of the ones that came.
    fn drain(events: &async_channel::Receiver<Event>) -> (Option<usize>, Vec<String>) {
        let (mut announced, mut names) = (None, Vec::new());
        loop {
            match events.recv_blocking().expect("a scan ends with Done") {
                Event::Discovered(count) => announced = Some(count),
                Event::Repo { info, .. } => names.push(info.name),
                Event::Done => break,
            }
        }
        names.sort();
        (announced, names)
    }

    #[test]
    fn a_scan_says_what_each_root_came_to_before_it_says_how_many_were_found() {
        let _guard = isolate();
        let base = scratch("disc-scan");
        for repo in ["code/one", "code/two"] {
            let dir = base.join(repo);
            std::fs::create_dir_all(&dir).unwrap();
            git(&dir, &["init", "-q", "-b", "main"]);
        }
        let roots = vec![root(&base.join("code")), root(&base.join("moved-away"))];

        let (events, reports) = spawn(roots.clone(), Vec::new(), 3);
        // The window takes the reports when it is told how many repositories
        // are coming, without waiting: they have to be there by then.
        let first = events.recv_blocking().unwrap();
        assert!(matches!(first, Event::Discovered(2)));
        let reports = reports.try_recv().expect("the reports are sent first");
        assert_eq!(reports.len(), 2);
        assert_eq!((reports[0].found, reports[0].says()), (2, None));
        assert_eq!(reports[1].says().as_deref(), Some("does not exist"));
        assert_eq!(drain(&events), (None, vec!["one".to_string(), "two".to_string()]));

        // And a scan nobody takes the reports from runs to its end all the
        // same.
        let (events, _) = spawn(roots, Vec::new(), 3);
        assert_eq!(drain(&events), (Some(2), vec!["one".to_string(), "two".to_string()]));
    }
}
