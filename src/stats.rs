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
//! A commit belongs to the day it was written, by its author's clock, because
//! it is its author who is credited with it. The committer's clock says when
//! this copy of it was made, which after a rebase is another day and may be
//! another person's doing.
//!
//! Authors are keyed by e-mail, so the same person with two spellings of their
//! name still lands in one row. The same person with two addresses is two rows
//! unless a mailmap says otherwise, and the one git reads is read here. Merge
//! commits count as commits but are left out of the line totals — their diffs
//! are their parents' work, counted already.

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

    /// Where the last day ends, and an hour over. The hour is for the one day
    /// in the year that is twenty-five long, and on the others it lets a
    /// commit from a clock that runs a little fast be today's.
    ///
    /// The window had no end. Anything dated after the last midnight was put
    /// in the last day, so one commit from a machine with the wrong year was
    /// in today's bar on every day until that year came round.
    fn until(&self) -> i64 {
        self.day_starts.last().map_or(0, |start| start + 25 * 3600)
    }

    fn index_of(&self, when: i64) -> Option<usize> {
        if when < self.since() || when >= self.until() {
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
    /// When it was written, by its author's clock. This is the one that says
    /// which day a commit belongs to and how old it is.
    pub when: i64,
    /// When this copy of it was made, by its committer's clock: later than
    /// `when` after a rebase, an amend or a cherry-pick.
    pub committed: i64,
    /// Lines added and removed, or nothing where they could not be read: see
    /// [`diff_stats`]. A merge has none of its own and says `(0, 0)`.
    pub lines: Option<(usize, usize)>,
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
    /// How many of `commits` have no line counts. The three figures above
    /// leave those commits out, so with any of these they are a lower bound.
    pub uncounted: usize,
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
    /// Commits counted whose lines could not be read, and so are in neither
    /// total above.
    pub uncounted: usize,
    /// Commits dated after the window ends, by a clock that was wrong. They
    /// are in no figure and no day.
    pub future: usize,
    /// One line for each repository that could not be read: what it is called
    /// and why. None of its commits are counted.
    pub errors: Vec<String>,
}

pub enum Event {
    /// A repository has been read; carries its name and how many repos remain.
    Progress(String, usize, usize),
    Done(Box<Contributions>),
}

pub fn spawn(repos: Vec<PathBuf>, window: Window) -> async_channel::Receiver<Event> {
    let (tx, rx) = async_channel::unbounded();

    std::thread::spawn(move || {
        let total = repos.len();
        let mut tally = Tally::new(window.days());
        let mut errors = Vec::new();

        for (n, (path, name)) in repos.iter().zip(labels(&repos)).enumerate() {
            // A repository is read to the end before any of it is counted, so
            // one that fails part of the way through is left out whole and
            // named, not counted as far as it got.
            match collect(path, &window, &tally.counted) {
                Ok(found) => tally.add(path, &name, found),
                Err(err) => errors.push(format!("{name}: {err}")),
            }

            if tx
                .send_blocking(Event::Progress(name, n + 1, total))
                .is_err()
            {
                return;
            }
        }

        let _ = tx.send_blocking(Event::Done(Box::new(tally.finish(window, total, errors))));
    });

    rx
}

/// Everything counted so far in one run.
struct Tally {
    days: usize,
    /// Each author, and beside them every spelling of their name that has
    /// been met with the number of commits that carry it.
    authors: HashMap<String, (Author, Vec<(String, usize)>)>,
    per_day_total: Vec<usize>,
    /// Every commit counted or set aside, in any repository. A linked
    /// worktree, a second clone and a fork beside its upstream are each a card
    /// of their own and hold the same commits; counted once per path, one
    /// repository with a worktree and a spare clone tripled everything on the
    /// page.
    counted: HashSet<Oid>,
    repos_active: usize,
    total_commits: usize,
    total_insertions: usize,
    total_deletions: usize,
    uncounted: usize,
    future: usize,
}

impl Tally {
    fn new(days: usize) -> Tally {
        Tally {
            days,
            authors: HashMap::new(),
            per_day_total: vec![0; days],
            counted: HashSet::new(),
            repos_active: 0,
            total_commits: 0,
            total_insertions: 0,
            total_deletions: 0,
            uncounted: 0,
            future: 0,
        }
    }

    /// Count what [`collect`] found in the repository at `path`.
    fn add(&mut self, path: &Path, repo_name: &str, found: Found) {
        if !found.commits.is_empty() {
            self.repos_active += 1;
        }
        self.future += found.future.len();
        self.counted.extend(found.future);
        for commit in found.commits {
            self.counted.insert(commit.oid);

            let key = if commit.email.is_empty() {
                commit.name.to_lowercase()
            } else {
                commit.email.clone()
            };
            let (entry, spellings) = self.authors.entry(key).or_insert_with(|| {
                let author = Author {
                    name: commit.name.clone(),
                    email: commit.email.clone(),
                    commits: 0,
                    merges: 0,
                    insertions: 0,
                    deletions: 0,
                    files: 0,
                    uncounted: 0,
                    per_day: vec![0; self.days],
                    repos: BTreeMap::new(),
                    last_seen: 0,
                };
                (author, Vec::new())
            });
            match spellings.iter_mut().find(|(name, _)| *name == commit.name) {
                Some((_, times)) => *times += 1,
                None => spellings.push((commit.name.clone(), 1)),
            }
            entry.commits += 1;
            if commit.merge {
                entry.merges += 1;
            }
            match commit.lines {
                Some((insertions, deletions, files)) => {
                    entry.insertions += insertions;
                    entry.deletions += deletions;
                    entry.files += files;
                    self.total_insertions += insertions;
                    self.total_deletions += deletions;
                }
                None => {
                    entry.uncounted += 1;
                    self.uncounted += 1;
                }
            }
            entry.per_day[commit.day] += 1;
            entry.last_seen = entry.last_seen.max(commit.when);

            let in_repo = entry.repos.entry(path.to_path_buf()).or_insert_with(|| RepoCommits {
                name: repo_name.to_string(),
                ..RepoCommits::default()
            });
            in_repo.total += 1;
            if in_repo.commits.len() < MAX_COMMITS_LISTED {
                in_repo.commits.push(Commit {
                    short_id: commit.oid.to_string().chars().take(7).collect(),
                    summary: commit.summary,
                    when: commit.when,
                    committed: commit.committed,
                    lines: commit.lines.map(|(insertions, deletions, _)| (insertions, deletions)),
                    merge: commit.merge,
                });
            }

            self.per_day_total[commit.day] += 1;
            self.total_commits += 1;
        }
    }

    fn finish(self, window: Window, repos_scanned: usize, errors: Vec<String>) -> Contributions {
        // One address, several spellings of the name: the row is titled with
        // the one most of the commits carry. It used to be whichever the walk
        // met first, which changed with the order the repositories were read
        // in and could be a login or a typo from one afternoon.
        let mut authors: Vec<Author> = self
            .authors
            .into_values()
            .map(|(mut author, spellings)| {
                let mut most = 0;
                for (name, times) in spellings {
                    if times > most {
                        most = times;
                        author.name = name;
                    }
                }
                author
            })
            .collect();
        authors.sort_by(|a, b| {
            b.commits
                .cmp(&a.commits)
                .then_with(|| b.churn().cmp(&a.churn()))
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        Contributions {
            window,
            authors,
            per_day_total: self.per_day_total,
            repos_scanned,
            repos_active: self.repos_active,
            total_commits: self.total_commits,
            total_insertions: self.total_insertions,
            total_deletions: self.total_deletions,
            uncounted: self.uncounted,
            future: self.future,
            errors,
        }
    }
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

/// One commit inside the window, read and not yet counted.
struct Seen {
    oid: Oid,
    name: String,
    email: String,
    summary: String,
    when: i64,
    committed: i64,
    day: usize,
    merge: bool,
    /// Insertions, deletions and files changed, or nothing where they could
    /// not be read.
    lines: Option<(usize, usize, usize)>,
}

/// What one repository holds that is not in `counted` already.
#[derive(Default)]
struct Found {
    /// Written inside the window, newest commit first.
    commits: Vec<Seen>,
    /// Dated after the window ends.
    future: Vec<Oid>,
}

/// Read one repository to the end, counting nothing. The error is what libgit2
/// said, for the page to show beside the repository's name.
fn collect(path: &Path, window: &Window, counted: &HashSet<Oid>) -> Result<Found, String> {
    let said = |e: git2::Error| e.message().to_string();
    let repo = Repository::open(path).map_err(said)?;
    let mut walk = repo.revwalk().map_err(said)?;
    walk.set_sorting(Sort::TIME).map_err(said)?;

    // HEAD and its upstream, and nothing else: see the top of the file. HEAD
    // used to be walked only when no branch could be, so a commit made on a
    // detached HEAD was counted nowhere.
    let mut found = Found::default();
    let Some(head) = head_commit(&repo).map_err(said)? else {
        return Ok(found);
    };
    // A window of no days holds nothing, and has no end for a commit to be
    // dated after.
    if window.days() == 0 {
        return Ok(found);
    }
    walk.push(head).map_err(said)?;
    let mut rewritten = HashSet::new();
    if let Some(upstream) = upstream_tip(&repo).map_err(said)? {
        walk.push(upstream).map_err(said)?;
        rewritten = rewritten_upstream(&repo, head, upstream, window.since()).map_err(said)?;
    }

    // Who is who, as `git shortlog` would have it: the `.mailmap` in the work
    // tree, then `mailmap.blob` and `mailmap.file` from the configuration. The
    // last is the one that covers every repository from one file. Without any
    // of it the author was whatever the commit said, and somebody who merges
    // pull requests on a forge is two people: the address on their machine
    // and the one the forge commits under.
    let mailmap = repo.mailmap().ok();

    for oid in walk {
        // A step that failed used to be skipped. A walk in time order reads
        // all of the history before it yields any of it, so one object missing
        // anywhere fails the first step and ends the walk: the repository came
        // out as having no commits at all, and the page said that nothing had
        // been committed.
        let oid = oid.map_err(said)?;
        let commit = repo.find_commit(oid).map_err(said)?;

        // The walk hands commits over in the order they were committed,
        // newest first and whatever branch they are on, so the first one
        // committed before the window is the end of it. A commit is committed
        // no earlier than it is written; only a clock that jumped backwards
        // between the two could hide one behind this.
        let committed = commit.time().seconds();
        if committed < window.since() {
            break;
        }
        if rewritten.contains(&oid) || counted.contains(&oid) {
            continue;
        }

        let as_written = commit.author();
        let mapped = mailmap.as_ref().and_then(|map| commit.author_with_mailmap(map).ok());
        let author = mapped.as_ref().unwrap_or(&as_written);

        // The author gets the credit, so the author's clock says when. The
        // window used to be tested on the committer's: rebase or cherry-pick a
        // branch written a month ago and its author was shown writing all of
        // it today, in today's bar, "just now". The same went for one's own
        // work after a `pull --rebase`.
        let when = author.when().seconds();
        if when >= window.until() {
            found.future.push(oid);
            continue;
        }
        let Some(day) = window.index_of(when) else { continue };

        let merge = commit.parent_count() > 1;
        found.commits.push(Seen {
            oid,
            name: author.name().unwrap_or("unknown").to_string(),
            email: author.email().unwrap_or("").trim().to_lowercase(),
            summary: commit
                .summary()
                .ok()
                .flatten()
                .unwrap_or("(no message)")
                .trim()
                .to_string(),
            when,
            committed,
            day,
            merge,
            lines: if merge { Some((0, 0, 0)) } else { diff_stats(&repo, &commit) },
        });
    }

    // Newest first by the clock the page goes by: only the first hundred of
    // an author's commits are listed, and the rest are called older.
    found.commits.sort_by(|a, b| b.when.cmp(&a.when));
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

/// What a commit keeps when it is made again.
struct Mark {
    author: Vec<u8>,
    written: i64,
    subject: Vec<u8>,
    parent: Option<Oid>,
}

impl Mark {
    fn of(commit: &git2::Commit<'_>) -> Mark {
        let author = commit.author();
        Mark {
            author: author.email_bytes().to_vec(),
            written: author.when().seconds(),
            subject: commit.summary_bytes().unwrap_or_default().to_vec(),
            parent: commit.parent_id(0).ok(),
        }
    }

    /// Whether the two are one piece of work committed twice. Every way of
    /// making a commit again keeps who wrote it and the second they did. A
    /// rebase and a cherry-pick keep what it is called as well and change what
    /// it stands on; an amend keeps what it stands on and may change the rest.
    fn is_copy_of(&self, other: &Mark) -> bool {
        self.author == other.author
            && self.written == other.written
            && (self.subject == other.subject || self.parent == other.parent)
    }
}

/// The commits on the upstream's side of a divergence that the local branch
/// holds a newer copy of.
///
/// HEAD and its upstream are both walked, and most of the time one contains the
/// other. When neither does, there are two ways it came about. Somebody pushed
/// while there was unpushed work here: two sets of commits, all to be counted.
/// Or work was pushed and then amended or rebased here, and not yet pushed
/// again: one set of commits, twice, and of each pair the upstream's is the
/// one about to be overwritten.
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
    let ours = only_on(repo, head, upstream, since)?;
    Ok(only_on(repo, upstream, head, since)?
        .into_iter()
        .filter(|(_, theirs)| ours.iter().any(|(_, ours)| theirs.is_copy_of(ours)))
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
) -> Result<Vec<(Oid, Mark)>, git2::Error> {
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
        side.push((oid, Mark::of(&commit)));
    }
    Ok(side)
}

/// Insertions, deletions and files changed against the first parent, or
/// nothing when they cannot be read.
///
/// "Cannot be read" used to come out as a number. Any failure was +0 −0, which
/// is what a partial clone showed for every commit whose files it had not
/// fetched. And a parent that could not be read was taken for no parent, so
/// the commit was compared with nothing and credited with every line in the
/// repository: the commit at the edge of a shallow clone, whose parent is not
/// there, was the largest contribution on the page.
fn diff_stats(repo: &Repository, commit: &git2::Commit<'_>) -> Option<(usize, usize, usize)> {
    let tree = commit.tree().ok()?;
    let parent_tree = if commit.parent_count() == 0 {
        // No parent, or none that libgit2 will admit to: at the edge of a
        // shallow clone it reports none, and the commit itself still names
        // the one that was not fetched.
        if commit.header_field_bytes("parent").is_ok() {
            return None;
        }
        None
    } else {
        Some(commit.parent(0).ok()?.tree().ok()?)
    };

    // Left to itself libgit2 opens every directory of both trees and compares
    // them file by file, so a commit that touched one file cost as much as the
    // repository has files: milliseconds each on a large tree, for every
    // commit in the window. The files that differ are found first, by going
    // down only where two directories disagree, and libgit2 is given the list.
    // Its iterators then leave every other directory shut, and the diff it
    // makes of what is left is the same diff.
    let mut opts = git2::DiffOptions::new();
    if let Some(parent) = &parent_tree {
        let mut changed = Changed::default();
        changed.below(repo, Some(parent), Some(&tree), &mut Vec::new()).ok()?;
        if changed.paths.is_empty() {
            return Some((0, 0, 0));
        }
        if !changed.submodule {
            opts.disable_pathspec_match(true);
            for path in changed.paths {
                opts.pathspec(path);
            }
        }
    }
    let mut diff = repo
        .diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), Some(&mut opts))
        .ok()?;
    find_renames(&mut diff).ok()?;
    let stats = diff.stats().ok()?;
    Some((stats.insertions(), stats.deletions(), stats.files_changed()))
}

/// What differs between two trees, for [`diff_stats`] to point libgit2 at.
#[derive(Default)]
struct Changed {
    /// The full path of each file and link that differs. Never a directory's:
    /// libgit2 walks this list in step with the tree, and a directory's bare
    /// name is not where the tree's order puts the directory. `foo-bar/x`
    /// comes before `foo/y` in a tree, and `foo` sorts before both.
    paths: Vec<Vec<u8>>,
    /// A submodule is among them. libgit2 takes the list as it stands for
    /// files and links, and reads each entry as a pattern for anything else:
    /// a submodule at `!vendor` would be a negation, and missed. A commit that
    /// moves one is rare enough to be diffed the long way.
    submodule: bool,
}

impl Changed {
    /// Compare two directories, either of which may not be there, going down
    /// into the directories inside them only where the two differ.
    fn below(
        &mut self,
        repo: &Repository,
        old: Option<&git2::Tree<'_>>,
        new: Option<&git2::Tree<'_>>,
        prefix: &mut Vec<u8>,
    ) -> Result<(), git2::Error> {
        if let Some(new) = new {
            for entry in new.iter() {
                let before = old.and_then(|tree| tree.get_name_bytes(entry.name_bytes()));
                let same = before.as_ref().is_some_and(|before| {
                    before.id() == entry.id() && before.filemode_raw() == entry.filemode_raw()
                });
                if !same {
                    self.named(repo, before.as_ref(), Some(&entry), prefix)?;
                }
            }
        }
        if let Some(old) = old {
            for entry in old.iter() {
                if new.is_none_or(|tree| tree.get_name_bytes(entry.name_bytes()).is_none()) {
                    self.named(repo, Some(&entry), None, prefix)?;
                }
            }
        }
        Ok(())
    }

    /// One name that is not the same thing on both sides. Where either side is
    /// a file its path goes on the list, and where either is a directory it is
    /// gone into, against the other side's if that is a directory too and
    /// against nothing if not.
    fn named(
        &mut self,
        repo: &Repository,
        old: Option<&git2::TreeEntry<'_>>,
        new: Option<&git2::TreeEntry<'_>>,
        prefix: &mut Vec<u8>,
    ) -> Result<(), git2::Error> {
        let Some(name) = new.or(old).map(|entry| entry.name_bytes()) else { return Ok(()) };
        let length = prefix.len();
        prefix.extend_from_slice(name);

        let (mut old_dir, mut new_dir) = (None, None);
        for (entry, dir) in [(old, &mut old_dir), (new, &mut new_dir)] {
            match entry.map(|entry| (entry, entry.kind())) {
                Some((entry, Some(git2::ObjectType::Tree))) => *dir = Some(repo.find_tree(entry.id())?),
                Some((_, kind)) => {
                    self.submodule |= kind == Some(git2::ObjectType::Commit);
                    if self.paths.last() != Some(&*prefix) {
                        self.paths.push(prefix.clone());
                    }
                }
                None => {}
            }
        }
        if old_dir.is_some() || new_dir.is_some() {
            prefix.push(b'/');
            self.below(repo, old_dir.as_ref(), new_dir.as_ref(), prefix)?;
        }

        prefix.truncate(length);
        Ok(())
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

    /// One identity committing to two branches in the same second, which is a
    /// release job and not a person. The two commits share an author and a
    /// date and nothing else, and are two commits.
    #[test]
    fn the_same_second_on_both_sides_does_not_alone_make_a_copy() {
        let _guard = isolate();
        let f = fleet("stats-same-second");
        let at = now() - 3600;
        let commit_at = |repo: &Path, when: i64, file: &str, message: &str| {
            write(&repo.join(file), "x\n");
            git(repo, &["add", "."]);
            git_at(repo, when, &["commit", "-q", "-m", message]);
        };
        commit_at(&f.seed, at - 20, "a.txt", "theirs");
        commit_at(&f.seed, at, "version.txt", "bump to 1.2.3");
        git(&f.seed, &["push", "-q", "origin", "main"]);
        commit_at(&f.work, at - 10, "b.txt", "mine");
        commit_at(&f.work, at, "next.txt", "bump to 1.3.0-dev");
        git(&f.work, &["fetch", "--quiet"]);

        let result = contributions(&[&f.work]);
        assert_eq!(by_author(&result), shortlog(&f.work, &["HEAD", "@{u}"]));
        assert_eq!(totals(&result), (5, 5, 0));
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

    // ---- who a commit is credited to ------------------------------------------

    /// One person, three ways of signing: the work machine, the address the
    /// forge commits under when a pull request is merged or a file is edited
    /// in the browser, and a second machine set up in a hurry. The
    /// repository's `.mailmap` says the first two are one.
    #[test]
    fn a_mailmap_makes_one_author_of_one_person() {
        let _guard = isolate();
        let dir = scratch("stats-mailmap");
        git(&dir, &["init", "-q", "-b", "main"]);
        let carol = ("Carol Example", "carol@work.example");
        let forge = ("carol-ex", "1234567+carol-ex@users.noreply.forge.example");
        commit_as(&dir, carol, "a.txt", "a\n", "one");
        commit_as(&dir, carol, "b.txt", "b\n", "two");
        commit_as(&dir, forge, "c.txt", "c\n", "merged on the forge");
        commit_as(&dir, ("carol", "Carol@Work.Example"), "d.txt", "d\n", "from the other machine");
        commit_as(&dir, ("robot[bot]", "robot@bots.example"), "e.lock", &lines(50), "bump");
        let map = format!("{} <{}> <{}>\n", carol.0, carol.1, forge.1);
        commit_file(&dir, ".mailmap", &map, "say who is who");

        let result = contributions(&[&dir]);
        assert_eq!(by_author(&result), shortlog(&dir, &["HEAD"]));
        assert_eq!(by_author(&result).get(carol.1), Some(&4));
        assert_eq!(result.authors.len(), 3, "Carol, the robot, and whoever wrote the mailmap");
        let row = result.authors.iter().find(|a| a.email == carol.1).unwrap();
        assert_eq!(row.name, "Carol Example", "the spelling most of her commits carry");

        // The same from a file named in the configuration, which is how one
        // mailmap serves repositories that do not carry their own.
        let dir = scratch("stats-mailmap-file");
        git(&dir, &["init", "-q", "-b", "main"]);
        commit_as(&dir, carol, "a.txt", "a\n", "one");
        commit_as(&dir, forge, "b.txt", "b\n", "merged on the forge");
        let elsewhere = scratch("stats-mailmap-file-map").join("who");
        write(&elsewhere, &map);
        git(&dir, &["config", "mailmap.file", elsewhere.to_str().unwrap()]);
        let result = contributions(&[&dir]);
        assert_eq!(by_author(&result), shortlog(&dir, &["HEAD"]));
        assert_eq!(by_author(&result).get(carol.1), Some(&2));
    }

    // ---- when a commit is counted ---------------------------------------------

    /// Run git with both of a commit's clocks set.
    fn git_at(dir: &Path, when: i64, args: &[&str]) {
        let stamp = format!("@{when} +0000");
        let out = std::process::Command::new("git")
            .current_dir(dir)
            .args(args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_AUTHOR_DATE", &stamp)
            .env("GIT_COMMITTER_DATE", &stamp)
            .output()
            .expect("git runs");
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    }

    /// Somebody's old branch, rebased today by somebody else. Each commit is
    /// dated forty days ago by the author's clock and today by the
    /// committer's. The window was tested on the second and the credit given
    /// to the first, so the author was shown doing all of it today.
    #[test]
    fn work_is_counted_on_the_day_it_was_written_not_the_day_it_was_rebased() {
        let _guard = isolate();
        let f = fleet("stats-rebased-old");
        git(&f.work, &["checkout", "-q", "-b", "old"]);
        let (name, email) = (format!("user.name={}", BOB.0), format!("user.email={}", BOB.1));
        for i in 0..4 {
            write(&f.work.join(format!("o{i}.txt")), &lines(25));
            git(&f.work, &["add", "."]);
            let message = format!("old {i}");
            let commit = ["-c", &name, "-c", &email, "commit", "-q", "-m", &message];
            git_at(&f.work, now() - 40 * DAY + i, &commit);
        }
        assert_eq!(totals(&contributions(&[&f.work])), (1, 1, 0), "nothing of Bob's is recent");

        f.remote_moves(1);
        git(&f.work, &["fetch", "--quiet"]);
        git(&f.work, &["rebase", "-q", "origin/main"]);
        let result = contributions(&[&f.work]);
        assert_eq!(by_author(&result).get(BOB.1), None, "and a rebase does not make it so");
        assert_eq!(totals(&result), (2, 2, 0));
        // Where the page parts from `git shortlog --since`, and means to: that
        // tests the committer's clock and credits the author.
        assert_eq!(shortlog(&f.work, &["HEAD"]).get(BOB.1), Some(&4));
    }

    /// A commit from a machine whose clock is wrong sat in today's bar on
    /// every day until its date came round.
    #[test]
    fn a_commit_dated_after_today_is_set_aside() {
        let _guard = isolate();
        let dir = scratch("stats-future");
        git(&dir, &["init", "-q", "-b", "main"]);
        commit_file(&dir, "a.txt", "a\n", "today");
        for (file, ahead, message) in [
            ("b.txt", 1800, "a clock half an hour fast"),
            ("c.txt", 3 * DAY, "three days ahead"),
            ("d.txt", 5 * 365 * DAY, "years ahead"),
        ] {
            write(&dir.join(file), "x\n");
            git(&dir, &["add", "."]);
            git_at(&dir, now() + ahead, &["commit", "-q", "-m", message]);
        }

        let result = contributions(&[&dir]);
        assert_eq!(result.total_commits, 2, "today's, and the one that is nearly today's");
        assert_eq!(result.per_day_total.iter().sum::<usize>(), 2);
        assert_eq!(result.future, 2);

        // And once, however many checkouts have it.
        let copy = scratch("stats-future-copy");
        git(&copy, &["clone", "-q", dir.to_str().unwrap(), "."]);
        assert_eq!(contributions(&[&dir, &copy]).future, 2);
    }

    #[test]
    fn the_window_ends_with_its_last_day() {
        let window = Window {
            day_starts: vec![0, DAY, 2 * DAY],
            day_labels: Vec::new(),
            day_titles: Vec::new(),
        };
        assert_eq!(window.index_of(-1), None);
        assert_eq!(window.index_of(0), Some(0));
        assert_eq!(window.index_of(DAY - 1), Some(0));
        assert_eq!(window.index_of(2 * DAY), Some(2));
        assert_eq!(window.index_of(3 * DAY + 3599), Some(2), "a clock a little fast");
        assert_eq!(window.index_of(3 * DAY + 3600), None);
    }

    // ---- what could not be read ---------------------------------------------

    /// One commit object gone from the middle of the history. A walk in time
    /// order reads all of the history before it yields any of it, so it fails
    /// once and ends. That one failure was skipped, and the page said nothing
    /// had been committed in a repository with five commits made today.
    #[test]
    fn a_history_that_cannot_be_walked_is_unreadable_and_not_quiet() {
        let _guard = isolate();
        let dir = scratch("stats-hole");
        git(&dir, &["init", "-q", "-b", "main"]);
        for i in 0..5 {
            commit_file(&dir, &format!("f{i}.txt"), "x\n", &format!("commit {i}"));
        }
        let lost = git(&dir, &["rev-parse", "HEAD~3"]);
        std::fs::remove_file(dir.join(".git/objects").join(&lost[..2]).join(&lost[2..])).unwrap();

        let result = contributions(&[&dir]);
        assert_eq!(result.total_commits, 0);
        assert_eq!(result.errors.len(), 1, "{:?}", result.errors);
        assert!(result.errors[0].starts_with("stats-hole: "), "{:?}", result.errors);
    }

    #[test]
    fn what_is_not_a_repository_is_an_error_and_an_empty_one_is_not() {
        let _guard = isolate();
        let plain = scratch("stats-not-a-repository");
        let empty = scratch("stats-unborn");
        git(&empty, &["init", "-q", "-b", "main"]);
        let f = fleet("stats-beside-unreadable");

        let result = contributions(&[&plain, &empty, &f.work]);
        assert_eq!(result.errors.len(), 1, "{:?}", result.errors);
        assert!(result.errors[0].starts_with("stats-not-a-repository: "), "{:?}", result.errors);
        assert_eq!((result.repos_active, result.repos_scanned), (1, 3));
        assert_eq!(result.total_commits, 1);
    }

    /// The commit at the edge of a shallow clone has a parent that was not
    /// fetched. Compared with nothing, it was every line in the repository.
    #[test]
    fn the_edge_of_a_shallow_clone_is_not_credited_with_the_whole_tree() {
        let _guard = isolate();
        let f = fleet("stats-shallow");
        f.remote_commits("the codebase", |seed| write(&seed.join("big.txt"), &lines(2000)));
        f.remote_commits("a small change", |seed| write(&seed.join("small.txt"), &lines(2)));
        f.remote_commits("another", |seed| write(&seed.join("other.txt"), &lines(3)));
        let url = format!("file://{}", f.remote.display());
        git(&f.root, &["clone", "-q", "--depth", "2", &url, "shallow"]);

        let result = contributions(&[&f.root.join("shallow")]);
        assert_eq!(result.errors, Vec::<String>::new());
        assert_eq!(listed(&result, "another").lines, Some((3, 0)));
        assert_eq!(listed(&result, "a small change").lines, None, "its parent is not here");
        assert_eq!((totals(&result), result.uncounted), ((2, 3, 0), 1));
        assert_eq!(result.authors[0].uncounted, 1);

        // The first commit of a history has no parent to be missing, and is
        // every line it adds.
        let result = contributions(&[&f.seed]);
        assert_eq!(listed(&result, "first").lines, Some((1, 0)));
        assert_eq!((totals(&result), result.uncounted), ((4, 2006, 0), 0));
    }

    /// A blobless clone has every commit, and of the files only what the
    /// checkout needed. Every diff it could not take was +0 −0.
    #[test]
    fn a_partial_clone_says_which_lines_it_could_not_read() {
        let _guard = isolate();
        let f = fleet("stats-partial");
        f.remote_commits("add", |seed| write(&seed.join("a.txt"), &lines(10)));
        f.remote_commits("grow", |seed| write(&seed.join("a.txt"), &lines(20)));
        f.remote_commits("grow again", |seed| write(&seed.join("a.txt"), &lines(30)));
        git(&f.remote, &["config", "uploadpack.allowFilter", "true"]);
        let url = format!("file://{}", f.remote.display());
        git(&f.root, &["clone", "-q", "--filter=blob:none", &url, "partial"]);

        let result = contributions(&[&f.root.join("partial")]);
        assert_eq!(result.errors, Vec::<String>::new());
        assert_eq!(listed(&result, "first").lines, Some((1, 0)), "README.md is checked out");
        for unread in ["add", "grow", "grow again"] {
            assert_eq!(listed(&result, unread).lines, None, "{unread}");
        }
        assert_eq!((totals(&result), result.uncounted), ((4, 1, 0), 3));
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
        let churn = |summary: &str| listed(&result, summary).lines;
        assert_eq!(churn("rename one file"), Some((0, 0)));
        assert_eq!(churn("move the directory"), Some((0, 0)));
        assert_eq!(churn("rename and touch one line"), Some((1, 1)));
        assert_eq!(churn("copy a file and touch the original"), Some((201, 1)));
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
        assert_eq!(listed(&result, "move the lot").lines, Some((0, 0)));
        assert_eq!(totals(&result), git_totals(&dir, &["HEAD"]));
    }

    // ---- what libgit2 is asked to compare -------------------------------------

    /// What `diff_stats` did before it told libgit2 where to look: the same
    /// diff over the whole of both trees.
    fn whole_tree_stats(repo: &Repository, commit: &git2::Commit<'_>) -> (usize, usize, usize) {
        let tree = commit.tree().unwrap();
        let parent = commit.parent(0).ok().map(|parent| parent.tree().unwrap());
        let mut diff = repo.diff_tree_to_tree(parent.as_ref(), Some(&tree), None).unwrap();
        find_renames(&mut diff).unwrap();
        let stats = diff.stats().unwrap();
        (stats.insertions(), stats.deletions(), stats.files_changed())
    }

    /// Hold every commit reachable from HEAD to the whole-tree diff, and say
    /// how many file changes that was.
    fn same_as_the_whole_tree(dir: &Path) -> (usize, usize) {
        let repo = Repository::open(dir).unwrap();
        let mut walk = repo.revwalk().unwrap();
        walk.push_head().unwrap();
        let (mut commits, mut files) = (0, 0);
        for oid in walk {
            let commit = repo.find_commit(oid.unwrap()).unwrap();
            let whole = whole_tree_stats(&repo, &commit);
            assert_eq!(diff_stats(&repo, &commit), Some(whole), "{:?}", commit.summary());
            commits += 1;
            files += whole.2;
        }
        (commits, files)
    }

    /// Every shape a change to a tree can take, under names picked to sort
    /// awkwardly beside each other and to mean something to a pattern. The
    /// list of paths handed to libgit2 comes from a walk written here, and a
    /// path missing from it is a change that silently is not counted.
    #[test]
    fn telling_libgit2_where_to_look_does_not_change_what_it_finds() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let _guard = isolate();
        let dir = scratch("stats-paths");
        git(&dir, &["init", "-q", "-b", "main"]);
        let put = |file: &str| write(&dir.join(file), &text(file, 12));
        let commit = |message: &str| {
            git(&dir, &["add", "-A", "."]);
            git(&dir, &["commit", "-q", "--allow-empty", "-m", message]);
        };
        let awkward = ["!bang", "sp ace", "back\\slash", "ünï.txt", "[b]", "st*r", "#hash", "tab\there"];

        for file in ["foo", "foo-bar/x", "foo-bar/y", "foo.txt", "foo+", "foo0", "dir/a"] {
            put(file);
        }
        for file in ["dir/sub/b", "dir/sub/c", "z/deep/er/one", "z/deep/er/two", "z/top"] {
            put(file);
        }
        awkward.iter().for_each(|file| put(file));
        commit("base");

        write(&dir.join("dir/sub/b"), &text("b again", 14));
        commit("edit a file two folders down");

        put("dir/sub/new");
        std::fs::remove_file(dir.join("dir/a")).unwrap();
        commit("add one and remove one");

        std::fs::remove_file(dir.join("foo")).unwrap();
        put("foo/inner");
        put("foo/other");
        commit("a file becomes a folder");

        std::fs::remove_dir_all(dir.join("foo-bar")).unwrap();
        put("foo-bar");
        commit("a folder becomes a file");

        std::fs::set_permissions(dir.join("foo.txt"), std::fs::Permissions::from_mode(0o755)).unwrap();
        commit("only the mode");

        symlink("foo.txt", dir.join("link")).unwrap();
        commit("a link");
        std::fs::remove_file(dir.join("link")).unwrap();
        symlink("foo+", dir.join("link")).unwrap();
        commit("the link points elsewhere");
        std::fs::remove_file(dir.join("foo0")).unwrap();
        symlink("foo.txt", dir.join("foo0")).unwrap();
        commit("a file becomes a link");

        git(&dir, &["mv", "dir", "moved"]);
        commit("move a folder");
        git(&dir, &["mv", "moved/sub/c", "moved/sub/sea"]);
        write(&dir.join("moved/sub/sea"), &text("dir/sub/c", 12).replace("line 5\n", "line five\n"));
        commit("rename a file and touch it");

        std::fs::remove_dir_all(dir.join("z/deep")).unwrap();
        commit("remove a tree and leave its neighbour");
        put("n/e/w/t/r/e/e/leaf");
        commit("a tree seven folders deep");
        commit("nothing at all");

        for file in awkward {
            write(&dir.join(file), &text(file, 13));
        }
        write(&dir.join("foo/inner"), &text("inner again", 5));
        write(&dir.join("foo-bar"), &text("bar again", 5));
        write(&dir.join("foo.txt"), &text("txt again", 5));
        commit("the awkward names, all at once");

        // A submodule is a line in a tree that names a commit somewhere else.
        // Nothing has to be checked out for it to be there.
        let pin = |path: &str, at: &str, message: &str| {
            let id = git(&dir, &["rev-parse", at]);
            git(&dir, &["update-index", "--add", "--cacheinfo", &format!("160000,{id},{path}")]);
            git(&dir, &["commit", "-q", "-m", message]);
        };
        pin("sub", "HEAD", "a submodule");
        pin("!sub", "HEAD~1", "a submodule a pattern would read as a negation");
        pin("sub", "HEAD~3", "the submodule moves");
        git(&dir, &["rm", "-q", "--cached", "sub", "!sub"]);
        git(&dir, &["commit", "-q", "-m", "the submodules go"]);

        assert_eq!(same_as_the_whole_tree(&dir).0, 19);
        assert_eq!(totals(&contributions(&[&dir])), git_totals(&dir, &["HEAD"]));
    }

    /// The same over a history nobody chose: two hundred commits of files
    /// written, removed and turned into folders and back, a few names deep,
    /// from eight names that are each other's prefixes. The sequence is fixed,
    /// so what fails here fails every time.
    #[test]
    fn nor_over_a_history_of_changes_picked_at_random() {
        use std::os::unix::fs::PermissionsExt;

        let _guard = isolate();
        let dir = scratch("stats-paths-random");
        git(&dir, &["init", "-q", "-b", "main"]);
        let names = ["a", "a-b", "a.b", "a0", "a+", "ab", "a b", "b"];
        let mut state = 0x2545_f491_4f6c_dd1d_u64;
        let mut below = |n: usize| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state % n as u64) as usize
        };

        for round in 0..200 {
            for _ in 0..1 + below(4) {
                let path: PathBuf = (0..1 + below(3)).map(|_| names[below(names.len())]).collect();
                let full = dir.join(&path);
                // Whatever is in the way goes: a file where a folder has to
                // be, a folder where the file is going.
                for above in full.ancestors().skip(1).filter(|above| above.starts_with(&dir)) {
                    if above.is_file() {
                        std::fs::remove_file(above).unwrap();
                    }
                }
                if full.is_dir() {
                    std::fs::remove_dir_all(&full).unwrap();
                } else if full.is_file() && below(3) == 0 {
                    std::fs::remove_file(&full).unwrap();
                    continue;
                }
                write(&full, &text(&format!("round {round}"), 1 + below(6)));
                if below(6) == 0 {
                    std::fs::set_permissions(&full, std::fs::Permissions::from_mode(0o755)).unwrap();
                }
            }
            git(&dir, &["add", "-A", "."]);
            git(&dir, &["commit", "-q", "--allow-empty", "-m", &format!("round {round}")]);
        }

        let (commits, files) = same_as_the_whole_tree(&dir);
        assert_eq!(commits, 200);
        assert!(files > 400, "the history has to have had something in it: {files}");
    }
}
