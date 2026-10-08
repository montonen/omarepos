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
use std::path::{Component, Path, PathBuf};

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
    /// What to call the repository: see [`labels`].
    pub name: String,
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
    /// Repository -> what they did there. Keyed by path: keyed by folder name,
    /// two repositories called `api` under two roots were one group, with one
    /// total, one list of both sets of commits and one cap of a hundred
    /// between them.
    pub repos: BTreeMap<PathBuf, RepoCommits>,
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
        // Every commit counted so far, in any repository. A linked worktree, a
        // second clone and a fork beside its upstream are each a card of their
        // own and hold the same commits; counted once per path, one repository
        // with a worktree and a spare clone tripled everything on the page.
        let mut counted: HashSet<Oid> = HashSet::new();

        for (n, (path, name)) in repos.iter().zip(labels(&repos)).enumerate() {
            match collect(path, &name, &window, &mut acc, &mut per_day_total, &mut counted) {
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

/// What to call each repository on the page: its folder name, with as many of
/// the folders above it in front as it takes to tell it from the others being
/// read. `api` is `api` until there are two of them, and then they are
/// `personal/api` and `client/api`.
fn labels(paths: &[PathBuf]) -> Vec<String> {
    let parts: Vec<Vec<String>> = paths
        .iter()
        .map(|path| {
            path.components()
                .filter_map(|part| match part {
                    Component::Normal(name) => Some(name.to_string_lossy().into_owned()),
                    _ => None,
                })
                .collect()
        })
        .collect();
    let mut depth = vec![1usize; paths.len()];
    loop {
        let names: Vec<String> = (0..paths.len())
            .map(|i| match parts[i].len().checked_sub(depth[i]) {
                Some(from) => parts[i][from..].join("/"),
                // `/` and the like: no folder to name it by.
                None => paths[i].display().to_string(),
            })
            .collect();
        // The same path listed twice is one repository, not two with one name.
        let mut owner: HashMap<&str, &Path> = HashMap::new();
        let mut shared: HashSet<&str> = HashSet::new();
        for (name, path) in names.iter().zip(paths) {
            if *owner.entry(name).or_insert(path) != path.as_path() {
                shared.insert(name);
            }
        }
        let mut grew = false;
        for (i, name) in names.iter().enumerate() {
            if shared.contains(name.as_str()) && depth[i] < parts[i].len() {
                depth[i] += 1;
                grew = true;
            }
        }
        if !grew {
            return names;
        }
    }
}

struct Found {
    commits: usize,
    insertions: usize,
    deletions: usize,
}

fn collect(
    path: &Path,
    repo_name: &str,
    window: &Window,
    acc: &mut HashMap<String, Author>,
    per_day_total: &mut [usize],
    counted: &mut HashSet<Oid>,
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
        if !counted.insert(oid) {
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

        let in_repo = entry.repos.entry(path.to_path_buf()).or_insert_with(|| RepoCommits {
            name: repo_name.to_string(),
            ..RepoCommits::default()
        });
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
    let Ok(mut diff) = repo.diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), None) else {
        return (0, 0, 0);
    };
    if find_renames(&mut diff).is_err() {
        return (0, 0, 0);
    }
    match diff.stats() {
        Ok(stats) => (stats.insertions(), stats.deletions(), stats.files_changed()),
        Err(_) => (0, 0, 0),
    }
}

/// Past this many pairs of a removed file and an added one, only files moved
/// without an edit are paired. Comparing contents is a couple of microseconds a
/// pair, and every pair is compared: half a second for one commit at this size.
const RENAME_PAIRS: usize = 250_000;

/// Pair removed files with added ones, so that a move reads as a move. Without
/// this a renamed file was its whole length removed and its whole length added
/// again: one `git mv` of a directory put whoever ran it at the top of the
/// page, and the commit pane, which does pair them, said +0 −0 of the same
/// commit.
///
/// These are the renames `git log --shortstat` finds. The commit pane also
/// looks for copies, which is the one place the two still differ: a file added
/// as a copy of one the same commit edits is all new lines here and in git.
fn find_renames(diff: &mut git2::Diff<'_>) -> Result<(), git2::Error> {
    let (mut added, mut removed) = (0usize, 0usize);
    for delta in diff.deltas() {
        match delta.status() {
            git2::Delta::Added => added += 1,
            git2::Delta::Deleted => removed += 1,
            _ => {}
        }
    }
    if added == 0 || removed == 0 {
        return Ok(());
    }
    let mut finds = git2::DiffFindOptions::new();
    // libgit2 stops looking for an added file's source after a thousand
    // candidates unless told how many there are, and a move of more files than
    // that came out partly a move and partly a delete and an add.
    finds.renames(true).rename_limit(added.max(removed));
    if added.saturating_mul(removed) > RENAME_PAIRS {
        finds.exact_match_only(true);
    }
    diff.find_similar(Some(&mut finds))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::*;

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

    /// One of the commits listed under an author, found by its subject.
    fn listed<'a>(result: &'a Contributions, summary: &str) -> &'a Commit {
        result
            .authors
            .iter()
            .flat_map(|author| author.repos.values())
            .flat_map(|in_repo| in_repo.commits.iter())
            .find(|commit| commit.summary == summary)
            .unwrap_or_else(|| panic!("no commit called {summary:?} is listed"))
    }

    /// `n` lines that no other file has.
    fn text(name: &str, n: usize) -> String {
        (0..n).map(|i| format!("{name} line {i}\n")).collect()
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

    // ---- more than one repository --------------------------------------------

    /// A linked worktree and a second clone hold the same commits as the
    /// repository they came from, and each is a card of its own on the grid.
    #[test]
    fn a_worktree_and_a_second_clone_do_not_multiply_the_totals() {
        let _guard = isolate();
        let f = fleet("stats-copies");
        commit_file(&f.work, "a.txt", &lines(30), "in the main checkout");
        let linked = f.root.join("linked");
        git(&f.work, &["worktree", "add", "-q", "-b", "side", linked.to_str().unwrap()]);
        // What only the worktree has is still counted: it is checked out there.
        commit_file(&linked, "b.txt", &lines(5), "in the worktree");
        git(&f.root, &["clone", "-q", f.work.to_str().unwrap(), "copy"]);
        let copy = f.root.join("copy");

        let result = contributions(&[&f.work, &linked, &copy]);
        assert_eq!(totals(&result), (3, 36, 0));
        assert_eq!(by_author(&result).get("fixture@example.invalid"), Some(&3));
        assert_eq!(result.per_day_total.iter().sum::<usize>(), 3);
        assert_eq!(
            (result.repos_active, result.repos_scanned),
            (2, 3),
            "the copy holds nothing the other two did not"
        );
    }

    #[test]
    fn two_repositories_with_one_folder_name_stay_two() {
        let _guard = isolate();
        let root = scratch("stats-namesakes");
        let (personal, client) = (root.join("personal/api"), root.join("client/api"));
        for dir in [&personal, &client] {
            std::fs::create_dir_all(dir).unwrap();
            git(dir, &["init", "-q", "-b", "main"]);
        }
        commit_file(&personal, "a.txt", "a\n", "personal one");
        commit_file(&personal, "b.txt", "b\n", "personal two");
        commit_file(&client, "c.txt", "c\n", "client one");

        let result = contributions(&[&personal, &client]);
        let author = &result.authors[0];
        assert_eq!(author.commits, 3);
        assert_eq!(author.repos.len(), 2, "one group for each repository");
        let groups: Vec<(&str, usize)> = [&personal, &client]
            .map(|path| (author.repos[path].name.as_str(), author.repos[path].total))
            .into();
        assert_eq!(groups, [("personal/api", 2), ("client/api", 1)]);
    }

    #[test]
    fn a_repository_is_called_by_as_much_of_its_path_as_tells_it_apart() {
        let named = |paths: &[&str]| labels(&paths.iter().map(PathBuf::from).collect::<Vec<_>>());
        assert_eq!(
            named(&["/home/u/code/api", "/home/u/work/api", "/home/u/work/web"]),
            ["code/api", "work/api", "web"]
        );
        assert_eq!(
            named(&["/srv/a/team/api", "/srv/b/team/api", "/srv/b/docs"]),
            ["a/team/api", "b/team/api", "docs"],
            "one folder up is not always enough"
        );
        assert_eq!(named(&["/x/api", "/x/api"]), ["api", "api"], "the same one twice");
        assert_eq!(named(&["/"]), ["/"]);
    }

    // ---- what a commit is counted as -----------------------------------------

    #[test]
    fn a_move_is_not_every_line_removed_and_added_again() {
        let _guard = isolate();
        let dir = scratch("stats-rename");
        git(&dir, &["init", "-q", "-b", "main"]);
        write(&dir.join("src/big.txt"), &text("big", 100));
        for i in 0..5 {
            write(&dir.join(format!("src/f{i}.txt")), &text(&format!("f{i}"), 200));
        }
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-q", "-m", "base"]);
        git(&dir, &["mv", "src/big.txt", "src/renamed.txt"]);
        git(&dir, &["commit", "-q", "-m", "rename one file"]);
        git(&dir, &["mv", "src", "lib"]);
        git(&dir, &["commit", "-q", "-m", "move the directory"]);
        git(&dir, &["mv", "lib/f0.txt", "lib/zero.txt"]);
        write(&dir.join("lib/zero.txt"), &text("f0", 200).replace("f0 line 7\n", "seven\n"));
        git(&dir, &["commit", "-q", "-a", "-m", "rename and touch one line"]);
        // A copy is new lines to git unless it is asked to look for copies,
        // and the page is held to what git says unasked.
        write(&dir.join("lib/f1-copy.txt"), &text("f1", 200));
        write(&dir.join("lib/f1.txt"), &text("f1", 200).replace("f1 line 7\n", "seven\n"));
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-q", "-m", "copy a file and touch the original"]);

        let result = contributions(&[&dir]);
        let churn = |summary: &str| {
            let commit = listed(&result, summary);
            (commit.insertions, commit.deletions)
        };
        assert_eq!(churn("rename one file"), (0, 0));
        assert_eq!(churn("move the directory"), (0, 0));
        assert_eq!(churn("rename and touch one line"), (1, 1));
        assert_eq!(churn("copy a file and touch the original"), (201, 1));
        assert_eq!(totals(&result), git_totals(&dir, &["HEAD"]));
        assert_eq!(totals(&result), (5, 1302, 2));
    }

    /// libgit2 compares each added file with at most a thousand removed ones
    /// unless told otherwise, so past a thousand files a move was partly a
    /// move and partly a delete and an add.
    #[test]
    fn a_move_of_more_than_a_thousand_files_is_still_a_move() {
        let _guard = isolate();
        let dir = scratch("stats-rename-many");
        git(&dir, &["init", "-q", "-b", "main"]);
        for i in 0..1100 {
            write(&dir.join(format!("old/f{i}.txt")), &text(&format!("f{i}"), 2));
        }
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-q", "-m", "base"]);
        git(&dir, &["mv", "old", "new"]);
        git(&dir, &["commit", "-q", "-m", "move the lot"]);

        let result = contributions(&[&dir]);
        let moved = listed(&result, "move the lot");
        assert_eq!((moved.insertions, moved.deletions), (0, 0));
        assert_eq!(totals(&result), git_totals(&dir, &["HEAD"]));
    }
}
