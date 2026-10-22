//! The per-repository commit graph page.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use gtk::pango;

use crate::commit::Target;
use crate::config::tildify;
use crate::graph::{self, Glance, Graph, RefKind, Row};
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
/// Ref labels drawn on a row before the rest are folded into one "+N".
const MAX_PILLS: usize = 3;
/// The longest a ref label is drawn. The whole name is in its tooltip.
const PILL_CHARS: i32 = 24;

fn lane_x(lane: usize) -> f64 {
    LANE_W * lane as f64 + LANE_W / 2.0 + 3.0
}

/// What a graph page asks of the window it is shown in. The page knows one
/// repository. What concerns the fleet, the settings or the window itself is
/// the window's, and reaches the page through here.
pub struct Host {
    /// Whether the detail pane starts open, and where a change is remembered.
    pub detail_open: bool,
    pub on_detail_toggled: Rc<dyn Fn(bool)>,
    pub on_terminal: Rc<dyn Fn()>,
    pub on_editor: Rc<dyn Fn()>,
    /// The repository's page on the web, if its remote has one: whether there
    /// is a button for it, and what the button says it opens.
    pub web: Option<String>,
    /// Opens that page, or says why there is none.
    pub on_web: Rc<dyn Fn()>,
    /// Opens the contributions page scoped to this repository.
    pub on_contributions: Rc<dyn Fn(PathBuf)>,
    /// Opens the commit search on this repository alone.
    pub on_search: Rc<dyn Fn()>,
    /// Fetch this repository. The window runs it and says what happened: it
    /// has the settings, knows whether something else is already running, and
    /// keeps the record of what a fetch could not do. The page had a copy of
    /// its own, with its own timeout, no guard against a second one, and no
    /// word to the card. It calls back with whether the remote was reached.
    pub on_fetch: Rc<dyn Fn(Rc<dyn Fn(bool)>)>,
    /// The window's toasts. They are over every page, so what was said is
    /// still on screen when this page has been left.
    pub toasts: adw::ToastOverlay,
}

/// Whether the keyboard is on `widget` or on something inside it.
fn holds_focus(widget: &gtk::Widget) -> bool {
    widget
        .root()
        .and_then(|root| root.focus())
        .is_some_and(|focus| &focus == widget || focus.is_ancestor(widget))
}

/// The window's handler that looks at the repository again on focus, and the
/// window it is connected to. One slot, so a reload replaces the handler
/// rather than stacking another on the window.
type Watcher = Rc<RefCell<Option<(glib::WeakRef<gtk::Window>, glib::SignalHandlerId)>>>;

