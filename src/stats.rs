//! Contribution aggregation: who committed what, over a rolling window.
//!
//! What is read is one line of history per repository: the commit HEAD is on,
//! and the tip of the branch the checked-out one tracks, so that what has been
//! fetched and not yet merged is there too. Not every branch. A change that was
//! squashed, rebased or amended exists once per copy, under a new id each time,
//! and the old copies stay reachable from the branch that was merged and from
//! its remote-tracking ref long after the change has landed. Walking every ref
//! counted each of them.
//!
//! Authors are keyed by e-mail, so the same person with two spellings of their
//! name still lands in one row. Merge commits count as commits but are left out
//! of the line totals — their diffs are their parents' work, counted already.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;

use git2::{Oid, Repository, Sort};

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

/// One commit, kept so an author's row can be expanded to show the work
/// rather than only its total.
#[derive(Debug, Clone)]
pub struct Commit {
    pub short_id: String,
    pub summary: String,
    pub when: i64,
    pub insertions: usize,
    pub deletions: usize,
    pub merge: bool,
}

/// An author's commits in one repository. `total` counts them all; `commits`
/// holds the newest few, so a month over a busy fleet does not keep every
/// message in memory for a list nobody scrolls that far down.
#[derive(Debug, Clone, Default)]
pub struct RepoCommits {
    pub commits: Vec<Commit>,
    pub total: usize,
}

/// Past this, the list is no longer telling anyone anything new.
pub const MAX_COMMITS_LISTED: usize = 100;

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
    /// Repository name -> what they did there.
    pub repos: BTreeMap<String, RepoCommits>,
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
    let said = |e: git2::Error| e.message().to_string();
    let repo = Repository::open(path).map_err(said)?;
    let mut walk = repo.revwalk().map_err(said)?;
    walk.set_sorting(Sort::TIME).map_err(said)?;

    // HEAD and its upstream, and nothing else: see the top of the file. HEAD
    // used to be walked only when no branch could be, so a commit made on a
    // detached HEAD was counted nowhere.
    let Some(head) = head_commit(&repo).map_err(said)? else {
        return Ok(Found { commits: 0, insertions: 0, deletions: 0 });
    };
    walk.push(head).map_err(said)?;
    let mut rewritten = HashSet::new();
    if let Some(upstream) = upstream_tip(&repo).map_err(said)? {
        walk.push(upstream).map_err(said)?;
        rewritten = rewritten_upstream(&repo, head, upstream, window.since()).map_err(said)?;
    }

    let days = window.days();
    let mut found = Found { commits: 0, insertions: 0, deletions: 0 };
    let mut stale = 0usize;

    for oid in walk {
        let Ok(oid) = oid else { continue };
        if rewritten.contains(&oid) {
            continue;
        }
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

        let in_repo = entry.repos.entry(repo_name.to_string()).or_default();
        in_repo.total += 1;
        if in_repo.commits.len() < MAX_COMMITS_LISTED {
            in_repo.commits.push(Commit {
                short_id: oid.to_string().chars().take(7).collect(),
                summary: commit
                    .summary()
                    .ok()
                    .flatten()
                    .unwrap_or("(no message)")
                    .trim()
                    .to_string(),
                when,
                insertions,
                deletions,
                merge: is_merge,
            });
        }
        entry.last_seen = entry.last_seen.max(when);

        per_day_total[day] += 1;
        found.commits += 1;
        found.insertions += insertions;
        found.deletions += deletions;
    }

    Ok(found)
}

/// The commit HEAD is on, or nothing in a repository that has no commit yet.
/// A HEAD that is there and cannot be read is an error: reported as "no
/// commits", it made a damaged repository look like a quiet one.
fn head_commit(repo: &Repository) -> Result<Option<Oid>, git2::Error> {
    match repo.head() {
        Ok(head) => Ok(Some(head.peel_to_commit()?.id())),
        Err(err) if err.code() == git2::ErrorCode::UnbornBranch => Ok(None),
        Err(err) => Err(err),
    }
}

/// The tip of the branch the checked-out one tracks, when it tracks one that
/// is there. Nothing on a detached HEAD, and nothing for a branch that was
/// never published or whose upstream is gone.
fn upstream_tip(repo: &Repository) -> Result<Option<Oid>, git2::Error> {
    let Ok(head) = repo.head() else { return Ok(None) };
    let Ok(upstream) = git2::Branch::wrap(head).upstream() else { return Ok(None) };
    Ok(Some(upstream.get().peel_to_commit()?.id()))
}

