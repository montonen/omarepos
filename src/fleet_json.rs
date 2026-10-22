//! The fleet as JSON on standard output, for scripts: `omarepos --json`
//! (ADR-0014).
//!
//! The window's reading half, with no window. The repositories are found and
//! read by the scan the grid runs, under the same roots and depth and with the
//! same refusals, and their tags come from `repos.toml`; then it prints, and
//! exits. Nothing is fetched, no `git` is run and no file is written: not
//! `repos.toml`, not the state the window keeps, not the last scan. Nothing of
//! GTK's is started either, so it runs where there is no display, over ssh and
//! from cron.
//!
//! What is printed is [`Fleet`], a schema of its own rather than [`RepoInfo`]
//! as it stands, so that what the window needs of a reading can change without
//! changing what a script was promised. It says its version, and
//! `docs/usage.md` says what each key holds.
//!
//! It prints what the grid could show and nothing more. What is never in it is
//! the URL a remote is configured with, which can carry a user name and a
//! token: the remote is given as the card names it, and what a failed fetch
//! said as the card says it, both with the login taken out
//! (`crate::remote_url`). And it stays a snapshot. A fetch from the command
//! line is not what this grows into.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::Serialize;

use crate::config::Config;
use crate::git::{Commit, RepoInfo, RootReport, Seed};
use crate::meta::Meta;
use crate::scan::{self, Event};

/// The version of what is printed, as `schema_version`. Raised when a key
/// goes, is renamed or comes to hold something else. Not for a key added: a
/// reader is to pass over keys it does not know.
const SCHEMA_VERSION: u32 = 1;

/// Everything printed.
#[derive(Serialize)]
struct Fleet {
    schema_version: u32,
    /// Why the record of failed fetches could not be read, when it could not.
    /// Then no failed fetch is known: every `fetch_failed` is `null` for not
    /// being known, and `needs_attention` counts none. The window takes such a
    /// record for no failures, and a script is not to.
    fetch_failures_unreadable: Option<String>,
    /// The roots in force, in the order they are given, and what each came
    /// to. A root that is not there is said here. Otherwise its repositories
    /// are merely missing, and a disk that is not mounted reads as a fleet
    /// with nothing to do.
    roots: Vec<Root>,
    /// Every repository the window would show, in the order of their paths.
    repos: Vec<Repo>,
}

/// A root, as the status bar speaks of it.
#[derive(Serialize)]
struct Root {
    /// Absolute, and where it is there, resolved as the repositories' paths
    /// are: what a root found has a path that starts with it.
    path: String,
    /// The repositories it put among `repos`. One that a narrower root also
    /// reaches is that root's.
    found: usize,
    /// What the status bar says of it, when it says anything: that it does
    /// not exist, that it found nothing, which repositories it left out.
    problem: Option<String>,
}

impl Root {
    fn of(report: &RootReport) -> Self {
        // A root is held as it was given, and `--root .` was printed as "."
        // beside repositories with whole paths, which nothing could tell
        // were under it.
        let given = Path::new(&report.root);
        let path = given
            .canonicalize()
            .or_else(|_| std::path::absolute(given))
            .unwrap_or_else(|_| given.to_path_buf());
        Root { path: path.display().to_string(), found: report.found, problem: report.says() }
    }
}

/// A repository, as its card shows it.
///
/// A reading that could not be taken is `null`, never the zero that stands in
/// for it in a [`RepoInfo`]. Where a card says "unreadable", "status
/// unreadable", "history unreadable" or "bare", a count of nothing would tell
/// a script that there is nothing to do.
#[derive(Serialize)]
struct Repo {
    name: String,
    path: String,
    /// What the window's warning toggle keeps: see
    /// [`RepoInfo::needs_attention`].
    needs_attention: bool,
    /// Why the repository could not be read at all. Then all that is read
    /// from it is `null`: everything below, but for `fetch_failed` and
    /// `tags`, which omarepos keeps itself.
    error: Option<String>,
    /// `host/owner/repository`, from `origin`, or else the first remote there
    /// is: tidied, as the card shows it.
    remote: Option<String>,
    /// The remote's page on the web, where its URL says where one is.
    web: Option<String>,
    /// The branch, or a short commit id when HEAD is detached.
    head: Option<String>,
    detached: Option<bool>,
    /// No commits yet: `head` is the branch the first one will be on.
    unborn: Option<bool>,
    bare: Option<bool>,
    /// The branch it tracks, as `origin/main`.
    upstream: Option<String>,
    /// What is uncommitted. `null` in a bare repository, which has no working
    /// tree, and where the index could not be read.
    working_tree: Option<WorkingTree>,
    /// Why the index could not be read.
    working_tree_unreadable: Option<String>,
    /// `null` where the list of stashes could not be read.
    stashes: Option<usize>,
    /// Commits here that the upstream does not have, and commits on the
    /// upstream that are not here yet, as of the last fetch. Zero with no
    /// upstream.
    ahead: Option<usize>,
    behind: Option<usize>,
    /// Commits on HEAD that are on no remote at all, counted up to
    /// [`crate::git::UNPUSHED_CAP`], which then means that many or more.
    /// `null`, with `stray_branches`, where they are not counted: in a bare
    /// repository, and in one with no commits yet.
    unpushed: Option<usize>,
    /// Other local branches that hold commits on no remote.
    stray_branches: Option<usize>,
    /// Why `ahead`, `behind`, `unpushed` and `stray_branches` are `null`: the
    /// commits between here and the remotes could not be read.
    history_unreadable: Option<String>,
    /// What git said the last time a fetch of it failed, until one works:
    /// as the card says it, with no login in it.
    fetch_failed: Option<String>,
    last_commit: Option<LastCommit>,
    /// As the window shows them.
    tags: Vec<String>,
}

