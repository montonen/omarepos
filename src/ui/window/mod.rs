//! The main window: the composition of the grid and its pages, and nothing
//! clever of its own. The filter and the sort are in `query`, the keyboard
//! and the pointer in `keyboard`, the window's side of fetching in
//! `refreshing`, and the tag chrome in `tags`; what is left here is the
//! widgets, the scan that fills them, and the wiring between the parts.

mod grid;
mod keyboard;
mod query;
mod refreshing;
mod scanning;
mod tags;

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gtk::gdk;
use gtk::gio;
use gtk::glib;
use adw::prelude::*;

use crate::config::Config;
use crate::meta::Meta;
use crate::refresh;
use crate::git::RepoInfo;
use crate::theme::Palette;
use crate::ui::{self, Action};

/// What the header needs with everything showing and no tag buttons in it,
/// and what the label of its Tags button takes of that. With tag buttons
/// there is no label: they say what it would.
const HEADER_FULL: f64 = 1010.0;
const TAGS_LABEL: f64 = 40.0;
/// What it needs without its labels, tag buttons not counted. Without those
/// and the sort order as well it needs 584.
const HEADER_BARE: f64 = 780.0;
/// Room to spare over each of those before the header gives something up.
/// All of it was measured with the debug dump under the built-in theme at
/// the default font size; a larger font needs more, which is what this is
/// for. The tag buttons are not in it: they are measured as they stand.
const SPARE: f64 = 40.0;
/// The least the window claims to be usable at. A window with breakpoints has
/// no minimum of its own: libadwaita drops the one its content would give it
/// and expects to be told.
const LEAST_WIDTH: i32 = 600;
const LEAST_HEIGHT: i32 = 400;

struct Ui {
    cfg: Rc<RefCell<Config>>,
    meta: Rc<RefCell<Meta>>,
    window: adw::ApplicationWindow,
    nav: adw::NavigationView,
    /// The page the grid is on, at the bottom of the stack.
    grid_page: adw::NavigationPage,
    provider: gtk::CssProvider,
    subtitle: gtk::Label,
    /// The name of the palette in use, as the subtitle shows it.
    theme_name: RefCell<String>,
    /// Whether that palette is a light one.
    light: Cell<bool>,
    /// The two widths at which the header gives something up: its labels,
    /// and below that its tag buttons and sort order. Kept because where
    /// they lie depends on how wide the tag buttons are; see `fit_header`.
    breakpoints: RefCell<Option<[adw::Breakpoint; 2]>>,
    /// Whether the window is too narrow for the header's labels.
    narrow: Cell<bool>,
    /// Whether it is too narrow for the tag chips as well.
    cramped: Cell<bool>,
    search: gtk::SearchEntry,
    sort: gtk::DropDown,
    dirty_only: gtk::ToggleButton,
    tags_button: gtk::MenuButton,
    tags_content: adw::ButtonContent,
    /// Per-tag toggles shown in the header instead of the popover when there
    /// are few enough tags to fit.
    tag_quick: gtk::Box,
    tag_quick_buttons: RefCell<Vec<(String, gtk::ToggleButton)>>,
    /// The tag set the quick row was last built from, so it is only rebuilt
    /// when the tags themselves change rather than on every grid rebuild.
    tag_signature: RefCell<String>,
    /// Set while the toggles are being synced, so programmatic changes do not
    /// come back round as user toggles.
    tag_syncing: Cell<bool>,
    /// True while the grid is being driven from the keyboard. Hover styling is
    /// suppressed then, so the pointer resting somewhere does not light a
    /// second card and leave two looking half-chosen.
    keyboard_nav: Cell<bool>,
    /// When the last key went to the grid. Arrow navigation scrolls the grid
    /// under a stationary pointer, which GTK reports as motion; ignoring
    /// motion briefly after a keypress keeps that from undoing the mode.
    last_key: Cell<Option<std::time::Instant>>,
    /// True while it is the pointer that is choosing a card: it has been
    /// moved, or the wheel turned, since a key last went to the grid or the
    /// filter was typed in. The card under it then shows what the card with
    /// the keyboard shows. Lying over a card is not enough: a pointer left
    /// where it was is over one every time the window opens.
    pointer_nav: Cell<bool>,
    /// Where in the window the pointer was when it was last seen to move.
    pointer_at: Cell<Option<(f64, f64)>>,
    scroller: gtk::ScrolledWindow,
    /// Lower-cased tags the grid is narrowed to; empty means no tag filter.
    tag_filter: RefCell<BTreeSet<String>>,
    /// Show only repositories with no tags at all.
    untagged_only: Cell<bool>,
    spinner: gtk::Spinner,
    stack: gtk::Stack,
    /// What the window shows in place of an empty grid.
    empty: adw::StatusPage,
    /// On that page, the way to Settings: shown when there is no repository
    /// to list at all, not when a filter hides the ones there are.
    empty_action: gtk::Button,
    /// The window has shown that page and the keyboard has not yet been
    /// started in the filter for the repositories that came after it.
    unstarted: Cell<bool>,
    flow: gtk::FlowBox,
    status: gtk::Label,
    /// Up for as long as a file the app keeps cannot be read.
    trouble: adw::Banner,
    toasts: adw::ToastOverlay,
    repos: RefCell<Vec<RepoInfo>>,
    /// The filtered, sorted repos currently in the grid, in flow box order.
    shown: RefCell<Vec<RepoInfo>>,
    /// What each card in `shown` was built from, as text, in the same order;
    /// see `grid`.
    signatures: RefCell<Vec<String>>,
    /// Bumped to have every card built again although nothing it says has
    /// changed: see `grid`.
    card_epoch: Cell<u64>,
    /// When every card was last built afresh, which is when the ages on
    /// them were last read off the clock.
    drawn_at: Cell<Option<std::time::Instant>>,
    /// Repositories whose card still shows what the last run read, until the
    /// scan reads them again; see `scanning`.
    from_last_run: RefCell<HashSet<PathBuf>>,
    /// When the last run's scan ended, for saying how old those cards are.
    last_run_at: Cell<Option<i64>>,
    /// Repos ticked for a contributions run. Keyed by path so the selection
    /// survives filtering and re-sorting.
    selected: RefCell<HashSet<PathBuf>>,
    contrib_content: adw::ButtonContent,
    generation: Cell<u64>,
    /// True from the start of a scan to its end. Asked of the spinner before,
    /// which answers "no" until the window is on screen, and which a fetch
    /// turns as well.
    scanning: Cell<bool>,
    /// Repositories whose cards no longer say what is true, because they
    /// were fetched from another page. Read again when the grid is next in
    /// front.
    stale: RefCell<BTreeSet<PathBuf>>,
    /// What the keyboard is to be given back to: the widget it was on when
    /// the content left the screen, until the content is back. See
    /// `note_keyboard`.
    owed: glib::WeakRef<gtk::Widget>,
    /// The repository whose page was opened last, which is the card the
    /// keyboard goes back to when that page is left.
    opened: RefCell<Option<PathBuf>>,
    /// Repositories handed to a terminal or an editor in this run. Whatever is
    /// done to them there is done behind the app's back, so they are read
    /// again each time the window is returned to.
    handed: RefCell<BTreeSet<PathBuf>>,
    expected: Cell<usize>,
    /// What the last scan made of each root: how many repositories it gave,
    /// or what stood in the way of it giving any.
    root_reports: RefCell<Vec<crate::git::RootReport>>,
    /// How many repositories the scan in progress has read so far. Not
    /// `repos.len()`: a rescan leaves the grid as it is until it has the whole
    /// answer, so the list on screen says nothing about how far along it is.
    scanned: Cell<usize>,
    /// Handle of the periodic refresh timer, so it can be rescheduled.
    refresh_source: RefCell<Option<glib::SourceId>>,
    /// Interval the live timer was armed with, so an unrelated settings change
    /// does not restart the countdown.
    refresh_armed: Cell<u64>,
    /// Whether the "is a refresh overdue" question has been asked this run.
    checked_overdue: Cell<bool>,
    /// Repositories the last refresh could not fetch, and
    /// why. Keyed by the path as a string so it can be written out and read
    /// back.
    fetch_errors: RefCell<BTreeMap<String, String>>,
    /// The repositories the user has seen; see `config::known_repos_path`.
    /// `None` until the first scan has recorded what was already here.
    known: RefCell<Option<BTreeSet<String>>>,
    /// The way to call off the pass across the fleet that is under way, when
    /// one is.
    refresh_cancel: RefCell<Option<refresh::Cancel>>,
    refreshing: Cell<bool>,
    refresh_progress: Cell<(usize, usize)>,
    /// Unix seconds of the last completed fetch pass.
    last_refresh: Cell<Option<i64>>,
    monitors: RefCell<Vec<gio::FileMonitor>>,
}

