//! The commit search page: one query across many repositories.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use gtk::gdk;
use gtk::glib;
use gtk::pango;

use crate::search::{self, Cancel, Hit, Stopped};
use crate::ui::{absolute_time, key_color, relative_time};

/// Below this a query matches most of history and the wait is not worth it.
const MIN_QUERY: usize = 2;
/// Typing is faster than walking a fleet's history, so wait for a pause.
const DEBOUNCE_MS: u64 = 350;
/// Rows past this are not worth building. It is as many as one repository may
/// hand over, which is what lets the status bar call the rows the newest: see
/// `search::MAX_HITS_PER_REPO`. The status bar says when there were more, and
/// when a repository stopped before the end of its history.
const MAX_ROWS: usize = search::MAX_HITS_PER_REPO;

/// What a finished search knows about itself, for the two places that say it:
/// the status bar, and the page shown when nothing was found.
///
/// The last two fields are what used to be missing. A repository stops at a
/// number of matches and at a number of commits, and told nobody: the sum of
/// what arrived was printed as the number of commits that match, and "Nothing
/// in N repositories matches" was said of history that had not been read.
#[derive(Debug, Clone, Copy, Default)]
struct Tally {
    /// Matches that arrived.
    found: usize,
    /// Repositories at least one of them came from.
    repos_hit: usize,
    /// Repositories searched.
    repos: usize,
    /// Repositories that stopped at `search::MAX_HITS_PER_REPO` matches. Each
    /// may hold more, older than the ones it handed over.
    at_matches: usize,
    /// Repositories read only `search::MAX_COMMITS_PER_REPO` commits back.
    at_depth: usize,
    /// Repositories that could not be read at all.
    unreadable: usize,
}

impl Tally {
    fn status(&self) -> String {
        let commits = plural(self.found, "commit", "commits");
        // With a repository that stopped, what arrived is a floor and not a
        // count.
        let partial = self.at_matches + self.at_depth + self.unreadable > 0;
        let mut parts = vec![
            if partial && self.found > 0 {
                format!("at least {} {commits}", thousands(self.found))
            } else {
                format!("{} {commits}", thousands(self.found))
            },
            format!("in {} of {} repos", self.repos_hit, self.repos),
        ];
        if self.at_matches > 0 {
            parts.push(format!(
                "{} {} stopped at {} matches, narrow the query",
                self.at_matches,
                plural(self.at_matches, "repo", "repos"),
                thousands(search::MAX_HITS_PER_REPO),
            ));
        }
        if self.at_depth > 0 {
            parts.push(format!(
                "{} {} read only {} commits back",
                self.at_depth,
                plural(self.at_depth, "repo", "repos"),
                thousands(search::MAX_COMMITS_PER_REPO),
            ));
        }
        if self.unreadable > 0 {
            parts.push(format!(
                "{} {} could not be read",
                self.unreadable,
                plural(self.unreadable, "repo", "repos"),
            ));
        }
        if self.found > MAX_ROWS {
            // Each repository hands over its newest matches, and as many as
            // are listed here, so the rows are the newest there are. Unless
            // one ran out of depth: what it did not read is older than
            // everything it did, and may still be newer than a match from
            // another repository.
            parts.push(if self.at_depth == 0 {
                format!("newest {MAX_ROWS} shown")
            } else {
                format!("{MAX_ROWS} shown")
            });
        }
        parts.join("  ·  ")
    }

    /// For the page shown when no match arrived.
    fn nothing(&self, query: &str) -> String {
        let repositories = plural(self.repos, "repository", "repositories");
        if self.at_depth == 0 && self.unreadable == 0 {
            return format!("Nothing in {} {repositories} matches “{query}”.", self.repos);
        }
        let mut said = format!("Nothing found for “{query}” in {} {repositories}.", self.repos);
        if self.at_depth > 0 {
            said.push_str(&format!(
                " {} of them {} read only {} commits back, so an older match would not have \
                 been found.",
                self.at_depth,
                plural(self.at_depth, "was", "were"),
                thousands(search::MAX_COMMITS_PER_REPO),
            ));
        }
        if self.unreadable > 0 {
            said.push_str(&format!(
                " {} could not be read at all.",
                self.unreadable
            ));
        }
        said
    }
}

