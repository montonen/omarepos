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
    Untracked,
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
            // The same mark git's porcelain uses.
            Change::Untracked => "?",
            Change::Other => "·",
        }
    }

    pub fn css_class(self) -> &'static str {
        match self {
            Change::Added => "added",
            Change::Deleted => "deleted",
            Change::Renamed | Change::Copied => "renamed",
            Change::Untracked => "untracked",
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
            git2::Delta::Untracked => Change::Untracked,
            _ => Change::Other,
        }
    }
}

/// Which part of a repository's state a file change belongs to. For a commit
/// there is only one; for the working tree the distinction is the point —
/// untracked files are the one kind that does not block a fast-forward.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Group {
    Committed,
    Staged,
    Unstaged,
    Conflicted,
    Untracked,
}

impl Group {
    pub fn heading(self) -> &'static str {
        match self {
            Group::Committed => "",
            Group::Staged => "Staged",
            Group::Unstaged => "Not staged",
            Group::Conflicted => "Conflicted",
            Group::Untracked => "Untracked",
        }
    }

    /// Whether changes of this kind stop the background refresh from
    /// fast-forwarding. Untracked files do not: git refuses rather than
    /// overwriting one.
    pub fn blocks_update(self) -> bool {
        matches!(self, Group::Staged | Group::Unstaged | Group::Conflicted)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChange {
    pub path: String,
    /// Where it came from, for a rename or copy.
    pub from: Option<String>,
    pub change: Change,
    pub insertions: usize,
    pub deletions: usize,
    pub binary: bool,
    /// libgit2 could not read one side of it, so the two counts above are not
    /// zero, they are unknown.
    pub unreadable: bool,
    pub group: Group,
}

/// What the pane is being asked to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Commit(String),
    /// Everything uncommitted, including untracked files.
    Working,
}

#[derive(Debug, Clone, PartialEq, Eq)]
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
    /// How many files could not be read, before the cap. The totals above do
    /// not include them, and say so.
    pub unreadable: usize,
    /// The commit has a parent and this clone does not: it is where a shallow
    /// clone's history stops. It is compared with nothing, as a first commit
    /// is, so every file in the tree reads as added by it.
    pub boundary: bool,
    /// For the working tree: how many entries each kind has, counted before
    /// the cap. What the pane says about the tree, and what Commit will take,
    /// are read from here and not from the list, which may have been cut.
    pub counts: Counts,
    /// For the working tree: where the list is known to differ from what
    /// `git status` would say, as sentences. The list is libgit2's, and
    /// libgit2 implements neither sparse checkouts nor the filters git runs.
    pub caveats: Vec<String>,
}

/// Entries per kind in a working tree.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    pub staged: usize,
    pub unstaged: usize,
    pub conflicted: usize,
    pub untracked: usize,
}

impl Counts {
    pub fn of(self, group: Group) -> usize {
        match group {
            Group::Staged => self.staged,
            Group::Unstaged => self.unstaged,
            Group::Conflicted => self.conflicted,
            Group::Untracked => self.untracked,
            Group::Committed => 0,
        }
    }

    /// Whether anything here holds the background refresh back.
    pub fn blocks_update(self) -> bool {
        self.staged + self.unstaged + self.conflicted > 0
    }
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
    target: Target,
) -> (async_channel::Receiver<Result<Box<Detail>, String>>, Cancel) {
    let (tx, rx) = async_channel::bounded(1);
    let cancel = Cancel::default();
    {
        let cancel = cancel.clone();
        std::thread::spawn(move || {
            let result = match &target {
                Target::Commit(id) => load(&repo, id, &cancel),
                Target::Working => load_working(&repo, &cancel),
            };
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

    let tree = commit.tree().map_err(|e| unreadable(&repo, &e))?;
    // A merge is diffed against its first parent: that shows what the merge
    // brought in, which is what someone looking at one means.
    let parent_tree = parent_tree(&repo, &commit)?;
    // At the edge of a shallow clone libgit2 hides a commit's parents, as git
    // does, and the commit's own header goes on naming them.
    let boundary = commit.parent_count() == 0
        && repo.is_shallow()
        && commit
            .raw_header()
            .is_ok_and(|header| header.lines().any(|line| line.starts_with("parent ")));

    let mut opts = DiffOptions::new();
    opts.include_typechange(true).context_lines(0);
    let mut diff: Diff = repo
        .diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), Some(&mut opts))
        .map_err(|e| unreadable(&repo, &e))?;

    // Without this a rename reads as a delete plus an add, which is three
    // wrong facts where there was one right one.
    let mut finds = DiffFindOptions::new();
    finds.renames(true).copies(true);
    let _ = diff.find_similar(Some(&mut finds));

    let total_files = diff.deltas().len();
    let mut files = Vec::with_capacity(total_files.min(MAX_FILES));
    let mut insertions = 0usize;
    let mut deletions = 0usize;
    let mut unread = 0usize;

    for (index, delta) in diff.deltas().enumerate() {
        if cancel.stopped() {
            return Err("cancelled".into());
        }
        let Measured { adds, dels, binary, unreadable } = measure(&diff, index);
        insertions += adds;
        deletions += dels;
        unread += usize::from(unreadable);

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
            unreadable,
            group: Group::Committed,
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
        unreadable: unread,
        boundary,
        counts: Counts::default(),
        caveats: Vec::new(),
    })
}

/// The tree a commit is compared with: its first parent's, or nothing for a
/// commit that has no parent.
///
/// A parent that could not be read used to be taken for no parent. In a clone
/// made with `--filter=tree:0` that is every commit but the newest, and a
/// one-line commit was shown as having added the whole repository.
fn parent_tree<'repo>(
    repo: &'repo Repository,
    commit: &git2::Commit<'repo>,
) -> Result<Option<git2::Tree<'repo>>, String> {
    if commit.parent_count() == 0 {
        return Ok(None);
    }
    commit
        .parent(0)
        .and_then(|parent| parent.tree())
        .map(Some)
        .map_err(|err| unreadable(repo, &err))
}

/// The tree HEAD stands for, or nothing when nothing has been committed yet:
/// then everything staged is new. Any other failure to read it is a failure,
/// and not a reason to list the whole index as added.
fn head_tree(repo: &Repository) -> Result<Option<git2::Tree<'_>>, String> {
    match repo.head() {
        Ok(head) => head.peel_to_tree().map(Some).map_err(|err| unreadable(repo, &err)),
        Err(err) if matches!(err.code(), git2::ErrorCode::UnbornBranch | git2::ErrorCode::NotFound) => {
            Ok(None)
        }
        Err(err) => Err(err.message().to_string()),
    }
}

/// libgit2's complaint about something it could not read, with the reason in
/// front where the reason is known. A partial clone is the ordinary one: git
/// fetches a missing object when it is asked for it, libgit2 does not, and
/// "object not found" is all it has to say about that.
fn unreadable(repo: &Repository, err: &git2::Error) -> String {
    if err.code() == git2::ErrorCode::NotFound && is_partial(repo) {
        format!(
            "This is a partial clone, and this content has not been downloaded. git fetches \
             it when asked: `git show` or `git diff` in a terminal. ({})",
            err.message()
        )
    } else {
        err.message().to_string()
    }
}

/// Whether objects may be missing here on purpose: a clone made with
/// `--filter`. git marks the remote it will fetch them from as a promisor;
/// older versions named it under `extensions` as well.
fn is_partial(repo: &Repository) -> bool {
    let Ok(config) = repo.config() else { return false };
    if config.get_string("extensions.partialclone").is_ok() {
        return true;
    }
    repo.remotes().is_ok_and(|remotes| {
        remotes
            .iter()
            .filter_map(|name| name.ok().flatten())
            .any(|name| config.get_bool(&format!("remote.{name}.promisor")).unwrap_or(false))
    })
}

/// One line of a unified diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    /// git's own marker: ' ', '+', '-', or '@' for a hunk header.
    pub origin: char,
    pub text: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileDiff {
    pub lines: Vec<Line>,
    pub truncated: bool,
    pub binary: bool,
    /// What there is to say when there are no lines to show and "nothing
    /// changed" would not be true: a file that was renamed and not edited.
    pub note: Option<String>,
    /// How much text `lines` holds, counted as it is collected.
    bytes: usize,
}

/// Past this a diff is not being read, it is being scrolled past.
const MAX_LINES: usize = 4_000;
/// A line longer than this is a minified bundle or a one-line JSON file, not
/// something anybody reads. It is cut here and says so. Shown whole, a single
/// such line froze the window for the better part of a minute: what a text
/// view does with a line grows with the square of its length.
const MAX_LINE_BYTES: usize = 10_000;
/// All the text one file's diff may hand to the view.
const MAX_BYTES: usize = 2 << 20;

/// A diff line as it will be shown: decoded, without its line ending, and cut
/// if it is too long to be worth showing whole.
fn shown(content: &[u8]) -> String {
    let text = String::from_utf8_lossy(content);
    let text = text.trim_end_matches('\n').trim_end_matches('\r');
    if text.len() <= MAX_LINE_BYTES {
        return text.to_string();
    }
    let mut cut = MAX_LINE_BYTES;
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{} … {} more bytes on this line", &text[..cut], text.len() - cut)
}

/// Read one file's diff. `from` is where the list says the file came from, for
/// a rename or a copy, and `side` is which half of a working-tree file the row
/// stands for: both are what make the diff the one the row describes.
pub fn spawn_patch(
    repo: PathBuf,
    target: Target,
    file: String,
    from: Option<String>,
    side: Option<Group>,
) -> (async_channel::Receiver<Result<Box<FileDiff>, String>>, Cancel) {
    let (tx, rx) = async_channel::bounded(1);
    let cancel = Cancel::default();
    {
        let cancel = cancel.clone();
        std::thread::spawn(move || {
            let result = match &target {
                Target::Commit(id) => patch_for(&repo, id, &file, from.as_deref()),
                Target::Working => patch_working(&repo, &file, side),
            };
            if !cancel.stopped() {
                let _ = tx.send_blocking(result.map(Box::new));
            }
        });
    }
    (rx, cancel)
}

/// Everything uncommitted: staged against HEAD, unstaged and untracked against
/// the index. Two diffs rather than one because the two halves answer different
/// questions and only one of them blocks an update.
fn load_working(path: &Path, cancel: &Cancel) -> Result<Detail, String> {
    let repo = crate::git::open_here(path)?;
    let head_tree = head_tree(&repo)?;
    let index = repo.index().map_err(|e| e.message().to_string())?;

    let mut files = Vec::new();
    let mut insertions = 0usize;
    let mut deletions = 0usize;

    // A half that cannot be read is an error, here and below. It used to be
    // left out: one file the user could not open, and the list was the staged
    // files alone under "nothing here blocks an update".
    let mut staged_opts = DiffOptions::new();
    staged_opts.include_typechange(true).context_lines(0);
    let diff = repo
        .diff_tree_to_index(head_tree.as_ref(), Some(&index), Some(&mut staged_opts))
        .map_err(|e| unreadable(&repo, &e))?;
    collect(&diff, Group::Staged, cancel, &mut files, &mut insertions, &mut deletions);

    let mut work_opts = DiffOptions::new();
    work_opts
        .include_typechange(true)
        .include_untracked(true)
        // One row per untracked file rather than one per untracked directory:
        // "two untracked things" is what the card already says, and this view
        // exists to say which.
        .recurse_untracked_dirs(true)
        .include_ignored(false)
        // The same rule as the card, the row count and the refresher: a
        // submodule's checkout being at another commit is not a change of
        // this repository's to stage. Listed, it had a stage button, and
        // staging it rewound the pointer to wherever the checkout happened
        // to be.
        .ignore_submodules(true)
        .context_lines(0);
    let diff = repo
        .diff_index_to_workdir(Some(&index), Some(&mut work_opts))
        .map_err(|e| unreadable(&repo, &e))?;
    collect(&diff, Group::Unstaged, cancel, &mut files, &mut insertions, &mut deletions);

    let total_files = files.len();
    let unread = files.iter().filter(|f| f.unreadable).count();
    let count = |group: Group| files.iter().filter(|f| f.group == group).count();
    let counts = Counts {
        staged: count(Group::Staged),
        unstaged: count(Group::Unstaged),
        conflicted: count(Group::Conflicted),
        untracked: count(Group::Untracked),
    };
    // The cap falls on what matters least. Untracked files are the bulk (one
    // unignored build directory is thousands) and the one kind that holds
    // nothing back; cut from the joined list as it came, they pushed the
    // single modified file off the end and the pane said nothing was in the
    // way. Stable, so the order within each kind is kept.
    files.sort_by_key(|f| !f.group.blocks_update());
    files.truncate(MAX_FILES);
    if let Some(workdir) = repo.workdir() {
        for file in files.iter_mut().filter(|file| file.group == Group::Untracked) {
            file.binary = looks_binary(&workdir.join(&file.path));
        }
    }
    let caveats = caveats(&repo, &index, &files);

    Ok(Detail {
        id: String::new(),
        short_id: "working".into(),
        summary: "Local changes".into(),
        body: String::new(),
        author: String::new(),
        email: String::new(),
        when: 0,
        committer: None,
        parents: Vec::new(),
        merge: false,
        files,
        total_files,
        insertions,
        deletions,
        unreadable: unread,
        boundary: false,
        counts,
        caveats,
    })
}