/// What is uncommitted: a card's `+1 ~3 ?4`, and what is in conflict. These
/// are entries, as `git status --short` lists them, and not files: a
/// directory with nothing tracked in it is one, whatever it holds. Each is
/// counted once, in the first of these it is: in conflict, staged, untracked,
/// modified.
#[derive(Serialize)]
struct WorkingTree {
    staged: usize,
    /// Modified, and not staged.
    unstaged: usize,
    untracked: usize,
    conflicted: usize,
}

/// The commit HEAD is on.
#[derive(Serialize)]
struct LastCommit {
    short_id: String,
    /// The first paragraph of its message, on one line.
    summary: String,
    /// The author's name.
    author: String,
    /// When it was committed, in seconds since 1970.
    when: i64,
}

impl Repo {
    fn of(info: &RepoInfo, tags: Vec<String>, fetch_failed: Option<&str>) -> Self {
        // What could be read at all, and of that what was counted.
        let read = info.error.is_none();
        let history = read && info.history_unreadable.is_none();
        let tree = read && !info.bare && info.tree.unreadable.is_none();
        // `inspect` walks for what is on no remote only where there is a
        // working tree, and a commit on HEAD to walk from.
        let walked = history && !info.bare && !info.unborn;
        let t = &info.tree;
        Repo {
            name: info.name.clone(),
            path: info.path.display().to_string(),
            needs_attention: info.needs_attention(fetch_failed.is_some()),
            error: info.error.clone(),
            remote: info.remote.clone(),
            web: info.web.clone(),
            head: read.then(|| info.head.clone()),
            detached: read.then_some(info.detached),
            unborn: read.then_some(info.unborn),
            bare: read.then_some(info.bare),
            upstream: info.upstream.clone(),
            working_tree: tree.then_some(WorkingTree {
                staged: t.staged,
                unstaged: t.unstaged,
                untracked: t.untracked,
                conflicted: t.conflicted,
            }),
            working_tree_unreadable: t.unreadable.clone(),
            stashes: info.stashes.filter(|_| read),
            ahead: history.then_some(info.ahead),
            behind: history.then_some(info.behind),
            unpushed: walked.then_some(info.unpushed),
            stray_branches: walked.then_some(info.stray_branches),
            history_unreadable: info.history_unreadable.clone(),
            fetch_failed: fetch_failed.map(str::to_string),
            last_commit: info.last_commit.as_ref().map(LastCommit::of),
            tags,
        }
    }
}

impl LastCommit {
    fn of(commit: &Commit) -> Self {
        LastCommit {
            short_id: commit.short_id.clone(),
            summary: commit.summary.clone(),
            author: commit.author.clone(),
            when: commit.when,
        }
    }
}

/// Print the snapshot on standard output: the fleet the configuration names,
/// or the roots and depth the command line gives (`--root`, `--depth`) for a
/// one-off look, as the window takes them. Whether it was printed whole.
/// Whatever stopped it is said on standard error, but for a reader that went
/// away (`omarepos --json | head`), who is not waiting to be told.
pub fn print(roots: Vec<String>, depth: Option<usize>) -> bool {
    if !STDOUT_WAS_OPEN.load(Ordering::Relaxed) {
        note!("omarepos: standard output is closed, so there is nowhere to print the snapshot");
        return false;
    }
    match write_to(&mut std::io::stdout().lock(), roots, depth) {
        Ok(()) => true,
        Err(Unprinted::ReaderGone) => false,
        Err(Unprinted::Because(why)) => {
            note!("omarepos: {why}");
            false
        }
    }
}

/// Whether standard output was open when the program was started.
///
/// A run with it closed (`omarepos --json >&-`) printed nothing and exited
/// with 0. Rust's runtime opens `/dev/null` in the place of a closed one
/// before `main`, so by then every write to it is done, and a closed one
/// looks like one sent to `/dev/null` on purpose. Only how it was opened
/// could tell them apart, and a script's own "throw this away" opens it
/// the same way: Python's `subprocess.DEVNULL` and Node's `'ignore'` both
/// do. So this looks before the runtime does: an entry in `.init_array`
/// runs as the program is loaded, ahead of the runtime's start.
static STDOUT_WAS_OPEN: AtomicBool = AtomicBool::new(true);

#[used]
#[unsafe(link_section = ".init_array")]
static LOOK_AT_STDOUT: extern "C" fn() = look_at_stdout;

extern "C" fn look_at_stdout() {
    // SAFETY: fcntl(2) with F_GETFD asks about a descriptor and changes
    // nothing. Neither it nor the store needs the runtime, which is not up.
    let open = unsafe { libc::fcntl(libc::STDOUT_FILENO, libc::F_GETFD) } != -1;
    STDOUT_WAS_OPEN.store(open, Ordering::Relaxed);
}