fn plural(n: usize, one: &'static str, many: &'static str) -> &'static str {
    if n == 1 { one } else { many }
}

/// Digits in threes, the way the contributions page writes them.
fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push('\u{202f}');
        }
        out.push(c);
    }
    out
}

/// The status bar holds one line and cuts what does not fit, and a search
/// that stopped somewhere has more to say than fits. The tooltip has it all.
fn say(status: &gtk::Label, text: &str) {
    status.set_label(text);
    status.set_tooltip_text(Some(text));
}

/// `on_open` takes a repository, its name and the commit to open on.
pub fn page(
    repos: Vec<PathBuf>,
    scope: String,
    on_open: Rc<dyn Fn(PathBuf, String, Option<String>)>,
) -> (adw::NavigationPage, gtk::SearchEntry) {
    let title = adw::WindowTitle::new("Search commits", &scope);
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&title));

    let entry = gtk::SearchEntry::builder()
        .placeholder_text("Message, author or sha…")
        .width_request(320)
        .build();
    entry.add_css_class("om-search");
    header.pack_end(&entry);

    let prompt = adw::StatusPage::builder()
        .icon_name("system-search-symbolic")
        .title("Search commits")
        .description("Matches subjects, full messages, authors and sha prefixes.")
        .build();
    let empty = adw::StatusPage::builder()
        .icon_name("system-search-symbolic")
        .title("No matching commits")
        .build();

    // A list box rather than a plain box: it makes every row focusable, gives
    // arrow navigation between them, and turns Enter into an activation.
    let rows = gtk::ListBox::new();
    rows.set_selection_mode(gtk::SelectionMode::None);
    rows.add_css_class("om-results");
    rows.set_margin_top(8);
    rows.set_margin_bottom(12);
    rows.set_margin_start(10);
    rows.set_margin_end(10);
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .child(&rows)
        .build();
    scroller.add_css_class("om-grid");

    let stack = gtk::Stack::new();
    stack.add_named(&prompt, Some("prompt"));
    stack.add_named(&scroller, Some("results"));
    stack.add_named(&empty, Some("empty"));
    stack.set_vexpand(true);

    let status = gtk::Label::builder()
        .xalign(0.0)
        .ellipsize(pango::EllipsizeMode::End)
        .build();
    status.add_css_class("om-statusbar");
    say(&status, &format!("{} · nothing searched yet", scope));

    let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
    body.append(&stack);
    body.append(&status);

    let toasts = adw::ToastOverlay::new();
    toasts.set_child(Some(&body));

    let view = adw::ToolbarView::new();
    view.add_top_bar(&header);
    view.set_content(Some(&toasts));

    let page = adw::NavigationPage::builder()
        .title("Search commits")
        .child(&view)
        .build();

    // One generation per keystroke: a result from an abandoned query must not
    // land in the list of the one the user is waiting for.
    let generation = Rc::new(RefCell::new(0u64));
    let running: Rc<RefCell<Option<Cancel>>> = Rc::new(RefCell::new(None));
    let repos = Rc::new(repos);
    // What each row stands for, by index, so activation can find it.
    let listed: Rc<RefCell<Vec<Hit>>> = Rc::new(RefCell::new(Vec::new()));

    {
        // Enter on a result takes you to the repository it is in.
        let listed = Rc::clone(&listed);
        let on_open = Rc::clone(&on_open);
        rows.connect_row_activated(move |_, row| {
            let index = row.index();
            if index < 0 {
                return;
            }
            let hit = listed.borrow().get(index as usize).cloned();
            if let Some(hit) = hit {
                on_open(hit.path.clone(), hit.repo.clone(), Some(hit.id.clone()));
            }
        });
    }
    {
        // Down from the query moves into the results, the way a launcher does.
        let rows = rows.clone();
        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed(move |_, key, _, _| {
            if key != gdk::Key::Down {
                return glib::Propagation::Proceed;
            }
            match rows.row_at_index(0) {
                Some(first) => {
                    first.grab_focus();
                    glib::Propagation::Stop
                }
                None => glib::Propagation::Proceed,
            }
        });
        entry.add_controller(keys);
    }
    {
        // ...and Up from the first result goes back to it.
        let entry = entry.clone();
        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed(move |controller, key, _, _| {
            if key != gdk::Key::Up {
                return glib::Propagation::Proceed;
            }
            let on_first = controller
                .widget()
                .and_downcast::<gtk::ListBox>()
                .and_then(|list| list.focus_child())
                .and_downcast::<gtk::ListBoxRow>()
                .is_some_and(|row| row.index() == 0);
            if on_first {
                entry.grab_focus();
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        rows.add_controller(keys);
    }

    {
        let generation = Rc::clone(&generation);
        let running = Rc::clone(&running);
        let repos = Rc::clone(&repos);
        let stack = stack.clone();
        let rows = rows.clone();
        let status = status.clone();
        let empty = empty.clone();
        let toasts = toasts.clone();
        let listed = Rc::clone(&listed);
        let scope = scope.clone();

        entry.connect_search_changed(move |entry| {
            let query = entry.text().trim().to_string();
            let mine = {
                let mut counter = generation.borrow_mut();
                *counter += 1;
                *counter
            };
            if let Some(cancel) = running.borrow_mut().take() {
                cancel.stop();
            }

            if query.chars().count() < MIN_QUERY {
                stack.set_visible_child_name("prompt");
                say(&status, &format!("{scope} · type at least {MIN_QUERY} characters"));
                return;
            }

            say(&status, &format!("{scope} · searching…"));

            let (rx, cancel) = search::spawn((*repos).clone(), query.clone(), 6);
            *running.borrow_mut() = Some(cancel);

            let generation = Rc::clone(&generation);
            let stack = stack.clone();
            let rows = rows.clone();
            let status = status.clone();
            let empty = empty.clone();
            let toasts = toasts.clone();
            let listed = Rc::clone(&listed);
            let scope = scope.clone();

            glib::spawn_future_local(async move {
                // Give the typist a moment before walking any history.
                glib::timeout_future(std::time::Duration::from_millis(DEBOUNCE_MS)).await;
                if *generation.borrow() != mine {
                    return;
                }

                let mut hits: Vec<Hit> = Vec::new();
                let mut tally = Tally::default();
                while let Ok(event) = rx.recv().await {
                    if *generation.borrow() != mine {
                        return;
                    }
                    match event {
                        search::Event::Progress(done, of) => {
                            tally.repos = of;
                            say(
                                &status,
                                &format!(
                                    "{scope} · searching {done}/{of} · {} so far",
                                    hits.len()
                                ),
                            );
                        }
                        search::Event::Hits(found) => hits.extend(found),
                        search::Event::Stopped(Stopped::Matches) => tally.at_matches += 1,
                        search::Event::Stopped(Stopped::Depth) => tally.at_depth += 1,
                        search::Event::Stopped(Stopped::Unreadable) => tally.unreadable += 1,
                        search::Event::Done => break,
                    }
                }
                if *generation.borrow() != mine {
                    return;
                }

                hits.sort_by_key(|hit| std::cmp::Reverse(hit.when));
                tally.found = hits.len();
                tally.repos_hit = {
                    // By path: two repositories can have the same name.
                    let mut from: Vec<&PathBuf> = hits.iter().map(|h| &h.path).collect();
                    from.sort_unstable();
                    from.dedup();
                    from.len()
                };

                rows.remove_all();
                let visible: Vec<Hit> = hits.iter().take(MAX_ROWS).cloned().collect();
                for hit in &visible {
                    rows.append(&row(hit, &toasts));
                }
                *listed.borrow_mut() = visible;

                if tally.found == 0 {
                    // A status page's description is markup, with no switch
                    // to say otherwise, and this is what was typed.
                    empty.set_description(Some(&glib::markup_escape_text(
                        &tally.nothing(&query),
                    )));
                    stack.set_visible_child_name("empty");
                } else {
                    stack.set_visible_child_name("results");
                }

                say(&status, &tally.status());
            });
        });
    }

    // The entry is the only thing worth typing into on this page, and the
    // caller puts the keyboard in it as soon as the page is pushed. It used
    // to be done here on `shown`, which is the end of the slide: what was
    // typed in the third of a second after Ctrl+Shift+F went nowhere. Coming
    // back to the page from a result still lands here.
    let focus = entry.clone();
    page.connect_shown(move |_| {
        if !focus.has_focus() {
            focus.grab_focus();
        }
    });

    (page, entry)
}

/// One result. Activating it is handled by the list; the button copies the sha,
/// which is the other thing anyone wants from a commit they have found.
fn row(hit: &Hit, toasts: &adw::ToastOverlay) -> gtk::ListBoxRow {
    let line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    line.set_height_request(28);

    let repo = gtk::Box::new(gtk::Orientation::Horizontal, 5);
    repo.append(&crate::ui::color_dot(key_color(&hit.repo)));
    let repo_label = gtk::Label::builder()
        .label(&hit.repo)
        .xalign(0.0)
        .width_chars(20)
        .max_width_chars(20)
        .ellipsize(pango::EllipsizeMode::End)
        .build();
    repo_label.add_css_class("om-meta");
    repo.append(&repo_label);
    line.append(&repo);

    let sha = gtk::Label::builder()
        .label(&hit.short_id)
        .xalign(0.0)
        .width_chars(7)
        .build();
    sha.add_css_class("om-sha");
    line.append(&sha);

    let where_matched = gtk::Label::new(Some(hit.matched.label()));
    where_matched.add_css_class("om-ref");
    line.append(&where_matched);

    let summary = gtk::Label::builder()
        .label(&hit.summary)
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(pango::EllipsizeMode::End)
        .build();
    summary.add_css_class("om-summary");
    line.append(&summary);

    let author = gtk::Label::builder()
        .label(&hit.author)
        .xalign(1.0)
        .width_chars(14)
        .max_width_chars(14)
        .ellipsize(pango::EllipsizeMode::End)
        .build();
    author.add_css_class("om-meta");
    line.append(&author);

    let when = gtk::Label::builder()
        .label(relative_time(hit.when))
        .xalign(1.0)
        .width_chars(13)
        .build();
    when.add_css_class("om-meta");
    line.append(&when);

    let copy = gtk::Button::builder()
        .icon_name("edit-copy-symbolic")
        .tooltip_text("Copy the full sha")
        .has_frame(false)
        .valign(gtk::Align::Center)
        .build();
    {
        let id = hit.id.clone();
        let short = hit.short_id.clone();
        let toasts = toasts.clone();
        copy.connect_clicked(move |button| {
            button.clipboard().set_text(&id);
            toasts.add_toast(crate::ui::toast(&format!("Copied {short}")));
        });
    }
    line.append(&copy);

    let row = gtk::ListBoxRow::builder().child(&line).build();
    row.add_css_class("om-row");
    row.set_tooltip_text(Some(&format!(
        "{}\n{}\n{} · {}\n{}\n\nEnter opens this repository's graph.",
        crate::config::tildify(&hit.path),
        hit.summary,
        hit.author,
        absolute_time(hit.when),
        hit.id,
    )));
    row
}

/// What the page says once a search is over. Words only: nothing here builds
/// a widget, so nothing needs a display.
#[cfg(test)]
mod tests {
    use super::*;

    /// Thin spaces are for the screen. Here they are in the way.
    fn plain(text: String) -> String {
        text.replace('\u{202f}', ",")
    }

    #[test]
    fn a_search_that_read_everything_gives_a_count() {
        let all = Tally { found: 37, repos_hit: 3, repos: 44, ..Tally::default() };
        assert_eq!(plain(all.status()), "37 commits  ·  in 3 of 44 repos");
        let one = Tally { found: 1, repos_hit: 1, repos: 1, ..Tally::default() };
        assert_eq!(plain(one.status()), "1 commit  ·  in 1 of 1 repos");

        let none = Tally { repos: 44, ..Tally::default() };
        assert_eq!(plain(none.status()), "0 commits  ·  in 0 of 44 repos");
        assert_eq!(none.nothing("typo"), "Nothing in 44 repositories matches “typo”.");
        let none_in_one = Tally { repos: 1, ..Tally::default() };
        assert_eq!(none_in_one.nothing("typo"), "Nothing in 1 repository matches “typo”.");
    }

    /// Three repositories with 400, 300 and 300 matches used to read "600
    /// commits · in 3 of 3 repos · newest 500 shown": a sum of limits given as
    /// a count, over rows that were not the newest.
    #[test]
    fn a_repository_that_stopped_at_its_matches_turns_the_count_into_a_floor() {
        let tally = Tally { found: 1037, repos_hit: 12, repos: 44, at_matches: 2, at_depth: 0, unreadable: 0 };
        assert_eq!(
            plain(tally.status()),
            "at least 1,037 commits  ·  in 12 of 44 repos  ·  \
             2 repos stopped at 500 matches, narrow the query  ·  newest 500 shown"
        );
        // Few enough to list them all: still not all there are.
        let listed = Tally { found: 500, repos_hit: 1, repos: 1, at_matches: 1, at_depth: 0, unreadable: 0 };
        assert_eq!(
            plain(listed.status()),
            "at least 500 commits  ·  in 1 of 1 repos  ·  \
             1 repo stopped at 500 matches, narrow the query"
        );
    }

    /// "Nothing in 1 repositories matches" was said of a repository whose
    /// only matches were further back than it had been read.
    #[test]
    fn a_repository_that_ran_out_of_depth_is_not_said_to_hold_nothing() {
        let tally = Tally { repos: 3, at_depth: 1, ..Tally::default() };
        assert_eq!(
            plain(tally.nothing("ticket-12")),
            "Nothing found for “ticket-12” in 3 repositories. 1 of them was read only 50,000 \
             commits back, so an older match would not have been found."
        );
        assert_eq!(
            plain(tally.status()),
            "0 commits  ·  in 0 of 3 repos  ·  1 repo read only 50,000 commits back"
        );

        // With rows to cut, they are no longer known to be the newest.
        let cut = Tally { found: 620, repos_hit: 5, repos: 9, at_matches: 0, at_depth: 2, unreadable: 0 };
        assert_eq!(
            plain(cut.status()),
            "at least 620 commits  ·  in 5 of 9 repos  ·  \
             2 repos read only 50,000 commits back  ·  500 shown"
        );
    }
    #[test]
    fn a_repository_that_could_not_be_read_is_said_in_both_places() {
        let tally = Tally { found: 3, repos_hit: 1, repos: 4, unreadable: 1, ..Tally::default() };
        assert_eq!(
            tally.status(),
            "at least 3 commits  ·  in 1 of 4 repos  ·  1 repo could not be read"
        );
        let nothing = Tally { repos: 4, unreadable: 2, ..Tally::default() };
        assert_eq!(
            nothing.nothing("fix"),
            "Nothing found for “fix” in 4 repositories. 2 could not be read at all."
        );
    }
}