pub fn build(app: &adw::Application, cfg: Rc<RefCell<Config>>, provider: gtk::CssProvider) {
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("omarepos")
        .default_width(1180)
        .default_height(780)
        .width_request(LEAST_WIDTH)
        .height_request(LEAST_HEIGHT)
        .build();
    window.add_css_class("omarepos");

    // --- header ---------------------------------------------------------
    let header = adw::HeaderBar::new();

    let title_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let title = gtk::Label::new(Some("OMAREPOS"));
    title.add_css_class("om-title");
    let subtitle = gtk::Label::builder()
        .ellipsize(gtk::pango::EllipsizeMode::Middle)
        .max_width_chars(44)
        .build();
    subtitle.add_css_class("om-subtitle");
    title_box.append(&title);
    title_box.append(&subtitle);
    header.set_title_widget(Some(&title_box));

    let refresh = gtk::Button::builder()
        .icon_name("view-refresh-symbolic")
        .tooltip_text("Rescan (Ctrl+R)  ·  run the refresh now (Ctrl+Shift+R)")
        .has_frame(false)
        .build();
    header.pack_start(&refresh);

    let spinner = gtk::Spinner::new();
    spinner.set_visible(false);
    header.pack_start(&spinner);

    let contrib_content = adw::ButtonContent::builder()
        .icon_name("system-users-symbolic")
        .label("Contributions")
        .build();
    let contributions = gtk::Button::builder()
        .child(&contrib_content)
        .tooltip_text("Contributions across the selected repos, or all of them")
        .has_frame(false)
        .build();
    header.pack_start(&contributions);

    let commit_search = gtk::Button::builder()
        .icon_name("edit-find-symbolic")
        .tooltip_text("Search commits across these repos (Ctrl+Shift+F)")
        .has_frame(false)
        .build();
    header.pack_start(&commit_search);

    let tags_content = adw::ButtonContent::builder()
        .icon_name("view-list-symbolic")
        .label("Tags")
        .build();
    let tags_button = gtk::MenuButton::builder()
        .child(&tags_content)
        .tooltip_text("Filter by tag, or tag the selected repos")
        .has_frame(false)
        .build();
    header.pack_start(&tags_button);

    let tag_quick = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    tag_quick.set_visible(false);
    header.pack_start(&tag_quick);

    let search = gtk::SearchEntry::builder()
        .placeholder_text("Filter repos…")
        .width_request(240)
        .build();
    search.add_css_class("om-search");

    let sort = gtk::DropDown::from_strings(query::SORT_LABELS);
    sort.add_css_class("om-sort");
    sort.set_tooltip_text(Some("Sort order"));

    let dirty_only = gtk::ToggleButton::builder()
        .icon_name("dialog-warning-symbolic")
        .tooltip_text("Show only repos needing attention")
        .has_frame(false)
        .build();

    let settings = gtk::Button::builder()
        .icon_name("emblem-system-symbolic")
        .tooltip_text("Settings (Ctrl+,)")
        .has_frame(false)
        .build();

    header.pack_end(&settings);
    header.pack_end(&search);
    header.pack_end(&sort);
    header.pack_end(&dirty_only);

    // --- body -----------------------------------------------------------
    let flow = gtk::FlowBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .homogeneous(false)
        .min_children_per_line(1)
        .max_children_per_line(6)
        .column_spacing(12)
        .row_spacing(12)
        .margin_top(14)
        .margin_bottom(14)
        .margin_start(14)
        .margin_end(14)
        .valign(gtk::Align::Start)
        .build();
    flow.add_css_class("om-flow");

    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&flow)
        .build();
    scroller.add_css_class("om-grid");

    // What a window with nothing to list offers: the way to Settings, which
    // is where a folder to scan is named. A first launch on a machine that
    // keeps its repositories somewhere else used to end at a folder icon and
    // "~/code: does not exist", with the keyboard in a filter that had
    // nothing to filter, and no word of where to go from there.
    let empty_action = gtk::Button::builder()
        .label("Choose folders…")
        .tooltip_text("Open Settings (Ctrl+,)")
        .action_name("win.settings")
        .halign(gtk::Align::Center)
        .build();
    empty_action.add_css_class("pill");
    empty_action.add_css_class("suggested-action");
    let empty = adw::StatusPage::builder()
        .icon_name("folder-symbolic")
        .title("No repositories found")
        .child(&empty_action)
        .build();

    let stack = gtk::Stack::new();
    stack.add_named(&scroller, Some("grid"));
    stack.add_named(&empty, Some("empty"));
    stack.set_vexpand(true);

    // A label's minimum width is its whole text unless it may ellipsize, and
    // this one gets long enough to force the window wider than the screen.
    let status = gtk::Label::builder()
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .build();
    status.add_css_class("om-statusbar");

    // Shown only when a file the app keeps could not be read at launch. It
    // stays up: the app is running on defaults and will not save, and a toast
    // that came and went would leave that to be discovered later.
    let trouble = adw::Banner::new("");
    trouble.set_use_markup(false);

    let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
    body.append(&trouble);
    body.append(&stack);
    body.append(&status);

    let view = adw::ToolbarView::new();
    view.add_top_bar(&header);
    view.set_content(Some(&body));

    let grid_page = adw::NavigationPage::builder()
        .title("omarepos")
        .child(&view)
        .build();
    let nav = adw::NavigationView::new();
    nav.add(&grid_page);
    // One overlay, around every page. It used to sit inside the grid's page,
    // so whatever the window had to say (what a refresh came to, "already
    // busy", why nothing was fetched) was said behind the page in front.
    let toasts = adw::ToastOverlay::new();
    toasts.set_child(Some(&nav));
    window.set_content(Some(&toasts));

    let ui = Rc::new(Ui {
        cfg,
        meta: Rc::new(RefCell::new(Meta::load())),
        window: window.clone(),
        nav: nav.clone(),
        grid_page: grid_page.clone(),
        provider,
        subtitle,
        theme_name: RefCell::new(String::new()),
        light: Cell::new(false),
        breakpoints: RefCell::new(None),
        narrow: Cell::new(false),
        cramped: Cell::new(false),
        search: search.clone(),
        sort: sort.clone(),
        dirty_only: dirty_only.clone(),
        tags_button: tags_button.clone(),
        tags_content,
        tag_quick: tag_quick.clone(),
        tag_quick_buttons: RefCell::new(Vec::new()),
        tag_signature: RefCell::new(String::new()),
        tag_syncing: Cell::new(false),
        keyboard_nav: Cell::new(false),
        last_key: Cell::new(None),
        pointer_nav: Cell::new(false),
        pointer_at: Cell::new(None),
        scroller: scroller.clone(),
        tag_filter: RefCell::new(BTreeSet::new()),
        untagged_only: Cell::new(false),
        spinner,
        stack,
        empty: empty.clone(),
        empty_action: empty_action.clone(),
        unstarted: Cell::new(false),
        flow,
        status,
        trouble,
        toasts,
        repos: RefCell::new(Vec::new()),
        shown: RefCell::new(Vec::new()),
        signatures: RefCell::new(Vec::new()),
        card_epoch: Cell::new(0),
        drawn_at: Cell::new(None),
        from_last_run: RefCell::new(HashSet::new()),
        last_run_at: Cell::new(None),
        selected: RefCell::new(HashSet::new()),
        contrib_content,
        generation: Cell::new(0),
        scanning: Cell::new(false),
        stale: RefCell::new(BTreeSet::new()),
        owed: glib::WeakRef::new(),
        opened: RefCell::new(None),
        handed: RefCell::new(BTreeSet::new()),
        expected: Cell::new(0),
        root_reports: RefCell::new(Vec::new()),
        scanned: Cell::new(0),
        refresh_source: RefCell::new(None),
        refresh_armed: Cell::new(0),
        checked_overdue: Cell::new(false),
        fetch_errors: RefCell::new(crate::refresh::shown_failures(crate::config::load_fetch_errors())),
        known: RefCell::new(crate::config::load_known_repos()),
        refresh_cancel: RefCell::new(None),
        refreshing: Cell::new(false),
        refresh_progress: Cell::new((0, 0)),
        last_refresh: Cell::new(crate::config::last_refresh()),
        monitors: RefCell::new(Vec::new()),
    });

    // --- wiring ---------------------------------------------------------
    {
        let ui = Rc::clone(&ui);
        refresh.connect_clicked(move |_| ui.start_scan());
    }
    {
        // Activation covers both a click on the card and Enter on a focused
        // one; interactive children (the checkbox, the menu) take their own
        // clicks before the flow box ever sees them.
        let ui = Rc::clone(&ui);
        ui.flow.clone().connect_child_activated(move |_, child| {
            let index = child.index();
            if index < 0 {
                return;
            }
            let info = ui.shown.borrow().get(index as usize).cloned();
            if let Some(info) = info {
                ui.open_graph(&info);
            }
        });
    }
    {
        let ui = Rc::clone(&ui);
        contributions.connect_clicked(move |_| {
            let (repos, scope) = ui.contribution_scope();
            ui.open_contributions(repos, scope);
        });
    }
    {
        let ui = Rc::clone(&ui);
        search.connect_search_changed(move |_| {
            // The cards are about to change places under a pointer that is
            // wherever it was left when the typing began.
            ui.pointer_rests();
            ui.rebuild();
        });
    }
    {
        // Space ticks the focused card, so the grid can be worked through
        // without reaching for the mouse.
        let keys = gtk::EventControllerKey::new();
        let nav = Rc::clone(&ui);
        // The letters below each start something outside the window, and a
        // held key arrives as a run of presses.
        let presses = ui::OncePerPress::default();
        keys.connect_key_pressed(move |controller, key, _, state| {
            nav.enter_keyboard_nav();

            // `l` for lazygit, on the card the keyboard is on. The controller
            // is on the grid rather than the window, so a bare letter cannot
            // fire while the filter box has the focus.
            if key == gdk::Key::l && ui::unmodified(state) {
                if let Some(path) = nav.focused_repo().filter(|_| presses.first(key)) {
                    nav.terminal_opener(&path)();
                }
                return glib::Propagation::Stop;
            }

            // `p` as lazygit means it: fetch, and fast-forward the branch if
            // the repository is clean and the branch only behind. The one
            // thing here that changes a working tree, and only when asked.
            if key == gdk::Key::p && ui::unmodified(state) {
                if let Some(path) = nav.focused_repo().filter(|_| presses.first(key)) {
                    nav.fetch_one(path, true, None);
                }
                return glib::Propagation::Stop;
            }

            // `f` fetches. It moves no branch and touches no file.
            if key == gdk::Key::f && ui::unmodified(state) {
                if let Some(path) = nav.focused_repo().filter(|_| presses.first(key)) {
                    nav.fetch_one(path, false, None);
                }
                return glib::Propagation::Stop;
            }

            if key == gdk::Key::e && ui::unmodified(state) {
                if let Some(path) = nav.focused_repo().filter(|_| presses.first(key)) {
                    nav.editor_opener(&path)();
                }
                return glib::Propagation::Stop;
            }

            // `o` opens the repository's page on the web.
            if key == gdk::Key::o && ui::unmodified(state) {
                if let Some(path) = nav.focused_repo().filter(|_| presses.first(key)) {
                    nav.open_web(&path);
                }
                return glib::Propagation::Stop;
            }

            // `?` lives here rather than on an accelerator, so typing one into
            // the filter box types one.
            if key == gdk::Key::question {
                ui::shortcuts::dialog().present(Some(&nav.window));
                return glib::Propagation::Stop;
            }

            // `/` for the filter, here for the same reason. Shift is allowed:
            // on a Swedish layout a slash is Shift+7.
            if key == gdk::Key::slash
                && !state.intersects(
                    gdk::ModifierType::CONTROL_MASK
                        | gdk::ModifierType::ALT_MASK
                        | gdk::ModifierType::SUPER_MASK,
                )
            {
                nav.search.grab_focus();
                return glib::Propagation::Stop;
            }

            if key != gdk::Key::space {
                return glib::Propagation::Proceed;
            }
            let Some(flow) = controller.widget().and_downcast::<gtk::FlowBox>() else {
                return glib::Propagation::Proceed;
            };
            let Some(child) = flow.focus_child() else {
                return glib::Propagation::Proceed;
            };
            match find_check(&child) {
                Some(check) => {
                    check.set_active(!check.is_active());
                    glib::Propagation::Stop
                }
                None => glib::Propagation::Proceed,
            }
        });
        ui.flow.add_controller(keys);
    }
    {
        // Moving the pointer hands control back to it.
        let motion = gtk::EventControllerMotion::new();
        let nav = Rc::clone(&ui);
        motion.connect_motion(move |_, x, y| {
            nav.leave_keyboard_nav();
            // Where in the window, and not where on the grid: that changes
            // when the grid scrolls under a pointer nobody has touched.
            let on_the_grid = gtk::graphene::Point::new(x as f32, y as f32);
            let in_the_window = nav.flow.compute_point(&nav.window, &on_the_grid);
            nav.pointer_seen(in_the_window.map(|at| (f64::from(at.x()), f64::from(at.y()))));
        });
        ui.flow.add_controller(motion);
    }
    {
        // And so does turning the wheel, which moves the cards under it.
        // Seen here and passed on: the scrolling is not this one's to do.
        let wheel =
            gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::BOTH_AXES);
        let nav = Rc::clone(&ui);
        wheel.connect_scroll(move |_, _, _| {
            nav.pointer_chooses();
            glib::Propagation::Proceed
        });
        ui.flow.add_controller(wheel);
    }
    {
        let ui = Rc::clone(&ui);
        commit_search.connect_clicked(move |_| ui.open_commit_search());
    }
    {
        let ui = Rc::clone(&ui);
        settings.connect_clicked(move |_| ui.open_settings());
    }
    {
        // The tag list changes under this button, so the popover is rebuilt
        // every time it opens rather than kept around.
        let ui = Rc::clone(&ui);
        tags_button.set_create_popup_func(move |button| {
            button.set_popover(Some(&ui.tag_popover()));
        });
    }
    {
        // Type to narrow, Enter to open the top hit.
        let ui = Rc::clone(&ui);
        search.connect_activate(move |_| {
            // Not `shown`. The grid follows the text through `search-changed`,
            // which the entry holds back for 150 ms after the last keystroke;
            // type a name and press Enter in one motion and the grid has not
            // caught up, so its first card is whatever sorted first before
            // the filter applied. Ask for the top hit of what is typed now.
            let info = ui.visible().into_iter().next();
            if let Some(info) = info {
                ui.open_graph(&info);
            }
        });
    }
    {
        let ui = Rc::clone(&ui);
        search.connect_stop_search(move |entry| {
            entry.set_text("");
            ui.rebuild();
        });
    }
    {
        // While the keyboard is in the filter, the grid shows which card
        // Enter would open: the stylesheet does it, told by this class.
        let typing = gtk::EventControllerFocus::new();
        let flow = ui.flow.clone();
        typing.connect_enter(move |_| flow.add_css_class("om-typing"));
        let flow = ui.flow.clone();
        typing.connect_leave(move |_| flow.remove_css_class("om-typing"));
        search.add_controller(typing);
    }
    {
        // Down from the filter moves into the grid, the way a launcher does:
        // the filter is where the keyboard starts, and the cards must not be
        // a Tab hunt away from it.
        let ui = Rc::clone(&ui);
        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed(move |_, key, _, state| {
            let down = key == gdk::Key::Down || key == gdk::Key::KP_Down;
            if !down || !ui::unmodified(state) || ui.flow.child_at_index(0).is_none() {
                return glib::Propagation::Proceed;
            }
            ui.focus_grid(true);
            glib::Propagation::Stop
        });
        search.add_controller(keys);
    }
    {
        let ui = Rc::clone(&ui);
        sort.connect_selected_notify(move |_| ui.rebuild());
    }
    {
        let ui = Rc::clone(&ui);
        dirty_only.connect_toggled(move |_| ui.rebuild());
    }

    let act_refresh = gio::SimpleAction::new("refresh", None);
    {
        let ui = Rc::clone(&ui);
        act_refresh.connect_activate(move |_, _| {
            // The key means "read again what I am looking at". With a graph
            // in front that is the graph: it used to rescan the grid behind
            // it and leave the page, which had no reload of its own, as it
            // was. A page with nothing to read again leaves the key alone.
            if ui.on_grid() {
                ui.start_scan();
            } else if let Some(page) = ui.nav.visible_page() {
                let _ = page.activate_action("page.reload", None);
            }
        });
    }
    window.add_action(&act_refresh);

    let act_fetch = gio::SimpleAction::new("fetch", None);
    {
        let ui = Rc::clone(&ui);
        act_fetch.connect_activate(move |_, _| ui.run_refresh(true));
    }
    window.add_action(&act_fetch);

    let act_contrib = gio::SimpleAction::new("contributions", None);
    {
        let ui = Rc::clone(&ui);
        act_contrib.connect_activate(move |_, _| {
            // From the grid only, like the button it stands for: pressed on a
            // page, it stacked another page on top built from a grid the user
            // could not see.
            if !ui.on_grid() {
                return;
            }
            let (repos, scope) = ui.contribution_scope();
            ui.open_contributions(repos, scope);
        });
    }
    window.add_action(&act_contrib);

    let act_select_all = gio::SimpleAction::new("select-all", None);
    {
        let ui = Rc::clone(&ui);
        act_select_all.connect_activate(move |_, _| ui.select_all_shown());
    }
    window.add_action(&act_select_all);

    let act_search_commits = gio::SimpleAction::new("search-commits", None);
    {
        let ui = Rc::clone(&ui);
        act_search_commits.connect_activate(move |_, _| {
            // The grid searches what it shows or what is ticked. A page that
            // is of some repositories searches those: Contributions its own,
            // a commit graph its one. Any other page leaves the key alone.
            if ui.on_grid() {
                ui.open_commit_search();
            } else if let Some(page) = ui.nav.visible_page() {
                let _ = page.activate_action("page.search-commits", None);
            }
        });
    }
    window.add_action(&act_search_commits);

    let act_keys = gio::SimpleAction::new("keyboard", None);
    {
        let ui = Rc::clone(&ui);
        act_keys.connect_activate(move |_, _| {
            ui::shortcuts::dialog().present(Some(&ui.window));
        });
    }
    window.add_action(&act_keys);

    let act_focus_grid = gio::SimpleAction::new("focus-grid", None);
    {
        let ui = Rc::clone(&ui);
        act_focus_grid.connect_activate(move |_, _| {
            // Bring the grid up to what is typed before stepping into it, for
            // the same reason Enter in the filter does not trust `shown`.
            ui.rebuild();
            ui.focus_grid(true);
        });
    }
    window.add_action(&act_focus_grid);

    let act_escape = gio::SimpleAction::new("escape", None);
    {
        let ui = Rc::clone(&ui);
        act_escape.connect_activate(move |_, _| ui.on_escape());
    }
    window.add_action(&act_escape);

    let act_clear = gio::SimpleAction::new("clear-selection", None);
    {
        let ui = Rc::clone(&ui);
        act_clear.connect_activate(move |_, _| {
            if !ui.on_grid() {
                return;
            }
            ui.selected.borrow_mut().clear();
            ui.rebuild();
        });
    }
    window.add_action(&act_clear);

    let act_settings = gio::SimpleAction::new("settings", None);
    {
        let ui = Rc::clone(&ui);
        act_settings.connect_activate(move |_, _| ui.open_settings());
    }
    window.add_action(&act_settings);

    let act_search = gio::SimpleAction::new("search", None);
    {
        let ui = Rc::clone(&ui);
        act_search.connect_activate(move |_, _| {
            // The filter belongs to the grid. With another page in front it is
            // out of sight, and focusing it would send the keyboard somewhere
            // the user cannot see what they are typing.
            if !ui.on_grid() {
                return;
            }
            ui.search.grab_focus();
        });
    }
    window.add_action(&act_search);

    {
        // The grid's own actions are switched off while another page is in
        // front, and every action but Escape while a dialog is up. Their
        // handlers check as well, but a switched-off action does not take its
        // key, and that is what gives Ctrl+A back to the diff and to a field
        // in Settings: an accelerator runs ahead of the focused widget, so
        // select-all in a diff ticked the whole fleet behind the page and
        // selected nothing.
        let grid_only = [
            act_search.clone(),
            act_select_all.clone(),
            act_clear.clone(),
            act_focus_grid.clone(),
            act_contrib.clone(),
        ];
        let anywhere = [
            act_search_commits.clone(),
            act_refresh.clone(),
            act_fetch.clone(),
            act_settings.clone(),
            act_keys.clone(),
        ];
        let sync: Rc<dyn Fn()> = {
            let ui = Rc::clone(&ui);
            Rc::new(move || {
                let dialog = ui.window.visible_dialog().is_some();
                let on_grid = ui.on_grid();
                for action in &grid_only {
                    action.set_enabled(on_grid && !dialog);
                }
                for action in &anywhere {
                    action.set_enabled(!dialog);
                }
            })
        };
        {
            let sync = Rc::clone(&sync);
            let ui = Rc::clone(&ui);
            window.connect_visible_dialog_notify(move |window| {
                sync();
                if window.visible_dialog().is_none() {
                    ui.keyboard_after_dialog();
                }
            });
        }
        let ui = Rc::clone(&ui);
        nav.connect_visible_page_notify(move |_| {
            sync();
            let on_grid = ui.on_grid();
            if on_grid {
                ui.read_again(true);
                // After the view has put the focus back where it remembers it.
                // That is the card the page was opened from, unless the grid
                // was built again while it was out of sight: the card it
                // remembers is then gone, and the keyboard came back on the
                // first button of the header.
                let ui = Rc::clone(&ui);
                glib::idle_add_local_once(move || ui.back_on_the_grid());
            }
        });
    }
    {
        // Coming back to the window: whatever was handed to a terminal has
        // had its chance to change.
        let ui = Rc::clone(&ui);
        window.connect_is_active_notify(move |window| {
            if window.is_active() {
                ui.read_again(true);
            }
        });
    }

    app.set_accels_for_action("win.refresh", &["<Ctrl>r", "F5"]);
    // Not a bare `slash` as well. An accelerator runs ahead of whatever has
    // the focus, so with `/` bound here one could not be typed anywhere in
    // the application: not a path in a commit message, not a branch name in
    // the commit search. It is handled by the grid, beside `?`.
    app.set_accels_for_action("win.search", &["<Ctrl>f"]);
    app.set_accels_for_action("win.contributions", &["<Ctrl>g"]);
    app.set_accels_for_action("win.settings", &["<Ctrl>comma"]);
    app.set_accels_for_action("win.fetch", &["<Ctrl><Shift>r"]);
    app.set_accels_for_action("win.select-all", &["<Ctrl>a"]);
    app.set_accels_for_action("win.escape", &["Escape"]);
    app.set_accels_for_action("win.search-commits", &["<Ctrl><Shift>f"]);
    app.set_accels_for_action("win.focus-grid", &["<Ctrl>l"]);
    // F1 only as an accelerator: binding `?` here would fire while typing one
    // into the filter box, since accelerators run ahead of the focused widget.
    app.set_accels_for_action("win.keyboard", &["F1"]);
    app.set_accels_for_action("win.clear-selection", &["<Ctrl><Shift>a"]);

    // At narrow widths the two labelled header buttons crowd out the title,
    // so below this width they fall back to their icons and the filter box
    // gives up a third of its width. The labels used to stay until 920, and
    // for the ninety pixels between that and what the header needs, which is
    // where half of a 1920 screen falls, the window was narrower than its own
    // header.
    // Where it lies is set by `fit_header`, which knows what is in the
    // header; until the first scan has said which tags there are, it is
    // where it would be with none.
    let breakpoint = adw::Breakpoint::new(below(HEADER_FULL + SPARE));
    breakpoint.add_setter(&ui.search, "width-request", Some(&160.to_value()));
    {
        // The labels are not given to the breakpoint as setters. The grid
        // writes them itself on every rebuild ("Contributions (3)", "Tags
        // (1)"), which put them straight back on a narrow window the first
        // time anything was typed in the filter. They are written in one
        // place, which is told how much room there is.
        let ui_for_apply = Rc::clone(&ui);
        breakpoint.connect_apply(move |_| {
            ui_for_apply.narrow.set(true);
            ui_for_apply.update_selection_label();
            ui_for_apply.sync_tag_controls();
        });
        let ui_for_unapply = Rc::clone(&ui);
        breakpoint.connect_unapply(move |_| {
            ui_for_unapply.narrow.set(false);
            ui_for_unapply.update_selection_label();
            ui_for_unapply.sync_tag_controls();
        });
    }
    window.add_breakpoint(breakpoint.clone());

    // Narrower still, and the sort order and the tag chips go too: both are
    // one click away elsewhere (the Tags menu filters), and with them the
    // header fits a third of a screen. Only the last breakpoint that matches
    // is applied, so this one says again what the wider one says.
    let cramped = adw::Breakpoint::new(below(HEADER_BARE + SPARE));
    cramped.add_setter(&ui.search, "width-request", Some(&120.to_value()));
    cramped.add_setter(&ui.sort, "visible", Some(&false.to_value()));
    // Not the tag chips, though they go too. A breakpoint puts back what it
    // found when it lets go, and what it found was the chips hidden, as they
    // are before the first scan has said which tags there are: a window that
    // was ever this narrow, at launch for an instant, had its chips hidden
    // for good. `sync_tag_controls` decides, and is told how wide things are.
    {
        let ui_for_apply = Rc::clone(&ui);
        cramped.connect_apply(move |_| {
            ui_for_apply.narrow.set(true);
            ui_for_apply.cramped.set(true);
            ui_for_apply.update_selection_label();
            ui_for_apply.sync_tag_controls();
        });
        let ui_for_unapply = Rc::clone(&ui);
        cramped.connect_unapply(move |_| {
            ui_for_unapply.narrow.set(false);
            ui_for_unapply.cramped.set(false);
            ui_for_unapply.update_selection_label();
            ui_for_unapply.sync_tag_controls();
        });
    }
    ui.breakpoints.replace(Some([breakpoint, cramped.clone()]));
    window.add_breakpoint(cramped);

    if let Some(content) = window.content() {
        // The content is what a change of breakpoint takes off screen and
        // puts back, and the keyboard is seen across that by hand.
        let leaving = Rc::clone(&ui);
        content.connect_unmap(move |_| leaving.note_keyboard());
        let back = Rc::clone(&ui);
        content.connect_map(move |_| back.give_keyboard_back());
    }

    let act_updated = gio::SimpleAction::new("updated-on-disk", None);
    {
        // Sent by a second launch that found this window already open, when
        // the program has been replaced on disk since this one started.
        let ui = Rc::clone(&ui);
        act_updated.connect_activate(move |_, _| {
            ui::complain(
                &ui.toasts,
                "omarepos has been reinstalled since this window opened, and this is still \
                 the old one. Quit (Ctrl+Q) and start it again for the new.",
            );
        });
    }
    window.add_action(&act_updated);

    let act_launched = gio::SimpleAction::new("launched-again", None);
    {
        // Sent by every launch that found this window already open. Whoever
        // pressed the launcher key is about to type a repository's name,
        // and cannot tell from the key whether the window was there before.
        let ui = Rc::clone(&ui);
        act_launched.connect_activate(move |_, _| ui.focus_filter());
    }
    window.add_action(&act_launched);

    ui.sync_trouble();
    // A new window means the application is not closing after all.
    crate::gitcmd::wind_down(false);
    {
        // Closing the window stops what git is doing, and does it properly.
        // A fetch under way is told to stop and the pass across the fleet is
        // called off: neither loses anything, and either could keep a closed
        // application alive for an hour. The window goes at once; the
        // application stays, without one, for the moment git takes to stop
        // and tidy its lock files away.
        let ui = Rc::clone(&ui);
        let app = app.clone();
        window.connect_close_request(move |_| {
            ui.cancel_refresh();
            if crate::gitcmd::busy() {
                crate::gitcmd::wind_down(true);
                let mut held = Some(app.hold());
                glib::timeout_add_local(std::time::Duration::from_millis(100), move || {
                    if crate::gitcmd::busy() {
                        return glib::ControlFlow::Continue;
                    }
                    // Letting go of the hold is what ends the application,
                    // unless a window has been opened meanwhile.
                    held.take();
                    glib::ControlFlow::Break
                });
            }
            glib::Propagation::Proceed
        });
    }

    #[cfg(debug_assertions)]
    ui::debug::install(&window);

    ui.apply_theme();
    ui.watch_theme();
    {
        // With no Omarchy theme the window follows the desktop's light or
        // dark, and so has to hear when the desktop changes it. Not in the
        // middle of being told: restyling sets the colour scheme, which is
        // what says this.
        let ui = Rc::clone(&ui);
        adw::StyleManager::default().connect_dark_notify(move |manager| {
            if crate::theme::has_omarchy_theme() || ui.light.get() != manager.is_dark() {
                return;
            }
            let ui = Rc::clone(&ui);
            glib::idle_add_local_once(move || ui.apply_theme());
        });
    }
    ui.schedule_refresh();
    ui.warm_start();
    ui.start_scan();
    // Before the window is shown: a window shown with nothing focused picks
    // the first button in its header.
    ui.focus_filter();
    window.present();
}

