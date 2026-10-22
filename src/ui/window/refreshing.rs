//! The window's side of fetching: the timer and what it covers, the record
//! of fetches that failed, which repositories count as seen by the user, and
//! reading a repository again once something has happened to it. What a
//! fetch *is* lives in `crate::refresh`; this is only when one runs, over
//! what, and what the grid makes of the answer.

use std::cell::Cell;
use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gtk::glib;
use adw::prelude::*;

use crate::refresh::{self, Summary};
use crate::ui;

use super::{OnDrop, Ui, noun};

impl Ui {
    /// (Re)arm the periodic refresh from the current settings. Called at
    /// startup and after every settings change.
    pub(super) fn schedule_refresh(self: &Rc<Self>) {
        let settings = self.cfg.borrow().refresh.clone();
        let armed = self.refresh_armed.get();
        if settings.every_minutes == armed
            && (armed == 0 || self.refresh_source.borrow().is_some())
        {
            // Same interval, so leave the running countdown where it is.
            self.update_status();
            return;
        }

        if let Some(source) = self.refresh_source.borrow_mut().take() {
            source.remove();
        }
        self.refresh_armed.set(settings.every_minutes);
        // A run pointed elsewhere by the command line is a look, not the
        // fleet's keeper: the ordinary window, if one is open, has that job,
        // and two timers fetching the same repositories get in each other's
        // way.
        if !settings.enabled() || self.cfg.borrow().one_off() {
            self.update_status();
            return;
        }
        let ui = Rc::clone(self);
        let source = glib::timeout_add_local(settings.interval(), move || {
            ui.run_refresh(false);
            glib::ControlFlow::Continue
        });
        *self.refresh_source.borrow_mut() = Some(source);
        self.update_status();
    }

    /// Whether the user has seen this repository. Everything counts as seen
    /// until the first scan has recorded what was already on the grid.
    pub(super) fn is_known(&self, path: &Path) -> bool {
        self.known
            .borrow()
            .as_ref()
            .is_none_or(|known| known.contains(&path.display().to_string()))
    }

    /// The user has opened this repository, or asked for it to be fetched.
    pub(super) fn accept(self: &Rc<Self>, path: &Path) {
        let fresh = match self.known.borrow_mut().as_mut() {
            Some(known) => known.insert(path.display().to_string()),
            None => false,
        };
        if fresh {
            if let Some(known) = self.known.borrow().as_ref() {
                crate::config::save_known_repos(known);
            }
            self.rebuild();
        }
    }

    /// A repository that is no longer on the grid cannot be shown its record,
    /// and the record would otherwise sit in the state file for good.
    pub(super) fn forget_trouble_for_the_departed(&self) {
        let here: HashSet<String> =
            self.repos.borrow().iter().map(|repo| repo.path.display().to_string()).collect();
        let mut errors = self.fetch_errors.borrow_mut();
        let before = errors.len();
        errors.retain(|path, _| here.contains(path));
        if errors.len() != before {
            crate::config::save_fetch_errors(&errors);
        }
    }

    /// At the end of a scan: on the first run, take everything found as seen;
    /// after that, forget repositories that are gone, so that something else
    /// appearing at the same path later is new again.
    pub(super) fn reconcile_known(&self) {
        let mut known = self.known.borrow_mut();
        match known.as_mut() {
            None => {
                let found: BTreeSet<String> = self
                    .repos
                    .borrow()
                    .iter()
                    .map(|repo| repo.path.display().to_string())
                    .collect();
                crate::config::save_known_repos(&found);
                *known = Some(found);
            }
            Some(known) => {
                let before = known.len();
                // Only where the parent is there to be looked in: a drive that
                // is not mounted has not had its repositories deleted.
                known.retain(|path| {
                    let path = Path::new(path);
                    path.exists() || !path.parent().is_some_and(Path::exists)
                });
                if known.len() != before {
                    crate::config::save_known_repos(known);
                }
            }
        }
    }