/// Where the working-tree list is libgit2's reading and known not to be
/// git's, said in sentences.
///
/// The rows themselves are left as libgit2 gives them. The card, the count on
/// the Local changes row and the refresher all take the same reading, and a
/// pane that quietly disagreed with the three of them would be a fourth
/// opinion. What it can do is say which rows not to believe.
fn caveats(repo: &Repository, index: &git2::Index, files: &[FileChange]) -> Vec<String> {
    let mut caveats = Vec::new();
    let unstaged = |change: Change| {
        files.iter().filter(move |file| file.group == Group::Unstaged && file.change == change)
    };

    // A sparse checkout marks what it leaves out in the index, and git then
    // does not expect the file to be there. libgit2 has no notion of one: the
    // file is missing, so it is deleted.
    let outside = unstaged(Change::Deleted)
        .filter(|file| {
            index.get_path(Path::new(&file.path), 0).is_some_and(|entry| {
                git2::IndexEntryExtendedFlag::from_bits_truncate(entry.flags_extended)
                    .is_skip_worktree()
            })
        })
        .count();
    if outside > 0 {
        caveats.push(format!(
            "{outside} of the files listed as deleted {} outside this sparse checkout and {} \
             not been deleted. omarepos reads the working tree through libgit2, which does not \
             know about sparse checkouts; `git status` in a terminal has the true list.",
            if outside == 1 { "is" } else { "are" },
            if outside == 1 { "has" } else { "have" },
        ));
    }

    // A filter (git-lfs, git-crypt) or a working-tree encoding: what is stored
    // is not what is in the file, and git converts before it compares.
    // libgit2 runs none of them, so when it has to compare content it
    // compares the two forms, finds them different, and shows the difference.
    let mut through: Vec<String> = Vec::new();
    let mut filtered = 0usize;
    for file in unstaged(Change::Modified) {
        let attribute = |name: &str| {
            let value = repo.get_attr(Path::new(&file.path), name, git2::AttrCheckFlags::default());
            match git2::AttrValue::from_string(value.ok().flatten()) {
                git2::AttrValue::String(driver) => Some(driver.to_string()),
                _ => None,
            }
        };
        let Some(driver) = attribute("filter").or_else(|| attribute("working-tree-encoding")) else {
            continue;
        };
        filtered += 1;
        if !through.contains(&driver) {
            through.push(driver);
        }
    }
    if filtered > 0 {
        caveats.push(format!(
            "{filtered} of the files listed as modified {} converted by git on the way in and \
             out ({}), which libgit2 does not do: {} may be unchanged, and the diff shown may \
             be of the stored form against the file. `git status` in a terminal settles it.",
            if filtered == 1 { "is" } else { "are" },
            through.join(", "),
            if filtered == 1 { "it" } else { "they" },
        ));
    }
    caveats
}

/// What building one delta's patch found out.
#[derive(Default)]
struct Measured {
    adds: usize,
    dels: usize,
    binary: bool,
    /// The patch could not be built. That is not "no lines changed".
    unreadable: bool,
}

/// One delta's added and removed lines, whether the file is binary, and
/// whether it could be read at all.
///
/// libgit2 finds out that a file is binary while it builds the patch, and
/// marks the delta then. The flag used to be read one statement before the
/// patch was built, so it was false for every file: the list never said
/// "binary", and an image was a row with a status letter and an empty cell,
/// which is also what a file that was only renamed looks like.
///
/// And a patch that could not be built used to be counted as one with no
/// lines in it. In a clone made with `--filter=blob:none` that is every
/// commit older than the checkout: each read "+0 −0", as a fact.
fn measure(diff: &Diff<'_>, index: usize) -> Measured {
    let patch = Patch::from_diff(diff, index);
    let binary = diff.get_delta(index).is_some_and(|delta| {
        delta.flags().is_binary() || delta.new_file().is_binary() || delta.old_file().is_binary()
    });
    match patch {
        Ok(Some(patch)) if !binary => {
            let (_, adds, dels) = patch.line_stats().unwrap_or((0, 0, 0));
            Measured { adds, dels, ..Measured::default() }
        }
        Ok(_) => Measured { binary, ..Measured::default() },
        Err(_) => Measured { unreadable: true, ..Measured::default() },
    }
}

/// Whether a file in the working tree is binary, by the rule git and libgit2
/// share: a NUL among its first 8000 bytes. For untracked files, which are
/// listed without a patch being built. Only a regular file is opened: a
/// symbolic link is not followed to whatever it names, which may be a pipe
/// that never answers.
fn looks_binary(path: &Path) -> bool {
    use std::io::Read;
    if !std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_file()) {
        return false;
    }
    let mut start = Vec::new();
    std::fs::File::open(path)
        .and_then(|file| file.take(8000).read_to_end(&mut start))
        .is_ok_and(|_| start.contains(&0))
}

/// Turn a diff's deltas into file changes, assigning each to a group. The group
/// passed in is the default; untracked and conflicted entries say so themselves.
fn collect(
    diff: &Diff<'_>,
    default_group: Group,
    cancel: &Cancel,
    files: &mut Vec<FileChange>,
    insertions: &mut usize,
    deletions: &mut usize,
) {
    for (index, delta) in diff.deltas().enumerate() {
        if cancel.stopped() {
            return;
        }
        let group = match delta.status() {
            git2::Delta::Untracked => Group::Untracked,
            // Both diffs report a conflicted path, and the first has already
            // listed it. Twice in the list reads as two files.
            git2::Delta::Conflicted if default_group == Group::Unstaged => continue,
            git2::Delta::Conflicted => Group::Conflicted,
            _ => default_group,
        };
        let Measured { adds, dels, binary, unreadable } = if group == Group::Untracked {
            // An untracked file has nothing to diff against; counting its whole
            // length as additions would drown the totals. Nor is it read here
            // to see whether it is binary: there can be thousands of them and
            // the list keeps a few hundred. The ones it keeps are looked at
            // once the cap has fallen.
            Measured::default()
        } else {
            measure(diff, index)
        };
        *insertions += adds;
        *deletions += dels;

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
            from: (matches!(change, Change::Renamed | Change::Copied) && !old_path.is_empty())
                .then_some(old_path),
            change,
            insertions: adds,
            deletions: dels,
            binary,
            unreadable,
            group,
        });
    }
}

fn patch_for(path: &Path, id: &str, file: &str, from: Option<&str>) -> Result<FileDiff, String> {
    let repo = Repository::open(path).map_err(|e| e.message().to_string())?;
    let oid = Oid::from_str(id).map_err(|e| e.message().to_string())?;
    let commit = repo.find_commit(oid).map_err(|e| e.message().to_string())?;
    let tree = commit.tree().map_err(|e| unreadable(&repo, &e))?;
    let parent_tree = parent_tree(&repo, &commit)?;

    // Narrowed to the one file, and with real context this time — the commit
    // summary asks for counts, this asks to be read.
    let mut opts = DiffOptions::new();
    opts.context_lines(3).include_typechange(true).pathspec(file).disable_pathspec_match(true);
    // A renamed file is a delete and an add until the two are compared, and
    // they can only be compared if both are in the diff. Narrowed to the new
    // name alone, the old one never was: the list said `R +1 −1` and the diff
    // beside it showed the whole file as added, with the one line that had
    // changed somewhere in it.
    if let Some(from) = from {
        opts.pathspec(from);
    }
    let mut diff = repo
        .diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), Some(&mut opts))
        .map_err(|e| unreadable(&repo, &e))?;
    if from.is_some() {
        // The same detection the list ran, so the two cannot disagree.
        let mut finds = DiffFindOptions::new();
        finds.renames(true).copies(true);
        let _ = diff.find_similar(Some(&mut finds));
    }

    let mut out = FileDiff::default();
    append_patch(&repo, &diff, Some(file), &mut out)?;
    Ok(out)
}

/// The uncommitted diff of one file, including an untracked one — which has no
/// blob to compare against, so git is asked to show its content as added.
/// One file's uncommitted diff. `side` is the row that asked: a file that has
/// been staged and then edited again has two rows, and each shows its own
/// half. The Staged row is what Commit will take, and it used to be shown
/// with the later edits appended, so what was reviewed was not what went in.
/// With no side given, or for a conflict, both halves are shown.
fn patch_working(path: &Path, file: &str, side: Option<Group>) -> Result<FileDiff, String> {
    let repo = crate::git::open_here(path)?;
    let index = repo.index().map_err(|e| e.message().to_string())?;

    let mut out = FileDiff::default();
    let mut opts = DiffOptions::new();
    opts.context_lines(3)
        .include_typechange(true)
        .include_untracked(true)
        .recurse_untracked_dirs(true)
        .show_untracked_content(true)
        .ignore_submodules(true)
        .pathspec(file)
        // The name is a name. Taken as a pattern, `app/[id]/page.tsx` also
        // matched `app/i/page.tsx`, and the diff shown was of another file.
        .disable_pathspec_match(true);

    let staged = !matches!(side, Some(Group::Unstaged) | Some(Group::Untracked));
    let unstaged = !matches!(side, Some(Group::Staged));

    let head_tree = head_tree(&repo)?;
    let mut staged_opts = DiffOptions::new();
    staged_opts
        .context_lines(3)
        .include_typechange(true)
        .pathspec(file)
        .disable_pathspec_match(true);
    if staged {
        let diff = repo
            .diff_tree_to_index(head_tree.as_ref(), Some(&index), Some(&mut staged_opts))
            .map_err(|e| unreadable(&repo, &e))?;
        append_patch(&repo, &diff, None, &mut out)?;
    }
    if unstaged {
        let diff = repo
            .diff_index_to_workdir(Some(&index), Some(&mut opts))
            .map_err(|e| unreadable(&repo, &e))?;
        append_patch(&repo, &diff, None, &mut out)?;
    }
    Ok(out)
}

/// Add what `diff` holds to `out`, as lines. `only` names the one file wanted
/// from a diff that had to take in another: where a renamed or copied file
/// came from, which is a delta of its own when it is still there.
///
/// A patch that cannot be built is an error. It used to be skipped, and a
/// diff with nothing in it is shown as "No textual changes.": a statement
/// about the file, made of a failure to read it.
fn append_patch(
    repo: &Repository,
    diff: &Diff<'_>,
    only: Option<&str>,
    out: &mut FileDiff,
) -> Result<(), String> {
    for index in 0..diff.deltas().len() {
        let Some(delta) = diff.get_delta(index) else { continue };
        let named = delta.new_file().path().or(delta.old_file().path());
        if only.is_some_and(|only| !named.is_some_and(|path| path.to_string_lossy() == only)) {
            continue;
        }
        // libgit2 gives an unmerged path no content on either side, so there
        // are never lines to show for one, and that is not "nothing changed"
        // either: it is the one file here that most needs reading.
        if delta.status() == git2::Delta::Conflicted {
            out.note = Some(
                "Conflicted. Resolve it in an editor or a terminal: omarepos shows no diff for \
                 a file in conflict, and will not stage one."
                    .to_string(),
            );
            continue;
        }
        let patch = Patch::from_diff(diff, index).map_err(|err| unreadable(repo, &err))?;
        let binary =
            delta.flags().is_binary() || delta.new_file().is_binary() || delta.old_file().is_binary();
        let Some(patch) = patch else {
            out.binary |= binary;
            continue;
        };
        if patch.num_hunks() == 0 {
            // Read now that the patch exists: it is building the patch that
            // tells libgit2 whether a file is binary.
            out.binary |= binary;
            let moved = match delta.status() {
                git2::Delta::Renamed => Some("Renamed"),
                git2::Delta::Copied => Some("Copied"),
                _ => None,
            };
            // No lines, and not nothing: the file is under another name.
            if let (false, Some(moved), Some(from)) = (binary, moved, delta.old_file().path()) {
                out.note = Some(format!("{moved} from {}. Its content is unchanged.", from.display()));
            }
        }
        for hunk_index in 0..patch.num_hunks() {
            let Ok((hunk, lines_in_hunk)) = patch.hunk(hunk_index) else { continue };
            out.lines.push(Line {
                origin: '@',
                text: String::from_utf8_lossy(hunk.header()).trim_end().to_string(),
            });
            for line_index in 0..lines_in_hunk {
                if out.lines.len() >= MAX_LINES || out.bytes >= MAX_BYTES {
                    out.truncated = true;
                    return Ok(());
                }
                let Ok(line) = patch.line_in_hunk(hunk_index, line_index) else { continue };
                let text = shown(line.content());
                out.bytes += text.len();
                out.lines.push(Line { origin: line.origin(), text });
            }
        }
    }
    Ok(())
}