impl Ui {
    /// Recompile the stylesheet from the active Omarchy palette, or, where
    /// there is none, from the built-in one for the desktop's light or dark.
    fn apply_theme(self: &Rc<Self>) {
        let manager = adw::StyleManager::default();
        let omarchy = crate::theme::has_omarchy_theme();
        if !omarchy {
            // Not Omarchy: the desktop's own light or dark, as any libadwaita
            // app has it, and the built-in palette to match. It was the dark
            // one on every desktop.
            manager.set_color_scheme(adw::ColorScheme::Default);
            crate::theme::set_desktop_dark(manager.is_dark());
        }
        let palette = Palette::load();
        self.provider.load_from_string(&palette.stylesheet());
        if omarchy {
            manager.set_color_scheme(if palette.dark {
                adw::ColorScheme::ForceDark
            } else {
                adw::ColorScheme::ForceLight
            });
        }
        *self.theme_name.borrow_mut() = palette.name.clone();
        self.sync_subtitle();
        // The lane and tag colours come in a set for each kind of background.
        // The dots on the cards are drawn once, when the card is built, so
        // the grid is built again when the kind has changed.
        let was_light = self.light.replace(!palette.dark);
        ui::accents_for(palette.dark);
        if was_light == palette.dark && !self.repos.borrow().is_empty() {
            self.redraw_cards();
        }
    }