    /// Keep the record of fetches that failed, from one report. Every way a
    /// repository gets fetched goes through here (the pass across the fleet,
    /// `f` on a card, and the same key on a graph page), so a card cannot go
    /// on saying "fetch failed" about a repository that has since been
    /// fetched by hand. Returns whether this is trouble the record did not
    /// already hold.
    fn note_outcome(&self, report: &refresh::Report) -> bool {
        let key = report.path.display().to_string();
        let trouble = match &report.outcome {
            refresh::Outcome::Failed(err) => Some(err.clone()),
            _ => None,
        };
        let mut errors = self.fetch_errors.borrow_mut();
        match trouble {
            Some(trouble) => {
                let new = errors.get(&key) != Some(&trouble);
                errors.insert(key, trouble);
                new
            }
            None => {
                // Only a fetch that happened clears one that failed.
                if report.fetched {
                    errors.remove(&key);
                }
                false
            }
        }
    }

    /// A fetch, of the fleet or of one repository, has ended. Safe to say
    /// twice: the future says it when it is done, and again when it goes.
    fn refresh_is_over(&self) {
        if !self.refreshing.replace(false) {
            return;
        }
        self.refresh_cancel.borrow_mut().take();
        self.spinner.stop();
        self.spinner.set_visible(self.scanning.get());
        self.update_status();
    }

    /// Call off the pass across the fleet, if one is under way.
    pub(super) fn cancel_refresh(&self) {
        if let Some(cancel) = self.refresh_cancel.borrow().as_ref() {
            cancel.stop();
        }
    }

    /// Which repositories a refresh covers: everything found, narrowed to the
    /// configured tags when there are any. Deliberately not the *filtered*
    /// grid — what you are looking at should not change what gets fetched.
    pub(super) fn refresh_scope(&self) -> Vec<PathBuf> {
        let wanted: Vec<String> = self
            .cfg
            .borrow()
            .refresh
            .only_tags
            .iter()
            .map(|t| t.trim().to_lowercase())
            .filter(|t| !t.is_empty())
            .collect();
        let meta = self.meta.borrow();
        self.repos
            .borrow()
            .iter()
            .filter(|repo| repo.error.is_none())
            .filter(|repo| {
                wanted.is_empty()
                    || meta
                        .tags_for(&repo.path)
                        .iter()
                        .any(|tag| wanted.contains(&tag.to_lowercase()))
            })
            .map(|repo| repo.path.clone())
            .collect()
    }

    /// A timer only runs while the window is open, and it first fires a whole
    /// interval after launch. An app opened for five minutes at a time would
    /// therefore never refresh at all — which is what happened. The last
    /// refresh is remembered across runs, and one that is due runs now.
    pub(super) fn refresh_if_overdue(self: &Rc<Self>) {
        if self.checked_overdue.replace(true) {
            return;
        }
        let settings = self.cfg.borrow().refresh.clone();
        if !settings.enabled() || !settings.will_fetch() || self.cfg.borrow().one_off() {
            return;
        }
        let due = match self.last_refresh.get() {
            // Saturating: the stamp is read from a file, and a file can say
            // anything.
            Some(last) => {
                now_seconds().saturating_sub(last) >= settings.interval().as_secs() as i64
            }
            None => true,
        };
        if due {
            self.run_refresh(false);
        }
    }