/// Exactly what a push of the checked-out branch would send, and where.
///
/// A bare `git push` leaves all of this to be decided at the moment it runs:
/// by whatever HEAD is by then, and by `push.default`, `push.followTags`,
/// `remote.<name>.push` and friends. The button, meanwhile, was drawn from a
/// page that may be minutes old. So the plan is worked out from the repository,
/// shown, and then handed to git spelled out in full — one branch, one
/// destination, no tags — after checking that the repository still gives the
/// same plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushPlan {
    pub branch: String,
    /// The commit at the branch's tip, in full: what would be sent.
    pub tip: String,
    pub remote: String,
    /// The ref on the remote, in full, e.g. `refs/heads/main`.
    pub destination: String,
    /// The branch has never been pushed: it has no upstream, and the remote
    /// has no branch of its name. Publishing is a different act from
    /// pushing — it is what makes the branch visible — and is named
    /// differently in the UI. It is the only time an upstream is set.
    pub publish: bool,
    /// `push.recurseSubmodules` asks git to push submodules along with (or
    /// instead of) this branch. That would be another repository's branch, so
    /// it is turned down to a check: git still refuses when a submodule commit
    /// this push refers to is not on its remote, and pushes nothing extra.
    pub check_submodules: bool,
}

/// Work out what a push would do, or say why there will not be one.
///
/// Every refusal here is about where the push would go; whether there is
/// anything to send is [`why_not_push`]'s question.
pub fn push_plan(repo: &Repository) -> Result<PushPlan, String> {
    let Ok(head) = repo.head() else {
        return Err("Nothing has been committed yet.".into());
    };
    if repo.head_detached().unwrap_or(false) {
        return Err("Detached HEAD — there is no branch to push from.".into());
    }
    let (Ok(branch), Some(tip)) = (head.shorthand(), head.target()) else {
        return Err("HEAD does not name a branch.".into());
    };
    let branch = branch.to_string();
    // Reachable: cloning a remote whose default branch is called `--mirror`
    // produces one. After `--` git would not take it for an option, but a
    // name like that is nobody's ordinary branch.
    if branch.starts_with('-') {
        return Err(format!(
            "A branch named {branch} reads like an option to git. Push it from a terminal."
        ));
    }

    let refname = format!("refs/heads/{branch}");
    let lossy = |buf: git2::Buf| String::from_utf8_lossy(&buf).to_string();
    let tracked = match (
        repo.branch_upstream_remote(&refname),
        repo.branch_upstream_merge(&refname),
    ) {
        (Ok(remote), Ok(merge)) => Some((lossy(remote), lossy(merge))),
        _ => None,
    };
    let (remote, destination, publish) = match tracked {
        Some((remote, merge)) => (remote, merge, false),
        None => match crate::graph::only_remote(repo) {
            // No upstream is not the same as never published. A branch cut
            // with `--no-track` from the remote's, or pushed once without
            // `-u`, has no upstream either, and the remote has a branch of
            // its name: "Publish" there lands the local commits on a branch
            // other people may already be using, under a word that says a
            // new one is being made.
            Some(remote) if repo.find_reference(&format!("refs/remotes/{remote}/{branch}")).is_ok() => {
                return Err(format!(
                    "{remote}/{branch} already exists and {branch} does not track it, so this \
                     would not be publishing a new branch. Set the upstream from a terminal: \
                     git branch --set-upstream-to={remote}/{branch} {branch}"
                ));
            }
            Some(remote) => (remote, refname.clone(), true),
            None => {
                return Err(
                    "This branch has no upstream, and there is no single remote to publish it to."
                        .into(),
                );
            }
        },
    };
    if remote == "." {
        return Err(format!("{branch} tracks a local branch, so there is nowhere to push it."));
    }
    // A branch can track a branch of another name: `git switch -c fix
    // origin/main` sets that up without being asked. The upstream is then
    // where the branch pulls from, not where it belongs, and git's own default
    // refuses the push for that reason. Spelling the destination out, as this
    // plan does, would walk straight past that refusal and put the branch's
    // commits on `main`.
    if !publish && destination != refname {
        return Err(format!(
            "{branch} tracks {remote}/{}, a branch of another name, and omarepos will not push \
             one branch onto another. Publish it under its own name from a terminal: \
             git push -u {remote} {branch}",
            destination.trim_start_matches("refs/heads/")
        ));
    }
    if remote.starts_with('-') || !destination.starts_with("refs/heads/") {
        return Err(format!(
            "{branch} tracks {remote} {destination}, which omarepos will not push to. \
             Push it from a terminal."
        ));
    }
    // An upstream that is configured and cannot be found. Either the branch
    // was deleted on the remote and pruned here, which is how a merged pull
    // request ends, or this clone does not fetch it (`--single-branch`, and
    // so `--depth`). There is then nothing to count ahead and behind against,
    // and a push would create the branch: for the first of those, a branch
    // somebody deleted on purpose. Whether to bring it back is not a button.
    let found = repo
        .find_branch(&branch, git2::BranchType::Local)
        .and_then(|local| local.upstream())
        .is_ok();
    if !publish && !found {
        let gone = destination.trim_start_matches("refs/heads/");
        return Err(format!(
            "The upstream of {branch}, {remote}/{gone}, is gone: deleted on {remote}, or not \
             fetched by this clone. A push would create it again, and that is for a terminal: \
             git push -u {remote} {branch}"
        ));
    }

    let config = repo.config().map_err(|err| err.message().to_string())?;
    // The fork workflow: follow one remote, push to another. Sending the
    // branch to the remote it tracks would be the one place the user has said
    // they do not push; sending it to the other would not be what the button
    // says. Neither is this application's call.
    let elsewhere = config
        .get_string(&format!("branch.{branch}.pushRemote"))
        .or_else(|_| config.get_string("remote.pushDefault"))
        .ok()
        .filter(|other| *other != remote);
    if let Some(other) = elsewhere {
        return Err(format!(
            "{branch} tracks {remote} but is set to push to {other}. omarepos only pushes a \
             branch to the remote it tracks. Push this one from a terminal."
        ));
    }
    if config.get_bool(&format!("remote.{remote}.mirror")).unwrap_or(false) {
        return Err(format!(
            "{remote} is a mirror: any push to it sends, and overwrites, every ref. \
             Push from a terminal."
        ));
    }
    let check_submodules = matches!(
        config.get_string("push.recurseSubmodules").as_deref(),
        Ok("on-demand") | Ok("only")
    );

    Ok(PushPlan { branch, tip: tip.to_string(), remote, destination, publish, check_submodules })
}

/// Why a push that has somewhere to go still cannot go ahead, as a sentence.
/// Checked before invoking git so the user gets an explanation rather than a
/// rejection from the far end.
pub fn why_not_push(plan: &PushPlan, ahead: usize, behind: usize) -> Option<String> {
    if plan.publish {
        return None;
    }
    if behind > 0 {
        return Some(format!(
            "{behind} commits are waiting on the upstream. Fast-forward first — a push would be rejected."
        ));
    }
    if ahead == 0 {
        return Some("Nothing to push.".into());
    }
    None
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PushError {
    /// The repository is no longer as the button showed it: HEAD is on another
    /// branch, the branch has new commits, or where it pushes to has changed.
    /// Nothing was sent. The page wants reading again.
    Stale(String),
    /// git, the remote or a hook said no, in these words.
    Git(String),
}

/// Push the checked-out branch as `armed` describes, or not at all. Never
/// forced, never `--all`, never tags, never a second branch: the argument list
/// names the one source and the one destination, so no `push.*` setting gets a
/// say in what is sent.
pub fn spawn_push(
    repo: PathBuf,
    armed: PushPlan,
) -> async_channel::Receiver<Result<String, PushError>> {
    let (tx, rx) = async_channel::bounded(1);
    std::thread::spawn(move || {
        let _ = tx.send_blocking(push(&repo, &armed));
    });
    rx
}

fn push(repo: &Path, armed: &PushPlan) -> Result<String, PushError> {
    // Ask again. The button was drawn from a page that sat open while a
    // terminal was one key away; what it promised is only sent if it is still
    // what the repository says.
    let opened = crate::git::open_here(repo);
    let now = opened.as_ref().map_err(String::clone).and_then(push_plan);
    match now {
        // Still the same plan. Whether there is still anything to send is not
        // part of a plan: the button counted the commits ahead when the page
        // was read, and a push from a terminal since leaves the branch where
        // it was with nothing ahead. git answers a push of nothing with
        // success, and "Pushed main to origin" was said of one that sent
        // nothing at all.
        Ok(now) if now == *armed => {
            let there = opened
                .as_ref()
                .ok()
                .filter(|_| !armed.publish)
                .and_then(|repo| {
                    let tracking = repo.branch_upstream_name(&format!("refs/heads/{}", armed.branch)).ok()?;
                    repo.refname_to_id(tracking.as_str().ok()?).ok()
                })
                .map(|id| id.to_string());
            if there.as_deref() == Some(armed.tip.as_str()) {
                return Err(PushError::Stale(format!(
                    "{} already has {} at this commit. Nothing was pushed.",
                    armed.remote, armed.branch
                )));
            }
        }
        Ok(now) if now.branch != armed.branch => {
            return Err(PushError::Stale(format!(
                "HEAD is on {} now, not {}. Nothing was pushed.",
                now.branch, armed.branch
            )));
        }
        Ok(now) if now.tip != armed.tip => {
            return Err(PushError::Stale(format!(
                "{} has moved since this page was read. Nothing was pushed.",
                armed.branch
            )));
        }
        Ok(_) => {
            return Err(PushError::Stale(format!(
                "Where {} pushes to has changed since this page was read. Nothing was pushed.",
                armed.branch
            )));
        }
        Err(why) => return Err(PushError::Stale(format!("{why} Nothing was pushed."))),
    }
    // The plan above is libgit2's reading of this directory. The push is
    // git's, and git can be told to look elsewhere.
    acts_here(repo).map_err(PushError::Git)?;

    let refspec = format!("refs/heads/{}:{}", armed.branch, armed.destination);
    let mut args = vec!["push", "--no-follow-tags"];
    if armed.check_submodules {
        args.push("--recurse-submodules=check");
    }
    if armed.publish {
        args.push("--set-upstream");
    }
    args.extend(["--", armed.remote.as_str(), refspec.as_str()]);

    // A pre-push hook can be a test suite, so the same patience as a commit.
    crate::gitcmd::run(repo, &args, WRITE_TIMEOUT)
        .map(|_| {
            let verb = if armed.publish { "Published" } else { "Pushed" };
            format!("{verb} {} to {}", armed.branch, armed.remote)
        })
        .map_err(PushError::Git)
}

/// A node in the tree the pane draws.
pub enum Node {
    /// A directory, with the path segment(s) it stands for. Single-child chains
    /// are collapsed into one node, so `src/Api/One/Interfaces` is one row
    /// rather than four.
    Dir { name: String, depth: usize },
    /// `file.path` stays the full path, since that is what a diff needs;
    /// `display` is the part worth putting on screen under its directory.
    File {
        file: FileChange,
        display: String,
        depth: usize,
    },
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
            let display = file.path.rsplit('/').next().unwrap_or(&file.path).to_string();
            out.push(Node::File { file: file.clone(), display, depth });
        }
    }

    let mut out = Vec::new();
    walk(&root, String::new(), 0, &mut out);
    out
}

