//! What a single commit changed.
//!
//! Reading a diff is far more expensive than reading a commit header, and the
//! pane that uses this reloads every time the cursor moves, so this runs off
//! the main thread and can be abandoned.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use git2::{Diff, DiffFindOptions, DiffOptions, Oid, Patch, Repository};

/// A commit touching more files than this is telling you something other than
/// which files it touched.
const MAX_FILES: usize = 400;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    Added,
    Modified,
    Deleted,
    Renamed,
    Copied,
    TypeChange,
    Other,
}

impl Change {
    /// The single letter git itself uses.
    pub fn letter(self) -> &'static str {
        match self {
            Change::Added => "A",
            Change::Modified => "M",
            Change::Deleted => "D",
            Change::Renamed => "R",
            Change::Copied => "C",
            Change::TypeChange => "T",
            Change::Other => "?",
        }
    }

    pub fn css_class(self) -> &'static str {
        match self {
            Change::Added => "added",
            Change::Deleted => "deleted",
            Change::Renamed | Change::Copied => "renamed",
            _ => "modified",
        }
    }

    fn from(delta: git2::Delta) -> Self {
        match delta {
            git2::Delta::Added => Change::Added,
            git2::Delta::Modified => Change::Modified,
            git2::Delta::Deleted => Change::Deleted,
            git2::Delta::Renamed => Change::Renamed,
            git2::Delta::Copied => Change::Copied,
            git2::Delta::Typechange => Change::TypeChange,
            _ => Change::Other,
        }
    }
}

#[derive(Debug, Clone)]
pub struct FileChange {
    pub path: String,
    /// Where it came from, for a rename or copy.
    pub from: Option<String>,
    pub change: Change,
    pub insertions: usize,
    pub deletions: usize,
    pub binary: bool,
}

#[derive(Debug, Clone)]
pub struct Detail {
    pub id: String,
    pub short_id: String,
    pub summary: String,
    pub body: String,
    pub author: String,
    pub email: String,
    pub when: i64,
    /// Only kept when it differs from the author, which is the only time it
    /// tells anyone anything.
    pub committer: Option<String>,
    pub parents: Vec<String>,
    pub merge: bool,
    pub files: Vec<FileChange>,
    /// How many files the commit touched, before the cap above.
    pub total_files: usize,
    pub insertions: usize,
    pub deletions: usize,
}

/// A flag the caller can set to abandon a load it no longer wants.
#[derive(Clone, Default)]
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
    repo: PathBuf,
    id: String,
) -> (async_channel::Receiver<Result<Box<Detail>, String>>, Cancel) {
    let (tx, rx) = async_channel::bounded(1);
    let cancel = Cancel::default();
    {
        let cancel = cancel.clone();
        std::thread::spawn(move || {
            let result = load(&repo, &id, &cancel);
            if !cancel.stopped() {
                let _ = tx.send_blocking(result.map(Box::new));
            }
        });
    }
    (rx, cancel)
}

