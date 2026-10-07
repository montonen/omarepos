//! The commit search page: one query across many repositories.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use gtk::gdk;
use gtk::glib;
use gtk::pango;

use crate::search::{self, Cancel, Hit};
use crate::ui::{absolute_time, key_color, relative_time};

/// Below this a query matches most of history and the wait is not worth it.
const MIN_QUERY: usize = 2;
/// Typing is faster than walking a fleet's history, so wait for a pause.
const DEBOUNCE_MS: u64 = 350;
/// Rows past this are not worth building; the status bar says how many there
/// were.
const MAX_ROWS: usize = 500;

/// `on_open` takes a repository path and name, and shows its commit graph.
pub fn page(
    repos: Vec<PathBuf>,
    scope: String,
    on_open: Rc<dyn Fn(PathBuf, String)>,
) -> adw::NavigationPage {
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
    status.set_label(&format!("{} · nothing searched yet", scope));

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
                on_open(hit.path.clone(), hit.repo.clone());
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
                status.set_label(&format!("{scope} · type at least {MIN_QUERY} characters"));
                return;
            }

            status.set_label(&format!("{scope} · searching…"));

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
                let mut searched = 0usize;
                let mut total = 0usize;
                while let Ok(event) = rx.recv().await {
                    if *generation.borrow() != mine {
                        return;
                    }
                    match event {
                        search::Event::Progress(done, of) => {
                            searched = done;
                            total = of;
                            status.set_label(&format!(
                                "{scope} · searching {done}/{of} · {} so far",
                                hits.len()
                            ));
                        }
                        search::Event::Hits(found) => hits.extend(found),
                        search::Event::Done => break,
                    }
                }
                if *generation.borrow() != mine {
                    return;
                }

                hits.sort_by(|a, b| b.when.cmp(&a.when));
                let found = hits.len();
                let repos_hit = {
                    let mut names: Vec<&str> = hits.iter().map(|h| h.repo.as_str()).collect();
                    names.sort_unstable();
                    names.dedup();
                    names.len()
                };

                rows.remove_all();
                let visible: Vec<Hit> = hits.iter().take(MAX_ROWS).cloned().collect();
                for hit in &visible {
                    rows.append(&row(hit, &toasts));
                }
                *listed.borrow_mut() = visible;

                if found == 0 {
                    empty.set_description(Some(&format!("Nothing in {searched} repositories matches “{query}”.")));
                    stack.set_visible_child_name("empty");
                } else {
                    stack.set_visible_child_name("results");
                }

                let mut parts = vec![
                    format!("{found} {}", if found == 1 { "commit" } else { "commits" }),
                    format!("in {repos_hit} of {total} repos"),
                ];
                if found > MAX_ROWS {
                    parts.push(format!("newest {MAX_ROWS} shown"));
                }
                status.set_label(&parts.join("  ·  "));
            });
        });
    }

    // The entry is the only thing worth typing into on this page.
    let focus = entry.clone();
    page.connect_shown(move |_| {
        focus.grab_focus();
    });

    page
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
            toasts.add_toast(adw::Toast::new(&format!("Copied {short}")));
        });
    }
    line.append(&copy);

    let row = gtk::ListBoxRow::builder().child(&line).build();
    row.add_css_class("om-row");
    row.set_tooltip_text(Some(&format!(
        "{}\n{}\n{} · {}\n{}\n\nEnter opens this repository's graph.",
        hit.repo,
        hit.summary,
        hit.author,
        absolute_time(hit.when),
        hit.id,
    )));
    row
}
