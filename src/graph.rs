//! Commit-graph construction: a revwalk turned into lanes a renderer can draw.
//!
//! The lane assignment is the usual one. We keep a vector of "lanes", each
//! holding the id of the commit expected next in that column. When a commit is
//! emitted it takes the lane that was waiting for it (allocating one if it is a
//! tip), hands that lane to its first parent, and parks any further parents in
//! lanes of their own. The one exception: a first parent already expected in a
//! lane further left is joined there, so a branch bends into the line it came
//! from and never the other way round. Every row records the lane state above
//! and below it, so a renderer can draw one row at a time without looking at
//! its neighbours.

use std::collections::HashMap;

use git2::{Oid, Repository, Sort};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefKind {
    Head,
    LocalBranch,
    RemoteBranch,
    Tag,
}

#[derive(Debug, Clone)]
pub struct RefLabel {
    pub name: String,
    pub kind: RefKind,
    /// True for the branch HEAD currently points at.
    pub current: bool,
}

#[derive(Debug, Clone)]
pub struct Row {
    pub id: Oid,
    pub short_id: String,
    pub summary: String,
    pub body: String,
    pub author: String,
    pub email: String,
    pub when: i64,
    pub refs: Vec<RefLabel>,
    pub is_merge: bool,
    /// Lane this commit's dot sits in.
    pub lane: usize,
    /// Lane contents arriving from the row above.
    pub incoming: Vec<Option<Oid>>,
    /// Lane contents leaving towards the row below.
    pub outgoing: Vec<Option<Oid>>,
    /// Lanes that carry this commit's parents, i.e. lines leaving the dot.
    pub down: Vec<usize>,
}

#[derive(Debug, Clone)]
pub struct Graph {
    pub rows: Vec<Row>,
    pub lanes: usize,
    /// More commits exist than `limit` allowed us to walk.
    pub truncated: bool,
    pub head: Option<String>,
    /// The commit HEAD is at, in full. Together with `head`, what tells the
    /// page that the repository has moved on since it was read.
    pub tip: Option<String>,
    /// Where the requested commit landed, if one was asked for and found.
    pub target: Option<usize>,
    /// True when a commit was asked for and the walk gave up before reaching it.
    pub target_missed: bool,
    /// How many uncommitted entries the working tree has, untracked included.
    /// Zero means there is nothing for a "Local changes" row to show, unless
    /// `working_unreadable` says the count could not be taken.
    pub working_changes: usize,
    /// Why `working_changes` is not a count: the status could not be read, and
    /// libgit2 said this. It refuses an index written with `core.splitIndex`
    /// or as a sparse index, and a damaged one. That used to come out as zero,
    /// which hid the row, which is the only way to the pane that would have
    /// shown the error: a repository with edits in it had no Local changes.
    pub working_unreadable: Option<String>,
    /// Branches that are there and are not in `rows`, because the commit they
    /// name could not be read. git says "does not point to a valid object"
    /// and carries on with the rest, and so does the walk; but a branch left
    /// out of the graph without a word reads as a branch that is not there.
    pub unreadable_refs: Vec<String>,
}

/// How far the walk will go past `limit` to reach a commit it was asked for.
/// A commit search reaches much further back than the graph normally draws, so
/// without this, following a result would land on the newest commits instead of
/// the one asked for.
const HUNT_CAP: usize = 20_000;
/// Rows to keep after the requested commit, for context below it.
const CONTEXT_AFTER: usize = 150;