    /// The line under the title: where the grid comes from, and the theme. Its
    /// own function because the roots change in Settings, where the theme
    /// does not, and the line went on naming a root that had been removed
    /// until the theme next changed.
    fn sync_subtitle(&self) {
        let roots = self.cfg.borrow().root_labels().join("  ");
        self.subtitle
            .set_label(&format!("{}  ·  {}", roots, self.theme_name.borrow()));
    }

    /// Restyle whenever Omarchy's current-theme directory changes.
    fn watch_theme(self: &Rc<Self>) {
        let dir = crate::theme::state_dir();
        let file = gio::File::for_path(&dir);
        let Ok(monitor) = file.monitor_directory(
            gio::FileMonitorFlags::WATCH_MOVES,
            gio::Cancellable::NONE,
        ) else {
            return;
        };
        let ui = Rc::clone(self);
        monitor.connect_changed(move |_, _, _, _| {
            // The switch is read here, not at install time, so flipping it in
            // settings takes effect without a restart.
            if ui.cfg.borrow().follow_omarchy_theme {
                ui.apply_theme();
            }
        });
        self.monitors.borrow_mut().push(monitor);
    }

    /// Say, for as long as it is so, that a file the app keeps cannot be read.
    /// A banner and not a toast: the app is running without the file and will
    /// not save over it, and a toast that came and went would leave that to be
    /// found out later.
    fn sync_trouble(&self) {
        let unusable: Vec<String> = [
            self.cfg.borrow().load_error.clone(),
            self.meta.borrow().load_error.clone(),
        ]
        .into_iter()
        .flatten()
        .collect();
        if !unusable.is_empty() {
            self.trouble.set_title(&format!(
                "Could not read {}. Nothing will be saved over it. Fix the file, then \
                 rescan (Ctrl+R).",
                unusable.join("; ")
            ));
        }
        self.trouble.set_revealed(!unusable.is_empty());
    }

