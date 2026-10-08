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
pub const MAX_COMMITS_PER_REPO: usize = 50_000;
/// A query matching this much of a repository's history is a query worth
/// narrowing.
///
/// It is also as many rows as the page lists, and has to be at least that. A
/// repository is walked newest first and hands over its newest matches, so the
/// newest N across all of them are certain to be among what arrived only if
/// each may hand over N. At 200 against a page of 500, a repository with 400
/// recent matches gave its newest 200 and the page filled up with older ones
/// from elsewhere, under a status line that said "newest 500 shown".
pub const MAX_HITS_PER_REPO: usize = 500;

/// Where a repository's search gives up.
#[derive(Debug, Clone, Copy)]
struct Limits {
    commits: usize,
    hits: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self { commits: MAX_COMMITS_PER_REPO, hits: MAX_HITS_PER_REPO }
    }
}

/// Why a repository's search ended with history still unread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stopped {
    /// It had as many matches as one repository may hand over. There may be
    /// more, all of them older than the ones found.
    Matches,
    /// It had read as many commits as one repository is read for. Whatever is
    /// older was not looked at.
    Depth,
}

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
    /// One repository stopped at a limit. An event of its own, and not a mark
    /// on `Hits`, because `Hits` is only sent when there are some: a
    /// repository that ran out of depth having found nothing is exactly the
    /// one a page about to say "nothing matches" needs to hear from.
    Stopped(Stopped),
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
    spawn_within(repos, query, parallel, Limits::default())
}