/// Walk every local and remote branch, newest first, and lay the result out in
/// lanes. Returns at most `limit` rows, unless `target` names a commit that is
/// further back — then the walk continues until it is found, and stops shortly
/// after.
///
/// A history that cannot be read is an error, and the only empty graph is the
/// one for a repository with no commits yet. Read errors used to be dropped
/// along the way, and what was left was drawn as fact: a walk that broke off
/// at a commit that was not there became "No commits. This repository has no
/// history yet", and a checked-out branch whose tip was gone became a graph of
/// the remote's branches with nothing to say that one was missing.
pub fn build(
    path: &std::path::Path,
    limit: usize,
    target: Option<&str>,
) -> Result<Graph, String> {
    let said = |err: git2::Error| err.message().to_string();
    let repo = Repository::open(path).map_err(said)?;
    // History is read from the object store wherever the work tree is said to
    // be. The work tree itself, and anything that would write, is not touched
    // unless the repository really is this directory's.
    let here = crate::git::belongs_here(&repo, path);
    let labels = ref_labels(&repo);

    // No commits yet is one thing, and everything else that stops HEAD from
    // being read is another. They used to be told apart by whether anything
    // could be pushed onto the walk, which a tip lost to a power cut fails
    // just as an unborn branch does.
    let head_ref = match repo.head() {
        Ok(head) => Some(head),
        Err(err) if err.code() == git2::ErrorCode::UnbornBranch => None,
        Err(err) => return Err(said(err)),
    };
    if let Some(head) = &head_ref {
        // git stops here too: "fatal: bad object HEAD".
        head.peel_to_commit()
            .map_err(|err| format!("Its newest commit cannot be read: {}", err.message()))?;
    }
    let head = head_ref
        .as_ref()
        .and_then(|h| h.shorthand().ok().map(|s| s.to_string()));

    let mut walk = repo.revwalk().map_err(said)?;
    walk.set_sorting(Sort::TOPOLOGICAL | Sort::TIME).map_err(said)?;

    let mut pushed = 0usize;
    let mut unreadable_refs = Vec::new();
    for reference in repo.references().map_err(said)? {
        let reference = reference.map_err(said)?;
        let Ok(name) = reference.name().map(str::to_string) else { continue };
        if !name.starts_with("refs/heads/") && !name.starts_with("refs/remotes/") {
            continue;
        }
        match reference.peel_to_commit() {
            Ok(commit) => {
                walk.push(commit.id()).map_err(said)?;
                pushed += 1;
            }
            // A name for another name that is not there: `origin/HEAD` after
            // the branch it pointed at was deleted. Ordinary, and nothing is
            // missing from the graph for it.
            Err(_) if reference.kind() == Some(git2::ReferenceType::Symbolic) => {}
            Err(_) => unreadable_refs.push(
                reference.shorthand().map(str::to_string).unwrap_or(name),
            ),
        }
    }
    let (working_changes, working_unreadable) = working_tree(&repo, here.is_ok());
    if pushed == 0 {
        match head_ref {
            // No commits yet. The working tree may still have plenty in it.
            None => {
                return Ok(Graph {
                    rows: Vec::new(),
                    lanes: 0,
                    truncated: false,
                    head,
                    tip: None,
                    target: None,
                    target_missed: target.is_some(),
                    working_changes,
                    working_unreadable,
                    unreadable_refs,
                });
            }
            // Nothing but a detached HEAD.
            Some(_) => walk.push_head().map_err(said)?,
        }
    }

    let mut rows: Vec<Row> = Vec::new();
    let mut state: Vec<Option<Oid>> = Vec::new();
    let mut truncated = false;
    let mut found_at: Option<usize> = None;

    for oid in walk {
        // Not skipped. Skipping an error here ends the walk: the list is put
        // together on the first call, libgit2 has by then marked every tip as
        // seen, and the call after the one that failed finds nothing left. So
        // one commit that could not be read anywhere in the history came out
        // as a history with no commits in it.
        let oid = oid.map_err(said)?;
        match found_at {
            // Already past the commit we came for: take a little context below
            // it and stop.
            Some(index) => {
                if rows.len() >= index + CONTEXT_AFTER {
                    truncated = true;
                    break;
                }
            }
            // Still hunting: `limit` does not apply until it is found, but the
            // hunt is not unbounded either.
            None if target.is_some() => {
                if rows.len() >= HUNT_CAP {
                    truncated = true;
                    break;
                }
            }
            None => {
                if rows.len() >= limit {
                    truncated = true;
                    break;
                }
            }
        }
        // Nor this: a row left out is a commit the graph says is not there.
        let commit = repo.find_commit(oid).map_err(said)?;
        let parents: Vec<Oid> = commit.parent_ids().collect();

        let incoming = state.clone();
        let (lane, down) = place(&mut state, oid, &parents);

        let author = commit.author();
        rows.push(Row {
            id: oid,
            short_id: oid.to_string().chars().take(7).collect(),
            summary: commit
                .summary()
                .ok()
                .flatten()
                .unwrap_or("(no message)")
                .trim()
                .to_string(),
            body: commit
                .message()
                .unwrap_or("")
                .trim()
                .chars()
                .take(600)
                .collect(),
            author: author.name().unwrap_or("unknown").to_string(),
            email: author.email().unwrap_or("").to_lowercase(),
            when: commit.time().seconds(),
            refs: labels.get(&oid).cloned().unwrap_or_default(),
            is_merge: parents.len() > 1,
            lane,
            incoming,
            outgoing: state.clone(),
            down,
        });

        if let Some(wanted) = target {
            if found_at.is_none() && oid.to_string().starts_with(wanted) {
                found_at = Some(rows.len() - 1);
            }
        }
    }

    let lanes = rows
        .iter()
        .map(|r| r.incoming.len().max(r.outgoing.len()).max(r.lane + 1))
        .max()
        .unwrap_or(0);

    let (_, tip) = head_of(&repo);

    Ok(Graph {
        rows,
        lanes,
        truncated,
        head,
        tip,
        target: found_at,
        target_missed: target.is_some() && found_at.is_none(),
        working_changes,
        working_unreadable,
        unreadable_refs,
    })
}