    /// Take up an edit made to the config file or the tag file since it was
    /// read. The README offers the files as a way to configure the app, and
    /// until now only a restart read them: Ctrl+R scanned the roots held in
    /// memory, and the next save put the launch-time copy back over the edit.
    fn reread_config(self: &Rc<Self>) {
        // Not while something up the stack is holding the settings.
        let Ok(mut cfg) = self.cfg.try_borrow_mut() else { return };
        let followed = cfg.follow_omarchy_theme;
        let outcome = cfg.reread();
        let follows = cfg.follow_omarchy_theme;
        drop(cfg);
        match outcome {
            crate::config::Reread::Unchanged => {}
            crate::config::Reread::Loaded => {
                self.sync_trouble();
                self.schedule_refresh();
                if follows != followed {
                    self.apply_theme();
                }
                self.sync_subtitle();
                self.toasts.add_toast(ui::toast("Read config.toml again"));
            }
            // The banner has the reason.
            crate::config::Reread::Broken(_) => self.sync_trouble(),
        }
        // And the tags, which are a file to edit as much as the settings are.
        let Ok(mut meta) = self.meta.try_borrow_mut() else { return };
        let outcome = meta.reread();
        drop(meta);
        match outcome {
            crate::config::Reread::Unchanged => {}
            crate::config::Reread::Loaded => {
                self.sync_trouble();
                self.toasts.add_toast(ui::toast("Read repos.toml again"));
            }
            crate::config::Reread::Broken(_) => self.sync_trouble(),
        }
    }

