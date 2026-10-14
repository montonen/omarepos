//! The main window: a searchable, sortable grid of repository cards.

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
use crate::refresh::{self, Summary};
use crate::git::RepoInfo;
use crate::scan::{self, Event};
use crate::theme::Palette;
use crate::ui::{self, Action, card};

/// How many tags get a button each in the header. Up to this many are one
/// click to filter by; more than this and the Tags menu lists them. It was
/// two, with the widths below fixed for two short names. They are worked out
/// from the buttons that are there now (`fit_header`), so the number is a
/// matter of taste and not of room.
const QUICK_TAGS: usize = 5;
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

const SORT_LABELS: &[&str] = &[
    "Recent activity",
    "Name",
    "Dirty first",
    "Unpushed first",
    "Tracked files",
];

#[derive(Clone, Copy, PartialEq)]
enum Sort {
    Activity,
    Name,
    Dirty,
    Unpushed,
    Files,
}

impl Sort {
    fn from_index(i: u32) -> Self {
        match i {
            1 => Sort::Name,
            2 => Sort::Dirty,
            3 => Sort::Unpushed,
            4 => Sort::Files,
            _ => Sort::Activity,
        }
    }
}

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

    let sort = gtk::DropDown::from_strings(SORT_LABELS);
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
        fetch_errors: RefCell::new(crate::config::load_fetch_errors()),
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
            if ui.on_grid() {
                ui.open_commit_search();
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
            act_search_commits.clone(),
        ];
        let anywhere = [
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
    ui.schedule_refresh();
    ui.start_scan();
    // Before the window is shown: a window shown with nothing focused picks
    // the first button in its header.
    ui.focus_filter();
    window.present();
}