    /// Run a refresh pass. `manual` means the user asked, which fetches even
    /// when the background setting is off and reports when nothing was done.
    pub(super) fn run_refresh(self: &Rc<Self>, manual: bool) {
        if self.refreshing.get() || self.scanning.get() {
            if manual {
                self.toasts
                    .add_toast(ui::toast("Already busy — try again in a moment"));
            }
            return;
        }

        let settings = self.cfg.borrow().refresh.clone();
        if !manual && !settings.will_fetch() {
            // Local-only refresh: just re-read what is on disk.
            self.start_scan();
            return;
        }

        // A pass across the fleet enters only repositories the user has seen.
        let (repos, unseen) =
            crate::config::only_known(self.refresh_scope(), self.known.borrow().as_ref());
        if repos.is_empty() {
            if manual {
                let only = settings
                    .only_tags
                    .iter()
                    .map(|tag| tag.trim())
                    .filter(|tag| !tag.is_empty())
                    .collect::<Vec<_>>()
                    .join(", ");
                self.toasts.add_toast(ui::toast(&if unseen > 0 {
                    format!("Nothing fetched: {unseen} new here, open each once to include it")
                } else if !only.is_empty() {
                    format!("Nothing fetched: the refresh covers what is tagged {only}, and nothing is")
                } else {
                    "Nothing in scope to fetch".to_string()
                }));
            }
            return;
        }

        // A pass nobody asked for does not go back to a remote that turned
        // the last fetch away. It did, every interval, for as long as the
        // window stayed open: a wrong or expired credential offered to the
        // same server again and again, which is how accounts get locked.
        // Asking by hand (`f`, Ctrl+Shift+R) still tries, and a fetch
        // that then works clears the record.
        let before = repos.len();
        let repos: Vec<PathBuf> = if manual {
            repos
        } else {
            let errors = self.fetch_errors.borrow();
            repos
                .into_iter()
                .filter(|path| {
                    !errors
                        .get(&path.display().to_string())
                        .is_some_and(|err| refused_by_remote(err))
                })
                .collect()
        };
        let given_up = before - repos.len();
        if repos.is_empty() {
            return;
        }

        let total = repos.len();
        // The spinner and the count are on the grid's page. Asked for from
        // any other, the pass would start without a sign of having started.
        if manual && !self.on_grid() {
            self.toasts.add_toast(ui::toast(&format!(
                "Fetching {total} {}…",
                noun(total, "repository", "repositories")
            )));
        }
        self.refreshing.set(true);
        self.refresh_progress.set((0, total));
        self.spinner.set_visible(true);
        self.spinner.start();
        self.update_status();

        let cancel = refresh::Cancel::default();
        *self.refresh_cancel.borrow_mut() = Some(cancel.clone());
        let rx = refresh::spawn(
            repos,
            refresh::Options {
                // A pass across the fleet fetches, and nothing else.
                fast_forward: false,
                max_parallel: settings.max_parallel,
                timeout: std::time::Duration::from_secs(settings.timeout_seconds.clamp(5, 600)),
                // Nobody picked these one by one, whether the timer started
                // the pass or a key did.
                pass: refresh::Pass::Fleet,
                cancel: cancel.clone(),
            },
        );

        let ui = Rc::clone(self);
        glib::spawn_future_local(async move {
            let _over = OnDrop({
                let ui = Rc::clone(&ui);
                move || ui.refresh_is_over()
            });
            let mut summary = Summary::default();
            for _ in 0..unseen {
                summary.add(&refresh::Outcome::Skipped(refresh::Skip::Unseen));
            }
            if given_up > 0 {
                summary.skipped += given_up;
                *summary.skips.entry("refused by the remote last time").or_insert(0) += given_up;
            }
            let mut done = 0usize;
            // Whether any remote was reached at all.
            let mut reached = false;
            // Trouble that was not already on a card. A repository that fails
            // the same way on every pass is said once, not every half hour.
            let mut new_trouble = 0usize;
            while let Ok(event) = rx.recv().await {
                match event {
                    refresh::Event::Repo(report) => {
                        done += 1;
                        reached |= report.fetched;
                        ui.refresh_progress.set((done, total));
                        summary.add(&report.outcome);
                        if ui.note_outcome(&report) {
                            new_trouble += 1;
                        }
                        // A graph page open on this repository draws what the
                        // pass has just moved: the remote's branches, and how
                        // far behind them HEAD is.
                        if report.fetched && !matches!(report.outcome, refresh::Outcome::UpToDate)
                        {
                            ui.reload_graph_pages(&report.path);
                        }
                        match &report.outcome {
                            refresh::Outcome::Failed(err) => note!(
                                "omarepos: {} failed: {err} ({})",
                                report.name,
                                report.path.display()
                            ),
                            refresh::Outcome::Skipped(skip) => note!(
                                "omarepos: {} left alone: {} ({})",
                                report.name,
                                skip.reason(),
                                report.path.display()
                            ),
                            _ => {}
                        }
                        ui.update_status();
                    }
                    refresh::Event::Done => break,
                }
            }

            ui.refresh_is_over();
            // A one-off look is at somebody else's repositories. What it
            // could not fetch, and when it last fetched, is not the fleet's
            // record to keep.
            let one_off = ui.cfg.borrow().one_off();
            if !one_off {
                crate::config::save_fetch_errors(&ui.fetch_errors.borrow());
            }
            if cancel.stopped() {
                // Not stamped as a refresh: the fleet was not covered, and the
                // next pass should not think it was.
                ui.toasts.add_toast(ui::toast(&format!(
                    "Refresh stopped after {done} of {total}: {}",
                    summary.line()
                )));
            } else {
                // Stamped only when a remote was reached. A pass run offline
                // failed forty-four times and then wrote "fetched just now",
                // which the status bar repeated and the next launch believed:
                // nothing was due again for a whole interval.
                if reached && !one_off {
                    let finished = now_seconds();
                    ui.last_refresh.set(Some(finished));
                    crate::config::record_refresh(finished);
                }
                if manual || new_trouble > 0 {
                    ui.toasts.add_toast(ui::toast(&summary.line()));
                }
            }
            // Re-read local state so the cards show what the fetch changed.
            ui.start_scan();
        });
    }