    fn update_status(&self) {
        let repos = self.repos.borrow();
        let total = repos.len();
        let dirty = repos.iter().filter(|r| !r.tree.is_clean()).count();
        let ahead = repos.iter().filter(|r| r.ahead > 0 || r.unpushed > 0).count();
        let behind = repos.iter().filter(|r| r.behind > 0).count();
        let broken = repos.iter().filter(|r| r.error.is_some()).count();

        let mut parts = vec![format!("{total} repos")];
        if self.refreshing.get() {
            let (done, of) = self.refresh_progress.get();
            parts = vec![format!("fetching {done}/{of}")];
        } else if self.scanning.get() {
            let expected = self.expected.get();
            if expected > 0 {
                parts = vec![format!("scanning {}/{expected}", self.scanned.get())];
            }
        }
        parts.extend(self.earlier_part());
        parts.push(format!("{dirty} dirty"));
        parts.push(format!("{ahead} unpushed"));
        parts.push(format!("{behind} behind"));
        if broken > 0 {
            parts.push(format!("{broken} unreadable"));
        }
        // A root that gave nothing, or less than it holds, says so here. It
        // used to say nothing anywhere: a folder that had been moved simply
        // took its repositories off the grid and out of every fetch.
        parts.extend(self.roots_amiss());
        {
            // Of the repositories on the grid: the record can hold one the
            // grid has since lost.
            let errors = self.fetch_errors.borrow();
            let failed = repos
                .iter()
                .filter(|repo| errors.contains_key(&repo.path.display().to_string()))
                .count();
            if failed > 0 {
                parts.push(format!("{failed} not fetched"));
            }
        }
        let shown = self.flow.observe_children().n_items();
        if (shown as usize) != total {
            parts.push(format!("{shown} shown"));
        }
        let selected = self.selected.borrow().len();
        if selected > 0 {
            parts.push(format!("{selected} selected"));
        }
        let cfg = self.cfg.borrow();
        let settings = &cfg.refresh;
        let only: Vec<&str> = settings
            .only_tags
            .iter()
            .map(|tag| tag.trim())
            .filter(|tag| !tag.is_empty())
            .collect();
        if let Some(when) = self.last_refresh.get() {
            // The stamp is the last pass, and a pass narrowed to some tags
            // says nothing about the rest of the grid.
            parts.push(if only.is_empty() {
                format!("fetched {}", ui::relative_time(when))
            } else {
                format!("fetched {} (tagged {})", ui::relative_time(when), only.join(", "))
            });
        }
        if cfg.one_off() {
            parts.push("one-off look: no background refresh".to_string());
        } else if settings.enabled() {
            let what = if settings.fetch { "fetch" } else { "rescan" };
            // A scope that matches nothing makes the timer a no-op, and the
            // bar went on promising a fetch every few minutes. Not asked
            // while a scan is under way: the grid is not all there yet.
            let scoped_to_nothing = settings.will_fetch()
                && !only.is_empty()
                && !self.scanning.get()
                && !repos.is_empty()
                && self.refresh_scope().is_empty();
            parts.push(if scoped_to_nothing {
                format!("auto {what} idle: nothing is tagged {}", only.join(", "))
            } else {
                format!("auto {what} every {}m", settings.every_minutes)
            });
        }
        let tag_filter = self.tag_filter.borrow();
        if !tag_filter.is_empty() {
            parts.push(format!(
                "tagged {}",
                tag_filter.iter().cloned().collect::<Vec<_>>().join(", ")
            ));
        } else if self.untagged_only.get() {
            parts.push("untagged only".to_string());
        }
        let line = parts.join("  ·  ");
        // The line is cut where the window ends; the tooltip has all of it.
        self.status.set_tooltip_text(Some(&line.replace("  ·  ", "\n")));
        self.status.set_label(&line);
    }

    /// What the last scan has to say about roots that did not simply give
    /// their repositories: one phrase each, led by the root.
    fn roots_amiss(&self) -> Vec<String> {
        self.root_reports
            .borrow()
            .iter()
            .filter_map(|report| {
                let amiss = report.says()?;
                Some(format!("{}: {amiss}", crate::config::tildify(Path::new(&report.root))))
            })
            .collect()
    }