impl Ui {
    /// Recompile the stylesheet from the active Omarchy palette.
    fn apply_theme(self: &Rc<Self>) {
        let palette = Palette::load();
        self.provider.load_from_string(&palette.stylesheet());
        adw::StyleManager::default().set_color_scheme(if palette.dark {
            adw::ColorScheme::ForceDark
        } else {
            adw::ColorScheme::ForceLight
        });
        *self.theme_name.borrow_mut() = palette.name.clone();
        self.sync_subtitle();
        // The lane and tag colours come in a set for each kind of background.
        // The dots on the cards are drawn once, when the card is built, so
        // the grid is built again when the kind has changed.
        let was_light = self.light.replace(!palette.dark);
        ui::accents_for(palette.dark);
        if was_light == palette.dark && !self.repos.borrow().is_empty() {
            self.rebuild();
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

    fn start_scan(self: &Rc<Self>) {
        self.reread_config();
        let generation = self.generation.get() + 1;
        self.generation.set(generation);
        self.scanning.set(true);
        self.expected.set(0);
        self.scanned.set(0);
        self.spinner.set_visible(true);
        self.spinner.start();
        self.status.set_label("Scanning…");

        // With nothing on screen there is nothing to keep, and cards arriving
        // one at a time beat an empty window. With a grid on screen the scan
        // collects off to the side and the grid is replaced once, at the end.
        // It used to be emptied first and refilled card by card: every refresh
        // blanked the window to "No repositories found", threw the keyboard
        // back to the first card and the scroll to the top, and built the grid
        // once per repository found.
        let streaming = self.repos.borrow().is_empty();
        if streaming {
            self.rebuild();
        }

        let (roots, extras, depth, one_off) = {
            let cfg = self.cfg.borrow();
            (cfg.roots(), cfg.extra_repos(), cfg.depth(), cfg.one_off())
        };
        let (rx, reports) = scan::spawn(roots, extras, depth);
        let ui = Rc::clone(self);
        glib::spawn_future_local(async move {
            // Whatever becomes of this future, the scan it stands for is over
            // when it is, unless a newer scan has taken over meanwhile.
            let _over = OnDrop({
                let ui = Rc::clone(&ui);
                move || {
                    if ui.generation.get() == generation && ui.scanning.replace(false) {
                        ui.spinner.stop();
                        ui.spinner.set_visible(ui.refreshing.get());
                        ui.update_status();
                    }
                }
            });
            // Roots that seed a tag write to the metadata store as repos come
            // in; saving once at the end keeps it to a single write.
            let mut seeded_any = false;
            let mut fresh: Vec<RepoInfo> = Vec::new();
            while let Ok(event) = rx.recv().await {
                if ui.generation.get() != generation {
                    break;
                }
                match event {
                    Event::Discovered(n) => {
                        ui.expected.set(n);
                        // Sent ahead of this event, so they are there.
                        if let Ok(found) = reports.try_recv() {
                            *ui.root_reports.borrow_mut() = found;
                        }
                        ui.update_status();
                    }
                    Event::Repo { info, seed } => {
                        // A run pointed somewhere else by the command line
                        // has no opinion about tags: its roots are not the
                        // configured ones, and "this root gives no tag" would
                        // be read as "take the tag away".
                        if let Some(seed) = seed.filter(|_| !one_off) {
                            let changed = ui.meta.borrow_mut().apply_root_tag(
                                &info.path,
                                &seed.root,
                                seed.tag.as_deref(),
                            );
                            seeded_any |= changed;
                        }
                        if streaming {
                            ui.repos.borrow_mut().push(info);
                            ui.scanned.set(ui.repos.borrow().len());
                            ui.rebuild();
                        } else {
                            fresh.push(info);
                            ui.scanned.set(fresh.len());
                            ui.update_status();
                        }
                    }
                    Event::Done => {
                        if !streaming {
                            *ui.repos.borrow_mut() = std::mem::take(&mut fresh);
                        }
                        // A tick on a repository the scan no longer finds is
                        // a tick nobody can see or take off, and it went on
                        // feeding Contributions, Search and the tag editor.
                        {
                            let found: HashSet<PathBuf> =
                                ui.repos.borrow().iter().map(|repo| repo.path.clone()).collect();
                            ui.selected.borrow_mut().retain(|path| found.contains(path));
                        }
                        // Pruning after the scan, so a repository that was just
                        // found is never a candidate for removal.
                        if !one_off {
                            seeded_any |= ui.meta.borrow_mut().prune() > 0;
                            ui.reconcile_known();
                            ui.forget_trouble_for_the_departed();
                        }
                        if seeded_any {
                            if let Err(err) = ui.meta.borrow().save() {
                                ui::complain(&ui.toasts, &format!("The tags this scan gave were {err}"));
                            }
                        }
                        ui.scanning.set(false);
                        ui.spinner.stop();
                        ui.spinner.set_visible(false);
                        ui.rebuild();
                        ui.read_again(false);
                        // The grid takes a keyboard that is nowhere in
                        // use, so the arrow keys work without hunting for
                        // the cards with Tab. Not at launch any more: the
                        // filter has it then, and this stands back for a
                        // text field. It is for the scans after that one,
                        // which leave the keyboard on whatever button
                        // started them. A beat later, because the grid has
                        // to be real before it can take anything.
                        {
                            let ui = Rc::clone(&ui);
                            glib::timeout_add_local_once(
                                std::time::Duration::from_millis(120),
                                move || ui.focus_grid(false),
                            );
                        }
                        ui.refresh_if_overdue();
                        break;
                    }
                }
            }
        });
    }

    /// What the warning toggle keeps. A repository the last fetch failed on
    /// is on this list too: it carries the reddest pill on the grid, and the
    /// toggle used to hide it for being clean.
    fn needs_attention(&self, info: &RepoInfo) -> bool {
        info.error.is_some()
            || !info.tree.is_clean()
            || info.ahead > 0
            || info.unpushed > 0
            || info.behind > 0
            || info.history_unreadable.is_some()
            || self
                .fetch_errors
                .borrow()
                .contains_key(&info.path.display().to_string())
    }

    fn visible(&self) -> Vec<RepoInfo> {
        let query = split_query(&self.search.text());
        let dirty_only = self.dirty_only.is_active();
        let tag_filter = self.tag_filter.borrow().clone();
        let untagged_only = self.untagged_only.get();
        let meta = self.meta.borrow();

        let mut repos: Vec<RepoInfo> = self
            .repos
            .borrow()
            .iter()
            .filter(|r| !dirty_only || self.needs_attention(r))
            .filter(|r| {
                let tags: Vec<String> = meta
                    .tags_for(&r.path)
                    .iter()
                    .map(|t| t.to_lowercase())
                    .collect();
                // `tag:` in the search box is an AND; the popover is an OR.
                if !query.tags.iter().all(|wanted| tags.iter().any(|t| t == wanted)) {
                    return false;
                }
                if tag_filter.is_empty() && !untagged_only {
                    return true;
                }
                (untagged_only && tags.is_empty())
                    || tags.iter().any(|t| tag_filter.contains(t))
            })
            .filter(|r| query.langs.iter().all(|lang| r.speaks(lang)))
            .filter(|r| query.words.is_empty() || query.matches(&r.haystack()))
            .cloned()
            .collect();
        drop(meta);

        match Sort::from_index(self.sort.selected()) {
            Sort::Activity => repos.sort_by(|a, b| {
                b.last_active()
                    .cmp(&a.last_active())
                    .then_with(|| a.name.cmp(&b.name))
            }),
            Sort::Name => repos.sort_by(|a, b| {
                a.name
                    .to_lowercase()
                    .cmp(&b.name.to_lowercase())
                    .then_with(|| a.path.cmp(&b.path))
            }),
            Sort::Dirty => repos.sort_by(|a, b| {
                b.tree
                    .total()
                    .cmp(&a.tree.total())
                    .then_with(|| b.last_active().cmp(&a.last_active()))
            }),
            Sort::Unpushed => repos.sort_by(|a, b| {
                // Work that is nowhere else comes first, then what is merely
                // out of step with its upstream.
                let key = |r: &RepoInfo| (r.unpushed.max(r.ahead), r.ahead + r.behind);
                key(b)
                    .cmp(&key(a))
                    .then_with(|| b.last_active().cmp(&a.last_active()))
            }),
            Sort::Files => repos.sort_by(|a, b| {
                b.tracked_files
                    .cmp(&a.tracked_files)
                    .then_with(|| a.name.cmp(&b.name))
            }),
        }
        repos
    }

    fn rebuild(self: &Rc<Self>) {
        // A background refresh rebuilds the grid under the user's hands, so
        // the scroll position has to survive it. Restoring on the next frame
        // rather than immediately gives the flow box time to allocate, without
        // which the adjustment clamps the value back to zero.
        let offset = self.scroller.vadjustment().value();
        // Rebuilding destroys the focused card. Without putting focus back on
        // the same repository, a background refresh would throw a keyboard
        // user to the top of the grid every few minutes.
        let focused = self.focused_repo();

        while let Some(child) = self.flow.first_child() {
            self.flow.remove(&child);
        }

        // A filter on a tag that nothing on the grid carries matches nothing,
        // and the controls that could take it off are built from the tags
        // that are there: the grid stayed empty until the app was restarted.
        // Not while the first scan is still bringing cards in, when a tag's
        // repositories may simply not have arrived yet.
        if !self.scanning.get() {
            let live: BTreeSet<String> = self
                .grid_tags()
                .into_iter()
                .map(|(tag, _)| tag.to_lowercase())
                .collect();
            self.tag_filter.borrow_mut().retain(|tag| live.contains(tag));
        }

        let repos = self.visible();
        let ui = Rc::clone(self);
        let on_action: Rc<dyn Fn(Action, &RepoInfo)> =
            Rc::new(move |action, info| ui.run_action(action, info));
        let ui = Rc::clone(self);
        let on_select: Rc<dyn Fn(&RepoInfo, bool)> = Rc::new(move |info, selected| {
            {
                let mut set = ui.selected.borrow_mut();
                if selected {
                    set.insert(info.path.clone());
                } else {
                    set.remove(&info.path);
                }
            }
            ui.update_selection_label();
            ui.update_status();
        });

        *self.shown.borrow_mut() = repos.clone();
        for (index, info) in repos.iter().enumerate() {
            let tags = self.meta.borrow().tags_for(&info.path).to_vec();
            let errors = self.fetch_errors.borrow();
            let ctx = card::Context {
                tags: &tags,
                selected: self.selected.borrow().contains(&info.path),
                fetch_failed: errors.get(&info.path.display().to_string()).map(String::as_str),
                unseen: !self.is_known(&info.path),
                // The one Enter opens from the filter: `visible()` again,
                // which is what the filter itself asks.
                top_hit: index == 0,
            };
            self.flow.append(&card::build(
                info,
                &ctx,
                Rc::clone(&on_action),
                Rc::clone(&on_select),
            ));
        }
        self.update_selection_label();
        self.sync_tag_controls();

        let page = if repos.is_empty() && !self.scanning.get() { "empty" } else { "grid" };
        // Nothing to list at all, as against nothing that passes the filter.
        let nothing_at_all = self.repos.borrow().is_empty();
        if page == "empty" {
            // Why, where it can be said. "No repositories found" over a
            // filter that hides forty of them, or over a root that has been
            // moved, sent the reader looking in the wrong place.
            let amiss = self.roots_amiss();
            let (title, why) = if !nothing_at_all {
                ("Nothing matches", "No repository on the grid passes the filter in force.".to_string())
            } else if amiss.is_empty() {
                ("No repositories found", "No folder is set to be scanned.".to_string())
            } else {
                ("No repositories found", amiss.join("\n"))
            };
            self.empty.set_title(title);
            self.empty.set_description(Some(&glib::markup_escape_text(&why)));
            // A filter that hides everything is undone in the filter. A
            // fleet with nothing in it is put right in Settings.
            self.empty_action.set_visible(nothing_at_all);
        }
        // Taken before the page changes: the button leaves the screen with
        // the page, and the keyboard with the button.
        let leaving_the_way = page != "empty" && self.empty_action.has_focus();
        self.stack.set_visible_child_name(page);
        self.update_status();
        if page == "empty" && nothing_at_all {
            self.unstarted.set(true);
            self.offer_settings();
        } else if nothing_at_all {
            // A scan has begun and found nothing yet. Left where it was,
            // the keyboard would be moved by GTK to whatever comes next,
            // which is a button in the header.
            if leaving_the_way {
                self.focus_filter();
            }
        } else if self.unstarted.get() {
            self.start_keyboard();
        }

        if let Some(path) = focused {
            if let Some(index) = repos.iter().position(|repo| repo.path == path) {
                if let Some(child) = self.flow.child_at_index(index as i32) {
                    // Through the flow box's own focus path, not `grab_focus`.
                    // The box keeps a cursor of its own, which is where the
                    // arrow keys start from, and `grab_focus` on a card moves
                    // the window's focus without moving that. The cursor went
                    // on naming a card this rebuild had just removed, and the
                    // next arrow key started from it: from the wrong place at
                    // best, from freed memory at worst.
                    child.child_focus(gtk::DirectionType::Down);
                }
            }
        }

        if offset > 0.0 {
            let adjustment = self.scroller.vadjustment();
            glib::timeout_add_local_once(std::time::Duration::from_millis(16), move || {
                adjustment.set_value(offset);
            });
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

    /// (Re)arm the periodic refresh from the current settings. Called at
    /// startup and after every settings change.
    fn schedule_refresh(self: &Rc<Self>) {
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
    fn is_known(&self, path: &Path) -> bool {
        self.known
            .borrow()
            .as_ref()
            .is_none_or(|known| known.contains(&path.display().to_string()))
    }

    /// The user has opened this repository, or asked for it to be fetched.
    fn accept(self: &Rc<Self>, path: &Path) {
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
    fn forget_trouble_for_the_departed(&self) {
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
    fn reconcile_known(&self) {
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
    fn cancel_refresh(&self) {
        if let Some(cancel) = self.refresh_cancel.borrow().as_ref() {
            cancel.stop();
        }
    }

    /// Which repositories a refresh covers: everything found, narrowed to the
    /// configured tags when there are any. Deliberately not the *filtered*
    /// grid — what you are looking at should not change what gets fetched.
    fn refresh_scope(&self) -> Vec<PathBuf> {
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
    fn refresh_if_overdue(self: &Rc<Self>) {
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
    fn run_refresh(self: &Rc<Self>, manual: bool) {
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

    /// The tags on the repositories of the grid, with how many carry each.
    /// Not the tags in the store, which also remembers repositories no root
    /// reaches any more: counted over that, a tag none of whose repositories
    /// was still on the grid kept its chip and its old count, and filtering
    /// by it emptied the grid.
    fn grid_tags(&self) -> Vec<(String, usize)> {
        let repos = self.repos.borrow();
        self.meta
            .borrow()
            .tags_among(repos.iter().map(|repo| repo.path.as_path()))
    }

    /// Keep the tag chrome in step with the tags that exist and the filter in
    /// force. Below three tags they become toggles in the header, which is one
    /// click instead of three; past that they would crowd out the title, so the
    /// popover takes over.
    fn sync_tag_controls(self: &Rc<Self>) {
        let tags = self.grid_tags();
        let quick = !tags.is_empty() && tags.len() <= QUICK_TAGS;
        let signature = tags
            .iter()
            .map(|(tag, count)| format!("{tag}\u{1f}{count}"))
            .collect::<Vec<_>>()
            .join("\u{1e}");

        if signature != *self.tag_signature.borrow() {
            *self.tag_signature.borrow_mut() = signature;
            self.build_quick_tags(&tags, quick);
            self.fit_header();
        }

        // Reflect the live filter without the writes bouncing back as toggles.
        self.tag_syncing.set(true);
        for (tag, button) in self.tag_quick_buttons.borrow().iter() {
            button.set_active(self.tag_filter.borrow().contains(&tag.to_lowercase()));
        }
        self.tag_syncing.set(false);

        // The buttons are there to be pressed only while there is room for
        // them. Without it the menu does the filtering, and says how much of
        // it is on: it used to go on saying nothing, as it does beside the
        // buttons, with the buttons gone.
        let buttons = self.tag_buttons_showing();
        self.tag_quick.set_visible(buttons);
        let active = self.tag_filter.borrow().len() + usize::from(self.untagged_only.get());
        self.tags_content.set_label(&match (buttons, self.narrow.get(), active) {
            // The buttons carry the filtering, so this one is only the menu.
            (true, _, _) => String::new(),
            (false, true, 0) => String::new(),
            (false, true, n) => n.to_string(),
            (false, false, 0) => "Tags".to_string(),
            (false, false, n) => format!("Tags ({n})"),
        });
        self.tags_button
            .set_tooltip_text(Some(if buttons {
                "Tag the selected repos, or filter by untagged"
            } else {
                "Filter by tag, or tag the selected repos"
            }));
    }

    /// Whether the header has a button for each tag at the moment: there are
    /// few enough of them, and the window is wide enough.
    fn tag_buttons_showing(&self) -> bool {
        let tags = self.grid_tags().len();
        (1..=QUICK_TAGS).contains(&tags) && !self.cramped.get()
    }

    /// Put the two widths at which the header gives something up where what
    /// is in it now needs them.
    ///
    /// They were two numbers, right for two tag buttons with two-letter
    /// names. A button is as wide as its tag's name, and there can be five
    /// of them, so the widths are worked out from the buttons as they stand:
    /// what the header needs without any, and the buttons on top of that.
    fn fit_header(&self) {
        let buttons = self.tag_quick_buttons.borrow();
        // Each button asked on its own. The row they are in is hidden when
        // the window is too narrow for it, and a hidden row asks for nothing:
        // measured as a whole it would call itself narrow enough to show
        // again the moment it had been put away.
        let wide: f64 = buttons
            .iter()
            .map(|(_, button)| {
                let (_, natural, _, _) = button.measure(gtk::Orientation::Horizontal, -1);
                f64::from(natural + self.tag_quick.spacing())
            })
            .sum();
        let label = if buttons.is_empty() { 0.0 } else { TAGS_LABEL };
        if let Some([narrow, cramped]) = self.breakpoints.borrow().as_ref() {
            narrow.set_condition(Some(&below(HEADER_FULL - label + wide + SPARE)));
            cramped.set_condition(Some(&below(HEADER_BARE + wide + SPARE)));
        }
    }

    fn build_quick_tags(self: &Rc<Self>, tags: &[(String, usize)], quick: bool) {
        while let Some(child) = self.tag_quick.first_child() {
            self.tag_quick.remove(&child);
        }
        self.tag_quick_buttons.borrow_mut().clear();
        if !quick {
            return;
        }

        for (tag, count) in tags {
            let content = gtk::Box::new(gtk::Orientation::Horizontal, 5);
            content.append(&ui::color_dot(ui::key_color(&tag.to_lowercase())));
            content.append(&gtk::Label::new(Some(tag)));

            let button = gtk::ToggleButton::builder().child(&content).build();
            button.add_css_class("om-quick-tag");
            button.set_tooltip_text(Some(&format!(
                "{count} {}. Click to see this tag alone, Ctrl+click to add it or take it away.",
                if *count == 1 { "repository" } else { "repositories" }
            )));

            // A click, and not a toggle: the button has flipped by the time
            // this runs, and what it flipped to is not the answer. What the
            // filter becomes is worked out from what it was, and every
            // button, this one too, is then set from that by the rebuild.
            let ui = Rc::clone(self);
            let key = tag.to_lowercase();
            button.connect_clicked(move |button| {
                if ui.tag_syncing.get() {
                    return;
                }
                let (chosen, untagged) = after_tag_click(
                    &ui.tag_filter.borrow(),
                    ui.untagged_only.get(),
                    &key,
                    ctrl_held(button),
                );
                *ui.tag_filter.borrow_mut() = chosen;
                ui.untagged_only.set(untagged);
                ui.rebuild();
            });

            self.tag_quick.append(&button);
            self.tag_quick_buttons
                .borrow_mut()
                .push((tag.clone(), button));
        }
    }

    /// Rebuilt on every open, because the set of tags changes under it.
    fn tag_popover(self: &Rc<Self>) -> gtk::Popover {
        let popover = gtk::Popover::builder().has_arrow(false).build();
        let body = gtk::Box::new(gtk::Orientation::Vertical, 6);
        body.set_width_request(230);

        let known = self.grid_tags();
        if self.tag_buttons_showing() {
            // The header's buttons already do the filtering; repeating it
            // here would be two controls for one setting. Only while they
            // are showing, though: on a window too narrow for them this
            // menu was all there was, and it did not list a single tag.
            let untagged = gtk::CheckButton::builder()
                .label("Untagged only")
                .active(self.untagged_only.get())
                .build();
            let ui = Rc::clone(self);
            untagged.connect_toggled(move |check| {
                ui.untagged_only.set(check.is_active());
                ui.rebuild();
            });
            body.append(&untagged);
        } else if known.is_empty() {
            let hint = gtk::Label::builder()
                .label("No tags yet.\n\nTick some repos, then\n“Tag selected…”.")
                .justify(gtk::Justification::Center)
                .build();
            hint.add_css_class("om-meta");
            body.append(&hint);
        } else {
            let list = gtk::Box::new(gtk::Orientation::Vertical, 2);
            for (tag, count) in known {
                let key = tag.to_lowercase();
                let check = gtk::CheckButton::builder()
                    .label(format!("{tag}  ({count})"))
                    .active(self.tag_filter.borrow().contains(&key))
                    .build();
                let ui = Rc::clone(self);
                check.connect_toggled(move |check| {
                    {
                        let mut filter = ui.tag_filter.borrow_mut();
                        if check.is_active() {
                            filter.insert(key.clone());
                        } else {
                            filter.remove(&key);
                        }
                    }
                    ui.rebuild();
                });
                list.append(&check);
            }
            let scroller = gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Never)
                .propagate_natural_height(true)
                .max_content_height(280)
                .child(&list)
                .build();
            body.append(&scroller);

            let untagged = gtk::CheckButton::builder()
                .label("Untagged only")
                .active(self.untagged_only.get())
                .build();
            let ui = Rc::clone(self);
            untagged.connect_toggled(move |check| {
                ui.untagged_only.set(check.is_active());
                ui.rebuild();
            });
            body.append(&untagged);

            let clear = gtk::Button::builder().label("Clear tag filter").build();
            let ui = Rc::clone(self);
            let popover_for_clear = popover.clone();
            clear.connect_clicked(move |_| {
                ui.tag_filter.borrow_mut().clear();
                ui.untagged_only.set(false);
                popover_for_clear.popdown();
                ui.rebuild();
            });
            body.append(&clear);
        }

        body.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

        let selected = self.selected.borrow().len();
        let edit = gtk::Button::builder()
            .label(if selected > 0 {
                format!("Tag {selected} selected…")
            } else {
                "Tag the repos shown…".to_string()
            })
            .build();
        let ui = Rc::clone(self);
        let popover_for_edit = popover.clone();
        edit.connect_clicked(move |_| {
            popover_for_edit.popdown();
            let repos = if ui.selected.borrow().is_empty() {
                ui.shown.borrow().iter().map(|r| r.path.clone()).collect()
            } else {
                let mut paths: Vec<PathBuf> = ui.selected.borrow().iter().cloned().collect();
                paths.sort();
                paths
            };
            ui.edit_tags(repos);
        });
        body.append(&edit);

        popover.set_child(Some(&body));
        popover
    }

    fn edit_tags(self: &Rc<Self>, repos: Vec<PathBuf>) {
        let ui = Rc::clone(self);
        ui::dialogs::edit_tags(
            &self.window,
            Rc::clone(&self.meta),
            repos,
            self.grid_tags(),
            Rc::new(move || ui.rebuild()),
            {
                let ui = Rc::clone(self);
                Rc::new(move |why: String| ui::complain(&ui.toasts, &why))
            },
        );
    }

    fn enter_keyboard_nav(&self) {
        self.last_key.set(Some(std::time::Instant::now()));
        if !self.keyboard_nav.replace(true) {
            self.flow.add_css_class("om-keyboard");
        }
        self.pointer_rests();
    }

    /// The pointer is over the grid, at `at` in the window. That is not yet
    /// the pointer moving: GTK says so again, from the same place, each time
    /// the cards move under it, and a scroll, a rescan and a change of width
    /// all move them. The first sighting is no movement either. A window
    /// opens under a pointer more often than not.
    fn pointer_seen(&self, at: Option<(f64, f64)>) {
        let Some(at) = at else { return };
        let was = self.pointer_at.get();
        if was.is_some_and(|was| !pointer_moved(was, at)) {
            return;
        }
        self.pointer_at.set(Some(at));
        // Not in the moment after a key, as with leaving the keyboard's turn
        // below: a hand on the arrow keys is not a hand on the mouse.
        if was.is_some() && !self.key_just_pressed() {
            self.pointer_chooses();
        }
    }

    /// Whether a key went to the grid within the last moment.
    fn key_just_pressed(&self) -> bool {
        self.last_key
            .get()
            .is_some_and(|when| when.elapsed() < std::time::Duration::from_millis(300))
    }

    /// The pointer has been moved, or the wheel turned: it is what chooses a
    /// card now, and the card under it shows what the one with the keyboard
    /// shows. The stylesheet does it, told by the class.
    fn pointer_chooses(&self) {
        if self.keyboard_nav.replace(false) {
            self.flow.remove_css_class("om-keyboard");
        }
        if !self.pointer_nav.replace(true) {
            self.flow.add_css_class("om-pointer");
        }
    }

    /// The keyboard is in use, and the pointer is wherever it was left.
    fn pointer_rests(&self) {
        if self.pointer_nav.replace(false) {
            self.flow.remove_css_class("om-pointer");
        }
    }

    fn leave_keyboard_nav(&self) {
        if !self.keyboard_nav.get() {
            return;
        }
        // Motion arriving right after a keypress is the grid scrolling beneath
        // a pointer that has not actually moved.
        if self.key_just_pressed() {
            return;
        }
        self.keyboard_nav.set(false);
        self.flow.remove_css_class("om-keyboard");
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

    /// Which repository the focused widget belongs to, if the focus is in the
    /// grid at all.
    fn focused_repo(&self) -> Option<PathBuf> {
        let mut widget = gtk::prelude::GtkWindowExt::focus(&self.window);
        while let Some(current) = widget {
            if let Ok(child) = current.clone().downcast::<gtk::FlowBoxChild>() {
                // The window goes on naming a card as its focus for a while
                // after the card has been taken out of the grid. Such a card
                // must not be asked for its index: the flow box frees the
                // slot it kept for it and leaves the card pointing there, so
                // the answer is read out of freed memory. That crashed the
                // app twice in an afternoon.
                let in_grid = child
                    .parent()
                    .is_some_and(|parent| &parent == self.flow.upcast_ref::<gtk::Widget>());
                if !in_grid {
                    return None;
                }
                let index = child.index();
                if index >= 0 {
                    return self
                        .shown
                        .borrow()
                        .get(index as usize)
                        .map(|repo| repo.path.clone());
                }
            }
            widget = current.parent();
        }
        None
    }

    /// Back from a page: the keyboard belongs on the card that was opened,
    /// if it is not on a card already.
    fn back_on_the_grid(&self) {
        if !self.on_grid() {
            return;
        }
        // Wherever the pointer was put while another page was in front, it
        // has not been moved here.
        self.pointer_at.set(None);
        self.pointer_rests();
        let focus = gtk::prelude::GtkWindowExt::focus(&self.window);
        let in_grid = focus.as_ref().is_some_and(|focus| focus.is_ancestor(&self.flow));
        let typing = focus.is_some_and(|focus| focus.is::<gtk::Editable>());
        if in_grid || typing {
            return;
        }
        let index = self.opened.borrow().as_ref().and_then(|opened| {
            self.shown.borrow().iter().position(|repo| &repo.path == opened)
        });
        match index.and_then(|index| self.flow.child_at_index(index as i32)) {
            // Through the box, as in `rebuild`, so its cursor comes too.
            Some(child) => {
                gtk::prelude::GtkWindowExt::set_focus(&self.window, gtk::Widget::NONE);
                child.child_focus(gtk::DirectionType::Down);
            }
            None => self.focus_grid(true),
        }
    }

    /// What has the keyboard: the window's focus, or, while a change of
    /// width has taken it, the widget it is about to be given back to.
    fn keyboard(&self) -> Option<gtk::Widget> {
        gtk::prelude::GtkWindowExt::focus(&self.window)
            .or_else(|| self.owed.upgrade().filter(|had| had.is_ancestor(&self.window)))
    }

    /// The content is leaving the screen: note what has the keyboard, so
    /// that it can be given back when the content returns.
    ///
    /// A change of breakpoint takes the whole content off screen for a
    /// frame, and GTK moves the keyboard off a widget that leaves the
    /// screen. libadwaita notes what had it and means to give it back, but
    /// GTK's move reaches it as a request for the keyboard, which it takes
    /// to be somebody's wish and honours instead: when the content comes
    /// back, the keyboard is on a button in the header. So any
    /// resize across one of the two widths, which on a tiling desktop is
    /// what opening a window beside this one does, took the keyboard out of
    /// the filter or off the card it was on, and no key did what it should
    /// until it had been put back by hand.
    ///
    /// GTK moves it at the end of the frame, so here it is still where the
    /// person left it.
    fn note_keyboard(&self) {
        // A second change of width can begin before the first has been made
        // good, with the keyboard on that button. The first note stands.
        if self.owed.upgrade().is_none() {
            self.owed.set(gtk::prelude::GtkWindowExt::focus(&self.window).as_ref());
        }
    }

    /// The content is back on screen: give the keyboard back to what
    /// `note_keyboard` noted.
    ///
    /// A moment later, not now. libadwaita puts the content back and then
    /// hands the keyboard to that button, in one go, and this is called in
    /// the middle of it. Ahead of anything typed, though, so that no key
    /// reaches the button.
    fn give_keyboard_back(self: &Rc<Self>) {
        if self.owed.upgrade().is_none() {
            return;
        }
        let ui = Rc::clone(self);
        glib::idle_add_local_full(glib::Priority::HIGH, move || {
            // Off screen again already, by another change of width: the
            // note is kept for when it comes back from that one.
            if !ui.window.content().is_some_and(|content| content.is_mapped()) {
                return glib::ControlFlow::Break;
            }
            let had = ui.owed.upgrade();
            ui.owed.set(None::<&gtk::Widget>);
            // Not if it has gone meanwhile, or is on a page that has been
            // left: the keyboard stays where GTK put it.
            let Some(had) = had.filter(|had| had.is_mapped()) else {
                return glib::ControlFlow::Break;
            };
            if gtk::prelude::GtkWindowExt::focus(&ui.window).as_ref() == Some(&had) {
                return glib::ControlFlow::Break;
            }
            // Not `grab_focus` for a text field. That selects what is in
            // it, and the next key typed would replace a filter that was
            // only being added to.
            match had.downcast_ref::<gtk::Text>() {
                Some(text) => {
                    text.grab_focus_without_selecting();
                }
                None => {
                    had.grab_focus();
                }
            }
            glib::ControlFlow::Break
        });
    }

    /// Give the filter the keyboard, with whatever is in it selected, so
    /// that what is typed next is the filter. This is where the keyboard
    /// starts: the window is opened to find a repository, the way a launcher
    /// is, and the letters that would name one mean other things on a card —
    /// `f` fetches the repository under the keyboard, `l` opens a terminal.
    /// The grid is one Down away.
    ///
    /// Only with the grid in front and nothing over it. Behind a dialog or
    /// another page the filter is out of sight, and the keyboard would go
    /// where nobody can see what it types.
    fn focus_filter(&self) {
        if !self.on_grid() || self.window.visible_dialog().is_some() {
            return;
        }
        self.search.grab_focus();
        self.search.select_region(0, -1);
    }

    /// With nothing to list there is nothing to filter, so the keyboard goes
    /// to the one thing on the page that leads somewhere, and Enter opens
    /// Settings. From an empty filter or from nowhere only: a keyboard that
    /// has been taken to a button in the header stays there, so does one in
    /// a dialog, and so does one in a filter that is being typed in. The
    /// fleet can go from under a sentence (a disk unmounted), and the space
    /// that came next would have opened Settings.
    fn offer_settings(&self) {
        if !self.on_grid() || self.window.visible_dialog().is_some() {
            return;
        }
        let free = match self.keyboard() {
            None => true,
            Some(had) => {
                !had.is_mapped() || (had.is_ancestor(&self.search) && self.search.text().is_empty())
            }
        };
        if free {
            self.empty_action.grab_focus();
        }
    }

    /// The first repositories have arrived in a window that had none to
    /// list. The keyboard starts in the filter, as it does at launch,
    /// wherever it was: on the button that led to Settings, or on the one in
    /// the header that started the scan. Not from under a dialog, which has
    /// the keyboard while it is open: then when it closes.
    fn start_keyboard(&self) {
        if !self.on_grid() || self.window.visible_dialog().is_some() {
            return;
        }
        self.unstarted.set(false);
        self.focus_filter();
    }

    /// A dialog has closed, and the keyboard goes back to what had it before
    /// the dialog opened: libadwaita sees to that. Three times it does not
    /// come out right, and this is for those.
    ///
    /// On the page that says there is nothing to list, the keyboard belongs
    /// on the button there, which is the one thing on the page that leads
    /// anywhere. Closed before it had been drawn, Settings left the keyboard
    /// on a button in the header instead.
    ///
    /// When Settings has given the grid its first repositories, the button
    /// the keyboard was on has left the screen with its page, and the
    /// keyboard starts in the filter as it does at launch.
    ///
    /// And when what had the keyboard has gone for any other reason (a card
    /// that was rebuilt under the tag editor), it was nowhere, and no key did
    /// anything until the filter or a card had been clicked. The filter
    /// again.
    ///
    /// A beat later, so that libadwaita has had its turn first.
    fn keyboard_after_dialog(self: &Rc<Self>) {
        let ui = Rc::clone(self);
        glib::timeout_add_local_once(std::time::Duration::from_millis(150), move || {
            if ui.window.visible_dialog().is_some() || !ui.on_grid() {
                return;
            }
            if ui.empty_action.is_mapped() {
                ui.empty_action.grab_focus();
            } else if ui.unstarted.get() {
                ui.start_keyboard();
            } else if !ui.keyboard().is_some_and(|had| had.is_mapped()) {
                ui.focus_filter();
            }
        });
    }

    /// Put the keyboard in the grid. `force` takes focus even from a text
    /// field, which is what Ctrl+L is for; without it the grid is only claimed
    /// when nothing else holds the focus, so a scan finishing mid-sentence
    /// does not swallow what is being typed.
    fn focus_grid(&self, force: bool) {
        // Not only the window's focus: a keyboard that has just been taken
        // from the filter by a change of width is still the filter's, and is
        // about to be given back to it.
        let current = self.keyboard();
        if !self.on_grid() {
            return;
        }
        // Unforced, the grid stands back for a keyboard that is in use:
        // typing, already on a card, or in a menu or a dialog. It used to
        // stand back for any focus at all, and a window always has one by the
        // time a scan ends, on the first button of the header. So this never
        // once ran, and the arrow keys did nothing until Tab had been found.
        if !force {
            if self.window.visible_dialog().is_some() {
                return;
            }
            if let Some(focus) = &current {
                let typing = focus.is::<gtk::Editable>();
                let in_grid = focus.is_ancestor(&self.flow);
                let in_menu = focus.ancestor(gtk::Popover::static_type()).is_some();
                if typing || in_grid || in_menu {
                    return;
                }
            }
        }
        if let Some(child) = self.flow.child_at_index(0) {
            // Asked for with a key, which makes it the keyboard's turn.
            if force {
                self.pointer_rests();
            }
            // As in `rebuild`: through the box, so its cursor comes too. GTK
            // leaves the cursor alone for a card that already holds the focus,
            // so the focus is let go of first.
            gtk::prelude::GtkWindowExt::set_focus(&self.window, gtk::Widget::NONE);
            child.child_focus(gtk::DirectionType::Down);
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
            ui.nav
                .push(&ui::stats::page(vec![path], name, ui.commit_opener()));
        });
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
            on_fetch,
            toasts: self.toasts.clone(),
        }
    }

    /// Fetch one repository, and fast-forward it too when asked, saying what
    /// happened: the pass the timer runs, narrowed to the repository in hand.
    /// The card's `p` and `f` come here, and so does a graph page's `f`,
    /// which hands in `done` to hear whether the remote was reached.
    fn fetch_one(
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
            let read: Vec<RepoInfo> = paths.iter().map(|path| crate::git::inspect(path)).collect();
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
    fn read_again(self: &Rc<Self>, handed_too: bool) {
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

    /// Whether the grid is the page in front. Asked of the page, not of the
    /// depth of the stack: on the way back from a page the view names the
    /// grid as visible before it has finished letting go of the other.
    fn on_grid(&self) -> bool {
        self.nav.visible_page().as_ref() == Some(&self.grid_page)
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
            .push(&ui::stats::page(repos, scope, self.commit_opener()));
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

/// What the tag buttons filter by once `tag`'s has been clicked: the tags to
/// show and whether "untagged only" stays on.
///
/// One tag at a time is the common case, so a click shows that tag and no
/// other, and a second click on it, shown alone, shows everything again. They
/// used to be switches each, and looking at one tag after another took a
/// click to turn the last one off every time. With Ctrl held a click adds the
/// tag to the ones shown, or takes it away, and leaves the rest alone.
fn after_tag_click(
    chosen: &BTreeSet<String>,
    untagged: bool,
    tag: &str,
    add: bool,
) -> (BTreeSet<String>, bool) {
    let mut next = chosen.clone();
    if add {
        if !next.remove(tag) {
            next.insert(tag.to_string());
        }
        return (next, untagged);
    }
    let alone = chosen.len() == 1 && chosen.contains(tag) && !untagged;
    next.clear();
    if !alone {
        next.insert(tag.to_string());
    }
    (next, false)
}

/// Whether Ctrl is held down, on the keyboard of the display `widget` is on.
fn ctrl_held(widget: &impl IsA<gtk::Widget>) -> bool {
    // A driven window is sent no keys, so the driver says it outright.
    #[cfg(debug_assertions)]
    if let Some(held) = ui::debug::ctrl_held() {
        return held;
    }
    widget
        .display()
        .default_seat()
        .and_then(|seat| seat.keyboard())
        .is_some_and(|keyboard| keyboard.modifier_state().contains(gdk::ModifierType::CONTROL_MASK))
}

/// Whether a pointer last seen at `was` and now at `at`, both in the window,
/// has been moved. By a few pixels: where it is comes through a float each
/// way, so the same place does not always come back as the same number, and
/// a mouse on a desk that is being typed at is not being moved either.
fn pointer_moved(was: (f64, f64), at: (f64, f64)) -> bool {
    const SLACK: f64 = 3.0;
    (was.0 - at.0).abs() >= SLACK || (was.1 - at.1).abs() >= SLACK
}

/// A parsed filter: free text, plus the `tag:` and `lang:` terms pulled out of
/// it. Each kind of term is matched against its own field, so a word can only
/// match where the user aimed it.
#[derive(Default, Debug, PartialEq)]
struct Query {
    /// Each has to be found, anywhere and in any order. They used to be
    /// joined back into one phrase, so a second word emptied the grid unless
    /// the two happened to sit side by side in the name or the path.
    words: Vec<String>,
    tags: Vec<String>,
    langs: Vec<String>,
}

impl Query {
    /// Whether every free word is somewhere in `haystack`, which is lower
    /// case already.
    fn matches(&self, haystack: &str) -> bool {
        self.words.iter().all(|word| haystack.contains(word.as_str()))
    }
}

fn split_query(raw: &str) -> Query {
    let mut query = Query::default();
    for token in raw.split_whitespace() {
        // Lowered before the prefixes are looked for, so `Tag:work` is a tag.
        let token = token.to_lowercase();
        if let Some(tag) = token.strip_prefix("tag:") {
            // A bare `tag:` is one that is still being typed. Taken as a
            // word, it emptied the grid between the colon and the first
            // letter after it.
            if !tag.is_empty() {
                query.tags.push(tag.to_string());
            }
        } else if let Some(lang) = token.strip_prefix("lang:") {
            if !lang.is_empty() {
                query.langs.push(lang.to_string());
            }
        } else {
            query.words.push(token);
        }
    }
    query
}

/// "Narrower than this", as a breakpoint is told it.
fn below(width: f64) -> adw::BreakpointCondition {
    adw::BreakpointCondition::new_length(
        adw::BreakpointConditionLengthType::MaxWidth,
        width,
        adw::LengthUnit::Px,
    )
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

fn now_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A click on a tag shows it alone, again shows everything; Ctrl+click
    /// adds to what is shown or takes from it.
    #[test]
    fn a_tag_click_switches_and_a_ctrl_click_adds() {
        let set = |tags: &[&str]| tags.iter().map(|t| t.to_string()).collect::<BTreeSet<_>>();
        let none = set(&[]);

        // From nothing: that tag.
        assert_eq!(after_tag_click(&none, false, "work", false), (set(&["work"]), false));
        // Another: that one instead, not both.
        assert_eq!(after_tag_click(&set(&["work"]), false, "side", false), (set(&["side"]), false));
        // The one shown, again: everything.
        assert_eq!(after_tag_click(&set(&["work"]), false, "work", false), (none.clone(), false));
        // One of several, plainly: that one alone, the others and "untagged
        // only" gone with them.
        assert_eq!(after_tag_click(&set(&["work", "side"]), true, "work", false), (set(&["work"]), false));
        // Shown together with the untagged, it is not alone: it stays, alone.
        assert_eq!(after_tag_click(&set(&["work"]), true, "work", false), (set(&["work"]), false));

        // With Ctrl: added, and taken away again; the rest left as they were.
        assert_eq!(after_tag_click(&set(&["work"]), true, "side", true), (set(&["work", "side"]), true));
        assert_eq!(after_tag_click(&set(&["work", "side"]), false, "work", true), (set(&["side"]), false));
        assert_eq!(after_tag_click(&set(&["side"]), false, "side", true), (none, false));
    }

    /// GTK reports a pointer again whenever the cards move under it, from the
    /// place it was already in. Only a report from somewhere else is the
    /// pointer having been moved.
    #[test]
    fn a_pointer_seen_again_in_the_same_place_has_not_moved() {
        let at = (412.0, 388.5);
        assert!(!pointer_moved(at, at));
        // What a place loses and gains on its way through a float.
        assert!(!pointer_moved(at, (412.0004, 388.4997)));
        // A mouse that shivers where it lies.
        assert!(!pointer_moved(at, (413.0, 387.5)));
        assert!(pointer_moved(at, (416.0, 388.5)));
        assert!(pointer_moved(at, (412.0, 380.0)));
        assert!(pointer_moved(at, (30.0, 900.0)));
    }

    #[test]
    fn two_words_both_have_to_be_there_in_either_order() {
        let hay = "billing ~/work/acme/billing main git@example.invalid:acme/billing";
        assert!(split_query("billing").matches(hay));
        assert!(split_query("acme billing").matches(hay));
        assert!(split_query("billing acme").matches(hay));
        assert!(split_query("Billing MAIN").matches(hay));
        assert!(!split_query("billing ledger").matches(hay));
        // No words is no narrowing.
        assert!(split_query("  ").matches(hay));
    }

    #[test]
    fn tag_and_lang_terms_are_pulled_out_whatever_their_case() {
        let query = split_query("Tag:Work lang:Rust api");
        assert_eq!(query.tags, ["work"]);
        assert_eq!(query.langs, ["rust"]);
        assert_eq!(query.words, ["api"]);
    }

    #[test]
    fn a_term_still_being_typed_narrows_nothing() {
        let query = split_query("tag: lang:");
        assert_eq!(query, Query::default());
        assert!(query.matches("anything at all"));
    }

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
}
