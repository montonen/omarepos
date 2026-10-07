//! Background scanning.
//!
//! Discovery and `git status` are both blocking I/O, so they run on worker
//! threads and stream results back to the GTK main loop over an async channel.
//! Cards appear as they are inspected rather than after the whole fleet is done.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::git::{self, RepoInfo};

pub enum Event {
    /// Discovery finished: this many repositories are on their way.
    Discovered(usize),
    Repo(RepoInfo),
    Done,
}

/// Kick off a scan. The returned receiver yields one `Discovered`, then one
/// `Repo` per repository, then `Done`. `extras` are individually added
/// repositories, included whether or not they sit under a root.
pub fn spawn(
    roots: Vec<PathBuf>,
    extras: Vec<PathBuf>,
    max_depth: usize,
) -> async_channel::Receiver<Event> {
    let (tx, rx) = async_channel::unbounded();

    std::thread::spawn(move || {
        let mut paths = git::discover(&roots, max_depth);
        let known: std::collections::HashSet<PathBuf> = paths
            .iter()
            .map(|p| p.canonicalize().unwrap_or_else(|_| p.clone()))
            .collect();
        for extra in extras {
            let canonical = extra.canonicalize().unwrap_or_else(|_| extra.clone());
            if !known.contains(&canonical) && canonical.join(".git").exists() {
                paths.push(canonical);
            }
        }
        paths.sort();
        paths.dedup();
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
                    let Some(path) = next else { return };
                    if tx.send_blocking(Event::Repo(git::inspect(&path))).is_err() {
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