    /// Escape backs out of whatever is in the way, nearest first: a dialog or
    /// a menu, the filter you are typing in, then the page you are on, then
    /// the selection, then a filter left in the box.
    ///
    /// Binding Escape on the window shadows the navigation view's own, so
    /// going back has to be handled here or it stops working at all.
    fn on_escape(self: &Rc<Self>) {
        // A dialog first, and a popover. Escape is bound on the window, which
        // gets it before they do: with the keyboard list or Settings up, it
        // left the page behind the dialog, or cleared the selection, and the
        // dialog stayed where it was.
        if let Some(dialog) = self.window.visible_dialog() {
            dialog.close();
            return;
        }
        if let Some(focused) = gtk::prelude::GtkWindowExt::focus(&self.window) {
            if let Some(popover) = focused
                .ancestor(gtk::Popover::static_type())
                .and_downcast::<gtk::Popover>()
            {
                popover.popdown();
                return;
            }
            // The arcade on Contributions, where Escape is game over, and
            // the page stays.
            if focused.activate_action("arcade.game-over", None).is_ok() {
                return;
            }
            if let Ok(editable) = focused.dynamic_cast::<gtk::Editable>() {
                if !editable.text().is_empty() {
                    editable.set_text("");
                    return;
                }
            }
        }

        if !self.on_grid() {
            self.nav.pop();
            return;
        }

        if !self.selected.borrow().is_empty() {
            self.selected.borrow_mut().clear();
            self.rebuild();
            return;
        }

        if !self.search.text().is_empty() {
            self.search.set_text("");
        }
    }

    /// Tick every repository the grid is currently showing, or untick them all
    /// when they are already ticked. Toggling means the selection can be
    /// undone with the key that made it, without reaching for a chord.
    ///
    /// An accelerator fires ahead of the focused widget, so Ctrl+A in the
    /// filter box would otherwise stop selecting its text. When the focus is
    /// somewhere typable it keeps its usual meaning.
    fn select_all_shown(self: &Rc<Self>) {
        if let Some(focused) = gtk::prelude::GtkWindowExt::focus(&self.window) {
            if let Ok(editable) = focused.dynamic_cast::<gtk::Editable>() {
                editable.select_region(0, -1);
                return;
            }
        }
        if !self.on_grid() {
            return;
        }

        let shown: Vec<PathBuf> = self
            .shown
            .borrow()
            .iter()
            .map(|repo| repo.path.clone())
            .collect();
        if shown.is_empty() {
            return;
        }
        let all_ticked = {
            let selected = self.selected.borrow();
            shown.iter().all(|path| selected.contains(path))
        };
        {
            let mut selected = self.selected.borrow_mut();
            for path in shown {
                if all_ticked {
                    selected.remove(&path);
                } else {
                    selected.insert(path);
                }
            }
        }
        self.rebuild();
    }

    fn open_settings(self: &Rc<Self>) {
        let rescan = {
            let ui = Rc::clone(self);
            Rc::new(move || {
                // What the pass in flight was started for no longer holds.
                ui.cancel_refresh();
                ui.schedule_refresh();
                ui.sync_subtitle();
                ui.start_scan();
            }) as Rc<dyn Fn()>
        };
        let reschedule = {
            let ui = Rc::clone(self);
            // Refresh settings move nothing on disk, so there is no rescan.
            // The cards are drawn again from what is already in memory.
            Rc::new(move || {
                // A pass under way was started with the settings as they
                // were. Switching fetching off, or the refresh off, while
                // the status bar reads "fetching 3/44" has to mean something
                // for the other forty-one; the next pass reads the settings
                // afresh.
                ui.cancel_refresh();
                ui.schedule_refresh();
                ui.rebuild();
            }) as Rc<dyn Fn()>
        };
        let restyle = {
            let ui = Rc::clone(self);
            Rc::new(move || ui.apply_theme()) as Rc<dyn Fn()>
        };
        // Both read before the call, each in a statement of its own. Written
        // as arguments, the borrow of the settings lived until the call
        // returned, and the dialog's first act is to write to them: opening
        // Settings ended the program.
        //
        // A look at somewhere else has no business forgetting the fleet's
        // tags, whatever is removed while it is open.
        let one_off = self.cfg.borrow().one_off();
        let reports = self.root_reports.borrow().clone();
        ui::dialogs::settings(
            &self.window,
            Rc::clone(&self.cfg),
            (!one_off).then(|| Rc::clone(&self.meta)),
            reports,
            rescan,
            reschedule,
            restyle,
        );
    }

    fn update_selection_label(&self) {
        // The rest of the grid steps back only when a ticked card is on it to
        // stand out from. With every ticked repository filtered out of sight
        // the whole grid went dim, for no reason anyone could see.
        let on_show = {
            let selected = self.selected.borrow();
            self.shown.borrow().iter().any(|repo| selected.contains(&repo.path))
        };
        let selected = self.selected.borrow().len();
        if on_show {
            self.flow.add_css_class("om-has-selection");
        } else {
            self.flow.remove_css_class("om-has-selection");
        }
        self.contrib_content.set_label(&match (self.narrow.get(), selected) {
            (true, 0) => String::new(),
            (true, n) => n.to_string(),
            (false, 0) => "Contributions".to_string(),
            (false, n) => format!("Contributions ({n})"),
        });
    }

    /// Which repos a contributions run covers: the ticked ones, or everything
    /// currently on screen when nothing is ticked.
    fn contribution_scope(&self) -> (Vec<PathBuf>, String) {
        let selected = self.selected.borrow();
        if !selected.is_empty() {
            let mut repos: Vec<PathBuf> = selected.iter().cloned().collect();
            repos.sort();
            let n = repos.len();
            return (repos, format!("{n} selected {}", noun(n, "repo", "repos")));
        }
        drop(selected);

        let visible = self.visible();
        let n = visible.len();
        let repos: Vec<PathBuf> = visible.into_iter().map(|r| r.path).collect();
        let filtered = !self.search.text().trim().is_empty() || self.dirty_only.is_active();
        let scope = if filtered {
            format!("{n} filtered {}", noun(n, "repo", "repos"))
        } else {
            format!("all {n} {}", noun(n, "repo", "repos"))
        };
        (repos, scope)
    }

    /// Remembering whether the detail pane is open, so it is not re-opened on
    /// every graph after being closed once.
    fn detail_prefs(self: &Rc<Self>) -> (bool, Rc<dyn Fn(bool)>) {
        let open = self.cfg.borrow().commit_detail;
        let ui = Rc::clone(self);
        let remember: Rc<dyn Fn(bool)> = Rc::new(move |open| {
            if ui.cfg.borrow().commit_detail == open {
                return;
            }
            ui.cfg.borrow_mut().commit_detail = open;
            if let Err(err) = ui.cfg.borrow().save() {
                // Said in the window: one reason is that the file was edited
                // elsewhere, and that wants doing something about.
                ui::complain(&ui.toasts, &format!("The pane's setting was {err}"));
            }
        });
        (open, remember)
    }

    fn open_graph(self: &Rc<Self>, info: &RepoInfo) {
        if let Some(err) = &info.error {
            self.toasts
                .add_toast(ui::toast(&format!("{}: {err}", info.name)));
            return;
        }
        self.accept(&info.path);
        *self.opened.borrow_mut() = Some(info.path.clone());
        self.nav.push(&ui::graph::page(
            &info.path,
            &info.name,
            None,
            self.graph_host(&info.path),
        ));
    }

    /// Search commits across the ticked repositories, or everything shown.
    fn open_commit_search(self: &Rc<Self>) {
        let (repos, scope) = self.contribution_scope();
        self.search_commits(repos, scope);
    }

    /// How a page that is of some repositories searches their commits.
    fn commit_searcher(self: &Rc<Self>) -> ui::stats::OnSearch {
        let ui = Rc::clone(self);
        Rc::new(move |repos, scope| ui.search_commits(repos, scope))
    }