/// Why the snapshot was not printed, or not all of it.
#[derive(Debug, PartialEq)]
enum Unprinted {
    /// What to say on standard error.
    Because(String),
    /// The reader closed its end, and is not waiting to be told.
    ReaderGone,
}

/// [`print`], to `out`. Nothing is written to it unless the snapshot was
/// taken whole.
fn write_to(
    out: &mut impl std::io::Write,
    roots: Vec<String>,
    depth: Option<usize>,
) -> Result<(), Unprinted> {
    let fleet = take(roots, depth).map_err(Unprinted::Because)?;
    if let Some(why) = &fleet.fetch_failures_unreadable {
        note!("omarepos: no failed fetch is in the snapshot, as their record cannot be read: {why}");
    }
    let text = encode(&fleet).map_err(Unprinted::Because)?;
    out.write_all(text.as_bytes()).and_then(|()| out.flush()).map_err(|err| match err.kind() {
        std::io::ErrorKind::BrokenPipe => Unprinted::ReaderGone,
        _ => Unprinted::Because(format!("the snapshot could not be written: {err}")),
    })
}

/// Read the fleet, or say why there is nothing to print.
fn take(roots: Vec<String>, depth: Option<usize>) -> Result<Fleet, String> {
    // The window carries on with the defaults when a file of its own does not
    // parse, and says so in a banner. There is no banner here, and a snapshot
    // of `~/Work` where the file names other roots, or of every repository
    // without its tags, would be a wrong answer given as if it were right.
    let cfg = Config::try_load()
        .map_err(|why| unusable(&Config::path(), &why))?
        .with_command_line(roots, depth);
    let mut meta = Meta::try_load().map_err(|why| unusable(&crate::meta::path(), &why))?;
    // State, and not what the user wrote: the snapshot is taken without it,
    // and says that it was.
    let (failed, fetch_failures_unreadable) = match crate::config::try_load_fetch_errors() {
        Ok(failed) => (failed, None),
        Err(why) => {
            let path = crate::config::fetch_errors_path();
            (Default::default(), Some(crate::config::why_unusable(&path, &why)))
        }
    };
    let one_off = cfg.one_off();

    let (events, reports) = scan::spawn(cfg.roots(), cfg.extra_repos(), cfg.depth());
    let mut read = Vec::new();
    for (info, seed) in gather(&events)? {
        // What the window's scan does with a root's tag, here in memory and
        // never saved: the tags printed are the ones the window shows from
        // its next scan on. Not for a one-off look, as there: its roots are
        // not the configured ones, and "this root gives no tag" would take
        // the tag away.
        if let Some(seed) = seed.filter(|_| !one_off) {
            meta.apply_root_tag(&info.path, &seed.root, seed.tag.as_deref());
        }
        read.push(info);
    }
    read.sort_by(|a, b| a.path.cmp(&b.path));
    // Sent before anything else, so there by now.
    let reports = reports.try_recv().unwrap_or_default();

    Ok(Fleet {
        schema_version: SCHEMA_VERSION,
        fetch_failures_unreadable,
        roots: reports.iter().map(Root::of).collect(),
        repos: read
            .iter()
            .map(|info| {
                let tags = meta.tags_for(&info.path).to_vec();
                let fetch_failed = failed.get(&info.path.display().to_string());
                Repo::of(info, tags, fetch_failed.map(String::as_str))
            })
            .collect(),
    })
}

/// Everything a scan sends, to its end: each repository it read, with what
/// the root it was found under wants tagged on it. An error when that is not
/// every repository it found.
fn gather(events: &async_channel::Receiver<Event>) -> Result<Vec<(RepoInfo, Option<Seed>)>, String> {
    let mut found = None;
    let mut read = Vec::new();
    loop {
        match events.recv_blocking() {
            Ok(Event::Discovered(count)) => found = Some(count),
            Ok(Event::Repo { info, seed }) => read.push((info, seed)),
            Ok(Event::Done) => break,
            // Only a panic on the scan's own thread ends it without `Done`.
            Err(_) => {
                return Err("no snapshot, because the scan stopped before it was done".to_string());
            }
        }
    }
    // A worker that panics is let go, and `Done` is sent all the same, for
    // the window has the rest of the grid to show. A snapshot short of a
    // repository is not the fleet, and would say nothing of the one missing.
    match found {
        Some(count) if count == read.len() => Ok(read),
        _ => Err(format!(
            "no snapshot, because the scan found {} repositories and read {} of them",
            found.unwrap_or(0),
            read.len()
        )),
    }
}

/// Why there is no snapshot, when a file it reads is there and cannot be
/// used. `why` is the parser's own message, lines and all: this goes to a
/// terminal or a log, where the line it stopped at is worth showing.
fn unusable(path: &Path, why: &str) -> String {
    format!("no snapshot, because {} cannot be used: {why}", path.display())
}