fn load(path: &Path, id: &str, cancel: &Cancel) -> Result<Detail, String> {
    let repo = Repository::open(path).map_err(|e| e.message().to_string())?;
    let oid = Oid::from_str(id).map_err(|e| e.message().to_string())?;
    let commit = repo.find_commit(oid).map_err(|e| e.message().to_string())?;

    let author = commit.author();
    let author_name = author.name().unwrap_or("unknown").to_string();
    let committer = commit.committer();
    let committer_name = committer.name().unwrap_or("").to_string();

    let tree = commit.tree().map_err(|e| e.message().to_string())?;
    // A merge is diffed against its first parent: that shows what the merge
    // brought in, which is what someone looking at one means.
    let parent_tree = commit.parent(0).ok().and_then(|p| p.tree().ok());

    let mut opts = DiffOptions::new();
    opts.include_typechange(true).context_lines(0);
    let mut diff: Diff = repo
        .diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), Some(&mut opts))
        .map_err(|e| e.message().to_string())?;

    // Without this a rename reads as a delete plus an add, which is three
    // wrong facts where there was one right one.
    let mut finds = DiffFindOptions::new();
    finds.renames(true).copies(true);
    let _ = diff.find_similar(Some(&mut finds));

    let total_files = diff.deltas().len();
    let mut files = Vec::with_capacity(total_files.min(MAX_FILES));
    let mut insertions = 0usize;
    let mut deletions = 0usize;

    for (index, delta) in diff.deltas().enumerate() {
        if cancel.stopped() {
            return Err("cancelled".into());
        }
        let binary = delta.new_file().is_binary() || delta.old_file().is_binary();
        let (adds, dels) = if binary {
            (0, 0)
        } else {
            match Patch::from_diff(&diff, index) {
                Ok(Some(patch)) => {
                    let (_, adds, dels) = patch.line_stats().unwrap_or((0, 0, 0));
                    (adds, dels)
                }
                _ => (0, 0),
            }
        };
        insertions += adds;
        deletions += dels;

        if files.len() >= MAX_FILES {
            continue;
        }
        let path_of = |f: git2::DiffFile<'_>| {
            f.path()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default()
        };
        let new_path = path_of(delta.new_file());
        let old_path = path_of(delta.old_file());
        let change = Change::from(delta.status());
        files.push(FileChange {
            path: if new_path.is_empty() { old_path.clone() } else { new_path },
            from: (matches!(change, Change::Renamed | Change::Copied) && old_path != *"")
                .then_some(old_path),
            change,
            insertions: adds,
            deletions: dels,
            binary,
        });
    }

    Ok(Detail {
        short_id: oid.to_string().chars().take(8).collect(),
        id: oid.to_string(),
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
            .splitn(2, '\n')
            .nth(1)
            .unwrap_or("")
            .trim()
            .to_string(),
        email: author.email().unwrap_or("").to_string(),
        author: author_name.clone(),
        when: commit.time().seconds(),
        committer: (!committer_name.is_empty() && committer_name != author_name)
            .then_some(committer_name),
        parents: commit
            .parent_ids()
            .map(|p| p.to_string().chars().take(8).collect())
            .collect(),
        merge: commit.parent_count() > 1,
        files,
        total_files,
        insertions,
        deletions,
    })
}

/// A node in the tree the pane draws.
pub enum Node {
    /// A directory, with the path segment(s) it stands for. Single-child chains
    /// are collapsed into one node, so `src/Api/One/Interfaces` is one row
    /// rather than four.
    Dir { name: String, depth: usize },
    File { file: FileChange, depth: usize },
}

/// Arrange changed files into an indented tree, flattened for rendering.
///
/// A flat list of paths like
/// `src/Orders.Api.Application/Checkout/Interfaces/IFoo.cs` is a
/// wall of identical prefixes; the shape of a change is what the list is for.
pub fn tree(files: &[FileChange]) -> Vec<Node> {
    #[derive(Default)]
    struct Dir {
        dirs: std::collections::BTreeMap<String, Dir>,
        files: Vec<FileChange>,
    }

    let mut root = Dir::default();
    for file in files {
        let mut parts: Vec<&str> = file.path.split('/').collect();
        let _name = parts.pop();
        let mut at = &mut root;
        for part in parts {
            at = at.dirs.entry(part.to_string()).or_default();
        }
        at.files.push(file.clone());
    }

    fn walk(dir: &Dir, prefix: String, depth: usize, out: &mut Vec<Node>) {
        for (name, child) in &dir.dirs {
            // Collapse a chain of directories that each hold only one thing.
            if child.files.is_empty() && child.dirs.len() == 1 {
                let joined = if prefix.is_empty() {
                    name.clone()
                } else {
                    format!("{prefix}/{name}")
                };
                walk(child, joined, depth, out);
                continue;
            }
            let label = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            out.push(Node::Dir { name: label, depth });
            walk(child, String::new(), depth + 1, out);
        }
        for file in &dir.files {
            let name = file.path.rsplit('/').next().unwrap_or(&file.path).to_string();
            let mut shown = file.clone();
            shown.path = name;
            out.push(Node::File { file: shown, depth });
        }
    }

    let mut out = Vec::new();
    walk(&root, String::new(), 0, &mut out);
    out
}