    /// How Contributions runs the race over what it is showing.
    fn race_runner(self: &Rc<Self>) -> ui::stats::OnRace {
        let ui = Rc::clone(self);
        Rc::new(move |field| ui.nav.push(&ui::race::page(field)))
    }

    fn search_commits(self: &Rc<Self>, repos: Vec<PathBuf>, scope: String) {
        if repos.is_empty() {
            self.toasts
                .add_toast(ui::toast("No repositories to search"));
            return;
        }
        let (page, query) = ui::search::page(repos, scope, self.commit_opener());
        self.nav.push(&page);
        // Now, not when the slide has finished: see `ui::search::page`.
        query.grab_focus();
    }

    /// How a page that lists commits (search, contributions) opens one: the
    /// graph of its repository, on that commit.
    fn commit_opener(self: &Rc<Self>) -> Rc<dyn Fn(PathBuf, String, Option<String>)> {
        let ui = Rc::clone(self);
        Rc::new(move |path: PathBuf, name: String, commit: Option<String>| {
            ui.accept(&path);
            ui.nav
                .push(&ui::graph::page(&path, &name, commit, ui.graph_host(&path)));
        })
    }

    /// What a graph page of the repository at `path` reaches the window with.
    fn graph_host(self: &Rc<Self>, path: &Path) -> ui::graph::Host {
        let (detail_open, on_detail_toggled) = self.detail_prefs();
        let ui = Rc::clone(self);
        let on_contributions: Rc<dyn Fn(PathBuf)> = Rc::new(move |path: PathBuf| {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| path.display().to_string());
            ui.nav.push(&ui::stats::page(vec![path], name, ui.commit_opener(), ui.commit_searcher(), ui.race_runner()));
        });
        let on_search: Rc<dyn Fn()> = {
            let ui = Rc::clone(self);
            let path = path.to_path_buf();
            Rc::new(move || {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| path.display().to_string());
                ui.search_commits(vec![path.clone()], name);
            })
        };
        let on_fetch: Rc<dyn Fn(Rc<dyn Fn(bool)>)> = {
            let ui = Rc::clone(self);
            let path = path.to_path_buf();
            Rc::new(move |done| ui.fetch_one(path.clone(), false, Some(done)))
        };
        ui::graph::Host {
            detail_open,
            on_detail_toggled,
            on_terminal: self.terminal_opener(path),
            on_editor: self.editor_opener(path),
            web: self.web_page(path).ok(),
            on_web: {
                let ui = Rc::clone(self);
                let path = path.to_path_buf();
                Rc::new(move || ui.open_web(&path))
            },
            on_contributions,
            on_search,
            on_fetch,
            toasts: self.toasts.clone(),
        }
    }

    /// Whether the grid is the page in front. Asked of the page, not of the
    /// depth of the stack: on the way back from a page the view names the
    /// grid as visible before it has finished letting go of the other.
    fn on_grid(&self) -> bool {
        self.nav.visible_page().as_ref() == Some(&self.grid_page)
    }

    /// Opening a terminal needs the configured terminal and runner, which the
    /// graph page has no business knowing about.
    ///
    /// Every way a repository is handed to a terminal or an editor comes
    /// through these two, which is what lets the grid know to look at it again
    /// afterwards: `l` opens lazygit, and a commit made there left the card
    /// saying what it said before.
    fn terminal_opener(self: &Rc<Self>, path: &Path) -> Rc<dyn Fn()> {
        let ui = Rc::clone(self);
        let path = path.to_path_buf();
        Rc::new(move || {
            ui.handed.borrow_mut().insert(path.clone());
            if let Err(why) = ui::open_terminal(&ui.cfg.borrow(), &path) {
                ui::complain(&ui.toasts, &why);
            }
        })
    }

    fn editor_opener(self: &Rc<Self>, path: &Path) -> Rc<dyn Fn()> {
        let ui = Rc::clone(self);
        let path = path.to_path_buf();
        Rc::new(move || {
            ui.handed.borrow_mut().insert(path.clone());
            if let Err(why) = ui::open_editor(&ui.cfg.borrow(), &path) {
                ui::complain(&ui.toasts, &why);
            }
        })
    }

    /// The page on the web of the repository at `path`, or the sentence that
    /// says why there is none to open.
    fn web_page(&self, path: &Path) -> Result<String, String> {
        let repos = self.repos.borrow();
        let Some(info) = repos.iter().find(|repo| repo.path == path) else {
            return Err(format!("{} is not among the repositories any more", path.display()));
        };
        match (&info.web, &info.remote) {
            (Some(page), _) => Ok(page.clone()),
            (None, Some(remote)) => Err(format!(
                "{}: no web page can be worked out from its remote, {remote}",
                info.name
            )),
            (None, None) => Err(format!("{} has no remote, and so no page to open", info.name)),
        }
    }

    /// Send the browser to that page. Every way of asking comes through
    /// here — the key, the card's menu, the button on a graph page — and the
    /// page is looked up in what the window knows now: a card's menu and a
    /// graph page each hold a copy of the repository from when they were
    /// built.
    fn open_web(&self, path: &Path) {
        match self.web_page(path) {
            Ok(page) => match ui::open_web(&page) {
                Ok(()) => self.toasts.add_toast(ui::toast(&format!("Opening {page}"))),
                Err(why) => ui::complain(&self.toasts, &why),
            },
            Err(why) => self.toasts.add_toast(ui::toast(&why)),
        }
    }

    fn open_contributions(self: &Rc<Self>, repos: Vec<PathBuf>, scope: String) {
        if repos.is_empty() {
            self.toasts
                .add_toast(ui::toast("No repositories to report on"));
            return;
        }
        self.nav
            .push(&ui::stats::page(repos, scope, self.commit_opener(), self.commit_searcher(), self.race_runner()));
    }

    fn run_action(self: &Rc<Self>, action: Action, info: &RepoInfo) {
        match action {
            Action::CommitGraph => self.open_graph(info),
            Action::Contributions => {
                self.open_contributions(vec![info.path.clone()], info.name.clone())
            }
            Action::EditTags => self.edit_tags(vec![info.path.clone()]),
            Action::OpenTerminal => self.terminal_opener(&info.path)(),
            Action::OpenEditor => self.editor_opener(&info.path)(),
            Action::OpenFolder => ui::open_folder(&info.path),
            Action::OpenWeb => self.open_web(&info.path),
            Action::CopyPath => {
                let path = info.path.display().to_string();
                self.window.clipboard().set_text(&path);
                self.toasts
                    .add_toast(ui::toast(&format!("Copied {path}")));
            }
        }
    }
}

/// Runs what it was given when it goes out of scope, however that happens.
///
/// glib catches a panic in a future it is running: the program goes on and
/// the future is simply gone, with whatever its last lines were going to put
/// right left as it was. For the three futures here those lines are what
/// says a scan or a fetch is over. Without them the spinner turned for good
/// and every later fetch was told "already busy".
struct OnDrop<F: FnMut()>(F);

impl<F: FnMut()> Drop for OnDrop<F> {
    fn drop(&mut self) {
        (self.0)();
    }
}

fn noun(n: usize, one: &'static str, many: &'static str) -> &'static str {
    if n == 1 { one } else { many }
}

/// "Narrower than this", as a breakpoint is told it.
fn below(width: f64) -> adw::BreakpointCondition {
    adw::BreakpointCondition::new_length(
        adw::BreakpointConditionLengthType::MaxWidth,
        width,
        adw::LengthUnit::Px,
    )
}

/// The selection checkbox of a card, found by walking down from its flow box
/// child. Cheap: a card is three levels deep and a handful of widgets wide.
fn find_check(widget: &gtk::Widget) -> Option<gtk::CheckButton> {
    if let Some(check) = widget.clone().downcast::<gtk::CheckButton>().ok() {
        return Some(check);
    }
    let mut child = widget.first_child();
    while let Some(current) = child {
        if let Some(found) = find_check(&current) {
            return Some(found);
        }
        child = current.next_sibling();
    }
    None
}
