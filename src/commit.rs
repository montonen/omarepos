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
// `TypeChange` is git's own word for a file that became a link or the like.
#[allow(clippy::enum_variant_names)]
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
/// there is only one; for the working tree the distinction is the point: what
/// is ready to commit, what is not yet, what is in conflict, and what git has
/// never been told about.
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

    /// Whether changes of this kind are to files git already knows. Untracked
    /// files are not, and are the bulk of most lists and the least of what
    /// anyone came to see.
    pub fn tracked(self) -> bool {
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

    let tree = commit.tree().map_err(|e| read_error(&repo, &e))?;
    // A merge is diffed against its first parent: that shows what the merge
    // brought in, which is what someone looking at one means.
    let parent_tree = parent_tree(&repo, &commit)?;
    // At the edge of a shallow clone libgit2 hides a commit's parents, as git
    // does, and the commit's own header goes on naming them.
    let boundary = commit.parent_count() == 0
        && repo.is_shallow()
        && commit.raw_header().is_ok_and(|header| header.lines().any(|line| line.starts_with("parent ")));

    let mut opts = DiffOptions::new();
    opts.include_typechange(true).context_lines(0);
    let mut diff: Diff = repo
        .diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), Some(&mut opts))
        .map_err(|e| read_error(&repo, &e))?;

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
        let path_of =
            |f: git2::DiffFile<'_>| f.path().map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
        let new_path = path_of(delta.new_file());
        let old_path = path_of(delta.old_file());
        let change = Change::from(delta.status());
        files.push(FileChange {
            path: if new_path.is_empty() { old_path.clone() } else { new_path },
            from: (matches!(change, Change::Renamed | Change::Copied) && old_path != *"").then_some(old_path),
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
        summary: commit.summary().ok().flatten().unwrap_or("(no message)").trim().to_string(),
        body: commit
            .message()
            .unwrap_or("")
            .trim()
            .split_once('\n')
            .map(|x| x.1)
            .unwrap_or("")
            .trim()
            .to_string(),
        email: author.email().unwrap_or("").to_string(),
        author: author_name.clone(),
        when: commit.time().seconds(),
        committer: (!committer_name.is_empty() && committer_name != author_name).then_some(committer_name),
        parents: commit.parent_ids().map(|p| p.to_string().chars().take(8).collect()).collect(),
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
    commit.parent(0).and_then(|parent| parent.tree()).map(Some).map_err(|err| read_error(repo, &err))
}

/// The tree HEAD stands for, or nothing when nothing has been committed yet:
/// then everything staged is new. Any other failure to read it is a failure,
/// and not a reason to list the whole index as added.
fn head_tree(repo: &Repository) -> Result<Option<git2::Tree<'_>>, String> {
    match repo.head() {
        Ok(head) => head.peel_to_tree().map(Some).map_err(|err| read_error(repo, &err)),
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
fn read_error(repo: &Repository, err: &git2::Error) -> String {
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
    // A NUL is shown as the symbol for one. A file is only called binary for
    // a NUL in its first 8000 bytes, so a later one arrives here as text, and
    // text with a NUL in it cannot be handed to GTK: the line went missing
    // from the diff, and a debug build stopped altogether.
    let text = String::from_utf8_lossy(content).replace('\0', "\u{2400}");
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
/// questions: what the next commit would hold, and what it would leave out.
fn load_working(path: &Path, cancel: &Cancel) -> Result<Detail, String> {
    let repo = crate::git::open_here(path)?;
    let head_tree = head_tree(&repo)?;
    let index = repo.index().map_err(|e| e.message().to_string())?;

    let mut files = Vec::new();
    let mut insertions = 0usize;
    let mut deletions = 0usize;

    // A half that cannot be read is an error, here and below. It used to be
    // left out: one file the user could not open, and the list was the staged
    // files alone, as if that were all of it.
    let mut staged_opts = DiffOptions::new();
    staged_opts.include_typechange(true).context_lines(0);
    let diff = repo
        .diff_tree_to_index(head_tree.as_ref(), Some(&index), Some(&mut staged_opts))
        .map_err(|e| read_error(&repo, &e))?;
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
        // The same rule as the card and the row count: a submodule's
        // checkout being at another commit is not a change to this
        // repository's files, and is not listed as one.
        .ignore_submodules(true)
        .context_lines(0);
    let diff =
        repo.diff_index_to_workdir(Some(&index), Some(&mut work_opts)).map_err(|e| read_error(&repo, &e))?;
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
    // unignored build directory is thousands); cut from the joined list as it
    // came, they pushed the single modified file off the end of it. Stable,
    // so the order within each kind is kept.
    files.sort_by_key(|f| !f.group.tracked());
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
/// The rows themselves are left as libgit2 gives them. The card and the count
/// on the Local changes row take the same reading, and a pane that quietly
/// disagreed with the two of them would be a third opinion. What it can do
/// is say which rows not to believe.
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
                git2::IndexEntryExtendedFlag::from_bits_truncate(entry.flags_extended).is_skip_worktree()
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

        let path_of =
            |f: git2::DiffFile<'_>| f.path().map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
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
    let tree = commit.tree().map_err(|e| read_error(&repo, &e))?;
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
        .map_err(|e| read_error(&repo, &e))?;
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
    staged_opts.context_lines(3).include_typechange(true).pathspec(file).disable_pathspec_match(true);
    if staged {
        let diff = repo
            .diff_tree_to_index(head_tree.as_ref(), Some(&index), Some(&mut staged_opts))
            .map_err(|e| read_error(&repo, &e))?;
        append_patch(&repo, &diff, None, &mut out)?;
    }
    if unstaged {
        let diff =
            repo.diff_index_to_workdir(Some(&index), Some(&mut opts)).map_err(|e| read_error(&repo, &e))?;
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
                 a file in conflict."
                    .to_string(),
            );
            continue;
        }
        let patch = Patch::from_diff(diff, index).map_err(|err| read_error(repo, &err))?;
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

/// A node in the tree the pane draws.
pub enum Node {
    /// A directory, with the path segment(s) it stands for. Single-child chains
    /// are collapsed into one node, so `src/Api/One/Interfaces` is one row
    /// rather than four.
    Dir { name: String, depth: usize },
    /// `file.path` stays the full path, since that is what a diff needs;
    /// `display` is the part worth putting on screen under its directory.
    File { file: FileChange, display: String, depth: usize },
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
                let joined = if prefix.is_empty() { name.clone() } else { format!("{prefix}/{name}") };
                walk(child, joined, depth, out);
                continue;
            }
            let label = if prefix.is_empty() { name.clone() } else { format!("{prefix}/{name}") };
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::*;

    // ---- the working tree, listed as it is ---------------------------------

    /// Route folders in Next.js, SvelteKit and Remix are named like patterns.
    /// The diff asked for by such a name is that file's, and no other's.
    #[test]
    fn a_file_name_is_a_name_and_not_a_pattern() {
        let _guard = isolate();
        let dir = scratch("stage-literal");
        git(&dir, &["init", "-q", "-b", "main"]);
        commit_file(&dir, "README.md", "one\n", "first");
        for name in ["app/[id]/page.tsx", "app/i/page.tsx", "app/d/page.tsx", "*", "notes.txt"] {
            write(&dir.join(name), "x\n");
        }
        git(&dir, &["--literal-pathspecs", "add", "--", "app/[id]/page.tsx"]);

        let diff = patch_working(&dir, "app/[id]/page.tsx", Some(Group::Staged)).unwrap();
        assert_eq!(diff.lines.iter().filter(|line| line.origin == '+').count(), 1);
    }

    /// A repository inside the repository is one row, a directory, and not
    /// every file in it.
    #[test]
    fn a_repository_inside_the_repository_is_listed_as_a_directory() {
        let _guard = isolate();
        let dir = scratch("stage-nested");
        git(&dir, &["init", "-q", "-b", "main"]);
        commit_file(&dir, "README.md", "one\n", "first");
        let inner = dir.join("vendor/thing");
        std::fs::create_dir_all(&inner).unwrap();
        git(&inner, &["init", "-q", "-b", "main"]);
        commit_file(&inner, "a.txt", "a\n", "inner");

        let listed = load_working(&dir, &Cancel::default()).unwrap();
        assert!(listed.files.iter().any(|f| f.path.ends_with('/')), "listed as a directory");
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
        assert_eq!(listed.files.len(), MAX_FILES);
        assert!(
            listed.files.iter().any(|f| f.path == "zz-last.txt"),
            "the modified file was cut off the end of the list"
        );
    }

    #[test]
    fn a_submodule_left_at_another_commit_is_not_listed() {
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

    // ---- where the list is libgit2's and not git's, it says so ----------------

    /// A sparse checkout: `git status` is clean, and libgit2, which has never
    /// heard of one, reports every file outside it as deleted. The card and
    /// the row count take the same reading, so the rows stay; what the pane
    /// can do is say that they are not what they look like.
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

    // ---- nothing reaches through a redirected work tree --------------------

    #[test]
    fn another_directorys_files_cannot_be_listed() {
        let _guard = isolate();
        let f = fleet("commit-redirect");
        let elsewhere = scratch("commit-redirect-victim");
        write(&elsewhere.join("id_ed25519"), "PRIVATE\n");
        git(&f.work, &["config", "core.worktree", elsewhere.to_str().unwrap()]);

        assert!(load_working(&f.work, &Cancel::default()).is_err());
        assert!(patch_working(&f.work, "id_ed25519", None).is_err());
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
        assert!(
            git(&dir, &["show", "--numstat", "--format="]).contains("-\t-\tlogo.png"),
            "git calls it binary"
        );

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
        let locked =
            |mode| std::fs::set_permissions(dir.join("secret.txt"), std::fs::Permissions::from_mode(mode));
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

    // ---- a conflict is said to be one --------------------------------------

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
    fn a_conflicted_file_is_listed_once_and_as_conflicted() {
        let _guard = isolate();
        let dir = stash_pop_conflict("commit-conflict");
        assert_eq!(
            Repository::open(&dir).unwrap().state(),
            git2::RepositoryState::Clean,
            "which is how it used to get through"
        );

        let listed = load_working(&dir, &Cancel::default()).unwrap();
        let rows: Vec<(&str, Group)> = listed.files.iter().map(|f| (f.path.as_str(), f.group)).collect();
        assert_eq!(rows, vec![("f.txt", Group::Conflicted)]);
    }

    /// git's remark about a missing final newline comes through as a line of
    /// its own, with an origin that is none of `+`, `-` and space. The pane
    /// tells it from a line of the file by that.
    #[test]
    fn the_no_newline_remark_is_not_a_line_of_the_file() {
        let _guard = isolate();
        let dir = scratch("cm-no-newline");
        git(&dir, &["init", "-q", "-b", "main"]);
        commit_file(&dir, "a.txt", "one\n", "first");
        write(&dir.join("a.txt"), "one\ntwo");

        let diff = patch_working(&dir, "a.txt", None).unwrap();
        let remark: Vec<&Line> =
            diff.lines.iter().filter(|line| matches!(line.origin, '=' | '>' | '<')).collect();
        assert_eq!(remark.len(), 1, "{:?}", diff.lines);
        assert!(remark[0].text.contains("No newline at end of file"), "{:?}", remark[0].text);
    }
    /// libgit2 calls a file binary for a NUL in its first 8000 bytes. One
    /// further in arrives as a line of text, and has to be text GTK will take.
    #[test]
    fn a_nul_late_in_a_text_file_is_shown_and_not_passed_on() {
        let _guard = isolate();
        let dir = scratch("cm-late-nul");
        git(&dir, &["init", "-q", "-b", "main"]);
        commit_file(&dir, "README.md", "one\n", "first");
        let mut body = "a line of ordinary text\n".repeat(500);
        body.push_str("before\0after\n");
        write(&dir.join("log.txt"), &body);

        let diff = patch_working(&dir, "log.txt", None).unwrap();
        assert!(diff.lines.len() > 400, "it is read as text: {} lines", diff.lines.len());
        assert!(diff.lines.iter().all(|line| !line.text.contains('\0')));
        assert!(diff.lines.iter().any(|line| line.text == "before\u{2400}after"));
    }
}