/// What one look at a repository says: how much is uncommitted, and where
/// HEAD is. Enough to tell a page that it is showing a repository that has
/// since moved — a branch switched or a commit made in a terminal while the
/// page sat open.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Glance {
    pub changes: usize,
    /// Why `changes` is not a count, as [`Graph::working_unreadable`] is for
    /// the page's first reading. Zero with this set is not "nothing changed".
    pub unreadable: Option<String>,
    /// What HEAD is called: a branch, or "HEAD" when detached.
    pub head: Option<String>,
    /// The commit HEAD is at, in full.
    pub tip: Option<String>,
}

/// Look at the repository off the main thread. The window asks for this every
/// time it regains focus, and a repository with a large untracked tree can take
/// long enough to be felt.
pub fn spawn_glance(path: std::path::PathBuf) -> async_channel::Receiver<Glance> {
    let (tx, rx) = async_channel::bounded(1);
    std::thread::spawn(move || {
        let glance = Repository::open(&path)
            .map(|repo| {
                let (head, tip) = head_of(&repo);
                let here = crate::git::belongs_here(&repo, &path).is_ok();
                let (changes, unreadable) = working_tree(&repo, here);
                Glance { changes, unreadable, head, tip }
            })
            .unwrap_or_default();
        let _ = tx.send_blocking(glance);
    });
    rx
}

/// What HEAD is called, and the commit it is at.
fn head_of(repo: &Repository) -> (Option<String>, Option<String>) {
    let Ok(head) = repo.head() else { return (None, None) };
    (
        head.shorthand().ok().map(str::to_string),
        head.target().map(|oid| oid.to_string()),
    )
}

/// Entries in the working tree, untracked included but never ignored, and why
/// they could not be counted when they could not. Counted without recursing
/// into untracked directories — this only decides whether to offer the row,
/// and the pane does the detailed reading.
///
/// `here` is whether the repository is this directory's. One that is not has
/// no working tree that is ours to read, and that is not a failure to read it.
///
/// A status that failed used to be a count of zero, the same as a clean tree:
/// a repository whose changes could not be read had a page that said there
/// were none.
fn working_tree(repo: &Repository, here: bool) -> (usize, Option<String>) {
    if !here {
        return (0, None);
    }
    let mut opts = git2::StatusOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(false)
        .include_ignored(false)
        .exclude_submodules(true);
    match repo.statuses(Some(&mut opts)) {
        Ok(statuses) => (statuses.len(), None),
        Err(err) => (0, Some(err.message().to_string())),
    }
}

/// Give `oid` its lane and hand lanes on to its parents. `state` holds, for
/// each lane, the commit expected next in it: the lanes as they arrive at this
/// row on the way in, and as they leave it on the way out. Returns the lane
/// the commit sits in and the lanes its parents were handed.
fn place(state: &mut Vec<Option<Oid>>, oid: Oid, parents: &[Oid]) -> (usize, Vec<usize>) {
    // The leftmost lane waiting for it, or a lane of its own if it is a tip.
    let lane = match state.iter().position(|slot| *slot == Some(oid)) {
        Some(i) => i,
        None => {
            let i = alloc(state);
            state[i] = Some(oid);
            i
        }
    };

    // Several lanes can be waiting for the same commit (it has several
    // children); they all converge here, so free every one of them.
    for slot in state.iter_mut() {
        if *slot == Some(oid) {
            *slot = None;
        }
    }

    let mut down: Vec<usize> = Vec::new();
    for (n, parent) in parents.iter().enumerate() {
        let target = match state.iter().position(|slot| *slot == Some(*parent)) {
            // A first parent that another lane, further right, already waits
            // for: a branch that forked from it was walked before this commit
            // was. Stay put and wait for it here as well. The two lanes meet
            // at the parent, in this one, which is where the lookup above
            // puts it.
            //
            // The line used to go to the lane that was waiting, wherever that
            // was, and leave its own empty. Nothing ever moved it back. So
            // below the first fork the trunk was drawn in the branch's lane
            // with nothing to its left, for the rest of the history; and with
            // enough branches open at the top it came to rest past the last
            // lane that is drawn, where every older row had an empty cell.
            Some(j) if n == 0 && j > lane => {
                state[lane] = Some(*parent);
                lane
            }
            // Waited for further left, or a parent merged in: bend into the
            // lane that has it. A first-parent line only ever moves left.
            Some(j) => j,
            None if n == 0 && state[lane].is_none() => {
                state[lane] = Some(*parent);
                lane
            }
            None => {
                let j = alloc(state);
                state[j] = Some(*parent);
                j
            }
        };
        if !down.contains(&target) {
            down.push(target);
        }
    }

    while state.last().is_some_and(|slot| slot.is_none()) {
        state.pop();
    }
    (lane, down)
}

