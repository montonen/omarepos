//! The per-repository commit graph page.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use gtk::pango;

use crate::config::tildify;
use crate::graph::{self, Graph, RefKind, Row};
use crate::ui::{absolute_time, lane_color, relative_time};

/// Rows are a fixed height so the graph cell can draw a row at a time without
/// knowing anything about its neighbours.
const ROW_H: i32 = 28;
const LANE_W: f64 = 14.0;
const DOT_R: f64 = 4.0;
/// Past this many lanes the graph stops being readable; wider rows are clipped.
const MAX_LANES: usize = 16;
const COMMIT_LIMIT: usize = 500;
/// Rows kept above a requested commit, for context.
const WINDOW_BEFORE: usize = 120;
/// Rows kept below it.
const WINDOW_AFTER: usize = 180;

fn lane_x(lane: usize) -> f64 {
    LANE_W * lane as f64 + LANE_W / 2.0 + 3.0
}

/// Build the page and start loading. `on_contributions` opens the stats page
/// scoped to this repository. `target` is a commit to open on — it is sought
/// out however far back it is, highlighted, and scrolled to.
pub fn page(
    path: &Path,
    name: &str,
    target: Option<String>,
    detail_open: bool,
    on_detail_toggled: Rc<dyn Fn(bool)>,
    on_terminal: Rc<dyn Fn()>,
    on_editor: Rc<dyn Fn()>,
    on_contributions: Rc<dyn Fn(PathBuf)>,
) -> adw::NavigationPage {
    let title = adw::WindowTitle::new(name, &tildify(path));
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&title));

    let contributions = gtk::Button::builder()
        .icon_name("system-users-symbolic")
        .tooltip_text("Contributions in this repository")
        .has_frame(false)
        .build();
    {
        let path = path.to_path_buf();
        contributions.connect_clicked(move |_| on_contributions(path.clone()));
    }
    header.pack_end(&contributions);

    // Publishing is a deliberate act, so it lives on a button a person presses
    // — never on the timer that fetches. See ADR-0005.
    let push_content = adw::ButtonContent::builder()
        .icon_name("send-to-symbolic")
        .label("Push")
        .build();
    let push = gtk::Button::builder()
        .child(&push_content)
        .has_frame(false)
        .sensitive(false)
        .build();
    header.pack_end(&push);

    let detail_toggle = gtk::ToggleButton::builder()
        .icon_name("view-paged-symbolic")
        .tooltip_text("Show what each commit changed (Ctrl+D)")
        .has_frame(false)
        .active(detail_open)
        .build();
    header.pack_end(&detail_toggle);

    let spinner = gtk::Spinner::new();
    spinner.start();
    let loading = gtk::Box::new(gtk::Orientation::Vertical, 10);
    loading.set_valign(gtk::Align::Center);
    loading.set_halign(gtk::Align::Center);
    loading.append(&spinner);
    let loading_label = gtk::Label::new(Some("Reading history…"));
    loading_label.add_css_class("om-meta");
    loading.append(&loading_label);

    // A list box so rows take focus, arrow keys walk them, and Enter means
    // the same thing a click does. Browse selection makes the selection follow
    // the cursor, which is what drives the detail pane below.
    let rows_box = gtk::ListBox::new();
    rows_box.set_selection_mode(gtk::SelectionMode::Browse);
    rows_box.add_css_class("om-results");
    rows_box.set_margin_top(8);
    rows_box.set_margin_bottom(12);
    rows_box.set_margin_start(10);
    rows_box.set_margin_end(10);

    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .child(&rows_box)
        .build();
    scroller.add_css_class("om-grid");

    let empty = adw::StatusPage::builder()
        .icon_name("document-open-recent-symbolic")
        .title("No commits")
        .description("This repository has no history yet.")
        .build();

    // The detail goes *below* the graph rather than beside it: the window is
    // often half a tiled screen, where a vertical split leaves neither half
    // usable, and height is the one thing such a window has to spare.
    // A write in the pane can change the history this page is drawing, so the
    // page reloads rather than quietly going stale under the user.
    let reload_page: Rc<RefCell<Option<Rc<dyn Fn()>>>> = Rc::new(RefCell::new(None));
    let on_changed: Rc<dyn Fn(bool)> = {
        let reload_page = Rc::clone(&reload_page);
        Rc::new(move |committed: bool| {
            if !committed {
                return;
            }
            if let Some(reload) = reload_page.borrow().clone() {
                reload();
            }
        })
    };
    let (detail, load_detail, focus_files) = crate::ui::commit::pane(on_changed);
    let split = gtk::Paned::builder()
        .orientation(gtk::Orientation::Vertical)
        .start_child(&scroller)
        .end_child(&detail)
        .resize_start_child(true)
        .shrink_start_child(false)
        .shrink_end_child(false)
        .build();
    detail.set_visible(detail_open);

    {
        // A fixed split is wrong at both ends — cramped on a short window,
        // wasteful on a tall one. Take a share of the height once, the first
        // time there is a height to take a share of, then leave it to the user.
        // Rc, not a bare Cell: cloning a Cell copies the value, so the flag
        // would never actually stick and a later map would undo the user's drag.
        let placed = Rc::new(std::cell::Cell::new(false));
        split.connect_map(move |split| {
            if placed.get() {
                return;
            }
            let split = split.clone();
            let placed_flag = Rc::clone(&placed);
            glib::timeout_add_local_once(std::time::Duration::from_millis(50), move || {
                let height = split.height();
                if height > 0 && !placed_flag.get() {
                    split.set_position((height as f64 * 0.62) as i32);
                    placed_flag.set(true);
                }
            });
        });
    }

    {
        // Without this, Tab steps through the commits one row at a time and the
        // pane below can never be reached from the keyboard at all.
        let focus_files = Rc::clone(&focus_files);
        let keys = gtk::EventControllerKey::new();
        let on_terminal = Rc::clone(&on_terminal);
        let on_editor = Rc::clone(&on_editor);
        keys.connect_key_pressed(move |_, key, _, state| {
            // Same letter as the grid: you are inside a repository here, so it
            // means the same thing.
            if key == gtk::gdk::Key::l && crate::ui::unmodified(state) {
                on_terminal();
                return glib::Propagation::Stop;
            }
            if key == gtk::gdk::Key::e && crate::ui::unmodified(state) {
                on_editor();
                return glib::Propagation::Stop;
            }
            if key != gtk::gdk::Key::Tab || state.contains(gtk::gdk::ModifierType::SHIFT_MASK) {
                return glib::Propagation::Proceed;
            }
            focus_files();
            glib::Propagation::Stop
        });
        rows_box.add_controller(keys);
    }

    let stack = gtk::Stack::new();
    stack.add_named(&loading, Some("loading"));
    stack.add_named(&split, Some("graph"));
    stack.add_named(&empty, Some("empty"));
    stack.set_vexpand(true);

    let status = gtk::Label::builder()
        .xalign(0.0)
        .ellipsize(pango::EllipsizeMode::End)
        .build();
    status.add_css_class("om-statusbar");

    let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
    body.append(&stack);
    body.append(&status);

    let toasts = adw::ToastOverlay::new();
    toasts.set_child(Some(&body));

    {
        // Separate controller only because the toast overlay does not exist
        // yet where the other one is attached.
        let repo = path.to_path_buf();
        // Weakly: the list this controller is on sits inside the overlay, and
        // a closure on the list that owned the overlay would keep the whole
        // page alive after it was closed.
        let toasts = toasts.downgrade();
        let reload = Rc::clone(&reload_page);
        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed(move |_, key, _, state| {
            if !crate::ui::unmodified(state) {
                return glib::Propagation::Proceed;
            }
            let Some(toasts) = toasts.upgrade() else {
                return glib::Propagation::Proceed;
            };
            match key {
                gtk::gdk::Key::p => {
                    pull(&repo, &toasts, Rc::clone(&reload), true);
                    glib::Propagation::Stop
                }
                gtk::gdk::Key::f => {
                    pull(&repo, &toasts, Rc::clone(&reload), false);
                    glib::Propagation::Stop
                }
                gtk::gdk::Key::question => {
                    if let Some(window) = toasts.root().and_downcast::<gtk::Window>() {
                        crate::ui::shortcuts::dialog().present(Some(&window));
                    }
                    glib::Propagation::Stop
                }
                _ => glib::Propagation::Proceed,
            }
        });
        rows_box.add_controller(keys);
    }

    let view = adw::ToolbarView::new();
    view.add_top_bar(&header);
    view.set_content(Some(&toasts));

    let page = adw::NavigationPage::builder()
        .title(name)
        .child(&view)
        .build();

    {
        let detail = detail.clone();
        detail_toggle.connect_toggled(move |button| {
            detail.set_visible(button.is_active());
            on_detail_toggled(button.is_active());
        });
    }
    {
        // Ctrl+D, scoped to this page rather than the window, so it means
        // nothing anywhere else.
        let detail_toggle = detail_toggle.clone();
        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed(move |_, key, _, state| {
            if key == gtk::gdk::Key::d && state.contains(gtk::gdk::ModifierType::CONTROL_MASK) {
                detail_toggle.set_active(!detail_toggle.is_active());
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        page.add_controller(keys);
    }

    // Loading happens off the main thread; a big repo takes a moment.
    let load_path = path.to_path_buf();
    let load_path_for_detail = path.to_path_buf();
    let page_for_focus = page.clone();
    let wanted = target.clone();
    // One handler slot for the focus watcher: reloading must replace it rather
    // than stack another one on the window.
    let focus_handler: Rc<RefCell<Option<glib::SignalHandlerId>>> = Rc::new(RefCell::new(None));
    // Likewise for the push button: populate runs again after every push, and a
    // second handler would send the same commits twice on the next click.
    let push_handler: Rc<RefCell<Option<glib::SignalHandlerId>>> = Rc::new(RefCell::new(None));

    // Populating is separate from loading, so a commit can ask for it again.
    let populate: Rc<dyn Fn(Result<Graph, String>)> = {
        let rows_box = rows_box.clone();
        let stack = stack.clone();
        let status = status.clone();
        let push = push.clone();
        let push_content = push_content.clone();
        let toasts_for_push = toasts.clone();
        let reload_for_push = Rc::clone(&reload_page);
        let push_handler = Rc::clone(&push_handler);
        let empty = empty.clone();
        let toasts_for_rows = toasts.clone();
        let load_detail = Rc::clone(&load_detail);
        let page_for_focus = page_for_focus.clone();
        let detail_for_focus = detail.clone();
        let load_path_for_detail = load_path_for_detail.clone();
        let focus_handler = Rc::clone(&focus_handler);
        Rc::new(move |result: Result<Graph, String>| {
            rows_box.remove_all();
            match result {
            Ok(graph) if graph.rows.is_empty() => {
                stack.set_visible_child_name("empty");
                status.set_label("no commits");
            }
            Ok(graph) => {
                let width = lanes_width(graph.lanes);
                // With a commit to open on, the walk may have gone thousands of
                // rows deep. Render a window around it rather than all of it —
                // the lane state of every row is self-contained, so a slice
                // draws correctly on its own.
                let (from, to) = match graph.target {
                    Some(index) => (
                        index.saturating_sub(WINDOW_BEFORE),
                        (index + WINDOW_AFTER).min(graph.rows.len()),
                    ),
                    None => (0, graph.rows.len().min(COMMIT_LIMIT)),
                };
                let slice = &graph.rows[from..to];

                // A row for everything uncommitted, above the history, when
                // there is anything to show. Offering a permanently empty one
                // on a clean repository would be noise in the place the eye
                // lands first.
                let has_working = graph.working_changes > 0;

                // Wired before the rows go in, and before anything is
                // selected. A list box in Browse mode selects the first row as
                // it is appended, and selecting the row we came for is a no-op
                // when that is already the first row — connecting afterwards
                // meant the pane stayed empty until the cursor moved.
                let ids: Rc<RefCell<Vec<crate::commit::Target>>> =
                    Rc::new(RefCell::new(Vec::new()));
                {
                    let ids = Rc::clone(&ids);
                    let repo = load_path_for_detail.clone();
                    let load_detail = Rc::clone(&load_detail);
                    rows_box.connect_row_selected(move |_, row| {
                        let Some(row) = row else { return };
                        let index = row.index();
                        if index < 0 {
                            return;
                        }
                        let target = ids.borrow().get(index as usize).cloned();
                        if let Some(target) = target {
                            load_detail(repo.clone(), target);
                        }
                    });
                }
                {
                    let mut targets = ids.borrow_mut();
                    // Always index zero, shown or not: hiding a row keeps its
                    // place in the list, so the map from row to commit stays
                    // put as the working tree comes and goes.
                    targets.push(crate::commit::Target::Working);
                    targets.extend(
                        slice
                            .iter()
                            .map(|row| crate::commit::Target::Commit(row.id.to_string())),
                    );
                }

                let (working, working_count) = working_row(graph.working_changes, width);
                working.set_visible(has_working);
                rows_box.append(&working);
                for row in slice {
                    rows_box.append(&row_widget(row, width));
                }

                let offset = 1;
                // Land on the commit asked for, or on the working tree when
                // there is one, or on the newest commit.
                let select_at = match (graph.target, has_working) {
                    (Some(index), _) => index - from + offset,
                    (None, true) => 0,
                    (None, false) => offset,
                };
                let landing = rows_box.row_at_index(select_at as i32);
                if let Some(row) = &landing {
                    rows_box.select_row(Some(row));
                    if graph.target.is_some() {
                        row.add_css_class("om-target");
                    }
                }
                // The keyboard goes where the selection is. Every key this
                // page answers to is bound on the list, and a page pushed
                // while it was still loading had nothing to focus but the
                // header bar: the top row looked chosen, and `l`, `p` and Tab
                // did nothing until Down had been pressed once. Not when the
                // keyboard is at work in the pane below, though. A reload
                // after a commit must not pull it out of the message box.
                if detail_for_focus.focus_child().is_none() {
                    if let Some(row) = landing {
                        // Once the list has been laid out, focusing the row
                        // brings it into view; doing it immediately would
                        // scroll against a height the list does not have yet.
                        glib::timeout_add_local_once(
                            std::time::Duration::from_millis(50),
                            move || {
                                row.grab_focus();
                            },
                        );
                    }
                }
                // Weakly, as with the key controller above.
                let toasts_for_rows = toasts_for_rows.downgrade();
                rows_box.connect_row_activated(move |_, row| {
                    let index = row.index();
                    if index < 0 {
                        return;
                    }
                    let target = ids.borrow().get(index as usize).cloned();
                    // The working-tree row has no sha to copy.
                    let Some(crate::commit::Target::Commit(id)) = target else { return };
                    row.clipboard().set_text(&id);
                    if let Some(toasts) = toasts_for_rows.upgrade() {
                        toasts.add_toast(adw::Toast::new(&format!(
                            "Copied {}",
                            &id[..7.min(id.len())]
                        )));
                    }
                });
                watch_working_tree(
                    &page_for_focus,
                    &load_path_for_detail,
                    &rows_box,
                    &working,
                    &working_count,
                    Rc::clone(&load_detail),
                    Rc::clone(&focus_handler),
                    (graph.head.clone(), graph.tip.clone()),
                    Rc::clone(&reload_for_push),
                );

                arm_push(
                    &push,
                    &push_content,
                    &graph,
                    &load_path_for_detail,
                    &toasts_for_push,
                    Rc::clone(&reload_for_push),
                    Rc::clone(&push_handler),
                );

                stack.set_visible_child_name("graph");
                status.set_label(&summary(&graph, from, to));
            }
            Err(err) => {
                empty.set_title("Could not read this repository");
                empty.set_description(Some(&err));
                stack.set_visible_child_name("empty");
                status.set_label(&err);
            }
        }
        })
    };

    let start_load: Rc<dyn Fn()> = {
        let populate = Rc::clone(&populate);
        let load_path = load_path.clone();
        let wanted = wanted.clone();
        Rc::new(move || {
            let (tx, rx) = async_channel::bounded(1);
            let path = load_path.clone();
            let target = wanted.clone();
            std::thread::spawn(move || {
                let _ = tx.send_blocking(graph::build(&path, COMMIT_LIMIT, target.as_deref()));
            });
            let populate = Rc::clone(&populate);
            glib::spawn_future_local(async move {
                if let Ok(result) = rx.recv().await {
                    populate(result);
                }
            });
        })
    };
    *reload_page.borrow_mut() = Some(Rc::clone(&start_load));
    start_load();

    // The slot holds the loader, the loader holds everything it fills in, and
    // several of those hold the slot: a ring that kept every page ever opened
    // in memory, five hundred rows and a diff buffer apiece. Emptying the slot
    // when the page is taken down lets go of all of it. `unrealize` is the
    // signal for that: unlike `hidden`, it does not fire when the page is
    // merely covered by another one.
    {
        let slot = Rc::clone(&reload_page);
        page.connect_unrealize(move |_| {
            slot.borrow_mut().take();
        });
    }

    page
}

fn lanes_width(lanes: usize) -> i32 {
    (lane_x(lanes.clamp(1, MAX_LANES).saturating_sub(1)) + LANE_W) as i32
}

fn summary(graph: &Graph, from: usize, to: usize) -> String {
    let mut parts = Vec::new();
    parts.push(format!("{} commits", to - from));
    if let Some(index) = graph.target {
        parts.push(format!("around {} (#{})", graph.rows[index].short_id, index + 1));
    } else if graph.target_missed {
        parts.push("that commit is further back than the walk reached".to_string());
    } else if graph.truncated {
        parts.push(format!("newest {COMMIT_LIMIT} shown"));
    }
    parts.push(format!("{} lanes", graph.lanes));
    if graph.lanes > MAX_LANES {
        parts.push(format!("lanes past {MAX_LANES} are clipped"));
    }
    if let Some(head) = &graph.head {
        parts.push(format!("HEAD {head}"));
    }
    parts.join("  ·  ")
}

/// The row that stands for everything uncommitted, drawn where the next commit
/// would go so the graph reads as continuing into the working tree.
/// Fetch and fast-forward this repository, saying what happened. The same pass
/// the timer runs, so a dirty tree or a diverged branch is refused for the same
/// reasons rather than a second set.
fn pull(
    repo: &Path,
    toasts: &adw::ToastOverlay,
    reload: Rc<RefCell<Option<Rc<dyn Fn()>>>>,
    fast_forward: bool,
) {
    let rx = crate::refresh::spawn(
        vec![repo.to_path_buf()],
        crate::refresh::Options {
            fast_forward,
            max_parallel: 1,
            timeout: std::time::Duration::from_secs(120),
            pass: crate::refresh::Pass::One,
            cancel: crate::refresh::Cancel::default(),
        },
    );
    let toasts = toasts.clone();
    glib::spawn_future_local(async move {
        let mut said = "Nothing to do".to_string();
        let mut moved = false;
        while let Ok(event) = rx.recv().await {
            match event {
                crate::refresh::Event::Repo(report) => {
                    said = match &report.outcome {
                        crate::refresh::Outcome::FastForwarded(n) => {
                            moved = true;
                            format!("{n} commits pulled")
                        }
                        crate::refresh::Outcome::UpToDate => "Up to date".into(),
                        crate::refresh::Outcome::Behind(n) => {
                            format!("Fetched — {n} behind")
                        }
                        crate::refresh::Outcome::Skipped(skip) => {
                            format!("Left alone: {}", skip.reason())
                        }
                        crate::refresh::Outcome::Refused(why) => {
                            format!("Fetched, not updated. {why}")
                        }
                        crate::refresh::Outcome::Failed(err) => err.clone(),
                    };
                }
                crate::refresh::Event::Done => break,
            }
        }
        toasts.add_toast(adw::Toast::new(&said));
        if moved {
            if let Some(reload) = reload.borrow().clone() {
                reload();
            }
        }
    });
}

/// Set the push button up for what this repository can actually do, and say in
/// the tooltip why when it can do nothing.
fn arm_push(
    push: &gtk::Button,
    content: &adw::ButtonContent,
    graph: &Graph,
    repo: &Path,
    toasts: &adw::ToastOverlay,
    reload: Rc<RefCell<Option<Rc<dyn Fn()>>>>,
    handler: Rc<RefCell<Option<glib::SignalHandlerId>>>,
) {
    if let Some(previous) = handler.borrow_mut().take() {
        push.disconnect(previous);
    }
    let plan = graph.push.as_ref().ok();
    let refusal = match &graph.push {
        Err(nowhere) => Some(nowhere.clone()),
        Ok(plan) => crate::commit::why_not_push(plan, graph.ahead, graph.behind),
    };
    let publish = plan.is_some_and(|plan| plan.publish);

    content.set_label(&match (publish, graph.ahead) {
        // A branch that has never been pushed is published, not pushed — the
        // word matters, because it is the act that makes it visible.
        (true, _) => "Publish".to_string(),
        (false, 0) => "Push".to_string(),
        (false, n) => format!("Push {n}"),
    });
    push.set_sensitive(refusal.is_none());
    push.set_tooltip_text(Some(&match (&refusal, plan) {
        (Some(reason), _) => reason.clone(),
        (None, Some(plan)) if plan.publish => {
            format!("Publish {} to {}", plan.branch, plan.remote)
        }
        (None, Some(plan)) => format!(
            "Send {} commits on {} to {}",
            graph.ahead,
            plan.branch,
            graph.upstream.as_deref().unwrap_or(&plan.remote)
        ),
        (None, None) => "Push".to_string(),
    }));

    let Some(armed) = plan.cloned() else { return };
    let repo = repo.to_path_buf();
    let toasts = toasts.clone();
    let id = push.connect_clicked(move |button| {
        button.set_sensitive(false);
        // What goes to git is the plan the button was drawn from, and only if
        // the repository still gives that plan when the worker asks again.
        let rx = crate::commit::spawn_push(repo.clone(), armed.clone());
        let button = button.clone();
        let toasts = toasts.clone();
        let reload = Rc::clone(&reload);
        glib::spawn_future_local(async move {
            match rx.recv().await {
                Ok(Ok(line)) => {
                    toasts.add_toast(adw::Toast::new(&line));
                    if let Some(reload) = reload.borrow().clone() {
                        reload();
                    }
                }
                // The page was out of date. Say so, and read it again so the
                // button shows what is true now.
                Ok(Err(crate::commit::PushError::Stale(why))) => {
                    toasts.add_toast(adw::Toast::new(&why));
                    if let Some(reload) = reload.borrow().clone() {
                        reload();
                    }
                }
                // A pre-push hook's refusal is quoted, not paraphrased.
                Ok(Err(crate::commit::PushError::Git(err))) => {
                    button.set_sensitive(true);
                    toasts.add_toast(adw::Toast::new(&err));
                }
                Err(_) => button.set_sensitive(true),
            }
        });
    });
    *handler.borrow_mut() = Some(id);
}

/// Re-read the working tree whenever the window regains focus. It changes
/// outside the app — that is rather the point of it — and nothing else would
/// notice. The handler lives only while this page is on screen; left connected
/// it would fire for pages the user has already left, and keep them alive.
///
/// `shown` is where HEAD was when the page was read: its name and its commit.
/// When either has changed — a branch switched, a commit made, in the terminal
/// this page offers on `l` — the whole page is read again, not just the count.
/// The history, the ahead and behind figures and the push button all hang off
/// HEAD, and all of them would otherwise go on describing where it used to be.
fn watch_working_tree(
    page: &adw::NavigationPage,
    repo: &Path,
    rows: &gtk::ListBox,
    working: &gtk::ListBoxRow,
    count_label: &gtk::Label,
    load_detail: Rc<dyn Fn(PathBuf, crate::commit::Target)>,
    handler: Rc<RefCell<Option<glib::SignalHandlerId>>>,
    shown: (Option<String>, Option<String>),
    reload: Rc<RefCell<Option<Rc<dyn Fn()>>>>,
) {
    let Some(window) = page.root().and_downcast::<gtk::Window>() else { return };

    // A reload populates the page again; without this the window would collect
    // one handler per commit, each holding widgets that no longer exist.
    if let Some(previous) = handler.borrow_mut().take() {
        window.disconnect(previous);
    }
    {
        let repo = repo.to_path_buf();
        let rows = rows.clone();
        let working = working.clone();
        let count_label = count_label.clone();
        let id = window.connect_is_active_notify(move |window| {
            if !window.is_active() {
                return;
            }
            let rx = crate::graph::spawn_glance(repo.clone());
            let repo = repo.clone();
            let rows = rows.clone();
            let working = working.clone();
            let count_label = count_label.clone();
            let load_detail = Rc::clone(&load_detail);
            let shown = shown.clone();
            let reload = Rc::clone(&reload);
            glib::spawn_future_local(async move {
                let Ok(glance) = rx.recv().await else { return };
                if (&glance.head, &glance.tip) != (&shown.0, &shown.1) {
                    if let Some(reload) = reload.borrow().clone() {
                        reload();
                    }
                    return;
                }
                let changes = glance.changes;
                working.set_visible(changes > 0);
                count_label.set_label(&entries(changes));
                // Only re-read the pane when it is the working tree on show;
                // a commit cannot have changed under us.
                let on_working = rows
                    .selected_row()
                    .map(|row| row.index() == 0)
                    .unwrap_or(false);
                if on_working && changes > 0 {
                    load_detail(repo, crate::commit::Target::Working);
                }
            });
        });
        *handler.borrow_mut() = Some(id);
    }

    let window = window.clone();
    page.connect_hidden(move |_| {
        if let Some(id) = handler.borrow_mut().take() {
            window.disconnect(id);
        }
    });
}

fn entries(changes: usize) -> String {
    format!("{changes} {}", if changes == 1 { "entry" } else { "entries" })
}

fn working_row(changes: usize, graph_width: i32) -> (gtk::ListBoxRow, gtk::Label) {
    let line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    line.set_height_request(ROW_H);

    let area = gtk::DrawingArea::builder()
        .content_width(graph_width)
        .content_height(ROW_H)
        .build();
    area.set_draw_func(move |_, cr, _, height| {
        let mid = height as f64 / 2.0;
        let x = lane_x(0);
        set_color(cr, lane_color(0));
        cr.set_line_width(1.6);
        // Dashed, because this is not a commit and should not pretend to be.
        cr.set_dash(&[3.0, 3.0], 0.0);
        cr.move_to(x, mid);
        cr.line_to(x, height as f64);
        let _ = cr.stroke();
        cr.set_dash(&[], 0.0);
        cr.arc(x, mid, DOT_R, 0.0, std::f64::consts::TAU);
        let _ = cr.stroke();
    });
    line.append(&area);

    let label = gtk::Label::builder()
        .label("Local changes")
        .xalign(0.0)
        .hexpand(true)
        .build();
    label.add_css_class("om-summary");
    line.append(&label);

    let count = gtk::Label::new(Some(&entries(changes)));
    count.add_css_class("om-meta");
    line.append(&count);

    let row = gtk::ListBoxRow::builder().child(&line).build();
    row.add_css_class("om-row");
    row.set_tooltip_text(Some(
        "Everything uncommitted, including untracked files.\nNot a commit — nothing to copy.",
    ));
    (row, count)
}

fn row_widget(row: &Row, graph_width: i32) -> gtk::ListBoxRow {
    let line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    line.set_height_request(ROW_H);

    line.append(&graph_cell(row, graph_width));

    let sha = gtk::Label::builder()
        .label(&row.short_id)
        .xalign(0.0)
        .width_chars(7)
        .build();
    sha.add_css_class("om-sha");
    line.append(&sha);

    for label in &row.refs {
        let pill = gtk::Label::new(Some(&label.name));
        pill.add_css_class("om-ref");
        pill.add_css_class(match label.kind {
            RefKind::Head => "head",
            RefKind::LocalBranch => "local",
            RefKind::RemoteBranch => "remote",
            RefKind::Tag => "tag",
        });
        if label.current {
            pill.add_css_class("current");
        }
        line.append(&pill);
    }

    let summary = gtk::Label::builder()
        .label(&row.summary)
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(pango::EllipsizeMode::End)
        .build();
    summary.add_css_class("om-summary");
    line.append(&summary);

    let author = gtk::Label::builder()
        .label(&row.author)
        .xalign(1.0)
        .width_chars(16)
        .max_width_chars(16)
        .ellipsize(pango::EllipsizeMode::End)
        .build();
    author.add_css_class("om-meta");
    line.append(&author);

    let when = gtk::Label::builder()
        .label(relative_time(row.when))
        .xalign(1.0)
        .width_chars(13)
        .build();
    when.add_css_class("om-meta");
    line.append(&when);

    let listed = gtk::ListBoxRow::builder().child(&line).build();
    listed.add_css_class("om-row");
    // Activating a row puts its sha on the clipboard — the thing you nearly
    // always want a commit graph for.
    listed.set_tooltip_text(Some(&format!(
        "{}\n\n{}\n{} <{}>\n{}\n\nEnter copies the sha.",
        row.short_id,
        if row.body.is_empty() { &row.summary } else { &row.body },
        row.author,
        row.email,
        absolute_time(row.when),
    )));
    listed
}

/// One row of the DAG. The top half draws what arrives from the row above, the
/// bottom half what leaves towards the row below, so neighbouring rows meet at
/// the boundary without either knowing about the other.
fn graph_cell(row: &Row, width: i32) -> gtk::DrawingArea {
    let area = gtk::DrawingArea::builder()
        .content_width(width)
        .content_height(ROW_H)
        .build();

    let row = row.clone();
    area.set_draw_func(move |_, cr, _, height| {
        let h = height as f64;
        let mid = h / 2.0;
        let dot_x = lane_x(row.lane);
        cr.set_line_width(1.6);

        // --- arriving from above -----------------------------------------
        for (i, slot) in row.incoming.iter().enumerate() {
            if slot.is_none() || i >= MAX_LANES {
                continue;
            }
            set_color(cr, lane_color(i));
            if *slot == Some(row.id) && i != row.lane {
                // A second child's lane bending into this commit.
                curve(cr, lane_x(i), 0.0, dot_x, mid);
            } else if *slot == Some(row.id) {
                cr.move_to(dot_x, 0.0);
                cr.line_to(dot_x, mid);
            } else {
                cr.move_to(lane_x(i), 0.0);
                cr.line_to(lane_x(i), mid);
            }
            let _ = cr.stroke();
        }

        // --- leaving towards the row below --------------------------------
        for (j, slot) in row.outgoing.iter().enumerate() {
            if slot.is_none() || j >= MAX_LANES {
                continue;
            }
            set_color(cr, lane_color(j));
            let unchanged = row.incoming.get(j).copied().flatten() == *slot;
            if unchanged {
                cr.move_to(lane_x(j), mid);
                cr.line_to(lane_x(j), h);
                let _ = cr.stroke();
            }
            if row.down.contains(&j) {
                if j == row.lane {
                    cr.move_to(dot_x, mid);
                    cr.line_to(dot_x, h);
                } else {
                    curve(cr, dot_x, mid, lane_x(j), h);
                }
                let _ = cr.stroke();
            }
        }

        // --- the commit itself ---------------------------------------------
        if row.lane < MAX_LANES {
            set_color(cr, lane_color(row.lane));
            if row.is_merge {
                // A ring, so merges read differently at a glance.
                cr.set_line_width(2.0);
                cr.arc(dot_x, mid, DOT_R, 0.0, std::f64::consts::TAU);
                let _ = cr.stroke();
            } else {
                cr.arc(dot_x, mid, DOT_R, 0.0, std::f64::consts::TAU);
                let _ = cr.fill();
            }
            if !row.refs.is_empty() {
                cr.set_line_width(1.0);
                cr.arc(dot_x, mid, DOT_R + 2.5, 0.0, std::f64::consts::TAU);
                let _ = cr.stroke();
            }
        }
    });

    area
}

fn curve(cr: &gtk::cairo::Context, x0: f64, y0: f64, x1: f64, y1: f64) {
    let dy = y1 - y0;
    cr.move_to(x0, y0);
    cr.curve_to(x0, y0 + dy * 0.55, x1, y1 - dy * 0.55, x1, y1);
}

fn set_color(cr: &gtk::cairo::Context, hex: &str) {
    let (r, g, b) = crate::theme::Palette::rgb(hex);
    cr.set_source_rgb(r, g, b);
}