    /// Fetch one repository, and fast-forward it too when asked, saying what
    /// happened: the pass the timer runs, narrowed to the repository in hand.
    /// The card's `p` and `f` come here, and so does a graph page's `f`,
    /// which hands in `done` to hear whether the remote was reached.
    pub(super) fn fetch_one(
        self: &Rc<Self>,
        path: PathBuf,
        fast_forward: bool,
        done: Option<Rc<dyn Fn(bool)>>,
    ) {
        if self.refreshing.get() || self.scanning.get() {
            self.toasts
                .add_toast(ui::toast("Already busy: a scan or a fetch is under way"));
            if let Some(done) = done {
                done(false);
            }
            return;
        }
        // Asking for a repository by name is looking at it.
        self.accept(&path);
        let settings = self.cfg.borrow().refresh.clone();
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| path.display().to_string());

        self.refreshing.set(true);
        self.refresh_progress.set((0, 1));
        self.spinner.set_visible(true);
        self.spinner.start();
        self.update_status();

        let rx = refresh::spawn(
            vec![path.clone()],
            refresh::Options {
                fast_forward,
                max_parallel: 1,
                timeout: std::time::Duration::from_secs(settings.timeout_seconds.clamp(5, 600)),
                pass: refresh::Pass::One,
                cancel: refresh::Cancel::default(),
            },
        );
        let ui = Rc::clone(self);
        glib::spawn_future_local(async move {
            // The page that asked is told how it went when this future goes,
            // whichever way it goes: it is showing "Fetching…" until then.
            let reached = Rc::new(Cell::new(false));
            let _over = OnDrop({
                let ui = Rc::clone(&ui);
                let reached = Rc::clone(&reached);
                let mut done = done;
                move || {
                    ui.refresh_is_over();
                    if let Some(done) = done.take() {
                        done(reached.get());
                    }
                }
            });
            let mut said = format!("{name}: nothing to do");
            while let Ok(event) = rx.recv().await {
                match event {
                    refresh::Event::Repo(report) => {
                        ui.note_outcome(&report);
                        reached.set(reached.get() | report.fetched);
                        said = format!("{name}: {}", ui::outcome_text(&report.outcome));
                    }
                    refresh::Event::Done => break,
                }
            }
            ui.refresh_is_over();
            if !ui.cfg.borrow().one_off() {
                crate::config::save_fetch_errors(&ui.fetch_errors.borrow());
            }
            ui::say(&ui.toasts, &said);
            // This one card, drawn again whatever was read: a fetch that
            // failed changes what the card says without changing the
            // repository. The whole fleet used to be scanned to learn it.
            ui.reread(vec![path], true);
        });
    }

    /// Read these repositories again and put what was read on their cards.
    /// The grid is only built again when a card would say something else,
    /// unless `redraw` asks for it regardless.
    fn reread(self: &Rc<Self>, paths: Vec<PathBuf>, redraw: bool) {
        if paths.is_empty() {
            return;
        }
        // A scan under way read these before whatever has just happened to
        // them, and will put that on the grid when it ends. They are read
        // again after it.
        if self.scanning.get() {
            self.stale.borrow_mut().extend(paths);
            return;
        }
        let generation = self.generation.get();
        let (tx, rx) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let read: Vec<crate::git::RepoInfo> =
                paths.iter().map(|path| crate::git::inspect(path)).collect();
            let _ = tx.send_blocking(read);
        });
        let ui = Rc::clone(self);
        glib::spawn_future_local(async move {
            let Ok(read) = rx.recv().await else { return };
            // A scan started in the meantime has the newer answer, or will.
            if ui.generation.get() != generation {
                return;
            }
            let mut changed = redraw;
            {
                let mut repos = ui.repos.borrow_mut();
                for info in read {
                    // Only what is on the grid: a repository that has left it
                    // is not brought back by having been looked at.
                    let Some(slot) = repos.iter_mut().find(|repo| repo.path == info.path) else {
                        continue;
                    };
                    // As text, which is everything a card is drawn from.
                    if format!("{slot:?}") != format!("{info:?}") {
                        *slot = info;
                        changed = true;
                    }
                }
            }
            if changed {
                ui.rebuild();
            }
        });
    }

    /// Catch the grid up with what has happened to repositories behind its
    /// back: those fetched from another page, and those handed to a terminal
    /// or an editor. Nothing did this before. Commit in the terminal `l`
    /// opened, come back, and the card still said dirty until the next
    /// rescan; with the timer off, which is how it comes, that was until
    /// Ctrl+R.
    ///
    /// `handed_too` is for a return to the window or to the grid, when the
    /// terminals have had their chance. The end of a scan only owes the
    /// repositories that were fetched while it ran.
    pub(super) fn read_again(self: &Rc<Self>, handed_too: bool) {
        // Only with the grid in front. A page shows its own repository and
        // reads it again itself, and the cards keep until they are looked at.
        if !self.on_grid() {
            return;
        }
        let mut paths = std::mem::take(&mut *self.stale.borrow_mut());
        if handed_too {
            paths.extend(self.handed.borrow().iter().cloned());
        }
        self.reread(paths.into_iter().collect(), false);
    }

    /// Have every open graph page of this repository read it again. Only a
    /// graph page has the action, and each checks the path for itself.
    fn reload_graph_pages(&self, path: &Path) {
        let which = path.display().to_string().to_variant();
        let stack = self.nav.navigation_stack();
        for index in 0..stack.n_items() {
            if let Some(page) = stack.item(index).and_downcast::<adw::NavigationPage>() {
                let _ = page.activate_action("page.reload-for", Some(&which));
            }
        }
    }
}