/// The text printed: indented, so that a person can read it as well, and
/// ended with a newline.
fn encode(fleet: &Fleet) -> Result<String, String> {
    let mut text = serde_json::to_string_pretty(fleet)
        .map_err(|err| format!("the snapshot could not be encoded: {err}"))?;
    text.push('\n');
    Ok(text)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use serde_json::{Value, json};

    use super::*;
    use crate::testkit::*;

    /// No configuration and no state. The tests share one isolated home, and
    /// what another test left there is not this test's fleet.
    fn start_afresh() {
        for file in [Config::path(), crate::config::last_refresh_path()] {
            if let Some(dir) = file.parent() {
                let _ = std::fs::remove_dir_all(dir);
            }
        }
    }

    /// A path as the scan reports it, which is how the tags and the failed
    /// fetches are keyed.
    fn key(path: &Path) -> String {
        path.canonicalize().unwrap().display().to_string()
    }

    /// `text` as TOML writes a string.
    fn quoted(text: &str) -> String {
        toml::Value::String(text.to_string()).to_string()
    }

    /// What `--json` would print, read back.
    fn snapshot(roots: Vec<String>, depth: Option<usize>) -> Value {
        let fleet = take(roots, depth).expect("a snapshot");
        serde_json::from_str(&encode(&fleet).unwrap()).unwrap()
    }

    /// The repository called `name` in what was printed.
    fn repo<'a>(printed: &'a Value, name: &str) -> &'a Value {
        printed["repos"]
            .as_array()
            .unwrap()
            .iter()
            .find(|repo| repo["name"] == name)
            .unwrap_or_else(|| panic!("no {name} in {printed:#}"))
    }

    fn names(printed: &Value) -> Vec<&str> {
        printed["repos"].as_array().unwrap().iter().map(|repo| repo["name"].as_str().unwrap()).collect()
    }

    /// Every key of a repository, as they sort. Pinned here so that a change
    /// to the schema is made on purpose: a script was promised these.
    const KEYS: [&str; 22] = [
        "ahead",
        "bare",
        "behind",
        "detached",
        "error",
        "fetch_failed",
        "head",
        "history_unreadable",
        "last_commit",
        "name",
        "needs_attention",
        "path",
        "remote",
        "stashes",
        "stray_branches",
        "tags",
        "unborn",
        "unpushed",
        "upstream",
        "web",
        "working_tree",
        "working_tree_unreadable",
    ];

    /// A fleet of four under a root with a tag: one with work of every kind
    /// in it and commits waiting on the remote, one level with the remote,
    /// one with no commits yet, and one the last fetch of which failed.
    #[test]
    fn the_fleet_is_printed_as_its_cards_show_it() {
        let _guard = isolate();
        start_afresh();
        let f = fleet("json-shape");
        commit_file(&f.work, "mine.txt", "mine\n", "work of its own");
        write(&f.work.join("README.md"), "edited\n");
        write(&f.work.join("notes.txt"), "not added\n");
        f.remote_moves(2);
        git(&f.work, &["fetch", "-q"]);
        git(&f.root, &["init", "-q", "-b", "main", "fresh"]);
        git(&f.root, &["clone", "-q", f.remote.to_str().unwrap(), "quiet"]);
        let quiet = f.root.join("quiet");

        let root = f.root.display().to_string();
        write(&Config::path(), &format!("roots = [{{ path = {}, tag = \"acme\" }}]\n", quoted(&root)));
        write(&crate::meta::path(), &format!("[repos.{}]\ntags = [\"mine\"]\n", quoted(&key(&f.seed))));
        write(
            &crate::config::fetch_errors_path(),
            &format!("{} = \"fatal: unable to access the remote\"\n", quoted(&key(&quiet))),
        );

        let printed = snapshot(Vec::new(), None);
        let mut top: Vec<&String> = printed.as_object().unwrap().keys().collect();
        top.sort();
        assert_eq!(top, ["fetch_failures_unreadable", "repos", "roots", "schema_version"]);
        assert_eq!(printed["schema_version"], 1);
        assert!(printed["fetch_failures_unreadable"].is_null());
        assert_eq!(printed["roots"], json!([{ "path": key(&f.root), "found": 4, "problem": null }]));
        assert_eq!(names(&printed), ["fresh", "quiet", "seed", "work"], "in the order of their paths");
        // Every key on every repository, `null` or not: a script need not ask
        // whether one is there.
        for repo in printed["repos"].as_array().unwrap() {
            let mut keys: Vec<&str> = repo.as_object().unwrap().keys().map(String::as_str).collect();
            keys.sort();
            assert_eq!(keys, KEYS, "{repo:#}");
        }

        let work = repo(&printed, "work");
        let head = git(&f.work, &["rev-parse", "HEAD"]);
        let when: i64 = git(&f.work, &["log", "-1", "--format=%ct"]).parse().unwrap();
        assert_eq!(work["path"], key(&f.work));
        assert_eq!(work["needs_attention"], true);
        assert_eq!((&work["head"], &work["upstream"]), (&json!("main"), &json!("origin/main")));
        assert_eq!((&work["detached"], &work["unborn"], &work["bare"]), (&json!(false), &json!(false), &json!(false)));
        assert_eq!(work["working_tree"], json!({ "staged": 0, "unstaged": 1, "untracked": 1, "conflicted": 0 }));
        assert_eq!(
            [&work["ahead"], &work["behind"], &work["unpushed"], &work["stray_branches"], &work["stashes"]],
            [&json!(1), &json!(2), &json!(1), &json!(0), &json!(0)]
        );
        assert_eq!(
            work["last_commit"],
            json!({ "short_id": &head[..7], "summary": "work of its own", "author": "Fixture", "when": when })
        );
        // The fixture's remote is a directory, which has no page on the web.
        assert_eq!(work["remote"], f.remote.display().to_string().trim_end_matches(".git"));
        assert!(work["web"].is_null());
        assert_eq!(work["tags"], json!(["acme"]), "the root's tag, as the window's next scan gives it");

        let seed = repo(&printed, "seed");
        assert_eq!(seed["needs_attention"], false);
        assert_eq!(seed["working_tree"], json!({ "staged": 0, "unstaged": 0, "untracked": 0, "conflicted": 0 }));
        assert_eq!([&seed["ahead"], &seed["behind"], &seed["unpushed"]], [&json!(0), &json!(0), &json!(0)]);
        assert_eq!(seed["tags"], json!(["acme", "mine"]), "its own tag, and the root's beside it");

        let quiet = repo(&printed, "quiet");
        assert_eq!(quiet["fetch_failed"], "fatal: unable to access the remote");
        assert_eq!(quiet["needs_attention"], true, "level, and the last fetch failed");

        let fresh = repo(&printed, "fresh");
        assert_eq!((&fresh["head"], &fresh["unborn"]), (&json!("main"), &json!(true)));
        assert_eq!(fresh["needs_attention"], false);
        for nothing in ["last_commit", "upstream", "remote", "unpushed", "stray_branches", "fetch_failed", "error"] {
            assert!(fresh[nothing].is_null(), "{nothing}: {fresh:#}");
        }

        start_afresh();
    }

    /// The URL a remote is configured with can carry a token, and a snapshot
    /// ends up in logs and in other programs. No part of a login is printed:
    /// not in the remote, not in its page, not in what a failed fetch said,
    /// whether the login was written properly or not, and whether git left
    /// it out of what it said or not.
    #[test]
    fn a_remote_is_printed_without_whoever_it_logs_in_as() {
        let _guard = isolate();
        start_afresh();
        let base = scratch("json-token");
        // The remote; what the card calls it; its page; and what git said
        // when it was fetched, as git said it.
        let remotes: [(&str, &str, &str, Option<&str>, Option<&str>); 4] = [
            (
                "token",
                "https://someone:SECRET-t0ken@git.example.invalid/acme/billing.git",
                "git.example.invalid/acme/billing",
                Some("https://git.example.invalid/acme/billing"),
                Some(
                    "fatal: unable to access \
                     'https://someone:SECRET-t0ken@git.example.invalid/acme/billing.git/': \
                     Could not resolve host: git.example.invalid",
                ),
            ),
            (
                "at",
                "https://jm:p@SECRET@github.com/acme/widgets.git",
                "github.com/acme/widgets",
                Some("https://github.com/acme/widgets"),
                Some("fatal: unable to access 'https://SECRET@github.com/acme/widgets.git/': URL rejected: Bad hostname"),
            ),
            ("scp", "oauth2:glpat-SECRET@gitlab.com:group/repo.git", "gitlab.com/group/repo", None, None),
            (
                "slash",
                "https://deploy:abc/SECRET@example.invalid/x.git",
                "example.invalid/x",
                None,
                Some(
                    "fatal: unable to access 'https://deploy:abc/SECRET@example.invalid/x.git/': \
                     URL rejected: Port number was not a decimal number between 0 and 65535",
                ),
            ),
        ];
        let mut record = String::new();
        for (name, url, _, _, said) in remotes {
            git(&base, &["init", "-q", "-b", "main", name]);
            let dir = base.join(name);
            commit_file(&dir, "a.txt", "a\n", "one");
            git(&dir, &["remote", "add", "origin", url]);
            if let Some(said) = said {
                record.push_str(&format!("{} = {}\n", quoted(&key(&dir)), quoted(said)));
            }
        }
        write(&crate::config::fetch_errors_path(), &record);

        let fleet = take(vec![base.display().to_string()], None).expect("a snapshot");
        let text = encode(&fleet).unwrap();
        for secret in ["SECRET", "someone", "deploy", "oauth2"] {
            assert!(!text.contains(secret), "{secret} in {text}");
        }

        let printed: Value = serde_json::from_str(&text).unwrap();
        for (name, _, named, page, said) in remotes {
            let shown = repo(&printed, name);
            assert_eq!(shown["remote"], named, "{name}");
            assert_eq!(shown["web"], json!(page), "{name}");
            assert_eq!(shown["fetch_failed"].is_string(), said.is_some(), "{name}");
        }
        assert_eq!(
            repo(&printed, "slash")["fetch_failed"],
            "fatal: unable to access 'https://example.invalid/x.git/': \
             URL rejected: Port number was not a decimal number between 0 and 65535"
        );

        start_afresh();
    }

    /// Every file and directory under `dir`, with what it holds and when it
    /// was last changed.
    fn every_file(dir: &Path) -> BTreeMap<PathBuf, (Vec<u8>, std::time::SystemTime)> {
        let mut found = BTreeMap::new();
        let mut waiting = vec![dir.to_path_buf()];
        while let Some(dir) = waiting.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else { continue };
            for entry in entries.flatten() {
                let (path, meta) = (entry.path(), entry.metadata().unwrap());
                let held = if meta.is_dir() {
                    waiting.push(path.clone());
                    Vec::new()
                } else {
                    std::fs::read(&path).unwrap_or_default()
                };
                found.insert(path, (held, meta.modified().unwrap()));
            }
        }
        found
    }

    /// The window's scan writes: the tags a root seeds, the repositories it
    /// has seen, the last scan. A snapshot writes none of it, nor anything
    /// else, in the home directory or in the repositories it reads, even
    /// with a root whose tag the tag file does not have yet.
    #[test]
    fn a_snapshot_writes_nothing() {
        let _guard = isolate();
        start_afresh();
        let f = fleet("json-writes-nothing");
        commit_file(&f.work, "mine.txt", "mine\n", "work of its own");
        write(&f.work.join("README.md"), "edited\n");
        let root = f.root.display().to_string();
        write(&Config::path(), &format!("roots = [{{ path = {}, tag = \"acme\" }}]\n", quoted(&root)));
        write(&crate::meta::path(), "# Tagged by hand, and nothing yet.\n");
        write(&crate::config::fetch_errors_path(), &format!("{} = \"timed out\"\n", quoted(&key(&f.work))));
        write(&crate::config::known_repos_path(), "/somewhere/seen\n");

        let home = crate::config::home();
        let before = (every_file(&home), every_file(&f.root));
        let printed = snapshot(Vec::new(), None);
        let after = (every_file(&home), every_file(&f.root));

        for (was, is) in [(&before.0, &after.0), (&before.1, &after.1)] {
            let written: Vec<&PathBuf> =
                was.keys().chain(is.keys()).filter(|path| was.get(*path) != is.get(*path)).collect();
            assert!(written.is_empty(), "written by a snapshot: {written:?}");
        }
        // ...where the window would have written the tag file.
        assert_eq!(repo(&printed, "work")["tags"], json!(["acme"]));
        assert_eq!(read(&crate::meta::path()), "# Tagged by hand, and nothing yet.\n");

        start_afresh();
    }

    /// The window runs on the defaults, under a banner, when a file of its
    /// own does not parse. A script has no banner to read: there is no
    /// snapshot, the error says why, and neither file is touched.
    #[test]
    fn a_file_that_cannot_be_used_stops_the_snapshot() {
        let _guard = isolate();
        start_afresh();
        let base = scratch("json-unusable");
        git(&base, &["init", "-q", "-b", "main", "app"]);
        let one_off = || vec![base.display().to_string()];

        let config = "roots = [\"~/Work\"]\nmax_dpeth = 4\n";
        write(&Config::path(), config);
        let why = take(Vec::new(), None).err().expect("no snapshot on the defaults");
        assert!(why.contains("config.toml") && why.contains("max_dpeth"), "{why}");
        // A one-off look takes its extra repositories from the file as well.
        assert!(take(one_off(), Some(1)).is_err());
        assert_eq!(read(&Config::path()), config);
        let _ = std::fs::remove_file(Config::path());

        let tags = "[repos.\"/srv/app\"]\ntags = [\"work\"\n";
        write(&crate::meta::path(), tags);
        let why = take(one_off(), None).err().expect("no snapshot without the tags");
        assert!(why.contains("repos.toml") && why.contains("TOML parse error"), "{why}");
        assert_eq!(read(&crate::meta::path()), tags);
        let _ = std::fs::remove_file(crate::meta::path());

        assert_eq!(names(&snapshot(one_off(), None)), ["app"]);
        // No file at all is the first run, as it is for the window: `~/Work`,
        // which is not there, and says so.
        let printed = snapshot(Vec::new(), None);
        assert_eq!(printed["roots"][0]["problem"], "does not exist");
        assert_eq!(printed["repos"], json!([]));

        start_afresh();
    }

    /// A record of failed fetches that cannot be read does not stop the
    /// snapshot, as the window goes on without it too. What is printed says
    /// that the failures are not known, which is not the same as none.
    #[test]
    fn a_record_of_failed_fetches_that_cannot_be_read_is_said() {
        let _guard = isolate();
        start_afresh();
        let f = fleet("json-unread-record");
        let root = || vec![f.root.display().to_string()];
        // As a write in place leaves it, caught half done.
        write(&crate::config::fetch_errors_path(), &format!("{} = \"timed ou", quoted(&key(&f.work))));

        let printed = snapshot(root(), None);
        let why = printed["fetch_failures_unreadable"].as_str().expect("said in what is printed");
        assert!(why.contains("fetch-errors.toml"), "{why}");
        let work = repo(&printed, "work");
        assert!(work["fetch_failed"].is_null(), "{work:#}");
        assert_eq!(work["needs_attention"], false);

        let _ = std::fs::remove_file(crate::config::fetch_errors_path());
        assert!(snapshot(root(), None)["fetch_failures_unreadable"].is_null(), "no record is no failures");

        start_afresh();
    }

    /// `--root` and `--depth` make it a one-off look, as they make the window
    /// one: those roots and that depth, for this run, and no root's tag
    /// applied or taken away. A root given on the command line names no tag,
    /// and taken at its word it would retire the tag it seeded before.
    #[test]
    fn the_command_line_makes_it_a_one_off_look() {
        let _guard = isolate();
        start_afresh();
        let base = scratch("json-one-off");
        for repo in ["configured/app", "elsewhere/top", "elsewhere/group/deep"] {
            let dir = base.join(repo);
            std::fs::create_dir_all(&dir).unwrap();
            git(&dir, &["init", "-q", "-b", "main"]);
        }
        let configured = base.join("configured").display().to_string();
        let elsewhere = base.join("elsewhere").display().to_string();
        write(&Config::path(), &format!("roots = [{{ path = {}, tag = \"acme\" }}]\n", quoted(&configured)));
        // `elsewhere` was once configured with a tag of its own.
        let top = quoted(&key(&base.join("elsewhere/top")));
        write(
            &crate::meta::path(),
            &format!("[repos.{top}]\ntags = [\"old\"]\n\n[repos.{top}.seeded_by]\n{} = \"old\"\n", quoted(&elsewhere)),
        );

        let printed = snapshot(vec![elsewhere.clone()], Some(1));
        assert_eq!(names(&printed), ["top"], "one level down, and not the configured root");
        assert_eq!(printed["roots"], json!([{ "path": key(&base.join("elsewhere")), "found": 1, "problem": null }]));
        assert_eq!(repo(&printed, "top")["tags"], json!(["old"]));

        assert_eq!(names(&snapshot(vec![elsewhere], None)), ["deep", "top"], "the configured depth");

        let printed = snapshot(Vec::new(), None);
        assert_eq!(names(&printed), ["app"]);
        assert_eq!(repo(&printed, "app")["tags"], json!(["acme"]));

        start_afresh();
    }

    /// Where a card says a reading could not be taken, what is printed is
    /// `null` and not the zero that stood in for it: a repository that cannot
    /// be read, a history with a commit missing from it, an index nothing can
    /// read, and a bare repository, which has no working tree to read.
    #[test]
    fn a_reading_that_could_not_be_taken_is_null_and_never_zero() {
        let _guard = isolate();
        start_afresh();
        let base = scratch("json-unread");
        for name in ["broken", "lost", "split"] {
            git(&base, &["init", "-q", "-b", "main", name]);
            for file in ["a", "b", "c"] {
                commit_file(&base.join(name), &format!("{file}.txt"), "x\n", file);
            }
        }
        lose_object(&base.join("broken"), "HEAD");
        lose_object(&base.join("lost"), "HEAD~1");
        let split = base.join("split");
        git(&split, &["update-index", "--split-index"]);
        write(&split.join("a.txt"), "edited\n");
        git(&base, &["init", "-q", "--bare", "-b", "main", "bare/.git"]);
        let stashed = base.join("stashed");
        git(&base, &["init", "-q", "-b", "main", "stashed"]);
        commit_file(&stashed, "a.txt", "a\n", "a");
        write(&stashed.join("a.txt"), "edited\n");
        git(&stashed, &["stash", "-q"]);
        write(&stashed.join(".git/refs/stash"), "cut short by a crash\n");

        let printed = snapshot(vec![base.display().to_string()], None);
        let null = |repo: &Value, keys: &[&str]| {
            for key in keys {
                assert!(repo[key].is_null(), "{key}: {repo:#}");
            }
        };

        let broken = repo(&printed, "broken");
        assert!(broken["error"].is_string(), "{broken:#}");
        assert_eq!(broken["needs_attention"], true);
        null(broken, &["head", "detached", "unborn", "bare", "working_tree", "stashes"]);
        null(broken, &["ahead", "behind", "unpushed", "stray_branches", "last_commit"]);

        let lost = repo(&printed, "lost");
        assert!(lost["history_unreadable"].is_string(), "{lost:#}");
        assert_eq!(lost["needs_attention"], true);
        null(lost, &["ahead", "behind", "unpushed", "stray_branches", "error"]);
        assert_eq!(lost["working_tree"], json!({ "staged": 0, "unstaged": 0, "untracked": 0, "conflicted": 0 }));
        assert_eq!(lost["last_commit"]["summary"], "c");

        let split = repo(&printed, "split");
        assert!(split["working_tree_unreadable"].is_string(), "{split:#}");
        assert_eq!(split["needs_attention"], true);
        null(split, &["working_tree", "error", "history_unreadable"]);
        assert_eq!(split["unpushed"], 3, "its history reads perfectly well");

        let bare = repo(&printed, "bare");
        assert_eq!(bare["bare"], true, "{bare:#}");
        null(bare, &["working_tree", "working_tree_unreadable", "unpushed", "stray_branches", "error"]);

        let stashed = repo(&printed, "stashed");
        null(stashed, &["stashes", "error"]);
        assert_eq!(stashed["head"], "main", "{stashed:#}");

        start_afresh();
    }

    /// A root that is there and gives nothing is said as the status bar says
    /// it: one with no repository in it, and a workspace whose clones its own
    /// repository does not ignore, which are left out and named.
    #[test]
    fn a_root_that_gives_nothing_says_why() {
        let _guard = isolate();
        start_afresh();
        let base = scratch("json-roots");
        std::fs::create_dir_all(base.join("empty/notes")).unwrap();
        git(&base, &["init", "-q", "-b", "main", "workspace"]);
        git(&base.join("workspace"), &["init", "-q", "-b", "main", "clone"]);
        let (empty, workspace) = (base.join("empty"), base.join("workspace"));
        write(
            &Config::path(),
            &format!(
                "roots = [{}, {{ path = {}, nested = true }}]\n",
                quoted(&empty.display().to_string()),
                quoted(&workspace.display().to_string()),
            ),
        );

        let printed = snapshot(Vec::new(), None);
        assert_eq!(
            printed["roots"],
            json!([
                { "path": key(&empty), "found": 0, "problem": "no repositories found" },
                {
                    "path": key(&workspace),
                    "found": 0,
                    "problem": "no repositories found: 1 repository in it is not ignored by its own \
                                repository, and so is left out (clone)",
                },
            ])
        );
        assert_eq!(printed["repos"], json!([]));

        start_afresh();
    }

    /// A root given as a relative path is printed whole, as the paths of the
    /// repositories under it are, whether it is there or not. The tests run
    /// in the crate's own directory, which is what these are relative to.
    #[test]
    fn a_relative_root_is_printed_whole() {
        let _guard = isolate();
        start_afresh();
        let base = scratch("json-relative");
        git(&base, &["init", "-q", "-b", "main", "app"]);
        let relative = "target/test-fixtures/json-relative";
        assert_eq!(Path::new(relative).canonicalize().unwrap(), base.canonicalize().unwrap());

        let printed = snapshot(vec![relative.to_string(), format!("{relative}-missing")], None);
        let (here, missing) = (&printed["roots"][0], &printed["roots"][1]);
        assert_eq!(here["path"], key(&base));
        assert!(repo(&printed, "app")["path"].as_str().unwrap().starts_with(&key(&base)));
        let absolute = std::path::absolute(format!("{relative}-missing")).unwrap();
        assert_eq!(missing["path"], absolute.display().to_string());
        assert_eq!(missing["problem"], "does not exist");

        start_afresh();
    }

    /// A worker that panics is let go, and the scan still ends with `Done`:
    /// the window has the rest of the grid to show. A snapshot one short is
    /// not the fleet, and is not printed.
    #[test]
    fn a_scan_that_read_fewer_than_it_found_is_no_snapshot() {
        let _guard = isolate();
        let dir = scratch("json-gather");
        git(&dir, &["init", "-q", "-b", "main"]);
        let reading = || Event::Repo { info: crate::git::inspect(&dir), seed: None };
        let gathered = |events: Vec<Event>| {
            let (tx, rx) = async_channel::unbounded();
            for event in events {
                tx.send_blocking(event).unwrap();
            }
            drop(tx);
            gather(&rx).map(|read| read.len())
        };

        assert_eq!(gathered(vec![Event::Discovered(2), reading(), reading(), Event::Done]), Ok(2));
        assert_eq!(
            gathered(vec![Event::Discovered(2), reading(), Event::Done]),
            Err("no snapshot, because the scan found 2 repositories and read 1 of them".to_string())
        );
        assert!(gathered(vec![Event::Discovered(1), reading()]).is_err(), "it never said Done");
    }

    /// A reader that is not there, in the way given.
    struct Gone(std::io::ErrorKind);

    impl std::io::Write for Gone {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(self.0.into())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// What reaches standard output: the whole snapshot or nothing, and what
    /// is said when it cannot be written, which is nothing to a reader that
    /// went away (`omarepos --json | head`).
    #[test]
    fn the_snapshot_is_printed_whole_or_not_at_all() {
        let _guard = isolate();
        start_afresh();
        let f = fleet("json-printed");
        let root = || vec![f.root.display().to_string()];

        let mut out = Vec::new();
        assert_eq!(write_to(&mut out, root(), None), Ok(()));
        let printed: Value = serde_json::from_slice(&out).expect("JSON, and nothing else");
        assert_eq!(names(&printed), ["seed", "work"]);
        assert!(out.ends_with(b"}\n"));

        write(&Config::path(), "max_dpeth = 4\n");
        let mut out = Vec::new();
        let Err(Unprinted::Because(why)) = write_to(&mut out, root(), None) else { panic!("printed") };
        assert!(why.contains("config.toml"), "{why}");
        assert!(out.is_empty(), "something printed: {}", String::from_utf8_lossy(&out));
        let _ = std::fs::remove_file(Config::path());

        let gone = write_to(&mut Gone(std::io::ErrorKind::BrokenPipe), root(), None);
        assert_eq!(gone, Err(Unprinted::ReaderGone));
        let Err(Unprinted::Because(why)) = write_to(&mut Gone(std::io::ErrorKind::StorageFull), root(), None)
        else {
            panic!("a full disk said nothing")
        };
        assert!(why.starts_with("the snapshot could not be written"), "{why}");

        start_afresh();
    }
}
