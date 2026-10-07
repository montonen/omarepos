//! Contribution aggregation: who committed what, over a rolling window.
//!
//! Authors are keyed by e-mail, so the same person with two spellings of their
//! name still lands in one row. Merge commits count as commits but are left out
//! of the line totals — their diffs are their parents' work, counted already.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use git2::{Repository, Sort};

/// Day boundaries are computed on the main thread, where the local timezone is
/// available, and handed to the worker as plain timestamps.
#[derive(Debug, Clone)]
pub struct Window {
    /// Local midnight for each day, oldest first.
    pub day_starts: Vec<i64>,
    /// Short label per day, e.g. "Mon".
    pub day_labels: Vec<String>,
    /// Full label per day, e.g. "Mon 6 Oct".
    pub day_titles: Vec<String>,
}

impl Window {
    pub fn since(&self) -> i64 {
        self.day_starts.first().copied().unwrap_or(0)
    }

    pub fn days(&self) -> usize {
        self.day_starts.len()
    }

    fn index_of(&self, when: i64) -> Option<usize> {
        if when < self.since() {
            return None;
        }
        let i = self.day_starts.partition_point(|start| *start <= when);
        i.checked_sub(1)
    }
}

#[derive(Debug, Clone)]
pub struct Author {
    pub name: String,
    pub email: String,
    pub commits: usize,
    pub merges: usize,
    pub insertions: usize,
    pub deletions: usize,
    pub files: usize,
    pub per_day: Vec<usize>,
    /// Repository name -> commits in the window.
    pub repos: BTreeMap<String, usize>,
    pub last_seen: i64,
}

impl Author {
    pub fn churn(&self) -> usize {
        self.insertions + self.deletions
    }
}

#[derive(Debug, Clone)]
pub struct Contributions {
    pub window: Window,
    pub authors: Vec<Author>,
    pub per_day_total: Vec<usize>,
    pub repos_scanned: usize,
    pub repos_active: usize,
    pub total_commits: usize,
    pub total_insertions: usize,
    pub total_deletions: usize,
    pub errors: Vec<String>,
}

pub enum Event {
    /// A repository has been read; carries its name and how many repos remain.
    Progress(String, usize, usize),
    Done(Box<Contributions>),
}

/// Walking the full history of a large repo is wasted work for a one-week
/// window, but topological order means we cannot stop at the first old commit.
/// Stop once this many consecutive out-of-window commits have gone by.
const STALE_RUN: usize = 400;

pub fn spawn(repos: Vec<PathBuf>, window: Window) -> async_channel::Receiver<Event> {
    let (tx, rx) = async_channel::unbounded();

    std::thread::spawn(move || {
        let days = window.days();
        let total = repos.len();
        let mut acc: HashMap<String, Author> = HashMap::new();
        let mut per_day_total = vec![0usize; days];
        let mut errors = Vec::new();
        let mut repos_active = 0usize;
        let mut total_commits = 0usize;
        let mut total_insertions = 0usize;
        let mut total_deletions = 0usize;

        for (n, path) in repos.iter().enumerate() {
            let name = path
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| path.display().to_string());

            match collect(path, &name, &window, &mut acc, &mut per_day_total) {
                Ok(found) => {
                    if found.commits > 0 {
                        repos_active += 1;
                    }
                    total_commits += found.commits;
                    total_insertions += found.insertions;
                    total_deletions += found.deletions;
                }
                Err(err) => errors.push(format!("{name}: {err}")),
            }

            if tx
                .send_blocking(Event::Progress(name, n + 1, total))
                .is_err()
            {
                return;
            }
        }

        let mut authors: Vec<Author> = acc.into_values().collect();
        authors.sort_by(|a, b| {
            b.commits
                .cmp(&a.commits)
                .then_with(|| b.churn().cmp(&a.churn()))
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });

        let _ = tx.send_blocking(Event::Done(Box::new(Contributions {
            window,
            authors,
            per_day_total,
            repos_scanned: total,
            repos_active,
            total_commits,
            total_insertions,
            total_deletions,
            errors,
        })));
    });

    rx
}

struct Found {
    commits: usize,
    insertions: usize,
    deletions: usize,
}

fn collect(
    path: &std::path::Path,
    repo_name: &str,
    window: &Window,
    acc: &mut HashMap<String, Author>,
    per_day_total: &mut [usize],
) -> Result<Found, String> {
    let repo = Repository::open(path).map_err(|e| e.message().to_string())?;
    let mut walk = repo.revwalk().map_err(|e| e.message().to_string())?;
    walk.set_sorting(Sort::TIME).map_err(|e| e.message().to_string())?;

    let mut pushed = 0usize;
    if let Ok(refs) = repo.references() {
        for reference in refs.flatten() {
            let Ok(name) = reference.name().map(str::to_string) else { continue };
            if !name.starts_with("refs/heads/") && !name.starts_with("refs/remotes/") {
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
        return Ok(Found { commits: 0, insertions: 0, deletions: 0 });
    }

    let days = window.days();
    let mut found = Found { commits: 0, insertions: 0, deletions: 0 };
    let mut stale = 0usize;

    for oid in walk {
        let Ok(oid) = oid else { continue };
        let Ok(commit) = repo.find_commit(oid) else { continue };
        let when = commit.time().seconds();

        let Some(day) = window.index_of(when) else {
            stale += 1;
            if stale >= STALE_RUN {
                break;
            }
            continue;
        };
        stale = 0;
        if day >= days {
            // Committed with a clock ahead of ours; fold into today.
            continue;
        }

        let author = commit.author();
        let name = author.name().unwrap_or("unknown").to_string();
        let email = author.email().unwrap_or("").trim().to_lowercase();
        let key = if email.is_empty() { name.to_lowercase() } else { email.clone() };

        let is_merge = commit.parent_count() > 1;
        let (insertions, deletions, files) = if is_merge {
            (0, 0, 0)
        } else {
            diff_stats(&repo, &commit)
        };

        let entry = acc.entry(key).or_insert_with(|| Author {
            name: name.clone(),
            email: email.clone(),
            commits: 0,
            merges: 0,
            insertions: 0,
            deletions: 0,
            files: 0,
            per_day: vec![0; days],
            repos: BTreeMap::new(),
            last_seen: 0,
        });
        entry.commits += 1;
        if is_merge {
            entry.merges += 1;
        }
        entry.insertions += insertions;
        entry.deletions += deletions;
        entry.files += files;
        entry.per_day[day] += 1;
        *entry.repos.entry(repo_name.to_string()).or_insert(0) += 1;
        entry.last_seen = entry.last_seen.max(when);

        per_day_total[day] += 1;
        found.commits += 1;
        found.insertions += insertions;
        found.deletions += deletions;
    }

    Ok(found)
}

fn diff_stats(repo: &Repository, commit: &git2::Commit<'_>) -> (usize, usize, usize) {
    let Ok(tree) = commit.tree() else { return (0, 0, 0) };
    let parent_tree = commit.parent(0).ok().and_then(|p| p.tree().ok());
    let Ok(diff) = repo.diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), None) else {
        return (0, 0, 0);
    };
    match diff.stats() {
        Ok(stats) => (stats.insertions(), stats.deletions(), stats.files_changed()),
        Err(_) => (0, 0, 0),
    }
}
