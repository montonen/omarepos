//! Searching commits across many repositories at once.
//!
//! Everything runs through libgit2 on worker threads; nothing shells out and
//! nothing touches the network. A search is abandoned the moment the query
//! changes, so typing does not queue up passes over the whole fleet.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use git2::{Repository, Sort};

/// Walking every commit of a large repository on every keystroke is not worth
/// it; this is far past any history worth scrolling.
const MAX_COMMITS_PER_REPO: usize = 50_000;
/// A query matching half a repository's history is a query worth narrowing.
const MAX_HITS_PER_REPO: usize = 200;

#[derive(Debug, Clone)]
pub struct Hit {
    pub repo: String,
    pub path: PathBuf,
    pub short_id: String,
    pub id: String,
    pub summary: String,
    pub author: String,
    pub when: i64,
    /// Which part of the commit matched, for the result row.
    pub matched: Match,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Match {
    Summary,
    Body,
    Author,
    Sha,
}

impl Match {
    pub fn label(self) -> &'static str {
        match self {
            Match::Summary => "subject",
            Match::Body => "message",
            Match::Author => "author",
            Match::Sha => "sha",
        }
    }
}

pub enum Event {
    /// One repository finished: how many are done, and of how many.
    Progress(usize, usize),
    Hits(Vec<Hit>),
    Done,
}

/// A handle that lets the caller abandon a search it no longer wants.
#[derive(Clone)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub fn stop(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    fn stopped(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

pub fn spawn(
    repos: Vec<PathBuf>,
    query: String,
    parallel: usize,
) -> (async_channel::Receiver<Event>, Cancel) {
    let (tx, rx) = async_channel::unbounded();
    let cancel = Cancel(Arc::new(AtomicBool::new(false)));
    let needle = query.trim().to_lowercase();

    {
        let cancel = cancel.clone();
        std::thread::spawn(move || {
            let total = repos.len();
            let workers = parallel.clamp(1, 8).min(total.max(1));
            let queue = Arc::new(Mutex::new(repos.into_iter()));
            let done = Arc::new(Mutex::new(0usize));
            let mut handles = Vec::with_capacity(workers);

            for _ in 0..workers {
                let queue = Arc::clone(&queue);
                let done = Arc::clone(&done);
                let needle = needle.clone();
                let cancel = cancel.clone();
                let tx = tx.clone();
                handles.push(std::thread::spawn(move || {
                    loop {
                        if cancel.stopped() {
                            return;
                        }
                        let next = {
                            let mut guard = match queue.lock() {
                                Ok(guard) => guard,
                                Err(_) => return,
                            };
                            guard.next()
                        };
                        let Some(path) = next else { return };

                        let hits = search_one(&path, &needle, &cancel);
                        let finished = {
                            let mut guard = match done.lock() {
                                Ok(guard) => guard,
                                Err(_) => return,
                            };
                            *guard += 1;
                            *guard
                        };
                        if !hits.is_empty() && tx.send_blocking(Event::Hits(hits)).is_err() {
                            return;
                        }
                        if tx.send_blocking(Event::Progress(finished, total)).is_err() {
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
    }

    (rx, cancel)
}

fn search_one(path: &Path, needle: &str, cancel: &Cancel) -> Vec<Hit> {
    let mut hits = Vec::new();
    let Ok(repo) = Repository::open(path) else { return hits };
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string());

    let Ok(mut walk) = repo.revwalk() else { return hits };
    if walk.set_sorting(Sort::TIME).is_err() {
        return hits;
    }

    let mut pushed = 0usize;
    if let Ok(refs) = repo.references() {
        for reference in refs.flatten() {
            let Ok(full) = reference.name().map(str::to_string) else { continue };
            if !full.starts_with("refs/heads/") && !full.starts_with("refs/remotes/") {
                continue;
            }
            if let Ok(commit) = reference.peel_to_commit() {
                if walk.push(commit.id()).is_ok() {
                    pushed += 1;
                }
            }
        }
    }
    if pushed == 0 && walk.push_head().is_err() {
        return hits;
    }

    for (seen, oid) in walk.enumerate() {
        if seen >= MAX_COMMITS_PER_REPO || hits.len() >= MAX_HITS_PER_REPO {
            break;
        }
        // Checking every commit would make an abandoned search outlive the
        // query that asked for it.
        if seen % 512 == 0 && cancel.stopped() {
            return Vec::new();
        }
        let Ok(oid) = oid else { continue };
        let Ok(commit) = repo.find_commit(oid) else { continue };

        let id = oid.to_string();
        let message = commit.message().unwrap_or("");
        let summary = commit.summary().ok().flatten().unwrap_or("").trim().to_string();
        let author = commit.author();
        let author_name = author.name().unwrap_or("unknown").to_string();
        let email = author.email().unwrap_or("");

        let matched = if summary.to_lowercase().contains(needle) {
            Some(Match::Summary)
        } else if message.to_lowercase().contains(needle) {
            Some(Match::Body)
        } else if author_name.to_lowercase().contains(needle)
            || email.to_lowercase().contains(needle)
        {
            Some(Match::Author)
        } else if id.starts_with(needle) {
            Some(Match::Sha)
        } else {
            None
        };

        if let Some(matched) = matched {
            hits.push(Hit {
                repo: name.clone(),
                path: path.to_path_buf(),
                short_id: id.chars().take(7).collect(),
                id,
                summary: if summary.is_empty() { "(no message)".into() } else { summary },
                author: author_name,
                when: commit.time().seconds(),
                matched,
            });
        }
    }

    hits
}