/// Who wrote a commit and the second they wrote it. A rebase, an amend and a
/// cherry-pick all make a new commit and all carry these two over.
type Stamp = (Vec<u8>, i64);

/// The commits on the upstream's side of a divergence that the local branch
/// holds a newer copy of.
///
/// HEAD and its upstream are both walked, and most of the time one contains the
/// other. When neither does, there are two ways it came about. Somebody pushed
/// while there was unpushed work here: two sets of commits, all to be counted.
/// Or work was pushed and then amended or rebased here, and not yet pushed
/// again: one set of commits, twice. The second is told from the first by a
/// commit on each side with the same author and the same author date, and of
/// such a pair the upstream's is the one about to be overwritten.
fn rewritten_upstream(
    repo: &Repository,
    head: Oid,
    upstream: Oid,
    since: i64,
) -> Result<HashSet<Oid>, git2::Error> {
    let (ahead, behind) = repo.graph_ahead_behind(head, upstream)?;
    if ahead == 0 || behind == 0 {
        return Ok(HashSet::new());
    }
    let ours: HashSet<Stamp> = only_on(repo, head, upstream, since)?
        .into_iter()
        .map(|(_, stamp)| stamp)
        .collect();
    Ok(only_on(repo, upstream, head, since)?
        .into_iter()
        .filter(|(_, stamp)| ours.contains(stamp))
        .map(|(oid, _)| oid)
        .collect())
}