fn spawn_within(
    repos: Vec<PathBuf>,
    query: String,
    parallel: usize,
    limits: Limits,
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

                        let (hits, stopped) = search_one(&path, &needle, &cancel, limits);
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
                        if let Some(why) = stopped {
                            if tx.send_blocking(Event::Stopped(why)).is_err() {
                                return;
                            }
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

/// One repository's matches, newest first, and whether the walk gave up with
/// history still unread. It used to return the matches alone, so a walk that
/// stopped at a limit looked the same as one that had read everything.
fn search_one(
    path: &Path,
    needle: &str,
    cancel: &Cancel,
    limits: Limits,
) -> (Vec<Hit>, Option<Stopped>) {
    let nothing = (Vec::new(), None);
    let Ok(repo) = Repository::open(path) else { return nothing };
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string());

    let Ok(mut walk) = repo.revwalk() else { return nothing };
    if walk.set_sorting(Sort::TIME).is_err() {
        return nothing;
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
        return nothing;
    }

    let mut hits = Vec::new();
    let mut stopped = None;
    for (seen, oid) in walk.enumerate() {
        // Both are asked with another commit already in hand, so a stop is
        // only reported when something really was left unread: a history
        // that ends exactly at a limit has been read to its end.
        if hits.len() >= limits.hits {
            stopped = Some(Stopped::Matches);
            break;
        }
        if seen >= limits.commits {
            stopped = Some(Stopped::Depth);
            break;
        }
        // Checking every commit would make an abandoned search outlive the
        // query that asked for it.
        if seen % 512 == 0 && cancel.stopped() {
            return nothing;
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

    (hits, stopped)
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::*;
    use crate::testkit::*;

    /// A repository whose history is `messages`, oldest first, a minute apart
    /// from `start`. The walk goes by committer time, and commits made within
    /// one second of each other have no order worth asserting on.
    fn history(name: &str, start: i64, messages: &[&str]) -> PathBuf {
        let repo = scratch(name);
        git(&repo, &["init", "-q", "."]);
        for (i, message) in messages.iter().enumerate() {
            let when = format!("@{} +0000", start + 60 * i as i64);
            let out = Command::new("git")
                .current_dir(&repo)
                .args(["commit", "-q", "--allow-empty", "-m", message])
                .env("GIT_AUTHOR_DATE", &when)
                .env("GIT_COMMITTER_DATE", &when)
                .output()
                .expect("git runs");
            assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        }
        repo
    }

    const THEN: i64 = 1_700_000_000;

    fn never() -> Cancel {
        Cancel(Arc::new(AtomicBool::new(false)))
    }

    fn summaries(hits: &[Hit]) -> Vec<&str> {
        hits.iter().map(|hit| hit.summary.as_str()).collect()
    }

    /// Everything a search sends, in the order it arrives, and its hits.
    fn run(repos: &[&Path], query: &str, limits: Limits) -> (Vec<String>, Vec<Hit>) {
        let repos = repos.iter().map(|repo| repo.to_path_buf()).collect();
        let (rx, _cancel) = spawn_within(repos, query.to_string(), 1, limits);
        let (mut heard, mut hits) = (Vec::new(), Vec::new());
        while let Ok(event) = rx.recv_blocking() {
            match event {
                Event::Hits(found) => {
                    heard.push(format!("{} hits", found.len()));
                    hits.extend(found);
                }
                Event::Stopped(why) => heard.push(format!("stopped at {why:?}")),
                Event::Progress(done, of) => heard.push(format!("{done}/{of}")),
                Event::Done => break,
            }
        }
        (heard, hits)
    }

    /// A repository stopped at its limit of matches without a word: 260
    /// commits saying "fix" were reported as "200 commits", the same as a
    /// repository with exactly 200.
    #[test]
    fn a_repository_with_more_matches_than_it_may_hand_over_says_so() {
        let _guard = isolate();
        let said = ["hotfix 1", "hotfix 2", "unrelated", "hotfix 3", "hotfix 4", "hotfix 5"];
        let repo = history("search-match-limit", THEN, &said);

        let three = Limits { commits: 1000, hits: 3 };
        let (hits, stopped) = search_one(&repo, "hotfix", &never(), three);
        assert_eq!(summaries(&hits), ["hotfix 5", "hotfix 4", "hotfix 3"], "its newest");
        assert_eq!(stopped, Some(Stopped::Matches));

        // Room for every one of them, and the history read to its end: then
        // the count is the count, and nothing is said.
        let five = Limits { commits: 1000, hits: 5 };
        let (hits, stopped) = search_one(&repo, "hotfix", &never(), five);
        assert_eq!(hits.len(), 5);
        assert_eq!(stopped, None);

        let (heard, _) = run(&[&repo], "hotfix", three);
        assert_eq!(heard, ["3 hits", "stopped at Matches", "1/1"]);
    }

    /// The other limit, and the one "Nothing in N repositories matches" was
    /// said over: a repository read through its newest commits only, with the
    /// one match further back.
    #[test]
    fn a_walk_that_ran_out_of_depth_says_so_even_with_nothing_found() {
        let _guard = isolate();
        let said = ["the rare one", "second", "third", "fourth", "fifth", "sixth"];
        let repo = history("search-depth-limit", THEN, &said);

        let four_deep = Limits { commits: 4, hits: 10 };
        let (hits, stopped) = search_one(&repo, "rare", &never(), four_deep);
        assert!(hits.is_empty(), "{:?}", summaries(&hits));
        assert_eq!(stopped, Some(Stopped::Depth));

        // It has to reach the page although there are no hits to carry it.
        let (heard, _) = run(&[&repo], "rare", four_deep);
        assert_eq!(heard, ["stopped at Depth", "1/1"]);

        // A history exactly as long as the limit was read to its end.
        let six_deep = Limits { commits: 6, hits: 10 };
        let (hits, stopped) = search_one(&repo, "rare", &never(), six_deep);
        assert_eq!(summaries(&hits), ["the rare one"]);
        assert_eq!(stopped, None);
    }

    /// What lets the page call its rows the newest: listing as many rows as
    /// one repository may hand over. With a smaller limit per repository, the
    /// busy repository's third-newest match was dropped and an older one from
    /// the quiet repository listed in its place.
    #[test]
    fn the_newest_matches_of_the_fleet_are_among_what_each_repository_hands_over() {
        let _guard = isolate();
        let quiet = history("search-newest-quiet", THEN, &["hotfix q1", "hotfix q2", "hotfix q3"]);
        let busy_said = ["hotfix b1", "hotfix b2", "hotfix b3", "hotfix b4", "hotfix b5"];
        let busy = history("search-newest-busy", THEN + 86_400, &busy_said);
        let rows = 3;

        let newest = |limits: Limits| -> Vec<String> {
            let (_, mut hits) = run(&[&quiet, &busy], "hotfix", limits);
            hits.sort_by(|a, b| b.when.cmp(&a.when));
            hits.iter().take(rows).map(|hit| hit.summary.clone()).collect()
        };
        assert_eq!(
            newest(Limits { commits: 1000, hits: rows }),
            ["hotfix b5", "hotfix b4", "hotfix b3"]
        );
        assert_eq!(
            newest(Limits { commits: 1000, hits: rows - 1 }),
            ["hotfix b5", "hotfix b4", "hotfix q3"],
            "the hole this test is about has moved"
        );
    }
}