fn alloc(state: &mut Vec<Option<Oid>>) -> usize {
    match state.iter().position(|slot| slot.is_none()) {
        Some(i) => i,
        None => {
            state.push(None);
            state.len() - 1
        }
    }
}

fn ref_labels(repo: &Repository) -> HashMap<Oid, Vec<RefLabel>> {
    let mut map: HashMap<Oid, Vec<RefLabel>> = HashMap::new();

    let current = repo
        .head()
        .ok()
        .filter(|_| !repo.head_detached().unwrap_or(false))
        .and_then(|h| h.shorthand().ok().map(|s| s.to_string()));

    let Ok(refs) = repo.references() else { return map };
    for reference in refs.flatten() {
        let Ok(full) = reference.name() else { continue };
        let kind = if full.starts_with("refs/heads/") {
            RefKind::LocalBranch
        } else if full.starts_with("refs/remotes/") {
            RefKind::RemoteBranch
        } else if full.starts_with("refs/tags/") {
            RefKind::Tag
        } else {
            continue;
        };
        let Ok(name) = reference.shorthand().map(str::to_string) else { continue };
        if kind == RefKind::RemoteBranch && name.ends_with("/HEAD") {
            continue;
        }
        let Ok(commit) = reference.peel_to_commit() else { continue };
        let is_current = kind == RefKind::LocalBranch && current.as_deref() == Some(name.as_str());
        map.entry(commit.id()).or_default().push(RefLabel {
            name,
            kind,
            current: is_current,
        });
    }

    // A detached HEAD gets its own marker, since no branch names it.
    if repo.head_detached().unwrap_or(false) {
        if let Ok(commit) = repo.head().and_then(|h| h.peel_to_commit()) {
            map.entry(commit.id()).or_default().insert(
                0,
                RefLabel { name: "HEAD".into(), kind: RefKind::Head, current: true },
            );
        }
    }

    for labels in map.values_mut() {
        labels.sort_by_key(|l| match l.kind {
            RefKind::Head => 0,
            RefKind::LocalBranch => 1,
            RefKind::Tag => 2,
            RefKind::RemoteBranch => 3,
        });
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::*;
    use std::path::Path;

    /// Commit `file` with the commit's own clock set to `when`. The walk is
    /// newest first, so a test about lanes has to say when things happened.
    fn commit_at(repo: &Path, file: &str, when: i64) {
        write(&repo.join(file), "x\n");
        git(repo, &["add", "--", file]);
        let date = format!("@{when} +0000");
        let done = std::process::Command::new("git")
            .current_dir(repo)
            .args(["commit", "-q", "-m", file])
            .env("GIT_AUTHOR_DATE", &date)
            .env("GIT_COMMITTER_DATE", &date)
            .status()
            .expect("git runs");
        assert!(done.success(), "fixture commit of {file} failed");
    }

    fn summaries(graph: &Graph) -> Vec<&str> {
        graph.rows.iter().map(|row| row.summary.as_str()).collect()
    }

    // ---- a history that cannot be read is not an empty one -----------------

    #[test]
    fn a_history_that_cannot_be_walked_is_an_error_and_not_no_commits() {
        let _guard = isolate();

        // A commit in the middle is gone. git lists the two above it and then
        // stops with an error; the page said "No commits".
        let f = fleet("lanes-lost-middle");
        for name in ["a", "b", "c", "d"] {
            commit_file(&f.work, &format!("{name}.txt"), "x\n", name);
        }
        assert_eq!(build(&f.work, 500, None).unwrap().rows.len(), 5);
        lose_object(&f.work, "HEAD~2");
        let said = build(&f.work, 500, None).expect_err("a walk that broke off");
        assert!(said.contains("not found"), "{said}");

        // The newest commit is gone, and there is no other branch to draw.
        let dir = scratch("lanes-lost-tip");
        git(&dir, &["init", "-q", "-b", "main"]);
        commit_file(&dir, "a.txt", "x\n", "a");
        commit_file(&dir, "b.txt", "x\n", "b");
        lose_object(&dir, "HEAD");
        assert!(build(&dir, 500, None).is_err());

        // The same with a remote to draw instead: the page showed the remote's
        // history with the checked-out branch quietly missing from it.
        let f = fleet("lanes-lost-tip-with-remote");
        commit_file(&f.work, "a.txt", "x\n", "a");
        lose_object(&f.work, "HEAD");
        assert!(build(&f.work, 500, None).is_err());
    }

    #[test]
    fn a_branch_that_cannot_be_read_is_named_and_the_rest_is_still_drawn() {
        let _guard = isolate();
        let f = fleet("lanes-lost-side");
        git(&f.work, &["checkout", "-q", "-b", "side"]);
        commit_file(&f.work, "s.txt", "x\n", "on the side");
        git(&f.work, &["checkout", "-q", "main"]);
        // A name for a name that is not there. That is ordinary, and nothing
        // is missing from the graph because of it.
        git(&f.work, &["symbolic-ref", "refs/remotes/origin/HEAD", "refs/remotes/origin/gone"]);
        let graph = build(&f.work, 500, None).unwrap();
        assert_eq!(summaries(&graph), ["on the side", "first"]);
        assert!(graph.unreadable_refs.is_empty(), "{:?}", graph.unreadable_refs);

        lose_object(&f.work, "side");
        let graph = build(&f.work, 500, None).expect("main and the remote still read");
        assert_eq!(summaries(&graph), ["first"]);
        assert_eq!(graph.unreadable_refs, ["side"]);
    }

    #[test]
    fn a_status_that_cannot_be_read_is_not_a_count_of_zero() {
        let _guard = isolate();
        let f = fleet("lanes-split-index");
        write(&f.work.join("README.md"), "one\ntwo\n");
        let graph = build(&f.work, 500, None).unwrap();
        assert_eq!((graph.working_changes, graph.working_unreadable.clone()), (1, None));

        // git reads this index without a word; libgit2 refuses all of it.
        git(&f.work, &["update-index", "--split-index"]);
        let graph = build(&f.work, 500, None).expect("its history reads as well as ever");
        assert_eq!(graph.rows.len(), 1);
        assert_eq!(graph.working_changes, 0);
        assert!(graph.working_unreadable.is_some_and(|why| why.contains("link")));
        // And the look the page takes each time the window comes back.
        let glance = recv(spawn_glance(f.work.clone()));
        assert!(glance.changes == 0 && glance.unreadable.is_some());
    }

    #[test]
    fn a_repository_with_no_commits_is_still_an_empty_graph() {
        let _guard = isolate();
        let dir = scratch("lanes-unborn");
        git(&dir, &["init", "-q", "-b", "main"]);
        write(&dir.join("README.md"), "one\n");
        let graph = build(&dir, 500, None).expect("nothing is wrong with it");
        assert!(graph.rows.is_empty());
        assert_eq!(graph.working_changes, 1);
    }

    // ---- lanes -------------------------------------------------------------

    #[test]
    fn the_trunk_keeps_its_lane_past_a_branch_that_forked_from_it() {
        let _guard = isolate();
        let dir = scratch("lanes-trunk");
        git(&dir, &["init", "-q", "-b", "main"]);
        commit_at(&dir, "one", 1_000);
        commit_at(&dir, "two", 2_000);
        commit_at(&dir, "three", 3_000);
        // A branch cut from `two` and committed to, after which main moved on:
        // the everyday shape of a feature branch.
        git(&dir, &["checkout", "-q", "-b", "side", "HEAD~1"]);
        commit_at(&dir, "side", 4_000);
        git(&dir, &["checkout", "-q", "main"]);
        commit_at(&dir, "four", 5_000);

        let graph = build(&dir, 500, None).unwrap();
        assert_eq!(summaries(&graph), ["four", "side", "three", "two", "one"]);
        // `three` used to follow its parent into the branch's lane, because
        // the branch had asked for `two` first, and everything older was then
        // drawn one lane out with nothing to its left.
        let lanes: Vec<usize> = graph.rows.iter().map(|row| row.lane).collect();
        assert_eq!(lanes, [0, 1, 0, 0, 0]);
        assert_eq!(graph.lanes, 2);
        // Both lanes arrive at `two` waiting for it, which is what the page
        // draws as the branch bending into the trunk.
        let two = &graph.rows[3];
        assert_eq!(two.incoming, [Some(two.id), Some(two.id)]);
        assert_eq!(two.outgoing.len(), 1);
    }

    fn id(n: u32) -> Oid {
        Oid::from_str(&format!("{n:040x}")).unwrap()
    }

    /// Lay out commits, given as (commit, its parents) in the order a walk
    /// would hand them over, without a repository: the shapes worth pinning
    /// take hundreds of commits. Returns each commit's lane and the most
    /// lanes any row had, and checks on the way that no line is dropped.
    fn lay_out(commits: &[(u32, Vec<u32>)]) -> (Vec<usize>, usize) {
        let mut state: Vec<Option<Oid>> = Vec::new();
        let (mut lanes, mut widest) = (Vec::new(), 0);
        for (commit, parents) in commits {
            let parents: Vec<Oid> = parents.iter().map(|parent| id(*parent)).collect();
            let arriving = state.len();
            let (lane, down) = place(&mut state, id(*commit), &parents);
            assert!(!state.contains(&Some(id(*commit))), "{commit} is still waited for");
            for parent in &parents {
                assert!(state.contains(&Some(*parent)), "nothing leads on to a parent of {commit}");
            }
            for lane in &down {
                assert!(state[*lane].is_some_and(|next| parents.contains(&next)));
            }
            widest = widest.max(arriving).max(state.len()).max(lane + 1);
            lanes.push(lane);
        }
        assert!(state.is_empty(), "lines left hanging: {state:?}");
        (lanes, widest)
    }

    #[test]
    fn stale_branches_do_not_walk_the_trunk_out_of_the_graph() {
        // Twenty branches of one commit each, cut from the twenty newest
        // commits of a long linear main and never merged: what a repository
        // whose pull requests are squashed looks like. The branch tips are
        // newer than main's, so they are walked first, and each waits for the
        // commit it was cut from.
        const BRANCHES: u32 = 20;
        const MAIN: u32 = 60;
        let mut commits: Vec<(u32, Vec<u32>)> = Vec::new();
        for branch in 1..=BRANCHES {
            commits.push((1000 + branch, vec![branch]));
        }
        for main in 1..=MAIN {
            commits.push((main, if main < MAIN { vec![main + 1] } else { vec![] }));
        }

        let (lanes, widest) = lay_out(&commits);
        // The trunk hopped one lane right at every fork, came to rest in the
        // twentieth, and stayed there for the rest of the history. Past the
        // sixteenth nothing is drawn, so every older row had an empty cell.
        let trunk = &lanes[BRANCHES as usize..];
        assert!(trunk.iter().all(|lane| *lane == 0), "{trunk:?}");
        // And it costs no lanes: there are twenty lines open at the top, as
        // there were.
        assert_eq!(widest, BRANCHES as usize);
    }

    #[test]
    fn branches_cut_from_one_commit_still_share_a_lane() {
        // The shape a cure for the drift must not make worse: keeping every
        // first parent in its own lane would give these twenty a lane each.
        let mut commits: Vec<(u32, Vec<u32>)> = Vec::new();
        for branch in 1..=20 {
            commits.push((1000 + branch, vec![2]));
        }
        commits.extend([(1, vec![2]), (2, vec![3]), (3, vec![])]);

        let (lanes, widest) = lay_out(&commits);
        assert_eq!(widest, 2);
        assert_eq!(lanes[20..], [1, 0, 0]);
    }

    #[test]
    fn a_merged_branch_meets_the_trunk_in_the_trunks_lane() {
        // 1 merges 20 into main; the branch is 20 and 21, cut from 3. Its
        // first commit is walked before main's 2, so it asks for 3 first.
        let commits = [
            (1, vec![2, 20]),
            (20, vec![21]),
            (21, vec![3]),
            (2, vec![3]),
            (3, vec![4]),
            (4, vec![]),
        ];
        let (lanes, widest) = lay_out(&commits);
        assert_eq!(lanes, [0, 1, 1, 0, 0, 0]);
        assert_eq!(widest, 2);
    }
}