/// Whether a fetch failed because the remote turned it away, and not because
/// it could not be reached. Trying again on a timer does not change the first
/// kind: the repository has moved, or the credential is wrong, and each
/// attempt is another refused login against somebody's server.
fn refused_by_remote(err: &str) -> bool {
    let err = err.to_lowercase();
    [
        "authentication failed",
        "could not read username",
        "could not read password",
        "permission denied",
        // ssh's word for the same refusal when the agent holds more keys
        // than the server lets it try: every one of them was turned away.
        "too many authentication failures",
        "access denied",
        "repository not found",
        "does not appear to be a git repository",
        "http 401",
        "http 403",
        "error: 401",
        "error: 403",
    ]
    .iter()
    .any(|sign| err.contains(sign))
        // Over https it is git that says so, with the address in the middle
        // of the sentence, and that is the line that is kept. The sign above
        // is the remote's own line, which is not: a repository GitHub would
        // not show was asked for again on every pass.
        || (err.contains("repository '") && err.contains("' not found"))
}

fn now_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;
    use std::time::Duration;

    use super::*;
    use crate::gitcmd::transfer_fenced;
    use crate::testkit::*;

    /// Long enough for any fetch here, none of which goes further than this
    /// machine.
    const PATIENT: Duration = Duration::from_secs(8);

    #[test]
    fn a_remote_that_refused_is_told_from_one_that_was_not_reached() {
        for refused in [
            "remote: Invalid username or token. Password authentication is not supported.\nfatal: Authentication failed for 'https://example.invalid/acme/billing.git/'",
            "fatal: could not read Username for 'https://example.invalid': terminal prompts disabled",
            "git@example.invalid: Permission denied (publickey).",
            "remote: Repository not found.",
            // What is kept of that over https: git's line, which has the
            // address in the middle of it. Alone, as a state file written
            // before the line above it was added still has it, and with it.
            "fatal: repository 'https://example.invalid/acme/billing.git/' not found",
            "repository not found, or the account git signs in with has no access to it\ngit said: fatal: repository 'https://example.invalid/acme/billing.git/' not found",
            "fatal: '/srv/git/billing.git' does not appear to be a git repository",
            "fatal: unable to access 'https://example.invalid/x.git/': The requested URL returned error: 403",
        ] {
            assert!(refused_by_remote(refused), "{refused}");
        }
        for unreachable in [
            "fatal: unable to access 'https://example.invalid/x.git/': Could not resolve host: example.invalid",
            "ssh: connect to host example.invalid port 22: Connection timed out",
            "git fetch gave no answer in 60 seconds",
            "fatal: unable to access 'https://example.invalid/x.git/': The requested URL returned error: 503",
        ] {
            assert!(!refused_by_remote(unreachable), "{unreachable}");
        }
    }

    /// The same, from what git and ssh print, as the timer's fetch keeps it.
    /// Over ssh, git's own complaint is one sentence whatever happened, and
    /// that was what was kept: a key turned away read as a host not reached,
    /// and was offered again on every pass. A stand-in for ssh says each
    /// here, so nothing goes near a network.
    #[test]
    fn over_ssh_a_refused_login_is_told_from_a_host_not_reached() {
        let _guard = isolate();
        let dir = scratch("refreshing-ssh-refused");
        git(&dir, &["init", "-q", "-b", "main"]);
        git(&dir, &["remote", "add", "origin", "git@example.invalid:acme/billing.git"]);
        let ssh = dir.join("bin").join("ssh");
        for (said, refused) in [
            ("git@example.invalid: Permission denied (publickey).", true),
            // The server's reason, and then ssh saying it was cut off.
            (
                "Received disconnect from 192.0.2.1 port 22:2: Too many authentication failures\n\
                 Disconnected from 192.0.2.1 port 22",
                true,
            ),
            ("ssh: connect to host example.invalid port 22: Connection timed out", false),
        ] {
            // As ssh does it: its own words on stderr, and 255.
            write(&ssh, &format!("#!/bin/sh\necho '{said}' >&2\nexit 255\n"));
            std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o755)).unwrap();
            // SAFETY: under the isolation lock, and taken out again below.
            unsafe { std::env::set_var("GIT_SSH_COMMAND", &ssh) };
            let got = transfer_fenced(&dir, &["fetch", "--progress"], PATIENT, PATIENT);
            unsafe { std::env::remove_var("GIT_SSH_COMMAND") };

            let err = got.expect_err(said);
            assert_eq!(err, said, "the card says what ssh said");
            assert_eq!(refused_by_remote(&err), refused, "{err}");
        }
    }

    /// What git says is read in English, whatever language git speaks. In
    /// Swedish a refused password is "Autentisering misslyckades", which was
    /// taken for a host not reached: the password in the remote's address was
    /// offered again on every pass. A server here turns every request away,
    /// as a host does a wrong password.
    ///
    /// Where git has no Swedish (CI's container keeps no translations, and a
    /// C locale does not read `LANGUAGE`) this holds without the fix as well.
    /// On a machine where git speaks Swedish, it is the fix that holds it.
    #[test]
    fn a_refused_login_is_told_whatever_language_git_speaks() {
        let _guard = isolate();
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = server.local_addr().unwrap().port();
        std::thread::spawn(move || {
            use std::io::{Read, Write};
            for stream in server.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut request = [0u8; 4096];
                let _ = stream.read(&mut request);
                let _ = stream.write_all(
                    b"HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Basic realm=\"acme\"\r\n\
                      Content-Length: 0\r\nConnection: close\r\n\r\n",
                );
            }
        });
        let dir = scratch("refreshing-refused-in-swedish");
        git(&dir, &["init", "-q", "-b", "main"]);
        let url = format!("http://deploy:wrong@127.0.0.1:{port}/acme/billing.git");
        git(&dir, &["remote", "add", "origin", &url]);

        let theirs = std::env::var_os("LANGUAGE");
        // SAFETY: under the isolation lock, and put back as it was below.
        unsafe { std::env::set_var("LANGUAGE", "sv") };
        let got = transfer_fenced(&dir, &["fetch", "--progress"], PATIENT, PATIENT);
        unsafe {
            match &theirs {
                Some(theirs) => std::env::set_var("LANGUAGE", theirs),
                None => std::env::remove_var("LANGUAGE"),
            }
        }

        let err = got.expect_err("every request is turned away");
        assert_eq!(
            err,
            format!("fatal: Authentication failed for 'http://127.0.0.1:{port}/acme/billing.git/'")
        );
        assert!(refused_by_remote(&err), "{err}");
    }
}