/// The commits reachable from `tip` and not from `other`, newest first, back to
/// the start of the window. A copy is committed after what it copies, so
/// nothing committed before the window can be a copy of anything inside it.
fn only_on(
    repo: &Repository,
    tip: Oid,
    other: Oid,
    since: i64,
) -> Result<Vec<(Oid, Stamp)>, git2::Error> {
    let mut walk = repo.revwalk()?;
    walk.set_sorting(Sort::TIME)?;
    walk.push(tip)?;
    walk.hide(other)?;
    let mut side = Vec::new();
    for oid in walk {
        let oid = oid?;
        let commit = repo.find_commit(oid)?;
        if commit.time().seconds() < since {
            break;
        }
        let author = commit.author();
        side.push((oid, (author.email_bytes().to_vec(), author.when().seconds())));
    }
    Ok(side)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::*;
    use std::path::Path;

    const DAY: i64 = 86_400;

    const BOB: (&str, &str) = ("Bob Builder", "bob@example.invalid");

    fn now() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap()
    }

    /// The last `days` days, cut at midnight UTC: the worker is handed plain
    /// timestamps and never asks which timezone they came from.
    fn window(days: usize) -> Window {
        let today = now() - now().rem_euclid(DAY);
        Window {
            day_starts: (0..days as i64).rev().map(|back| today - back * DAY).collect(),
            day_labels: vec![String::new(); days],
            day_titles: vec![String::new(); days],
        }
    }

    /// What the page would show for these repositories over the last week.
    fn contributions(paths: &[&Path]) -> Contributions {
        let rx = spawn(paths.iter().map(|p| p.to_path_buf()).collect(), window(7));
        loop {
            if let Event::Done(result) = rx.recv_blocking().expect("the worker reported") {
                return *result;
            }
        }
    }

    /// (commits, insertions, deletions) across everything read.
    fn totals(result: &Contributions) -> (usize, usize, usize) {
        (result.total_commits, result.total_insertions, result.total_deletions)
    }

    /// E-mail -> commits, as the page has it.
    fn by_author(result: &Contributions) -> BTreeMap<String, usize> {
        result.authors.iter().map(|a| (a.email.clone(), a.commits)).collect()
    }

    fn lines(n: usize) -> String {
        (0..n).map(|i| format!("line {i}\n")).collect()
    }

    /// Commit whatever is staged as somebody other than the fixture's own
    /// identity.
    fn commit_staged_as(repo: &Path, who: (&str, &str), message: &str) {
        let (name, email) = (format!("user.name={}", who.0), format!("user.email={}", who.1));
        git(repo, &["-c", &name, "-c", &email, "commit", "-q", "-m", message]);
    }

    fn commit_as(repo: &Path, who: (&str, &str), file: &str, body: &str, message: &str) {
        write(&repo.join(file), body);
        git(repo, &["add", "--", file]);
        commit_staged_as(repo, who, message);
    }

    /// E-mail -> commits, as `git shortlog -sne --since=<the window>` has it
    /// for the same history.
    fn shortlog(repo: &Path, revs: &[&str]) -> BTreeMap<String, usize> {
        let since = format!("--since=@{} +0000", window(7).since());
        let mut args = vec!["shortlog", "-sne", since.as_str()];
        args.extend(revs);
        let mut counts = BTreeMap::new();
        for line in git(repo, &args).lines() {
            let (count, who) = line.trim().split_once('\t').expect("a count and a name");
            let email = who.rsplit_once('<').expect("an address").1.trim_end_matches('>');
            *counts.entry(email.to_lowercase()).or_insert(0) += count.trim().parse::<usize>().unwrap();
        }
        counts
    }

    /// (commits, insertions, deletions) as `git log --shortstat` has them for
    /// the same history: a merge is a commit and adds no lines.
    fn git_totals(repo: &Path, revs: &[&str]) -> (usize, usize, usize) {
        let since = format!("--since=@{} +0000", window(7).since());
        let mut count = vec!["rev-list", "--count", since.as_str()];
        count.extend(revs);
        let commits = git(repo, &count).parse::<usize>().unwrap();

        let mut stat = vec!["log", "--no-merges", "--shortstat", "--format=", since.as_str()];
        stat.extend(revs);
        let (mut insertions, mut deletions) = (0, 0);
        for line in git(repo, &stat).lines() {
            for part in line.split(',') {
                let n = part.trim().split(' ').next().and_then(|n| n.parse::<usize>().ok());
                if part.contains("insertion") {
                    insertions += n.unwrap();
                } else if part.contains("deletion") {
                    deletions += n.unwrap();
                }
            }
        }
        (commits, insertions, deletions)
    }

    // ---- which commits are walked -------------------------------------------

    /// The ordinary life of a pull request: pushed as a branch, fetched here
    /// while it was open, landed on main as one new commit, deleted on the
    /// forge. The fetch this application runs does not prune, so the branch's
    /// remote-tracking ref stays behind with the three commits that were
    /// squashed.
    #[test]
    fn a_squashed_branch_left_behind_is_not_counted_beside_what_landed() {
        let _guard = isolate();
        let f = fleet("stats-squash");
        git(&f.seed, &["checkout", "-q", "-b", "feature"]);
        for i in 0..3 {
            commit_as(&f.seed, BOB, &format!("f{i}.txt"), &lines(10), &format!("feature {i}"));
        }
        git(&f.seed, &["push", "-q", "-u", "origin", "feature"]);
        git(&f.work, &["fetch", "--quiet"]);

        git(&f.seed, &["checkout", "-q", "main"]);
        git(&f.seed, &["merge", "-q", "--squash", "feature"]);
        commit_staged_as(&f.seed, BOB, "Add the feature (#12)");
        git(&f.seed, &["push", "-q", "origin", "main", ":feature"]);
        git(&f.work, &["fetch", "--quiet"]);
        git(&f.work, &["merge", "-q", "--ff-only", "@{u}"]);
        assert!(
            git(&f.work, &["for-each-ref", "refs/remotes/origin/feature"]).contains("feature"),
            "the fetch left the deleted branch's ref behind, which is the point"
        );

        let result = contributions(&[&f.work]);
        assert_eq!(by_author(&result), shortlog(&f.work, &["HEAD", "@{u}"]));
        assert_eq!(totals(&result), git_totals(&f.work, &["HEAD", "@{u}"]));
        assert_eq!(by_author(&result).get(BOB.1), Some(&1), "one commit landed");
        assert_eq!(totals(&result), (2, 31, 0), "the first commit and the squash");
    }

    /// The same, landed with "rebase and merge", and seen from the clone of
    /// whoever wrote it: their own local branch is still there too.
    #[test]
    fn a_rebased_branch_left_behind_is_not_counted_beside_what_landed() {
        let _guard = isolate();
        let f = fleet("stats-rebase-merge");
        git(&f.work, &["checkout", "-q", "-b", "feature"]);
        for i in 0..3 {
            commit_file(&f.work, &format!("f{i}.txt"), &lines(10), &format!("feature {i}"));
        }
        git(&f.work, &["push", "-q", "-u", "origin", "feature"]);
        git(&f.work, &["checkout", "-q", "main"]);

        // Main moves, so the rebase makes three new commits out of the three.
        f.remote_moves(1);
        git(&f.seed, &["fetch", "-q"]);
        git(&f.seed, &["checkout", "-q", "feature"]);
        git(&f.seed, &["rebase", "-q", "main"]);
        git(&f.seed, &["checkout", "-q", "main"]);
        git(&f.seed, &["merge", "-q", "--ff-only", "feature"]);
        git(&f.seed, &["push", "-q", "origin", "main"]);
        git(&f.work, &["fetch", "--quiet"]);
        git(&f.work, &["merge", "-q", "--ff-only", "@{u}"]);

        let result = contributions(&[&f.work]);
        assert_eq!(by_author(&result), shortlog(&f.work, &["HEAD", "@{u}"]));
        assert_eq!(totals(&result), git_totals(&f.work, &["HEAD", "@{u}"]));
        assert_eq!(totals(&result), (5, 32, 0), "first, upstream 0, and the three once");
    }

    /// A commit amended after it was pushed, and a branch rebased after it was
    /// pushed, exist twice until the force-push: once on HEAD and once on the
    /// upstream, which are the two tips walked.
    #[test]
    fn work_rewritten_after_it_was_pushed_is_counted_once() {
        let _guard = isolate();
        let f = fleet("stats-amend");
        commit_file(&f.work, "feature.txt", &lines(20), "Add feature");
        git(&f.work, &["push", "-q", "origin", "main"]);
        write(&f.work.join("feature.txt"), &lines(21));
        git(&f.work, &["commit", "-q", "-a", "--amend", "-m", "Add the feature, tested"]);
        let result = contributions(&[&f.work]);
        assert_eq!(by_author(&result), shortlog(&f.work, &["HEAD"]));
        assert_eq!(totals(&result), (2, 22, 0), "the amended commit, and not the one it replaced");

        let f = fleet("stats-rebase-own");
        git(&f.work, &["checkout", "-q", "-b", "topic"]);
        commit_file(&f.work, "a.txt", &lines(5), "topic a");
        commit_file(&f.work, "b.txt", &lines(5), "topic b");
        git(&f.work, &["push", "-q", "-u", "origin", "topic"]);
        f.remote_moves(1);
        git(&f.work, &["fetch", "--quiet"]);
        git(&f.work, &["rebase", "-q", "origin/main"]);
        let result = contributions(&[&f.work]);
        assert_eq!(by_author(&result), shortlog(&f.work, &["HEAD"]));
        assert_eq!(totals(&result), (4, 12, 0), "first, upstream 0, and the two once");
    }

    /// The other way to have diverged: somebody pushed while there was work
    /// here that had not been. Neither side is a copy of the other.
    #[test]
    fn work_on_both_sides_of_a_divergence_is_all_counted() {
        let _guard = isolate();
        let f = fleet("stats-diverged");
        f.remote_commits("theirs", |seed| write(&seed.join("theirs.txt"), &lines(7)));
        commit_as(&f.work, BOB, "mine.txt", &lines(3), "mine");
        git(&f.work, &["fetch", "--quiet"]);

        let result = contributions(&[&f.work]);
        assert_eq!(by_author(&result), shortlog(&f.work, &["HEAD", "@{u}"]));
        assert_eq!(totals(&result), git_totals(&f.work, &["HEAD", "@{u}"]));
        assert_eq!(totals(&result), (3, 11, 0));
    }

    /// The seed loop only fell back to HEAD when there was no branch at all, so
    /// a commit made on a detached HEAD was on no walk.
    #[test]
    fn a_detached_head_is_walked_and_so_is_what_was_fetched_and_not_merged() {
        let _guard = isolate();
        let f = fleet("stats-detached");
        git(&f.work, &["checkout", "-q", "--detach"]);
        commit_file(&f.work, "d.txt", &lines(4), "on no branch");
        assert_eq!(totals(&contributions(&[&f.work])), (2, 5, 0));

        let f = fleet("stats-behind");
        f.remote_moves(2);
        git(&f.work, &["fetch", "--quiet"]);
        assert_eq!(totals(&contributions(&[&f.work])), (3, 3, 0), "main is two behind");
    }
}
