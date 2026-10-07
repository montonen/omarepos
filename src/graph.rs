//! Commit-graph construction: a revwalk turned into lanes a renderer can draw.
//!
//! The lane assignment is the usual one. We keep a vector of "lanes", each
//! holding the id of the commit expected next in that column. When a commit is
//! emitted it takes the lane that was waiting for it (allocating one if it is a
//! tip), hands that lane to its first parent, and parks any further parents in
//! lanes of their own. Every row records the lane state above and below it, so
//! a renderer can draw one row at a time without looking at its neighbours.

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
    /// Where the requested commit landed, if one was asked for and found.
    pub target: Option<usize>,
    /// True when a commit was asked for and the walk gave up before reaching it.
    pub target_missed: bool,
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
pub fn build(
    path: &std::path::Path,
    limit: usize,
    target: Option<&str>,
) -> Result<Graph, String> {
    let repo = Repository::open(path).map_err(|e| e.message().to_string())?;
    let labels = ref_labels(&repo);

    let head = repo
        .head()
        .ok()
        .and_then(|h| h.shorthand().ok().map(|s| s.to_string()));

    let mut walk = repo.revwalk().map_err(|e| e.message().to_string())?;
    walk.set_sorting(Sort::TOPOLOGICAL | Sort::TIME)
        .map_err(|e| e.message().to_string())?;

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
    if pushed == 0 {
        // Unborn branch, or a repo with nothing but a detached HEAD.
        if walk.push_head().is_err() {
            return Ok(Graph {
                rows: Vec::new(),
                lanes: 0,
                truncated: false,
                head,
                target: None,
                target_missed: target.is_some(),
            });
        }
    }

    let mut rows: Vec<Row> = Vec::new();
    let mut state: Vec<Option<Oid>> = Vec::new();
    let mut truncated = false;
    let mut found_at: Option<usize> = None;

    for oid in walk {
        let Ok(oid) = oid else { continue };
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
        let Ok(commit) = repo.find_commit(oid) else { continue };
        let parents: Vec<Oid> = commit.parent_ids().collect();

        let incoming = state.clone();
        let lane = match state.iter().position(|slot| *slot == Some(oid)) {
            Some(i) => i,
            None => {
                let i = alloc(&mut state);
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
                Some(j) => j,
                None if n == 0 && state[lane].is_none() => {
                    state[lane] = Some(*parent);
                    lane
                }
                None => {
                    let j = alloc(&mut state);
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

    Ok(Graph {
        rows,
        lanes,
        truncated,
        head,
        target: found_at,
        target_missed: target.is_some() && found_at.is_none(),
    })
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
