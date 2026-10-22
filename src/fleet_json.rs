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
//! token: the remote is given tidied, as the card gives it, and a URL in what
//! a failed fetch said loses whoever it logs in as. And it stays a snapshot. A
//! fetch from the command line is not what this grows into.

use std::io::Write as _;
use std::path::Path;

use serde::Serialize;

use crate::config::Config;
use crate::git::{Commit, RepoInfo, RootReport};
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
    /// As configured, with `~` expanded.
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
        Root { path: report.root.clone(), found: report.found, problem: report.says() }
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
    /// What git said the last time a fetch of it failed, until one works.
    fetch_failed: Option<String>,
    last_commit: Option<LastCommit>,
    /// As the window shows them.
    tags: Vec<String>,
}

/// What is uncommitted: a card's `+1 ~3 ?4`, and what is in conflict. A file
/// is counted once, in the first of these it is: in conflict, staged,
/// untracked, modified.
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
            stashes: read.then_some(info.stashes),
            ahead: history.then_some(info.ahead),
            behind: history.then_some(info.behind),
            unpushed: walked.then_some(info.unpushed),
            stray_branches: walked.then_some(info.stray_branches),
            history_unreadable: info.history_unreadable.clone(),
            fetch_failed: fetch_failed.map(without_credentials),
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

/// `said` with whoever a URL in it logs in as taken out, so that
/// `https://me:token@host/x` is printed as `https://host/x`.
///
/// What a failed fetch said is git's own words, and git leaves the user name
/// and the password out of the URLs it prints in nearly all of them. A
/// snapshot is copied into logs and handed to other programs, and nearly all
/// is not enough.
fn without_credentials(said: &str) -> String {
    let mut out = String::with_capacity(said.len());
    let mut rest = said;
    while let Some(at) = rest.find("://") {
        let (before, after) = rest.split_at(at + "://".len());
        out.push_str(before);
        // Up to where the host ends. A URL in a sentence is often quoted.
        let end = after
            .find(|c: char| c == '/' || c.is_whitespace() || matches!(c, '\'' | '"' | '<' | '>'))
            .unwrap_or(after.len());
        let authority = &after[..end];
        // The last `@`: a password with one in it has to be written `%40` to
        // work, but one written as it is must not leave half of itself here.
        out.push_str(authority.rsplit_once('@').map_or(authority, |(_, host)| host));
        rest = &after[end..];
    }
    out.push_str(rest);
    out
}

/// Print the snapshot on standard output: the fleet the configuration names,
/// or the roots and depth the command line gives (`--root`, `--depth`) for a
/// one-off look, as the window takes them. Whether it was printed whole.
/// Whatever stopped it is said on standard error, but for a reader that went
/// away (`omarepos --json | head`), who is not waiting to be told.
pub fn print(roots: Vec<String>, depth: Option<usize>) -> bool {
    let text = match take(roots, depth).and_then(|fleet| encode(&fleet)) {
        Ok(text) => text,
        Err(why) => {
            note!("omarepos: {why}");
            return false;
        }
    };
    let mut out = std::io::stdout().lock();
    match out.write_all(text.as_bytes()).and_then(|()| out.flush()) {
        Ok(()) => true,
        Err(err) if err.kind() == std::io::ErrorKind::BrokenPipe => false,
        Err(err) => {
            note!("omarepos: the snapshot could not be written: {err}");
            false
        }
    }
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
    let failed = crate::config::load_fetch_errors();
    let one_off = cfg.one_off();

    let (events, reports) = scan::spawn(cfg.roots(), cfg.extra_repos(), cfg.depth());
    let mut read = Vec::new();
    loop {
        match events.recv_blocking() {
            Ok(Event::Discovered(_)) => {}
            Ok(Event::Repo { info, seed }) => {
                // What the window's scan does with a root's tag, here in
                // memory and never saved: the tags printed are the ones the
                // window shows from its next scan on. Not for a one-off look,
                // as there: its roots are not the configured ones, and "this
                // root gives no tag" would take the tag away.
                if let Some(seed) = seed.filter(|_| !one_off) {
                    meta.apply_root_tag(&info.path, &seed.root, seed.tag.as_deref());
                }
                read.push(info);
            }
            Ok(Event::Done) => break,
            // Only a panic on the scan's own thread ends it without `Done`,
            // and a fleet less whatever it had not reached is not the fleet.
            Err(_) => return Err("the scan stopped before it had read every repository".to_string()),
        }
    }
    read.sort_by(|a, b| a.path.cmp(&b.path));
    // Sent before anything else, so there by now.
    let reports = reports.try_recv().unwrap_or_default();

    Ok(Fleet {
        schema_version: SCHEMA_VERSION,
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
        assert_eq!(top, ["repos", "roots", "schema_version"]);
        assert_eq!(printed["schema_version"], 1);
        assert_eq!(printed["roots"], json!([{ "path": root, "found": 4, "problem": null }]));
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
    /// ends up in logs and in other programs. Neither the token nor the name
    /// it logs in as is printed: not in the remote, not in its page, not in
    /// what a failed fetch said.
    #[test]
    fn a_remote_is_printed_without_whoever_it_logs_in_as() {
        let _guard = isolate();
        start_afresh();
        let base = scratch("json-token");
        git(&base, &["init", "-q", "-b", "main", "billing"]);
        let billing = base.join("billing");
        commit_file(&billing, "a.txt", "a\n", "one");
        let url = "https://someone:s3cr3t-t0ken@git.example.invalid/acme/billing.git";
        git(&billing, &["remote", "add", "origin", url]);
        let said = format!("fatal: unable to access '{url}/': Could not resolve host: git.example.invalid");
        write(&crate::config::fetch_errors_path(), &format!("{} = {}\n", quoted(&key(&billing)), quoted(&said)));

        let fleet = take(vec![base.display().to_string()], None).expect("a snapshot");
        let text = encode(&fleet).unwrap();
        assert!(!text.contains("s3cr3t-t0ken"), "{text}");
        assert!(!text.contains("someone"), "{text}");

        let printed: Value = serde_json::from_str(&text).unwrap();
        let billing = repo(&printed, "billing");
        assert_eq!(billing["remote"], "git.example.invalid/acme/billing");
        assert_eq!(billing["web"], "https://git.example.invalid/acme/billing");
        assert_eq!(
            billing["fetch_failed"],
            "fatal: unable to access 'https://git.example.invalid/acme/billing.git/': \
             Could not resolve host: git.example.invalid"
        );

        start_afresh();
    }

    #[test]
    fn whoever_a_url_logs_in_as_is_taken_out_of_what_git_said() {
        for (said, printed) in [
            (
                "fatal: unable to access 'https://me:tok@en@host.example/x.git/': error: 403",
                "fatal: unable to access 'https://host.example/x.git/': error: 403",
            ),
            ("ssh://deploy:hunter2@host.example:2222/x.git: refused", "ssh://host.example:2222/x.git: refused"),
            ("could not reach https://ghp_token@host.example", "could not reach https://host.example"),
            // Nothing secret where there is no URL: whom an ssh remote logs in
            // as, and an `@` in a path.
            ("git@github.com: Permission denied (publickey).", "git@github.com: Permission denied (publickey)."),
            ("https://host.example/team@2024/x.git not found", "https://host.example/team@2024/x.git not found"),
            ("git fetch gave no answer in 60 seconds", "git fetch gave no answer in 60 seconds"),
        ] {
            assert_eq!(without_credentials(said), printed, "{said}");
        }
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
        assert_eq!(printed["roots"], json!([{ "path": elsewhere, "found": 1, "problem": null }]));
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

        start_afresh();
    }
}