/// How long a `git` write is given. Generous, because a `pre-commit` hook can
/// be a test suite.
const WRITE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// The variables through which whatever started the application can send git
/// to another repository than the one it is run in.
const REDIRECTS: [&str; 5] =
    ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_OBJECT_DIRECTORY", "GIT_COMMON_DIR"];

/// Make sure a git run in `dir` would act on the repository in `dir`.
///
/// Every write here names its repository by the directory git is run in, and
/// git lets its environment outrank that. libgit2, which reads everything on
/// screen and makes every check before a write, does not look at the
/// environment at all. So with `GIT_DIR` inherited from wherever the
/// application was started (a shell inside a linked worktree's `!` alias or
/// hook, a dotfiles wrapper, `git --git-dir=… <something that launches this>`),
/// the pane showed one repository while stage, commit and push went to
/// another, and said they had worked.
///
/// The question is put to git itself, through the runner the write will use,
/// so that the answer is about what that write would do: a relative
/// `GIT_INDEX_FILE=.git/index`, which a hook's environment carries, points at
/// this repository's own index and is let through, and a runner that clears
/// these variables makes the answer "here" without this function changing.
/// Nothing is asked when none of them is set, which is every ordinary start.
fn acts_here(dir: &Path) -> Result<(), String> {
    let set: Vec<&str> =
        REDIRECTS.into_iter().filter(|name| std::env::var_os(name).is_some()).collect();
    if set.is_empty() {
        return Ok(());
    }
    let refusal = |what: String| {
        format!(
            "omarepos was started with {} set, and git obeys that before the directory it is \
             run in. {what} Nothing was done. Start omarepos from an environment without {}.",
            set.join(" and "),
            if set.len() == 1 { "it" } else { "them" }
        )
    };

    let repo = crate::git::open_here(dir)?;
    let index = repo.index().map_err(|err| err.message().to_string())?;
    let objects = repo.commondir().join("objects");
    let here = [Some(repo.path()), repo.workdir(), index.path(), Some(objects.as_path())];
    let asked = [
        "rev-parse",
        "--path-format=absolute",
        "--git-dir",
        "--show-toplevel",
        "--git-path",
        "index",
        "--git-path",
        "objects",
    ];
    let said = crate::gitcmd::run(dir, &asked, std::time::Duration::from_secs(5))
        .map_err(|why| refusal(format!("Asked where it would act, git said: {why}")))?;
    let there: Vec<&Path> = said.lines().map(Path::new).collect();
    if there.len() != here.len() {
        return Err(refusal("git would not say where it would act.".to_string()));
    }
    for (there, here) in there.into_iter().zip(here) {
        let Some(here) = here else { continue };
        if resolved(there) != resolved(here) {
            return Err(refusal(format!(
                "It would act on {} and not on {}.",
                there.display(),
                here.display()
            )));
        }
    }
    Ok(())
}

/// A path with its symbolic links followed, for comparing two spellings of
/// one place. The last part need not exist: a repository has no index file
/// until something is staged in it.
fn resolved(path: &Path) -> PathBuf {
    if let Ok(whole) = path.canonicalize() {
        return whole;
    }
    match (path.parent().map(Path::canonicalize), path.file_name()) {
        (Some(Ok(parent)), Some(name)) => parent.join(name),
        _ => path.to_path_buf(),
    }
}

/// What a commit was refused for, as a sentence rather than a subprocess error.
pub fn why_not_commit(path: &Path, staged: usize, message: &str) -> Option<String> {
    if message.trim().is_empty() {
        return Some("Write a commit message first.".into());
    }
    if staged == 0 {
        return Some("Nothing is staged. Stage a file to commit it.".into());
    }
    let repo = match crate::git::open_here(path) {
        Ok(repo) => repo,
        Err(why) => return Some(why),
    };
    let state = repo.state();
    if state != git2::RepositoryState::Clean {
        // omarepos has no conflict story and must not pretend otherwise.
        return Some(format!(
            "This repository is in the middle of something ({state:?}). Finish or abort it in a terminal."
        ));
    }
    // A conflict does not always come with something in progress. `git stash
    // pop` and `checkout -m` leave unmerged entries and no MERGE_HEAD, so the
    // state above reads Clean with conflict markers sitting in the files.
    if repo.index().map(|index| index.has_conflicts()).unwrap_or(false) {
        return Some("There are unresolved conflicts here. Resolve them in a terminal first.".into());
    }
    if repo.head_detached().unwrap_or(false) {
        return Some(
            "Detached HEAD — a commit made here would belong to no branch. Commit from a terminal."
                .into(),
        );
    }
    // A commit on a branch that is behind leaves it diverged, and a diverged
    // branch is one this application can neither push nor fast-forward: the
    // ways out are a rebase or a merge, which it does not do. The pane used to
    // recommend exactly this commit as the way to unblock an update.
    if let Ok((_, behind)) = crate::refresh::divergence(&repo) {
        if behind > 0 {
            return Some(format!(
                "{behind} commits are waiting on the upstream. Update first (p): a commit made \
                 now would leave the branch diverged, which omarepos can neither push nor \
                 fast-forward."
            ));
        }
    }
    None
}

/// The first of `files` that the index holds as a conflict, if any.
fn conflicted_among(repo: &Path, files: &[String]) -> Option<String> {
    let index = Repository::open(repo).ok()?.index().ok()?;
    files
        .iter()
        .find(|file| (1..=3).any(|stage| index.get_path(Path::new(file.as_str()), stage).is_some()))
        .cloned()
}

/// Who a commit made here would be recorded as. Asked of the git that will
/// make it, because it is the only one that knows: libgit2 evaluates some
/// conditional includes and not others, and reads neither `author.*` nor the
/// environment git honours.
pub fn identity(repo: &Path) -> Result<String, String> {
    // Asked of another repository, the answer would be that one's.
    acts_here(repo)?;
    let said = crate::gitcmd::run(repo, &["var", "GIT_AUTHOR_IDENT"], std::time::Duration::from_secs(5))?;
    // "Name <email> 1791399981 +0200": the date is not part of who.
    match said.trim().rsplit_once('>') {
        Some((who, _)) => Ok(format!("{who}>")),
        None => Err("git gave no identity".to_string()),
    }
}