/// Build the page and start loading. `target` is a commit to open on — it is
/// sought out however far back it is, highlighted, and scrolled to.
pub fn page(path: &Path, name: &str, target: Option<String>, host: Host) -> adw::NavigationPage {
    let Host {
        detail_open,
        on_detail_toggled,
        on_terminal,
        on_editor,
        web,
        on_web,
        on_contributions,
        on_search,
        on_fetch,
        toasts,
    } = host;

    let title = adw::WindowTitle::new(name, &tildify(path));
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&title));

    // Outermost, being the one button here that leaves the window.
    if let Some(page) = &web {
        let open = gtk::Button::builder()
            .icon_name("web-browser-symbolic")
            .tooltip_text(format!("Open on {} (o)\n{page}", crate::remote_url::web_site(page)))
            .has_frame(false)
            .build();
        let on_web = Rc::clone(&on_web);
        open.connect_clicked(move |_| on_web());
        header.pack_end(&open);
    }

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

    // A list box so rows take focus and arrow keys walk them. Browse selection
    // makes the selection follow the cursor, which is what drives the detail
    // pane below.
    let rows_box = gtk::ListBox::new();
    rows_box.set_selection_mode(gtk::SelectionMode::Browse);
    // A click selects, and selecting is what fills the pane. Left at the
    // default a click activated the row as well, so reading history with the
    // mouse put a sha on the clipboard and a toast on screen for every commit
    // looked at. Enter copies, and so does a double click.
    rows_box.set_activate_on_single_click(false);
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

    let status = gtk::Label::builder()
        .xalign(0.0)
        .ellipsize(pango::EllipsizeMode::End)
        .build();
    status.add_css_class("om-statusbar");

    // The page reads itself again when its repository has been fetched, and
    // when the window is returned to: what it draws changes under it, in the
    // terminal or the editor it hands the repository to.
    let reload_page: Rc<RefCell<Option<Rc<dyn Fn()>>>> = Rc::new(RefCell::new(None));
    // The commit a search result opened the page on. It is kept across every
    // reload, so that the reader is not thrown off the commit they came for.
    let wanted: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(target));
    // The detail goes *below* the graph rather than beside it: the window is
    // often half a tiled screen, where a vertical split leaves neither half
    // usable, and height is the one thing such a window has to spare.
    let (detail, load_pane, focus_files) = crate::ui::commit::pane();
    // What the pane was last asked to show, whether or not it was open to be
    // shown it. Closed, the pane was only hidden: every move of the cursor
    // still read the commit, rebuilt the file list and highlighted the first
    // file's diff, for nobody. Now it is told when it is open, and catches up
    // when it is opened.
    let wanted_detail: Rc<RefCell<Option<(PathBuf, Target)>>> = Rc::new(RefCell::new(None));
    let load_detail: Rc<dyn Fn(PathBuf, Target)> = {
        let detail = detail.clone();
        let wanted = Rc::clone(&wanted_detail);
        Rc::new(move |repo: PathBuf, target: Target| {
            if detail.get_visible() {
                load_pane(repo.clone(), target.clone());
            }
            *wanted.borrow_mut() = Some((repo, target));
        })
    };
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
        let placed = Rc::new(Cell::new(false));
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
        let focus_files = Rc::clone(&focus_files);
        let detail = detail.clone();
        let status = status.clone();
        let reload = Rc::clone(&reload_page);
        let toasts = toasts.clone();
        let presses = crate::ui::OncePerPress::default();
        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed(move |_, key, _, state| {
            // Ahead of the modifier test. A question mark is a shifted key on
            // every common layout, and behind that test it was never reached.
            if key == gtk::gdk::Key::question {
                if let Some(window) = toasts.root().and_downcast::<gtk::Window>() {
                    crate::ui::shortcuts::dialog().present(Some(&window));
                }
                return glib::Propagation::Stop;
            }
            if key == gtk::gdk::Key::Tab && !state.contains(gtk::gdk::ModifierType::SHIFT_MASK) {
                // Without this, Tab steps through the commits one row at a
                // time and the pane below can never be reached from the
                // keyboard at all. Only when the pane is there to be reached,
                // though: sent into a hidden one, the keyboard went to a row
                // nobody could see, and Tab could not leave the list either.
                if !detail.is_mapped() {
                    return glib::Propagation::Proceed;
                }
                focus_files();
                return glib::Propagation::Stop;
            }
            if !crate::ui::unmodified(state) {
                return glib::Propagation::Proceed;
            }
            match key {
                // The same letters as the grid: you are inside a repository
                // here, so they mean the same thing.
                gtk::gdk::Key::l => {
                    if presses.first(key) {
                        on_terminal();
                    }
                    glib::Propagation::Stop
                }
                gtk::gdk::Key::e => {
                    if presses.first(key) {
                        on_editor();
                    }
                    glib::Propagation::Stop
                }
                gtk::gdk::Key::o => {
                    if presses.first(key) {
                        on_web();
                    }
                    glib::Propagation::Stop
                }
                gtk::gdk::Key::f => {
                    if presses.first(key) {
                        // The spinner that shows a fetch is on the grid's
                        // header, which this page is in front of.
                        let before = status.label();
                        status.set_label("Fetching…");
                        let status = status.clone();
                        let reload = Rc::clone(&reload);
                        on_fetch(Rc::new(move |reached| {
                            // A fetch moves the remote's branches, and the
                            // labels and the ahead and behind figures are
                            // read from those: without drawing again,
                            // "fetched, 3 behind" appeared over a graph that
                            // showed none of it.
                            match reload.borrow().clone() {
                                Some(reload) if reached => reload(),
                                _ => status.set_label(&before),
                            }
                        }));
                    }
                    glib::Propagation::Stop
                }
                _ => glib::Propagation::Proceed,
            }
        });
        rows_box.add_controller(keys);
    }

    let stack = gtk::Stack::new();
    stack.add_named(&loading, Some("loading"));
    stack.add_named(&split, Some("graph"));
    stack.add_named(&empty, Some("empty"));
    stack.set_vexpand(true);

    let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
    body.append(&stack);
    body.append(&status);

    let view = adw::ToolbarView::new();
    view.add_top_bar(&header);
    view.set_content(Some(&body));

    let page = adw::NavigationPage::builder()
        .title(name)
        .child(&view)
        .build();

    {
        let detail = detail.clone();
        let rows_box = rows_box.clone();
        let load_detail = Rc::clone(&load_detail);
        detail_toggle.connect_toggled(move |button| {
            let open = button.is_active();
            // Out of the pane before it goes. Hidden with the keyboard still
            // inside it, GTK gave the focus to the nearest thing left on
            // screen, the handle of the split: the arrow keys then moved a
            // divider nobody could see instead of walking the commits.
            if !open && holds_focus(&detail) {
                match rows_box.selected_row() {
                    Some(row) => row.grab_focus(),
                    None => rows_box.grab_focus(),
                };
            }
            detail.set_visible(open);
            if open {
                let wanted = wanted_detail.borrow().clone();
                if let Some((repo, target)) = wanted {
                    load_detail(repo, target);
                }
            }
            on_detail_toggled(open);
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

    let load_path = path.to_path_buf();

    // Which commit each row stands for. Made once, and the two handlers that
    // read it connected once, here. Every populate used to connect its own
    // pair with its own list, and a list box keeps its handlers when its rows
    // are taken out: after a reload, Enter copied the sha the row
    // used to hold and then the one it holds now, and each move of the cursor
    // loaded two commits into the pane.
    let ids: Rc<RefCell<Vec<Target>>> = Rc::new(RefCell::new(Vec::new()));
    {
        let ids = Rc::clone(&ids);
        let repo = load_path.clone();
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
        let ids = Rc::clone(&ids);
        let toasts = toasts.clone();
        rows_box.connect_row_activated(move |_, row| {
            let index = row.index();
            if index < 0 {
                return;
            }
            let target = ids.borrow().get(index as usize).cloned();
            // The working-tree row has no sha to copy.
            let Some(Target::Commit(id)) = target else { return };
            row.clipboard().set_text(&id);
            toasts.add_toast(crate::ui::toast(&format!(
                "Copied {}",
                &id[..7.min(id.len())]
            )));
        });
    }

    // One slot for the focus watcher: populate runs again on every reload,
    // and a second watcher would read the repository twice on each return.
    let watcher: Watcher = Rc::new(RefCell::new(None));
    // Whether the page has been filled before. Arriving puts the keyboard on
    // the list; a reload under a reader must leave it where they have it.
    let arrived = Rc::new(Cell::new(false));

    // Populating is separate from loading, so a reload can ask for it again.
    let populate: Rc<dyn Fn(Result<Graph, String>)> = {
        let rows_box = rows_box.clone();
        let stack = stack.clone();
        let status = status.clone();
        let reload = Rc::clone(&reload_page);
        let empty = empty.clone();
        let load_detail = Rc::clone(&load_detail);
        let detail = detail.clone();
        let load_path = load_path.clone();
        let watcher = Rc::clone(&watcher);
        let ids = Rc::clone(&ids);
        let arrived = Rc::clone(&arrived);
        Rc::new(move |result: Result<Graph, String>| {
            // Where the selection and the keyboard were, before the rows go.
            let kept = rows_box.selected_row().and_then(|row| {
                usize::try_from(row.index()).ok().and_then(|index| ids.borrow().get(index).cloned())
            });
            let in_pane = holds_focus(&detail);
            let arriving = !arrived.replace(true);
            // Emptied first: taking the rows out unselects one, and nothing
            // should be loaded on the way down.
            ids.borrow_mut().clear();
            rows_box.remove_all();

            let graph = match result {
                Ok(graph) => graph,
                Err(err) => {
                    stop_watching(&watcher);
                    empty.set_title("Could not read this repository");
                    empty.set_description(Some(&glib::markup_escape_text(&err)));
                    stack.set_visible_child_name("empty");
                    status.set_label(&err);
                    return;
                }
            };

            // Empty in both senses: no history, and nothing waiting to become
            // any. A repository with files and no commits yet is not that. It
            // used to get this page too, which hid the one row its first
            // commit is made from.
            if graph.rows.is_empty()
                && graph.working_changes == 0
                && graph.working_unreadable.is_none()
            {
                empty.set_title("No commits");
                empty.set_description(Some("This repository has no history yet."));
                stack.set_visible_child_name("empty");
                status.set_label("no commits");
                // Still watched, so the first file to appear brings the page
                // to life without it having to be closed and opened.
                let reload = Rc::clone(&reload);
                watch(
                    &stack,
                    &load_path,
                    &watcher,
                    Rc::new(move |glance: Glance| {
                        if glance.changes > 0 || glance.unreadable.is_some() || glance.tip.is_some() {
                            if let Some(reload) = reload.borrow().clone() {
                                reload();
                            }
                        }
                    }),
                );
                return;
            }

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
            // A working tree that could not be read gets the row as well. Its
            // count is not zero, it is unknown, and the row is the only way to
            // the pane that says why.
            let has_working = graph.working_changes > 0 || graph.working_unreadable.is_some();

            // Filled before the rows go in: selecting a row is what loads the
            // pane, and the handler reads this to know which commit it is.
            {
                let mut targets = ids.borrow_mut();
                // Always index zero, shown or not: hiding a row keeps its
                // place in the list, so the map from row to commit stays
                // put as the working tree comes and goes.
                targets.push(Target::Working);
                targets.extend(slice.iter().map(|row| Target::Commit(row.id.to_string())));
            }

            // The dashed tail says "this is where the next commit goes", which
            // is only true when the row beneath is the commit HEAD is at. The
            // first row is the newest commit of any branch, the remote's
            // included: in a repository that is behind, the tail hung the
            // uncommitted work on a commit it is not based on.
            let tail = slice
                .first()
                .filter(|row| row.refs.iter().any(|label| label.current))
                .map(|row| row.lane);
            let (working, working_count) = working_row(graph.working_changes, width, tail);
            if let Some(why) = &graph.working_unreadable {
                say_unreadable(&working_count, why);
            }
            working.set_visible(has_working);
            rows_box.append(&working);
            for row in slice {
                rows_box.append(&row_widget(row, width));
            }

            let offset = 1;
            let asked_for = graph.target.map(|index| index - from + offset);
            // Arriving: on the commit asked for, or on the working tree when
            // there is one, or on the newest commit. After a reload: where the
            // selection was, when that commit is still in the list. A fetch
            // should not throw the reader back to the top.
            let stayed = match (&kept, arriving) {
                (Some(Target::Commit(id)), false) => slice
                    .iter()
                    .position(|row| row.id.to_string() == *id)
                    .map(|index| index + offset),
                (Some(Target::Working), false) if has_working => Some(0),
                _ => None,
            };
            let select_at = stayed.unwrap_or(match (asked_for, has_working) {
                (Some(index), _) => index,
                (None, true) => 0,
                (None, false) => offset,
            });
            if let Some(row) = asked_for.and_then(|index| rows_box.row_at_index(index as i32)) {
                row.add_css_class("om-target");
            }
            let landing = rows_box.row_at_index(select_at as i32);
            if let Some(row) = &landing {
                rows_box.select_row(Some(row));
            }
            // The keyboard goes where the selection is. Every key this
            // page answers to is bound on the list, and a page pushed
            // while it was still loading had nothing to focus but the
            // header bar: the top row looked chosen, and `l`, `f` and Tab
            // did nothing until Down had been pressed once. Not when the
            // keyboard is at work in the pane below, though. A reload
            // must not pull it out of the list of changed files.
            if !in_pane {
                if let Some(row) = landing {
                    // Once the list has been laid out, focusing the row
                    // brings it into view; doing it immediately would
                    // scroll against a height the list does not have yet.
                    glib::timeout_add_local_once(
                        std::time::Duration::from_millis(50),
                        move || {
                            // Not into a page that another one is covering:
                            // the window's refresh reloads those too.
                            if row.is_mapped() {
                                row.grab_focus();
                            }
                        },
                    );
                }
            }

            {
                // `shown` is where HEAD was when the page was read: its name
                // and its commit. When either has changed (a branch switched,
                // a commit made, in the terminal this page offers on `l`) the
                // whole page is read again, not just the count. The history
                // hangs off HEAD, and would otherwise go on describing where
                // it used to be.
                let shown = (graph.head.clone(), graph.tip.clone());
                let reload = Rc::clone(&reload);
                let rows = rows_box.clone();
                let repo = load_path.clone();
                let load_detail = Rc::clone(&load_detail);
                watch(
                    &stack,
                    &load_path,
                    &watcher,
                    Rc::new(move |glance: Glance| {
                        if (&glance.head, &glance.tip) != (&shown.0, &shown.1) {
                            if let Some(reload) = reload.borrow().clone() {
                                reload();
                            }
                            return;
                        }
                        let changes = glance.changes;
                        let something = changes > 0 || glance.unreadable.is_some();
                        working.set_visible(something);
                        match &glance.unreadable {
                            Some(why) => say_unreadable(&working_count, why),
                            None => {
                                working_count.set_label(&entries(changes));
                                working_count.set_tooltip_text(None);
                            }
                        }
                        let on_working = rows
                            .selected_row()
                            .map(|row| row.index() == 0)
                            .unwrap_or(false);
                        if !on_working {
                            // A commit cannot have changed under us.
                            return;
                        }
                        if something {
                            load_detail(repo.clone(), Target::Working);
                            return;
                        }
                        // The tree went clean somewhere else while its row was
                        // the one selected. Hiding the row does not move the
                        // selection off it, and the pane went on showing the
                        // old files.
                        // Step to the newest commit, which loads it.
                        match rows.row_at_index(1) {
                            Some(first) => {
                                let had_focus = holds_focus(rows.upcast_ref());
                                rows.select_row(Some(&first));
                                if had_focus {
                                    first.grab_focus();
                                }
                            }
                            // No commits either, any more: start over.
                            None => {
                                if let Some(reload) = reload.borrow().clone() {
                                    reload();
                                }
                            }
                        }
                    }),
                );
            }

            stack.set_visible_child_name("graph");
            status.set_label(&summary(&graph, from, to));
        })
    };

    let start_load: Rc<dyn Fn()> = {
        let populate = Rc::clone(&populate);
        let load_path = load_path.clone();
        let wanted = Rc::clone(&wanted);
        Rc::new(move || {
            let (tx, rx) = async_channel::bounded(1);
            let path = load_path.clone();
            let target = wanted.borrow().clone();
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

    {
        // How the window reaches this page: `page.reload` for the refresh key
        // while the page is in front, and `page.reload-for` with a path, which
        // the window sends to every open page when its own refresh has moved a
        // repository. Both go through the slot, so neither keeps the page.
        // And `page.search-commits`, the commit search key, which searches
        // this repository.
        let group = gtk::gio::SimpleActionGroup::new();
        let reload = gtk::gio::SimpleAction::new("reload", None);
        {
            let slot = Rc::clone(&reload_page);
            reload.connect_activate(move |_, _| {
                if let Some(reload) = slot.borrow().clone() {
                    reload();
                }
            });
        }
        group.add_action(&reload);
        let reload_for =
            gtk::gio::SimpleAction::new("reload-for", Some(glib::VariantTy::STRING));
        {
            let slot = Rc::clone(&reload_page);
            let mine = load_path.display().to_string();
            reload_for.connect_activate(move |_, which| {
                if which.and_then(|which| which.str()) != Some(mine.as_str()) {
                    return;
                }
                if let Some(reload) = slot.borrow().clone() {
                    reload();
                }
            });
        }
        group.add_action(&reload_for);
        let search = gtk::gio::SimpleAction::new("search-commits", None);
        search.connect_activate(move |_, _| on_search());
        group.add_action(&search);
        page.insert_action_group("page", Some(&group));
    }

    // The slot holds the loader, the loader holds everything it fills in, and
    // several of those hold the slot: a ring that kept every page ever opened
    // in memory, five hundred rows and a diff buffer apiece. Emptying the slot
    // when the page is taken down lets go of all of it, and the window's
    // handler goes with it. `unrealize` is the signal for that: unlike
    // `hidden`, it does not fire when the page is merely covered by another.
    {
        let slot = Rc::clone(&reload_page);
        let watcher = Rc::clone(&watcher);
        page.connect_unrealize(move |_| {
            slot.borrow_mut().take();
            stop_watching(&watcher);
        });
    }

    page
}

fn lanes_width(lanes: usize) -> i32 {
    (lane_x(lanes.clamp(1, MAX_LANES).saturating_sub(1)) + LANE_W) as i32
}

fn summary(graph: &Graph, from: usize, to: usize) -> String {
    if graph.rows.is_empty() {
        return match &graph.head {
            Some(head) => format!("no commits yet  ·  HEAD {head}"),
            None => "no commits yet".to_string(),
        };
    }
    let count = |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
    let mut parts = Vec::new();
    parts.push(count(to - from, "commit", "commits"));
    if let Some(index) = graph.target {
        parts.push(format!("around {} (#{})", graph.rows[index].short_id, index + 1));
        // A window has edges, and "270 commits" alone read as the whole of it.
        if from > 0 || graph.truncated {
            parts.push("a window: the history goes on beyond it".to_string());
        }
    } else if graph.target_missed {
        parts.push("that commit is further back than the walk reached".to_string());
    } else if graph.truncated {
        parts.push(format!("newest {COMMIT_LIMIT} shown"));
    }
    parts.push(count(graph.lanes, "lane", "lanes"));
    if graph.lanes > MAX_LANES {
        parts.push(format!("lanes past {MAX_LANES} are clipped"));
    }
    if let Some(head) = &graph.head {
        parts.push(format!("HEAD {head}"));
    }
    // A branch whose commit cannot be read is left out of the rows, and left
    // out without a word it reads as a branch that is not there.
    if !graph.unreadable_refs.is_empty() {
        parts.push(format!("not drawn, unreadable: {}", graph.unreadable_refs.join(", ")));
    }
    parts.join("  ·  ")
}

fn stop_watching(watcher: &Watcher) {
    if let Some((window, id)) = watcher.borrow_mut().take() {
        if let Some(window) = window.upgrade() {
            window.disconnect(id);
        }
    }
}

/// Look at the repository again whenever the window regains focus. It changes
/// outside the app — that is rather the point of it — and nothing else would
/// notice. `on_glance` is handed what was seen. The handler is on the window,
/// so it is taken off again when the page goes; left connected it would keep
/// the page's widgets alive and go on firing for a page nobody can see.
///
/// `body` is the page's content, whichever of its faces is showing.
fn watch(body: &gtk::Stack, repo: &Path, watcher: &Watcher, on_glance: Rc<dyn Fn(Glance)>) {
    stop_watching(watcher);
    let Some(window) = body.root().and_downcast::<gtk::Window>() else { return };
    let repo = repo.to_path_buf();
    let body = body.downgrade();
    let id = window.connect_is_active_notify(move |window| {
        if !window.is_active() {
            return;
        }
        // Covered by another page, this one is not on screen, and what it
        // would be told can wait for the next time it is.
        if !body.upgrade().is_some_and(|body| body.is_mapped()) {
            return;
        }
        let rx = graph::spawn_glance(repo.clone());
        let on_glance = Rc::clone(&on_glance);
        glib::spawn_future_local(async move {
            if let Ok(glance) = rx.recv().await {
                on_glance(glance);
            }
        });
    });
    *watcher.borrow_mut() = Some((window.downgrade(), id));
}

/// The count on the Local changes row, when there is no count to give.
fn say_unreadable(count: &gtk::Label, why: &str) {
    count.set_label("unreadable");
    count.set_tooltip_text(Some(&format!("The working tree could not be read:\n{why}")));
}

fn entries(changes: usize) -> String {
    format!("{changes} {}", if changes == 1 { "entry" } else { "entries" })
}

/// The row that stands for everything uncommitted. `tail` is the lane of the
/// commit it would become a child of, when that commit is the row beneath: the
/// dashed line is then drawn down to it, so the graph reads as continuing into
/// the working tree. Without one the hollow dot stands alone.
fn working_row(
    changes: usize,
    graph_width: i32,
    tail: Option<usize>,
) -> (gtk::ListBoxRow, gtk::Label) {
    let line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    line.set_height_request(ROW_H);

    let area = gtk::DrawingArea::builder()
        .content_width(graph_width)
        .content_height(ROW_H)
        .build();
    let lane = tail.unwrap_or(0).min(MAX_LANES - 1);
    area.set_draw_func(move |_, cr, _, height| {
        let mid = height as f64 / 2.0;
        let x = lane_x(lane);
        set_color(cr, lane_color(lane));
        cr.set_line_width(1.6);
        if tail.is_some() {
            // Dashed, because this is not a commit and should not pretend to be.
            cr.set_dash(&[3.0, 3.0], 0.0);
            cr.move_to(x, mid);
            cr.line_to(x, height as f64);
            let _ = cr.stroke();
            cr.set_dash(&[], 0.0);
        }
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

/// One ref label on a row, held to a width. A label that may not shorten has
/// its whole text as its minimum width, a row is as wide as its labels, and
/// the list as wide as its widest row: one commit carrying a long branch name
/// pushed every row's summary, author and date out of the window.
fn ref_pill(text: &str, tooltip: &str, kind: &str, current: bool) -> gtk::Label {
    let pill = gtk::Label::builder()
        .label(text)
        .ellipsize(pango::EllipsizeMode::Middle)
        .max_width_chars(PILL_CHARS)
        .tooltip_text(tooltip)
        .build();
    pill.add_css_class("om-ref");
    pill.add_css_class(kind);
    if current {
        pill.add_css_class("current");
    }
    pill
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

    let kind = |kind: RefKind| match kind {
        RefKind::Head => "head",
        RefKind::LocalBranch => "local",
        RefKind::RemoteBranch => "remote",
        RefKind::Tag => "tag",
    };
    for label in row.refs.iter().take(MAX_PILLS) {
        line.append(&ref_pill(&label.name, &label.name, kind(label.kind), label.current));
    }
    if let Some(rest) = row.refs.get(MAX_PILLS..).filter(|rest| !rest.is_empty()) {
        let names: Vec<&str> = rest.iter().map(|label| label.name.as_str()).collect();
        line.append(&ref_pill(
            &format!("+{}", rest.len()),
            &names.join("\n"),
            kind(rest[0].kind),
            false,
        ));
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