pub fn spawn_identity(repo: PathBuf) -> async_channel::Receiver<Result<String, String>> {
    let (tx, rx) = async_channel::bounded(1);
    std::thread::spawn(move || {
        let _ = tx.send_blocking(identity(&repo));
    });
    rx
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Write {
    Stage,
    Unstage,
}

/// Stage or unstage whole files. Never a hunk: see ADR-0004 — hunk staging is
/// the heart of a committing tool, and this is not one.
pub fn spawn_write(
    repo: PathBuf,
    what: Write,
    files: Vec<String>,
) -> async_channel::Receiver<Result<(), String>> {
    let (tx, rx) = async_channel::bounded(1);
    std::thread::spawn(move || {
        // `git add` is how git is told a conflict has been resolved, and
        // unstaging the path, before or after, throws the three-way record
        // away. Either would settle a merge by accident: the first commits the
        // conflict markers, the second silently keeps this side and drops the
        // other.
        if let Err(why) = crate::git::open_here(&repo).and_then(|_| acts_here(&repo)) {
            let _ = tx.send_blocking(Err(why));
            return;
        }
        if let Some(file) = conflicted_among(&repo, &files) {
            let _ = tx.send_blocking(Err(format!(
                "{file} is conflicted. Resolve it in a terminal: staging it here would record \
                 it as resolved, markers and all."
            )));
            return;
        }
        // An untracked directory that libgit2 will not look inside is another
        // repository. `git add` on it records a gitlink: a submodule with no
        // `.gitmodules`, which nobody asked for.
        if what == Write::Stage {
            if let Some(nested) = files.iter().find(|file| file.ends_with('/')) {
                let _ = tx.send_blocking(Err(format!(
                    "{nested} is another git repository. Add it as a submodule, or ignore it, \
                     from a terminal."
                )));
                return;
            }
        }
        // `--` stops git reading a name as an option; it does not stop git
        // reading it as a pattern. Staging `app/[id]/page.tsx` also staged
        // `app/i/page.tsx` and `app/d/page.tsx`, and a file called `*` staged
        // everything. Only for these two commands, and not through the
        // environment: a hook inherits that, and its own patterns stop
        // matching.
        let mut args: Vec<&str> = match what {
            Write::Stage => vec!["--literal-pathspecs", "add", "--"],
            // `reset` with paths, and not `restore --staged`, which this used
            // to be on the grounds that the two are the same thing. They are,
            // once there is a commit. `restore --staged` restores from HEAD,
            // and before the first commit, or on an orphan branch, there is
            // no HEAD: it fails with "could not resolve 'HEAD'" and a file
            // staged there could not be unstaged at all. `reset` takes the
            // paths out of the index in that case. Either way the working
            // tree is not touched. (`rm --cached` would be the other way to
            // say it, and refuses a file that was edited after it was staged.)
            Write::Unstage => vec!["--literal-pathspecs", "reset", "-q", "--"],
        };
        args.extend(files.iter().map(String::as_str));
        let _ = tx.send_blocking(crate::gitcmd::run(&repo, &args, WRITE_TIMEOUT).map(|_| ()));
    });
    rx
}

/// Commit what is staged. `-m` only: no `-a`, no `--amend`, nothing that could
/// take in work the user has not looked at or rewrite what is already there.
///
/// The answer is the new commit's short id, to be shown, or nothing where it
/// could not be asked for: the commit is made by then, and failing to name it
/// must not be reported as failing to make it.
pub fn spawn_commit(
    repo: PathBuf,
    message: String,
) -> async_channel::Receiver<Result<String, String>> {
    let (tx, rx) = async_channel::bounded(1);
    std::thread::spawn(move || {
        if let Err(why) = crate::git::open_here(&repo).and_then(|_| acts_here(&repo)) {
            let _ = tx.send_blocking(Err(why));
            return;
        }
        let result = crate::gitcmd::run(&repo, &["commit", "-m", &message], WRITE_TIMEOUT).map(|_| {
            crate::gitcmd::run(&repo, &["rev-parse", "--short=8", "HEAD"], WRITE_TIMEOUT)
                .map(|sha| sha.trim().to_string())
                .unwrap_or_default()
        });
        let _ = tx.send_blocking(result);
    });
    rx
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::*;

    fn plan(f: &Fleet) -> Result<PushPlan, String> {
        push_plan(&Repository::open(&f.work).unwrap())
    }

    // ---- staging takes the file that was pointed at, and only that ----------

    fn staged(dir: &Path) -> String {
        git(dir, &["diff", "--cached", "--name-only"])
    }

    /// Route folders in Next.js, SvelteKit and Remix are named like patterns.
    #[test]
    fn a_file_name_is_a_name_and_not_a_pattern() {
        let _guard = isolate();
        let dir = scratch("stage-literal");
        git(&dir, &["init", "-q", "-b", "main"]);
        commit_file(&dir, "README.md", "one\n", "first");
        for name in ["app/[id]/page.tsx", "app/i/page.tsx", "app/d/page.tsx", "*", "notes.txt"] {
            write(&dir.join(name), "x\n");
        }

        recv(spawn_write(dir.clone(), Write::Stage, vec!["app/[id]/page.tsx".to_string()])).unwrap();
        assert_eq!(staged(&dir), "app/[id]/page.tsx");

        recv(spawn_write(dir.clone(), Write::Stage, vec!["*".to_string()])).unwrap();
        assert_eq!(staged(&dir), "*\napp/[id]/page.tsx", "a file called * staged everything");

        recv(spawn_write(dir.clone(), Write::Unstage, vec!["*".to_string()])).unwrap();
        assert_eq!(staged(&dir), "app/[id]/page.tsx");

        // And the diff shown for it is its own.
        let diff = patch_working(&dir, "app/[id]/page.tsx", Some(Group::Staged)).unwrap();
        assert_eq!(diff.lines.iter().filter(|line| line.origin == '+').count(), 1);
    }

    /// `git init`, add the files, think again about one: the first commit of a
    /// project. `git restore --staged` needs a HEAD to restore from, and
    /// before the first commit there is none.
    #[test]
    fn a_file_staged_before_the_first_commit_can_be_unstaged() {
        let _guard = isolate();
        let dir = scratch("cm-unstage-unborn");
        git(&dir, &["init", "-q", "-b", "main"]);
        write(&dir.join("README.md"), "one\n");
        write(&dir.join("notes.txt"), "staged\n");
        git(&dir, &["add", "README.md", "notes.txt"]);
        write(&dir.join("notes.txt"), "staged\nand edited since\n");
        assert_eq!(git(&dir, &["status", "--porcelain"]), "A  README.md\nAM notes.txt");

        recv(spawn_write(dir.clone(), Write::Unstage, vec!["README.md".to_string()])).unwrap();
        assert_eq!(git(&dir, &["status", "--porcelain"]), "AM notes.txt\n?? README.md");

        // Staged and then edited: `git rm --cached` refuses this one. The
        // later edit is the user's, and stays where it is.
        recv(spawn_write(dir.clone(), Write::Unstage, vec!["notes.txt".to_string()])).unwrap();
        assert_eq!(git(&dir, &["status", "--porcelain"]), "?? README.md\n?? notes.txt");
        assert_eq!(read(&dir.join("notes.txt")), "staged\nand edited since\n");

        // An orphan branch has history behind it and still no HEAD to name.
        git(&dir, &["add", "README.md"]);
        git(&dir, &["commit", "-q", "-m", "first"]);
        git(&dir, &["checkout", "-q", "--orphan", "fresh"]);
        assert_eq!(staged(&dir), "README.md");
        recv(spawn_write(dir.clone(), Write::Unstage, vec!["README.md".to_string()])).unwrap();
        assert_eq!(staged(&dir), "");
        assert_eq!(read(&dir.join("README.md")), "one\n");
    }

    /// With a HEAD, unstaging does what it always did, for each kind of
    /// staged change.
    #[test]
    fn unstaging_puts_the_index_back_and_leaves_the_files_alone() {
        let _guard = isolate();
        let dir = scratch("cm-unstage-born");
        git(&dir, &["init", "-q", "-b", "main"]);
        commit_file(&dir, "edited.txt", "one\n", "first");
        commit_file(&dir, "gone.txt", "one\n", "second");
        write(&dir.join("edited.txt"), "one\ntwo\n");
        write(&dir.join("new.txt"), "new\n");
        std::fs::remove_file(dir.join("gone.txt")).unwrap();
        git(&dir, &["add", "-A"]);
        write(&dir.join("edited.txt"), "one\ntwo\nthree\n");
        assert_eq!(git(&dir, &["status", "--porcelain"]), "MM edited.txt\nD  gone.txt\nA  new.txt");

        let all = ["edited.txt", "gone.txt", "new.txt"].map(str::to_string).to_vec();
        recv(spawn_write(dir.clone(), Write::Unstage, all)).unwrap();

        assert_eq!(git(&dir, &["status", "--porcelain"]), "M edited.txt\n D gone.txt\n?? new.txt");
        assert_eq!(read(&dir.join("edited.txt")), "one\ntwo\nthree\n");
        assert!(!dir.join("gone.txt").exists(), "the deletion is the user's too");
    }

    #[test]
    fn a_repository_inside_the_repository_is_not_staged_as_a_gitlink() {
        let _guard = isolate();
        let dir = scratch("stage-nested");
        git(&dir, &["init", "-q", "-b", "main"]);
        commit_file(&dir, "README.md", "one\n", "first");
        let inner = dir.join("vendor/thing");
        std::fs::create_dir_all(&inner).unwrap();
        git(&inner, &["init", "-q", "-b", "main"]);
        commit_file(&inner, "a.txt", "a\n", "inner");

        let listed = load_working(&dir, &Cancel::default()).unwrap();
        let nested = listed.files.iter().find(|f| f.path.ends_with('/')).expect("listed as a directory");
        let said = recv(spawn_write(dir.clone(), Write::Stage, vec![nested.path.clone()]));
        assert!(said.unwrap_err().contains("another git repository"));
        assert_eq!(staged(&dir), "");
    }

    /// Stage a file, go on editing it: two rows, and each shows its own half.
    #[test]
    fn the_staged_row_shows_what_commit_will_take_and_nothing_else() {
        let _guard = isolate();
        let dir = scratch("stage-two-halves");
        git(&dir, &["init", "-q", "-b", "main"]);
        commit_file(&dir, "a.txt", "one\n", "first");
        write(&dir.join("a.txt"), "one\nstaged\n");
        git(&dir, &["add", "a.txt"]);
        write(&dir.join("a.txt"), "one\nstaged\nlater\n");

        let added = |side| -> Vec<String> {
            patch_working(&dir, "a.txt", side)
                .unwrap()
                .lines
                .into_iter()
                .filter(|line| line.origin == '+')
                .map(|line| line.text)
                .collect()
        };
        assert_eq!(added(Some(Group::Staged)), ["staged"]);
        assert_eq!(added(Some(Group::Unstaged)), ["later"]);
        assert_eq!(added(None), ["staged", "later"]);
    }

    /// One unignored build directory, and the one file that matters.
    #[test]
    fn the_cap_falls_on_untracked_files_and_the_counts_are_taken_first() {
        let _guard = isolate();
        let dir = scratch("working-cap");
        git(&dir, &["init", "-q", "-b", "main"]);
        commit_file(&dir, "zz-last.txt", "one\n", "first");
        write(&dir.join("zz-last.txt"), "edited\n");
        for i in 0..(MAX_FILES + 50) {
            write(&dir.join(format!("build/out-{i:04}.o")), "x\n");
        }

        let listed = load_working(&dir, &Cancel::default()).unwrap();

        assert_eq!(listed.counts.unstaged, 1);
        assert_eq!(listed.counts.untracked, MAX_FILES + 50);
        assert!(listed.counts.blocks_update());
        assert_eq!(listed.files.len(), MAX_FILES);
        assert!(
            listed.files.iter().any(|f| f.path == "zz-last.txt"),
            "the modified file was cut off the end of the list"
        );
    }

    #[test]
    fn a_submodule_left_at_another_commit_is_not_offered_for_staging() {
        let _guard = isolate();
        let f = fleet("working-submodule");
        let lib = fleet("working-submodule-lib");
        git(&f.work, &["submodule", "add", "-q", lib.remote.to_str().unwrap(), "lib"]);
        git(&f.work, &["commit", "-q", "-m", "add lib"]);
        commit_file(&f.work.join("lib"), "more.txt", "x\n", "lib moves on");
        assert!(git(&f.work, &["status", "--porcelain"]).contains("lib"), "git does see it");

        let listed = load_working(&f.work, &Cancel::default()).unwrap();
        let rows: Vec<&str> = listed.files.iter().map(|f| f.path.as_str()).collect();
        assert!(rows.is_empty(), "{rows:?}");
    }

    #[test]
    fn the_identity_is_the_one_git_will_record() {
        let _guard = isolate();
        let f = fleet("commit-identity");
        assert_eq!(identity(&f.work), Ok("Fixture <fixture@example.invalid>".to_string()));
        git(&f.work, &["config", "author.name", "Somebody Else"]);
        assert_eq!(identity(&f.work), Ok("Somebody Else <fixture@example.invalid>".to_string()));
    }

    // ---- where the list is libgit2's and not git's, it says so ----------------

    /// A sparse checkout: `git status` is clean, and libgit2, which has never
    /// heard of one, reports every file outside it as deleted. The card, the
    /// row count and the refresher take the same reading, so the rows stay;
    /// what the pane can do is say that they are not what they look like.
    #[test]
    fn a_sparse_checkout_is_told_that_its_deletions_are_not_deletions() {
        let _guard = isolate();
        let f = fleet("cm-sparse");
        f.remote_commits("two services", |seed| {
            write(&seed.join("services/api/a.txt"), "a\n");
            write(&seed.join("services/web/w.txt"), "w\n");
            write(&seed.join("services/web/x.txt"), "x\n");
        });
        git(&f.work, &["pull", "-q", "--ff-only"]);
        let ordinary = load_working(&f.work, &Cancel::default()).unwrap();
        assert!(ordinary.files.is_empty() && ordinary.caveats.is_empty(), "{:?}", ordinary.caveats);

        git(&f.work, &["sparse-checkout", "set", "--cone", "services/api"]);
        assert_eq!(git(&f.work, &["status", "--porcelain"]), "", "git calls it clean");
        // And one deletion that is real.
        std::fs::remove_file(f.work.join("services/api/a.txt")).unwrap();

        let listed = load_working(&f.work, &Cancel::default()).unwrap();
        assert_eq!(listed.counts.unstaged, 3, "libgit2's reading, which the card shares");
        assert_eq!(listed.caveats.len(), 1, "{:?}", listed.caveats);
        assert!(listed.caveats[0].starts_with("2 of the files listed as deleted"), "{}", listed.caveats[0]);
        assert!(listed.caveats[0].contains("sparse checkout"), "{}", listed.caveats[0]);
    }

    /// A stand-in for git-lfs or git-crypt: what is stored is not what is in
    /// the file. git runs the filter before comparing and libgit2 does not,
    /// so libgit2 can call a file modified that git calls clean.
    #[test]
    fn a_file_behind_a_filter_is_told_that_it_may_not_be_modified() {
        let _guard = isolate();
        let dir = scratch("cm-filter");
        git(&dir, &["init", "-q", "-b", "main"]);
        git(&dir, &["config", "filter.standin.clean", "tr a-z A-Z"]);
        git(&dir, &["config", "filter.standin.smudge", "tr A-Z a-z"]);
        write(&dir.join(".gitattributes"), "*.dat filter=standin\n");
        write(&dir.join("asset.dat"), "hello asset\n");
        write(&dir.join("plain.txt"), "one\n");
        git(&dir, &["add", "-A"]);
        git(&dir, &["commit", "-q", "-m", "first"]);
        assert_eq!(git(&dir, &["show", "HEAD:asset.dat"]), "HELLO ASSET", "stored in another form");

        write(&dir.join("plain.txt"), "one\ntwo\n");
        let listed = load_working(&dir, &Cancel::default()).unwrap();
        assert!(listed.caveats.is_empty(), "nothing filtered is listed: {:?}", listed.caveats);

        write(&dir.join("asset.dat"), "hello asset, edited\n");
        let listed = load_working(&dir, &Cancel::default()).unwrap();
        assert_eq!(listed.caveats.len(), 1, "{:?}", listed.caveats);
        assert!(listed.caveats[0].contains("standin"), "{}", listed.caveats[0]);
    }

    // ---- a write lands in the repository on screen, or nowhere ---------------

    /// A variable in this process's environment for as long as the value
    /// lives, taken out again however the test ends: left behind, it would
    /// send every later test's git to the wrong repository.
    struct Inherited(&'static str);

    impl Inherited {
        fn set(name: &'static str, value: &std::ffi::OsStr) -> Self {
            // SAFETY: under the isolation lock, like every reader of it.
            unsafe { std::env::set_var(name, value) };
            Self(name)
        }
    }

    impl Drop for Inherited {
        fn drop(&mut self) {
            // SAFETY: as above.
            unsafe { std::env::remove_var(self.0) };
        }
    }

    /// The application started from a shell that has `GIT_DIR` exported: a
    /// linked worktree's hook or `!` alias, a dotfiles wrapper. libgit2 goes
    /// on showing the repository asked for; git acts on the one named.
    ///
    /// Each write may be refused, or may be made in the repository on screen
    /// (which is what a runner that clears the variable gives). What it may
    /// not do is succeed somewhere else.
    #[test]
    fn an_inherited_git_dir_does_not_send_a_write_to_another_repository() {
        let _guard = isolate();
        let elsewhere = fleet("cm-env-elsewhere");
        let shown = fleet("cm-env-shown");
        // Something for a misdirected push and a misdirected commit to take.
        commit_file(&elsewhere.work, "ahead.txt", "x\n", "unpushed, elsewhere");
        write(&elsewhere.work.join("theirs.txt"), "theirs\n");
        git(&elsewhere.work, &["add", "theirs.txt"]);
        let (their_head, their_remote) = (elsewhere.head(), elsewhere.remote_ref("refs/heads/main"));
        // And what the pane shows: a file to stage, and a commit to push.
        commit_file(&shown.work, "mine.txt", "mine\n", "ready");
        write(&shown.work.join("secret.txt"), "secret\n");
        write(&shown.work.join("second.txt"), "second\n");
        git(&shown.work, &["add", "second.txt"]);
        let armed = plan(&shown).unwrap();
        let before = shown.head();

        let inherited = Inherited::set("GIT_DIR", elsewhere.work.join(".git").as_os_str());
        let staged_it = recv(spawn_write(shown.work.clone(), Write::Stage, vec!["secret.txt".to_string()]));
        let committed = recv(spawn_commit(shown.work.clone(), "second".to_string()));
        let pushed = push(&shown.work, &armed);
        let who = identity(&shown.work);
        drop(inherited);

        // Nothing of the repository on screen reached the other one.
        assert_eq!(elsewhere.head(), their_head, "the commit was made in another repository");
        assert_eq!(staged(&elsewhere.work), "theirs.txt", "the file was staged in another repository");
        assert_eq!(elsewhere.remote_ref("refs/heads/main"), their_remote, "another repository was pushed");
        // And each answer is true of the one on screen.
        assert_eq!(staged_it.is_ok(), staged(&shown.work).contains("secret.txt"), "{staged_it:?}");
        assert_eq!(committed.is_ok(), shown.head() != before, "{committed:?}");
        assert_eq!(pushed.is_ok(), shown.remote_ref("refs/heads/main") == armed.tip, "{pushed:?}");
        if let Err(why) = &staged_it {
            assert!(why.contains("GIT_DIR"), "a refusal has to name what is in the way: {why}");
        }
        assert!(who.is_err() || who == Ok("Fixture <fixture@example.invalid>".to_string()), "{who:?}");
    }

    /// What a `pre-commit` hook's environment carries: the index, by a path
    /// relative to the work tree. Run in the repository, that is the
    /// repository's own index, and nothing to refuse.
    #[test]
    fn an_inherited_variable_that_points_at_this_repository_is_let_through() {
        let _guard = isolate();
        let dir = scratch("cm-env-harmless");
        git(&dir, &["init", "-q", "-b", "main"]);
        commit_file(&dir, "README.md", "one\n", "first");
        write(&dir.join("notes.txt"), "x\n");

        let inherited = Inherited::set("GIT_INDEX_FILE", std::ffi::OsStr::new(".git/index"));
        let staged_it = recv(spawn_write(dir.clone(), Write::Stage, vec!["notes.txt".to_string()]));
        drop(inherited);

        assert_eq!(staged_it, Ok(()));
        assert_eq!(staged(&dir), "notes.txt");
    }

    // ---- nothing reaches through a redirected work tree --------------------

    #[test]
    fn another_directorys_files_cannot_be_listed_staged_or_committed() {
        let _guard = isolate();
        let f = fleet("commit-redirect");
        let elsewhere = scratch("commit-redirect-victim");
        write(&elsewhere.join("id_ed25519"), "PRIVATE\n");
        git(&f.work, &["config", "core.worktree", elsewhere.to_str().unwrap()]);

        assert!(load_working(&f.work, &Cancel::default()).is_err());
        assert!(patch_working(&f.work, "id_ed25519", None).is_err());
        let staged = recv(spawn_write(f.work.clone(), Write::Stage, vec!["id_ed25519".to_string()]));
        assert!(staged.is_err());
        assert!(why_not_commit(&f.work, 1, "exfiltrate").is_some());
        assert!(recv(spawn_commit(f.work.clone(), "exfiltrate".to_string())).is_err());

        git(&f.work, &["config", "--unset", "core.worktree"]);
        assert_eq!(git(&f.work, &["ls-files"]), "README.md", "something was staged");
        assert_eq!(git(&f.work, &["log", "--format=%s", "-1"]), "first", "something was committed");
    }

    // ---- a diff the view can survive ---------------------------------------

    /// A committed bundle: one line, most of a megabyte. libgit2 does not call
    /// this binary, and shown whole it froze the window.
    #[test]
    fn a_very_long_line_is_cut_and_says_so() {
        let _guard = isolate();
        let dir = scratch("diff-long-line");
        git(&dir, &["init", "-q", "-b", "main"]);
        commit_file(&dir, "bundle.min.js", &format!("var a={};\n", "x+".repeat(300_000)), "bundle");
        let head = git(&dir, &["rev-parse", "HEAD"]);

        let diff = patch_for(&dir, &head, "bundle.min.js", None).unwrap();

        let longest = diff.lines.iter().map(|line| line.text.len()).max().unwrap();
        assert!(longest < MAX_LINE_BYTES + 100, "a line of {longest} bytes reached the view");
        assert!(
            diff.lines.iter().any(|line| line.text.ends_with("more bytes on this line")),
            "a cut line has to say it was cut"
        );

        // The same file before it is committed takes the other path.
        write(&dir.join("other.min.js"), &format!("var b={};\n", "y+".repeat(300_000)));
        let untracked = patch_working(&dir, "other.min.js", None).unwrap();
        let longest = untracked.lines.iter().map(|line| line.text.len()).max().unwrap();
        assert!(longest < MAX_LINE_BYTES + 100, "{longest}");
    }

    #[test]
    fn a_diff_stops_at_its_byte_budget_as_well_as_its_line_count() {
        let _guard = isolate();
        let dir = scratch("diff-budget");
        git(&dir, &["init", "-q", "-b", "main"]);
        // Well under the line limit, several times over the byte budget.
        let body: String = (0..1_000).map(|i| format!("{i}:{}\n", "z".repeat(8_000))).collect();
        commit_file(&dir, "wide.txt", &body, "wide");
        let head = git(&dir, &["rev-parse", "HEAD"]);

        let diff = patch_for(&dir, &head, "wide.txt", None).unwrap();

        assert!(diff.truncated);
        let total: usize = diff.lines.iter().map(|line| line.text.len()).sum();
        assert!(total <= MAX_BYTES + MAX_LINE_BYTES, "{total} bytes reached the view");
    }

    // ---- the list and the diff beside it say the same thing ----------------

    /// The row said nothing at all for an image: a status letter and an empty
    /// cell, the same as a file that was only renamed.
    #[test]
    fn a_binary_file_says_so_in_the_list_and_not_only_in_the_diff() {
        let _guard = isolate();
        let dir = scratch("cm-binary");
        git(&dir, &["init", "-q", "-b", "main"]);
        commit_file(&dir, "notes.txt", "one\n", "first");
        write(&dir.join("logo.png"), "\u{89}PNG\0\0\0one\0\n");
        write(&dir.join("notes.txt"), "one\ntwo\n");
        git(&dir, &["add", "-A"]);
        git(&dir, &["commit", "-q", "-m", "a logo"]);
        let head = git(&dir, &["rev-parse", "HEAD"]);
        assert!(git(&dir, &["show", "--numstat", "--format="]).contains("-\t-\tlogo.png"), "git calls it binary");

        let listed = load(&dir, &head, &Cancel::default()).unwrap();
        let rows: Vec<(&str, bool)> = listed.files.iter().map(|f| (f.path.as_str(), f.binary)).collect();
        assert_eq!(rows, vec![("logo.png", true), ("notes.txt", false)]);
        assert_eq!((listed.insertions, listed.deletions), (1, 0));

        // The working tree has three ways to hold one: staged, edited, new.
        write(&dir.join("logo.png"), "\u{89}PNG\0\0\0two\0\n");
        write(&dir.join("staged.bin"), "\0\0staged\0");
        git(&dir, &["add", "staged.bin"]);
        write(&dir.join("new.bin"), "\0\0untracked\0");
        write(&dir.join("new.txt"), "text\n");

        let working = load_working(&dir, &Cancel::default()).unwrap();
        let binary = |path: &str| working.files.iter().find(|f| f.path == path).map(|f| f.binary);
        assert_eq!(binary("logo.png"), Some(true), "modified");
        assert_eq!(binary("staged.bin"), Some(true), "staged");
        assert_eq!(binary("new.bin"), Some(true), "untracked");
        assert_eq!(binary("new.txt"), Some(false));
    }

    /// Forty lines, of which `edited` differ from the plain file.
    fn forty_lines(edited: &[usize]) -> String {
        (0..40)
            .map(|i| if edited.contains(&i) { format!("line {i}, edited\n") } else { format!("line {i}\n") })
            .collect()
    }

    fn marked(diff: &FileDiff, origin: char) -> usize {
        diff.lines.iter().filter(|line| line.origin == origin).count()
    }

    /// The list said `R +1 −1` and the diff beside it showed forty added
    /// lines: the one line that had changed could not be found in it.
    #[test]
    fn a_renamed_file_shows_its_edit_and_not_the_whole_file_as_added() {
        let _guard = isolate();
        let dir = scratch("cm-rename");
        git(&dir, &["init", "-q", "-b", "main"]);
        commit_file(&dir, "old_name.txt", &forty_lines(&[]), "first");
        git(&dir, &["mv", "old_name.txt", "new_name.txt"]);
        write(&dir.join("new_name.txt"), &forty_lines(&[20]));
        git(&dir, &["add", "-A"]);
        git(&dir, &["commit", "-q", "-m", "rename, and one edit"]);
        let head = git(&dir, &["rev-parse", "HEAD"]);

        let listed = load(&dir, &head, &Cancel::default()).unwrap();
        assert_eq!(listed.files.len(), 1);
        let row = &listed.files[0];
        assert_eq!(
            (row.change, row.from.as_deref(), row.insertions, row.deletions),
            (Change::Renamed, Some("old_name.txt"), 1, 1)
        );

        let diff = patch_for(&dir, &head, &row.path, row.from.as_deref()).unwrap();
        assert_eq!((marked(&diff, '+'), marked(&diff, '-')), (1, 1), "the diff beside that row");
        assert!(diff.lines[0].text.starts_with("@@ -18,7 +18,7 @@"), "{}", diff.lines[0].text);
    }

    /// Nothing to show is not nothing to say: the file is under another name.
    #[test]
    fn a_file_renamed_and_not_edited_says_where_it_came_from() {
        let _guard = isolate();
        let dir = scratch("cm-rename-only");
        git(&dir, &["init", "-q", "-b", "main"]);
        commit_file(&dir, "docs/old.md", &forty_lines(&[]), "first");
        git(&dir, &["mv", "docs/old.md", "docs/new.md"]);
        git(&dir, &["commit", "-q", "-m", "rename"]);
        let head = git(&dir, &["rev-parse", "HEAD"]);

        let listed = load(&dir, &head, &Cancel::default()).unwrap();
        let row = &listed.files[0];
        assert_eq!((row.change, row.insertions, row.deletions), (Change::Renamed, 0, 0));

        let diff = patch_for(&dir, &head, &row.path, row.from.as_deref()).unwrap();
        assert!(diff.lines.is_empty() && !diff.binary);
        assert_eq!(diff.note.as_deref(), Some("Renamed from docs/old.md. Its content is unchanged."));
    }

    /// A copy leaves its source in the commit as a change of its own, and the
    /// diff asked for is the copy's.
    #[test]
    fn a_copied_file_shows_its_own_diff_and_not_its_sources() {
        let _guard = isolate();
        let dir = scratch("cm-copy");
        git(&dir, &["init", "-q", "-b", "main"]);
        commit_file(&dir, "a.txt", &forty_lines(&[]), "first");
        write(&dir.join("b.txt"), &forty_lines(&[30]));
        write(&dir.join("a.txt"), &forty_lines(&[3, 4]));
        git(&dir, &["add", "-A"]);
        git(&dir, &["commit", "-q", "-m", "copy a to b, edit both"]);
        let head = git(&dir, &["rev-parse", "HEAD"]);

        let listed = load(&dir, &head, &Cancel::default()).unwrap();
        let row = |path: &str| listed.files.iter().find(|f| f.path == path).unwrap();
        assert_eq!((row("b.txt").change, row("b.txt").from.as_deref()), (Change::Copied, Some("a.txt")));
        assert_eq!((row("b.txt").insertions, row("b.txt").deletions), (1, 1));
        assert_eq!((row("a.txt").change, row("a.txt").insertions), (Change::Modified, 2));

        let copy = patch_for(&dir, &head, "b.txt", row("b.txt").from.as_deref()).unwrap();
        assert_eq!((marked(&copy, '+'), marked(&copy, '-')), (1, 1));
        let source = patch_for(&dir, &head, "a.txt", None).unwrap();
        assert_eq!((marked(&source, '+'), marked(&source, '-')), (2, 2));
    }

    // ---- what could not be read is not reported as nothing ------------------

    /// A remote whose `README.md` has been edited twice since the first
    /// commit, and a clone of it made with `how`.
    fn cloned(name: &str, how: &[&str]) -> (Fleet, PathBuf) {
        let f = fleet(name);
        f.remote_commits("second", |seed| write(&seed.join("README.md"), "one\ntwo\n"));
        f.remote_commits("third", |seed| write(&seed.join("README.md"), "one\ntwo\nthree\n"));
        git(&f.remote, &["config", "uploadpack.allowFilter", "true"]);
        let url = format!("file://{}", f.remote.display());
        let mut args = vec!["clone", "-q"];
        args.extend(how);
        args.extend([url.as_str(), "clone"]);
        git(&f.root, &args);
        let clone = f.root.join("clone");
        (f, clone)
    }

    /// `git clone --filter=blob:none`, the usual advice for a large
    /// repository. libgit2 does not fetch what is missing, and every commit
    /// older than the checkout read "+0 −0" and "No textual changes."
    #[test]
    fn a_file_whose_content_is_not_here_is_unreadable_and_not_unchanged() {
        let _guard = isolate();
        let (_f, clone) = cloned("cm-blobless", &["--filter=blob:none"]);
        let head = git(&clone, &["rev-parse", "HEAD"]);

        let listed = load(&clone, &head, &Cancel::default()).unwrap();
        assert_eq!(listed.total_files, 1);
        assert!(listed.files[0].unreadable, "the row read +0 −0, as if it had been looked at");
        assert_eq!(listed.unreadable, 1);

        let why = patch_for(&clone, &head, "README.md", None).unwrap_err();
        assert!(why.contains("partial clone"), "{why}");

        // A commit that can be read is still read.
        commit_file(&clone, "mine.txt", "mine\n", "mine");
        let mine = git(&clone, &["rev-parse", "HEAD"]);
        let listed = load(&clone, &mine, &Cancel::default()).unwrap();
        assert_eq!((listed.unreadable, listed.insertions), (0, 1));
    }

    /// `--filter=tree:0`: the parent is there and its tree is not. Taken for
    /// "no parent", a one-line commit read as the whole repository added.
    #[test]
    fn a_parent_that_cannot_be_read_is_not_taken_for_no_parent() {
        let _guard = isolate();
        let (_f, clone) = cloned("cm-treeless", &["--filter=tree:0"]);
        let head = git(&clone, &["rev-parse", "HEAD"]);

        assert!(load(&clone, &head, &Cancel::default()).is_err());
        assert!(patch_for(&clone, &head, "README.md", None).is_err());
    }

    /// `--depth 1`: here there really is nothing to compare with, and git
    /// itself shows the whole tree as added. It marks the commit as grafted;
    /// so does the pane.
    #[test]
    fn the_commit_where_a_shallow_clone_stops_says_so() {
        let _guard = isolate();
        let (f, clone) = cloned("cm-shallow", &["--depth", "1"]);
        let head = git(&clone, &["rev-parse", "HEAD"]);

        let listed = load(&clone, &head, &Cancel::default()).unwrap();
        assert!(listed.boundary);
        assert_eq!(listed.insertions, 3, "compared with nothing, as git does");

        // A first commit has no parent anywhere, and is not one of those.
        let root = git(&f.seed, &["rev-list", "--max-parents=0", "HEAD"]);
        assert!(!load(&f.seed, &root, &Cancel::default()).unwrap().boundary);
    }

    /// A file the user may not read: the row is there, and its count is not
    /// zero but unknown.
    #[test]
    fn a_working_file_that_cannot_be_opened_is_unreadable_and_not_unchanged() {
        use std::os::unix::fs::PermissionsExt;
        let _guard = isolate();
        let dir = scratch("cm-unreadable");
        git(&dir, &["init", "-q", "-b", "main"]);
        commit_file(&dir, "secret.txt", "one\n", "first");
        write(&dir.join("secret.txt"), "one\ntwo, and longer\n");
        let locked = |mode| std::fs::set_permissions(dir.join("secret.txt"), std::fs::Permissions::from_mode(mode));
        locked(0o000).unwrap();
        if std::fs::read(dir.join("secret.txt")).is_ok() {
            // Run as root, nothing is unreadable and there is nothing to test.
            return;
        }

        let listed = load_working(&dir, &Cancel::default());
        let diff = patch_working(&dir, "secret.txt", Some(Group::Unstaged));
        locked(0o644).unwrap();

        let listed = listed.unwrap();
        assert_eq!(listed.files.len(), 1);
        assert!(listed.files[0].unreadable);
        assert_eq!(listed.unreadable, 1);
        assert!(diff.is_err(), "the diff said: {:?}", diff.map(|d| d.lines.len()));
    }

    /// libgit2 gives a conflicted path no content on either side, so its diff
    /// has no lines. The pane said "No textual changes." about the one file
    /// that most needed reading.
    #[test]
    fn a_conflicted_files_diff_says_it_is_conflicted() {
        let _guard = isolate();
        let dir = stash_pop_conflict("cm-conflict-diff");
        assert!(read(&dir.join("f.txt")).contains("<<<<<<<"));

        for side in [Some(Group::Conflicted), None] {
            let diff = patch_working(&dir, "f.txt", side).unwrap();
            assert!(diff.lines.is_empty() && !diff.binary);
            let note = diff.note.expect("it has to say something");
            assert!(note.starts_with("Conflicted"), "{note}");
        }
    }

    // ---- the commit box: nothing it cannot finish ---------------------------

    /// A conflict with nothing "in progress": what `git stash pop` leaves.
    /// There is no MERGE_HEAD, so the repository's state reads Clean.
    fn stash_pop_conflict(name: &str) -> PathBuf {
        let dir = scratch(name);
        git(&dir, &["init", "-q", "-b", "main"]);
        commit_file(&dir, "f.txt", "base\n", "base");
        write(&dir.join("f.txt"), "MY CHANGE\n");
        git(&dir, &["stash", "-q"]);
        commit_file(&dir, "f.txt", "THEIR CHANGE\n", "theirs");
        let (popped, _) = try_git(&dir, &["stash", "pop"]);
        assert!(!popped, "the fixture needs the pop to conflict");
        dir
    }

    #[test]
    fn a_conflicted_file_is_listed_once_and_cannot_be_staged_or_committed() {
        let _guard = isolate();
        let dir = stash_pop_conflict("commit-conflict");
        assert_eq!(
            Repository::open(&dir).unwrap().state(),
            git2::RepositoryState::Clean,
            "which is how it used to get through"
        );

        let listed = load_working(&dir, &Cancel::default()).unwrap();
        let rows: Vec<(&str, Group)> =
            listed.files.iter().map(|f| (f.path.as_str(), f.group)).collect();
        assert_eq!(rows, vec![("f.txt", Group::Conflicted)]);

        // Staging is how git is told a conflict has been resolved.
        let staged = recv(spawn_write(dir.clone(), Write::Stage, vec!["f.txt".to_string()]));
        assert!(staged.unwrap_err().contains("conflicted"));
        let unstaged = recv(spawn_write(dir.clone(), Write::Unstage, vec!["f.txt".to_string()]));
        assert!(unstaged.is_err());
        assert!(!git(&dir, &["ls-files", "-u"]).is_empty(), "the three-way record is gone");

        let why = why_not_commit(&dir, 1, "resolved, honest").unwrap();
        assert!(why.contains("unresolved conflicts"), "{why}");
        assert!(read(&dir.join("f.txt")).contains("<<<<<<<"), "the markers are still the user's to deal with");
    }

    #[test]
    fn a_commit_says_which_commit_it_made() {
        let _guard = isolate();
        let f = fleet("cm-commit-made");
        write(&f.work.join("mine.txt"), "mine\n");
        git(&f.work, &["add", "mine.txt"]);

        let made = recv(spawn_commit(f.work.clone(), "mine".to_string())).unwrap();

        assert_eq!(made.len(), 8);
        assert_eq!(made, git(&f.work, &["rev-parse", "--short=8", "HEAD"]));
        assert_eq!(git(&f.work, &["log", "--format=%s", "-1"]), "mine");
    }

    /// A hook that fixes what it objects to and then refuses, which is what
    /// the formatters run by the pre-commit framework do. The refusal is
    /// quoted, and the tree is no longer what the list was drawn from: the
    /// file is staged as it was and modified as it now is. The pane reads the
    /// tree again after a refusal for this reason.
    #[test]
    fn a_hook_that_rewrites_a_file_and_refuses_leaves_a_tree_worth_reading_again() {
        use std::os::unix::fs::PermissionsExt;
        let _guard = isolate();
        let dir = scratch("cm-hook-rewrites");
        git(&dir, &["init", "-q", "-b", "main"]);
        commit_file(&dir, "f.txt", "one\n", "first");
        let hook = dir.join(".git/hooks/pre-commit");
        write(&hook, "#!/bin/sh\nprintf 'one\\ntwo\\n' > f.txt\necho 'f.txt: trailing whitespace, fixed' >&2\nexit 1\n");
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        write(&dir.join("f.txt"), "one\ntwo  \n");
        git(&dir, &["add", "f.txt"]);

        let groups = || -> Vec<Group> {
            load_working(&dir, &Cancel::default()).unwrap().files.iter().map(|f| f.group).collect()
        };
        assert_eq!(groups(), [Group::Staged]);

        let refused = recv(spawn_commit(dir.clone(), "add two".to_string())).unwrap_err();

        assert!(refused.contains("trailing whitespace, fixed"), "{refused}");
        assert_eq!(git(&dir, &["log", "--format=%s", "-1"]), "first", "nothing was committed");
        assert_eq!(groups(), [Group::Staged, Group::Unstaged]);
    }

    #[test]
    fn a_commit_that_would_diverge_the_branch_is_refused() {
        let _guard = isolate();
        let f = fleet("commit-behind");
        write(&f.work.join("mine.txt"), "mine\n");
        git(&f.work, &["add", "mine.txt"]);
        assert_eq!(why_not_commit(&f.work, 1, "mine"), None, "level with the upstream");

        f.remote_moves(3);
        git(&f.work, &["fetch", "-q"]);

        let why = why_not_commit(&f.work, 1, "mine").unwrap();
        assert!(why.starts_with("3 commits are waiting"), "{why}");
        assert!(why.contains("Update first"), "{why}");
    }

    #[test]
    fn a_detached_head_is_not_committed_to() {
        let _guard = isolate();
        let f = fleet("commit-detached");
        git(&f.work, &["checkout", "-q", "--detach"]);
        write(&f.work.join("mine.txt"), "mine\n");
        git(&f.work, &["add", "mine.txt"]);
        let why = why_not_commit(&f.work, 1, "mine").unwrap();
        assert!(why.starts_with("Detached HEAD"), "{why}");
    }

    #[test]
    fn the_ordinary_refusals_still_come_first() {
        let _guard = isolate();
        let f = fleet("commit-ordinary");
        assert_eq!(why_not_commit(&f.work, 1, "  "), Some("Write a commit message first.".into()));
        assert!(why_not_commit(&f.work, 0, "message").unwrap().starts_with("Nothing is staged"));
    }

    // ---- push: one branch, to the place the button named, or nothing ------

    #[test]
    fn a_push_sends_the_checked_out_branch_to_its_upstream() {
        let _guard = isolate();
        let f = fleet("push-plain");
        commit_file(&f.work, "mine.txt", "mine\n", "ready");

        let armed = plan(&f).unwrap();
        assert_eq!(armed.branch, "main");
        assert_eq!(armed.tip, f.head());
        assert_eq!((armed.remote.as_str(), armed.destination.as_str()), ("origin", "refs/heads/main"));
        assert!(!armed.publish);

        assert_eq!(push(&f.work, &armed), Ok("Pushed main to origin".to_string()));
        assert_eq!(f.remote_ref("refs/heads/main"), f.head());
    }

    /// The settings that widen a bare `git push`, all at once: `matching`
    /// sends every branch with a namesake on the remote, `followTags` sends
    /// tags, and a `+` push refspec makes it all a forced update.
    #[test]
    fn a_push_sends_nothing_but_the_branch_whatever_the_push_settings_say() {
        let _guard = isolate();
        let f = fleet("push-settings");

        // A second branch, published, that a teammate has since added to...
        git(&f.work, &["checkout", "-q", "-b", "other"]);
        commit_file(&f.work, "other.txt", "one\n", "other, shared");
        git(&f.work, &["push", "-q", "-u", "origin", "other"]);
        git(&f.seed, &["fetch", "-q"]);
        git(&f.seed, &["checkout", "-q", "other"]);
        commit_file(&f.seed, "theirs.txt", "theirs\n", "a teammate's commit");
        git(&f.seed, &["push", "-q", "origin", "other"]);
        let theirs = f.remote_ref("refs/heads/other");
        // ...and that has work here nobody meant to send.
        commit_file(&f.work, "wip.txt", "wip\n", "WIP do not push");

        git(&f.work, &["checkout", "-q", "main"]);
        commit_file(&f.work, "mine.txt", "mine\n", "ready");
        git(&f.work, &["tag", "-a", "v1", "-m", "v1"]);
        git(&f.work, &["config", "push.default", "matching"]);
        git(&f.work, &["config", "push.followTags", "true"]);
        git(&f.work, &["config", "remote.origin.push", "+refs/heads/*:refs/heads/*"]);

        let armed = plan(&f).unwrap();
        push(&f.work, &armed).unwrap();

        assert_eq!(f.remote_ref("refs/heads/main"), f.head(), "main was meant to go");
        assert_eq!(
            f.remote_ref("refs/heads/other"),
            theirs,
            "another branch was sent, over a teammate's commit"
        );
        assert_eq!(f.remote_ref("refs/tags/v1"), "", "a tag was sent");
    }

    /// The page sat open on `main`; a terminal moved HEAD to `wip`. The button
    /// still says main.
    #[test]
    fn a_push_armed_on_one_branch_sends_nothing_once_head_is_on_another() {
        let _guard = isolate();
        let f = fleet("push-stale-head");
        git(&f.work, &["checkout", "-q", "-b", "wip"]);
        commit_file(&f.work, "wip.txt", "one\n", "wip, shared");
        git(&f.work, &["push", "-q", "-u", "origin", "wip"]);
        commit_file(&f.work, "more.txt", "more\n", "WIP do not push");
        git(&f.work, &["checkout", "-q", "main"]);
        commit_file(&f.work, "mine.txt", "mine\n", "ready");

        let armed = plan(&f).unwrap();
        let (main_there, wip_there) =
            (f.remote_ref("refs/heads/main"), f.remote_ref("refs/heads/wip"));

        git(&f.work, &["checkout", "-q", "wip"]);

        let got = push(&f.work, &armed);
        let Err(PushError::Stale(why)) = &got else { panic!("{got:?}") };
        assert!(why.contains("wip") && why.contains("main"), "{why}");
        assert_eq!(f.remote_ref("refs/heads/wip"), wip_there, "the WIP commit was published");
        assert_eq!(f.remote_ref("refs/heads/main"), main_there);
    }

    /// The button said "Push 1". A second commit has been made since.
    #[test]
    fn a_push_sends_nothing_when_the_branch_has_moved_since_it_was_armed() {
        let _guard = isolate();
        let f = fleet("push-stale-tip");
        commit_file(&f.work, "mine.txt", "mine\n", "ready");
        let armed = plan(&f).unwrap();
        let there = f.remote_ref("refs/heads/main");

        commit_file(&f.work, "more.txt", "more\n", "not looked at yet");

        let got = push(&f.work, &armed);
        assert!(matches!(got, Err(PushError::Stale(_))), "{got:?}");
        assert_eq!(f.remote_ref("refs/heads/main"), there);
    }

    /// The button said "Push 1", and the commit has since gone out from a
    /// terminal. git answers a push of nothing with success, and the toast
    /// said "Pushed main to origin" about it.
    #[test]
    fn a_push_with_nothing_left_to_send_is_not_reported_as_a_push() {
        let _guard = isolate();
        let f = fleet("cm-push-nothing");
        commit_file(&f.work, "mine.txt", "mine\n", "ready");
        let armed = plan(&f).unwrap();
        git(&f.work, &["push", "-q", "origin", "main"]);
        assert_eq!(plan(&f).unwrap(), armed, "same branch, same commit, same place");

        let got = push(&f.work, &armed);

        let Err(PushError::Stale(why)) = &got else { panic!("{got:?}") };
        assert!(why.contains("origin already has main"), "{why}");
    }

    #[test]
    fn publishing_sends_the_one_branch_and_sets_its_upstream() {
        let _guard = isolate();
        let f = fleet("push-publish");
        git(&f.work, &["checkout", "-q", "-b", "feature"]);
        commit_file(&f.work, "feature.txt", "new\n", "a feature");
        git(&f.work, &["tag", "-a", "v2", "-m", "v2"]);
        git(&f.work, &["config", "push.followTags", "true"]);

        let armed = plan(&f).unwrap();
        assert!(armed.publish);
        assert_eq!((armed.remote.as_str(), armed.destination.as_str()), ("origin", "refs/heads/feature"));
        assert_eq!(why_not_push(&armed, 0, 0), None, "a publish needs no count to justify it");

        assert_eq!(push(&f.work, &armed), Ok("Published feature to origin".to_string()));
        assert_eq!(f.remote_ref("refs/heads/feature"), f.head());
        assert_eq!(f.remote_ref("refs/tags/v2"), "");
        assert_eq!(git(&f.work, &["rev-parse", "--abbrev-ref", "feature@{u}"]), "origin/feature");

        // From here on it is an ordinary push.
        let again = plan(&f).unwrap();
        assert!(!again.publish);
        assert_eq!(why_not_push(&again, 0, 0), Some("Nothing to push.".to_string()));
    }

    /// `git switch -c topic origin/main` makes `topic` track `origin/main`
    /// without being asked. git's default refuses to push it; a plan that
    /// named `branch.topic.merge` as the destination would put it on main.
    #[test]
    fn a_branch_is_never_pushed_onto_a_branch_of_another_name() {
        let _guard = isolate();
        let f = fleet("push-other-name");
        git(&f.work, &["checkout", "-q", "-b", "topic", "origin/main"]);
        assert_eq!(git(&f.work, &["rev-parse", "--abbrev-ref", "topic@{u}"]), "origin/main");
        commit_file(&f.work, "t.txt", "x\n", "topic work");
        let main_there = f.remote_ref("refs/heads/main");

        let why = plan(&f).unwrap_err();

        assert!(why.contains("another name"), "{why}");
        assert!(why.contains("git push -u origin topic"), "it should say what to run: {why}");
        assert_eq!(f.remote_ref("refs/heads/main"), main_there);
    }

    /// The everyday end of a pull request: merged on the server, the branch
    /// deleted there, and the clone prunes on fetch. The branch still names
    /// its upstream; the upstream is not there to be found.
    #[test]
    fn a_branch_whose_upstream_was_deleted_is_not_offered_for_publishing() {
        let _guard = isolate();
        let f = fleet("cm-push-gone");
        git(&f.work, &["checkout", "-q", "-b", "feature"]);
        commit_file(&f.work, "feature.txt", "new\n", "a feature");
        git(&f.work, &["push", "-q", "-u", "origin", "feature"]);
        assert!(!plan(&f).unwrap().publish, "pushed and tracked: an ordinary push");

        git(&f.remote, &["update-ref", "-d", "refs/heads/feature"]);
        git(&f.work, &["fetch", "-q", "--prune"]);
        assert!(git(&f.work, &["status", "-sb"]).contains("[gone]"), "git says so");

        let why = plan(&f).unwrap_err();
        assert!(why.contains("origin/feature") && why.contains("gone"), "{why}");
        assert!(why.contains("git push -u origin feature"), "it should say what would bring it back: {why}");
        assert_eq!(f.remote_ref("refs/heads/feature"), "");
    }

    /// `git clone --single-branch` fetches one branch and no other. A second
    /// branch pushed from it tracks a branch this clone keeps no copy of.
    #[test]
    fn a_branch_this_clone_does_not_fetch_is_not_offered_for_publishing() {
        let _guard = isolate();
        let f = fleet("cm-push-single-branch");
        let url = format!("file://{}", f.remote.display());
        git(&f.root, &["clone", "-q", "--single-branch", &url, "single"]);
        let single = f.root.join("single");
        git(&single, &["checkout", "-q", "-b", "topic"]);
        commit_file(&single, "topic.txt", "x\n", "topic work");
        git(&single, &["push", "-q", "-u", "origin", "topic"]);
        assert_eq!(git(&single, &["config", "branch.topic.merge"]), "refs/heads/topic");

        let why = push_plan(&Repository::open(&single).unwrap()).unwrap_err();
        assert!(why.contains("origin/topic"), "{why}");
    }

    /// `git switch -c develop --no-track origin/develop`, or a branch pushed
    /// once without `-u`: no upstream is configured, and the remote has a
    /// branch of this name all the same. "Publish" would land on it.
    #[test]
    fn a_branch_with_a_namesake_on_the_remote_is_not_published_over_it() {
        let _guard = isolate();
        let f = fleet("cm-push-namesake");
        git(&f.seed, &["checkout", "-q", "-b", "develop"]);
        commit_file(&f.seed, "shared.txt", "theirs\n", "shared work");
        git(&f.seed, &["push", "-q", "-u", "origin", "develop"]);
        let theirs = f.remote_ref("refs/heads/develop");
        git(&f.work, &["fetch", "-q"]);
        git(&f.work, &["checkout", "-q", "-b", "develop", "--no-track", "origin/develop"]);
        commit_file(&f.work, "mine.txt", "mine\n", "two local commits");
        assert!(try_git(&f.work, &["config", "branch.develop.merge"]).1.is_empty(), "no upstream is set");

        let why = plan(&f).unwrap_err();
        assert!(why.contains("origin/develop already exists"), "{why}");
        assert!(why.contains("--set-upstream-to=origin/develop"), "{why}");
        assert_eq!(f.remote_ref("refs/heads/develop"), theirs);
    }

    #[test]
    fn there_is_no_plan_for_a_push_with_nowhere_proper_to_go() {
        let _guard = isolate();

        let f = fleet("push-detached");
        git(&f.work, &["checkout", "-q", "--detach"]);
        let why = plan(&f).unwrap_err();
        assert!(why.contains("Detached HEAD"), "{why}");

        // What cloning a remote whose default branch is `--mirror` leaves.
        let f = fleet("push-option-name");
        git(&f.work, &["update-ref", "refs/heads/--mirror", "HEAD"]);
        git(&f.work, &["symbolic-ref", "HEAD", "refs/heads/--mirror"]);
        let why = plan(&f).unwrap_err();
        assert!(why.contains("--mirror"), "{why}");

        let f = fleet("push-fork");
        git(&f.work, &["remote", "add", "fork", f.remote.to_str().unwrap()]);
        git(&f.work, &["config", "remote.pushDefault", "fork"]);
        let why = plan(&f).unwrap_err();
        assert!(why.contains("fork") && why.contains("origin"), "{why}");

        let f = fleet("push-mirror");
        git(&f.work, &["config", "remote.origin.mirror", "true"]);
        let why = plan(&f).unwrap_err();
        assert!(why.contains("mirror"), "{why}");

        let f = fleet("push-two-remotes");
        git(&f.work, &["remote", "add", "second", f.remote.to_str().unwrap()]);
        git(&f.work, &["checkout", "-q", "-b", "unpublished"]);
        let why = plan(&f).unwrap_err();
        assert!(why.contains("no single remote"), "{why}");

        let f = fleet("push-local-upstream");
        git(&f.work, &["checkout", "-q", "-b", "topic", "--track", "main"]);
        let why = plan(&f).unwrap_err();
        assert!(why.contains("local branch"), "{why}");
    }

    #[test]
    fn the_button_says_why_when_a_push_would_do_nothing_or_be_rejected() {
        let _guard = isolate();
        let f = fleet("push-why-not");
        let armed = plan(&f).unwrap();
        assert_eq!(why_not_push(&armed, 0, 0), Some("Nothing to push.".to_string()));
        assert_eq!(why_not_push(&armed, 2, 0), None);
        let behind = why_not_push(&armed, 1, 3).unwrap();
        assert!(behind.starts_with("3 commits are waiting"), "{behind}");
    }
}
