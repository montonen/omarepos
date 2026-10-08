//! The main window: a searchable, sortable grid of repository cards.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gtk::gdk;
use gtk::gio;
use gtk::glib;
use adw::prelude::*;

use crate::config::{Config, Trouble};
use crate::meta::Meta;
use crate::refresh::{self, Summary};
use crate::git::RepoInfo;
use crate::scan::{self, Event};
use crate::theme::Palette;
use crate::ui::{self, Action, card};

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
    scroller: gtk::ScrolledWindow,
    /// Lower-cased tags the grid is narrowed to; empty means no tag filter.
    tag_filter: RefCell<BTreeSet<String>>,
    /// Show only repositories with no tags at all.
    untagged_only: Cell<bool>,
    spinner: gtk::Spinner,
    stack: gtk::Stack,
    flow: gtk::FlowBox,
    status: gtk::Label,
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
    /// Repositories whose cards no longer say what is true, because the app
    /// wrote to them from another page. Read again when the grid is next in
    /// front.
    stale: RefCell<BTreeSet<PathBuf>>,
    /// The repository whose page was opened last, which is the card the
    /// keyboard goes back to when that page is left.
    opened: RefCell<Option<PathBuf>>,
    /// Repositories handed to a terminal or an editor in this run. Whatever is
    /// done to them there is done behind the app's back, so they are read
    /// again each time the window is returned to.
    handed: RefCell<BTreeSet<PathBuf>>,
    expected: Cell<usize>,
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
    /// Repositories the last refresh could not fetch or could not update, and
    /// why. Keyed by the path as a string so it can be written out and read
    /// back.
    fetch_errors: RefCell<BTreeMap<String, Trouble>>,
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

    let empty = adw::StatusPage::builder()
        .icon_name("folder-symbolic")
        .title("No repositories found")
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
        scroller: scroller.clone(),
        tag_filter: RefCell::new(BTreeSet::new()),
        untagged_only: Cell::new(false),
        spinner,
        stack,
        flow,
        status,
        toasts,
        repos: RefCell::new(Vec::new()),
        shown: RefCell::new(Vec::new()),
        selected: RefCell::new(HashSet::new()),
        contrib_content,
        generation: Cell::new(0),
        scanning: Cell::new(false),
        stale: RefCell::new(BTreeSet::new()),
        opened: RefCell::new(None),
        handed: RefCell::new(BTreeSet::new()),
        expected: Cell::new(0),
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
        search.connect_search_changed(move |_| ui.rebuild());
    }
    {
        // Space ticks the focused card, so the grid can be worked through
        // without reaching for the mouse.
        let keys = gtk::EventControllerKey::new();
        let nav = Rc::clone(&ui);
        // The four letters below each start something outside the window, and
        // a held key arrives as a run of presses.
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

            // `p` as lazygit means it. Fetch and fast-forward, with every
            // refusal the background pass already knows how to make.
            if key == gdk::Key::p && ui::unmodified(state) {
                if let Some(path) = nav.focused_repo().filter(|_| presses.first(key)) {
                    nav.pull_one(path, true, None);
                }
                return glib::Propagation::Stop;
            }

            // `f` fetches without touching the working tree.
            if key == gdk::Key::f && ui::unmodified(state) {
                if let Some(path) = nav.focused_repo().filter(|_| presses.first(key)) {
                    nav.pull_one(path, false, None);
                }
                return glib::Propagation::Stop;
            }

            if key == gdk::Key::e && ui::unmodified(state) {
                if let Some(path) = nav.focused_repo().filter(|_| presses.first(key)) {
                    nav.editor_opener(&path)();
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
        motion.connect_motion(move |_, _, _| nav.leave_keyboard_nav());
        ui.flow.add_controller(motion);
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
            window.connect_visible_dialog_notify(move |_| sync());
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
    // so below this width they fall back to their icons.
    let breakpoint = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
        adw::BreakpointConditionLengthType::MaxWidth,
        920.0,
        adw::LengthUnit::Px,
    ));
    breakpoint.add_setter(&ui.contrib_content, "label", Some(&"".to_value()));
    breakpoint.add_setter(&ui.tags_content, "label", Some(&"".to_value()));
    window.add_breakpoint(breakpoint);

    let unusable: Vec<String> = [
        ui.cfg.borrow().load_error.clone(),
        ui.meta.borrow().load_error.clone(),
    ]
    .into_iter()
    .flatten()
    .collect();
    if !unusable.is_empty() {
        trouble.set_title(&format!(
            "Could not read {}. Running without it, and nothing will be saved over it. \
             Fix the file and start omarepos again.",
            unusable.join("; ")
        ));
        trouble.set_revealed(true);
    }

    #[cfg(debug_assertions)]
    ui::debug::install(&window);

    ui.apply_theme();
    ui.watch_theme();
    ui.schedule_refresh();
    ui.start_scan();
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
        let roots = self.cfg.borrow().root_labels().join("  ");
        self.subtitle
            .set_label(&format!("{}  ·  {}", roots, palette.name));
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

    fn start_scan(self: &Rc<Self>) {
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
        let rx = scan::spawn(roots, extras, depth);
        let ui = Rc::clone(self);
        glib::spawn_future_local(async move {
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
                        // Pruning after the scan, so a repository that was just
                        // found is never a candidate for removal.
                        if !one_off {
                            seeded_any |= ui.meta.borrow_mut().prune() > 0;
                            ui.reconcile_known();
                            ui.forget_trouble_for_the_departed();
                        }
                        if seeded_any {
                            if let Err(err) = ui.meta.borrow().save() {
                                eprintln!("omarepos: could not write the tag file: {err}");
                            }
                        }
                        ui.scanning.set(false);
                        ui.spinner.stop();
                        ui.spinner.set_visible(false);
                        ui.rebuild();
                        ui.read_again(false);
                        // Arrow keys should work the moment the grid has
                        // something in it, without hunting for it with Tab.
                        // On a short scan this runs before the window is
                        // mapped, where grab_focus does nothing and GTK then
                        // hands focus to the header instead. A beat later the
                        // grid is real and can take it.
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

    fn needs_attention(info: &RepoInfo) -> bool {
        info.error.is_some()
            || !info.tree.is_clean()
            || info.ahead > 0
            || info.unpushed > 0
            || info.behind > 0
            || info.tree.conflicted > 0
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
            .filter(|r| !dirty_only || Self::needs_attention(r))
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
            .filter(|r| query.text.is_empty() || r.haystack().contains(&query.text))
            .cloned()
            .collect();
        drop(meta);

        match Sort::from_index(self.sort.selected()) {
            Sort::Activity => repos.sort_by(|a, b| {
                b.activity()
                    .cmp(&a.activity())
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
                    .then_with(|| b.activity().cmp(&a.activity()))
            }),
            Sort::Unpushed => repos.sort_by(|a, b| {
                // Work that is nowhere else comes first, then what is merely
                // out of step with its upstream.
                let key = |r: &RepoInfo| (r.unpushed.max(r.ahead), r.ahead + r.behind);
                key(b)
                    .cmp(&key(a))
                    .then_with(|| b.activity().cmp(&a.activity()))
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
        let auto_update = {
            let cfg = self.cfg.borrow();
            cfg.refresh.enabled() && cfg.refresh.fast_forward
        };
        // "Held back" is a statement about the refresh, and only true of a
        // repository the refresh would have updated: one outside `only_tags`,
        // or not yet seen, is not being held back by anything.
        let in_scope: HashSet<PathBuf> =
            crate::config::only_known(self.refresh_scope(), self.known.borrow().as_ref())
                .0
                .into_iter()
                .collect();
        for info in &repos {
            let tags = self.meta.borrow().tags_for(&info.path).to_vec();
            let errors = self.fetch_errors.borrow();
            let ctx = card::Context {
                tags: &tags,
                selected: self.selected.borrow().contains(&info.path),
                auto_update: auto_update && in_scope.contains(&info.path),
                trouble: errors.get(&info.path.display().to_string()),
                unseen: !self.is_known(&info.path),
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
        self.stack.set_visible_child_name(page);
        self.update_status();

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
        let shown = self.flow.observe_children().n_items();
        if (shown as usize) != total {
            parts.push(format!("{shown} shown"));
        }
        let selected = self.selected.borrow().len();
        if selected > 0 {
            parts.push(format!("{selected} selected"));
        }
        if let Some(when) = self.last_refresh.get() {
            parts.push(format!("fetched {}", ui::relative_time(when)));
        }
        let cfg = self.cfg.borrow();
        let settings = &cfg.refresh;
        if cfg.one_off() {
            parts.push("one-off look: no background refresh".to_string());
        } else if settings.enabled() {
            let what = if settings.fast_forward {
                "fast-forward"
            } else if settings.fetch {
                "fetch"
            } else {
                "rescan"
            };
            parts.push(format!("auto {what} every {}m", settings.every_minutes));
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
        self.status.set_label(&parts.join("  ·  "));
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

    /// Which repositories a refresh covers: everything found, narrowed to the
    /// configured tags when there are any. Deliberately not the *filtered*
    /// grid — what you are looking at should not change what gets fetched.
    /// Whether the user has seen this repository. Everything counts as seen
    /// until the first scan has recorded what was already on the grid.
    fn is_known(&self, path: &Path) -> bool {
        self.known
            .borrow()
            .as_ref()
            .is_none_or(|known| known.contains(&path.display().to_string()))
    }

    /// The user has opened this repository, or asked for it to be updated.
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

    /// Keep the record of what the last fetch or update of a repository could
    /// not do, from one report. Every way a repository gets fetched goes
    /// through here (the pass across the fleet, `p` and `f` on a card, and the
    /// same keys on a graph page), so a card cannot go on saying "fetch failed"
    /// about a repository that has since been fetched by hand. Returns whether
    /// this is trouble the record did not already hold.
    fn note_outcome(&self, report: &refresh::Report) -> bool {
        let key = report.path.display().to_string();
        let trouble = match &report.outcome {
            refresh::Outcome::Failed(err) => Some(Trouble::Fetch(err.clone())),
            refresh::Outcome::Refused(why) => Some(Trouble::Update { refused: why.clone() }),
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

    /// Call off the pass across the fleet, if one is under way.
    fn cancel_refresh(&self) {
        if let Some(cancel) = self.refresh_cancel.borrow().as_ref() {
            cancel.stop();
        }
    }

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

    /// Run a refresh pass. `manual` means the user asked, which fetches even
    /// when the background setting is off and reports when nothing was done.
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
            Some(last) => now_seconds() - last >= settings.interval().as_secs() as i64,
            None => true,
        };
        if due {
            self.run_refresh(false);
        }
    }

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
                self.toasts.add_toast(ui::toast(&if unseen > 0 {
                    format!("Nothing fetched: {unseen} new here, open each once to include it")
                } else {
                    "Nothing in scope to fetch".to_string()
                }));
            }
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
                fast_forward: settings.fast_forward,
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
            let mut summary = Summary::default();
            for _ in 0..unseen {
                summary.add(&refresh::Outcome::Skipped(refresh::Skip::Unseen));
            }
            let mut done = 0usize;
            // Trouble that was not already on a card. A repository that fails
            // the same way on every pass is said once, not every half hour.
            let mut new_trouble = 0usize;
            while let Ok(event) = rx.recv().await {
                match event {
                    refresh::Event::Repo(report) => {
                        done += 1;
                        ui.refresh_progress.set((done, total));
                        summary.add(&report.outcome);
                        if ui.note_outcome(&report) {
                            new_trouble += 1;
                        }
                        // A graph page open on this repository draws what the
                        // pass has just moved: the remote's branches, how far
                        // behind HEAD is, what the push button would send.
                        if report.fetched && !matches!(report.outcome, refresh::Outcome::UpToDate)
                        {
                            ui.reload_graph_pages(&report.path);
                        }
                        match &report.outcome {
                            refresh::Outcome::Failed(err) => eprintln!(
                                "omarepos: {} failed: {err} ({})",
                                report.name,
                                report.path.display()
                            ),
                            refresh::Outcome::Refused(why) => eprintln!(
                                "omarepos: {} fetched but not updated: {why} ({})",
                                report.name,
                                report.path.display()
                            ),
                            refresh::Outcome::FastForwarded(n) => eprintln!(
                                "omarepos: {} fast-forwarded {n} commits ({})",
                                report.name,
                                report.path.display()
                            ),
                            refresh::Outcome::Skipped(skip) => eprintln!(
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

            ui.refreshing.set(false);
            ui.spinner.stop();
            ui.spinner.set_visible(false);
            ui.refresh_cancel.borrow_mut().take();
            crate::config::save_fetch_errors(&ui.fetch_errors.borrow());
            if cancel.stopped() {
                // Not stamped as a refresh: the fleet was not covered, and the
                // next pass should not think it was.
                ui.toasts.add_toast(ui::toast(&format!(
                    "Refresh stopped after {done} of {total}: {}",
                    summary.line()
                )));
            } else {
                let finished = now_seconds();
                ui.last_refresh.set(Some(finished));
                crate::config::record_refresh(finished);
                if manual || summary.updated > 0 || new_trouble > 0 {
                    ui.toasts.add_toast(ui::toast(&summary.line()));
                }
            }
            // Re-read local state so the cards show what the fetch changed.
            ui.start_scan();
        });
    }

    /// Keep the tag chrome in step with the tags that exist and the filter in
    /// force. Below three tags they become toggles in the header, which is one
    /// click instead of three; past that they would crowd out the title, so the
    /// popover takes over.
    fn sync_tag_controls(self: &Rc<Self>) {
        const QUICK_LIMIT: usize = 3;

        let tags = self.meta.borrow().all_tags();
        let quick = !tags.is_empty() && tags.len() < QUICK_LIMIT;
        let signature = tags
            .iter()
            .map(|(tag, count)| format!("{tag}\u{1f}{count}"))
            .collect::<Vec<_>>()
            .join("\u{1e}");

        if signature != *self.tag_signature.borrow() {
            *self.tag_signature.borrow_mut() = signature;
            self.build_quick_tags(&tags, quick);
        }

        // Reflect the live filter without the writes bouncing back as toggles.
        self.tag_syncing.set(true);
        for (tag, button) in self.tag_quick_buttons.borrow().iter() {
            button.set_active(self.tag_filter.borrow().contains(&tag.to_lowercase()));
        }
        self.tag_syncing.set(false);

        self.tag_quick.set_visible(quick);
        let active = self.tag_filter.borrow().len() + usize::from(self.untagged_only.get());
        self.tags_content.set_label(&if quick {
            // The chips carry the filtering, so the button is only the menu.
            String::new()
        } else if active > 0 {
            format!("Tags ({active})")
        } else {
            "Tags".to_string()
        });
        self.tags_button
            .set_tooltip_text(Some(if quick {
                "Tag the selected repos, or filter by untagged"
            } else {
                "Filter by tag, or tag the selected repos"
            }));
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
                "{count} {}",
                if *count == 1 { "repository" } else { "repositories" }
            )));

            let ui = Rc::clone(self);
            let key = tag.to_lowercase();
            button.connect_toggled(move |button| {
                if ui.tag_syncing.get() {
                    return;
                }
                {
                    let mut filter = ui.tag_filter.borrow_mut();
                    if button.is_active() {
                        filter.insert(key.clone());
                    } else {
                        filter.remove(&key);
                    }
                }
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

        let known = self.meta.borrow().all_tags();
        let quick = !known.is_empty() && known.len() < 3;
        if quick {
            // The header chips already do the filtering; repeating it here
            // would be two controls for one setting.
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
            Rc::new(move || ui.rebuild()),
        );
    }

    /// Tick every repository the grid is currently showing, or untick them all
    /// when they are already ticked. Toggling means the selection can be
    /// undone with the key that made it, without reaching for a chord.
    ///
    /// An accelerator fires ahead of the focused widget, so Ctrl+A in the
    /// filter box would otherwise stop selecting its text. When the focus is
    /// somewhere typable it keeps its usual meaning.
    fn enter_keyboard_nav(&self) {
        self.last_key.set(Some(std::time::Instant::now()));
        if !self.keyboard_nav.replace(true) {
            self.flow.add_css_class("om-keyboard");
        }
    }

    fn leave_keyboard_nav(&self) {
        if !self.keyboard_nav.get() {
            return;
        }
        // Motion arriving right after a keypress is the grid scrolling beneath
        // a pointer that has not actually moved.
        if let Some(when) = self.last_key.get() {
            if when.elapsed() < std::time::Duration::from_millis(300) {
                return;
            }
        }
        self.keyboard_nav.set(false);
        self.flow.remove_css_class("om-keyboard");
    }

    /// Escape backs out of whatever is in the way, nearest first: a dialog or
    /// a menu, the filter you are typing in (or the commit message, which it steps out of and
    /// leaves alone), then the page you are on, then the selection, then a
    /// filter left in the box.
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
                    // A filter is cleared. A commit message is not a filter:
                    // it is a sentence someone has just written, and the hand
                    // that learnt Escape in vim presses it on finishing one.
                    // That erased the message, and the next press left the
                    // page. Where the page can take the keyboard out of the
                    // field instead, it does, and the text stays.
                    let stepped_out = self
                        .nav
                        .visible_page()
                        .is_some_and(|page| page.activate_action("page.leave-field", None).is_ok());
                    if !stepped_out {
                        editable.set_text("");
                    }
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

    /// Put the keyboard in the grid. `force` takes focus even from a text
    /// field, which is what Ctrl+L is for; without it the grid is only claimed
    /// when nothing else holds the focus, so a scan finishing mid-sentence
    /// does not swallow what is being typed.
    fn focus_grid(&self, force: bool) {
        let current = gtk::prelude::GtkWindowExt::focus(&self.window);
        if !self.on_grid() {
            return;
        }
        if !force && current.is_some() {
            return;
        }
        if let Some(child) = self.flow.child_at_index(0) {
            // As in `rebuild`: through the box, so its cursor comes too. GTK
            // leaves the cursor alone for a card that already holds the focus,
            // so the focus is let go of first.
            gtk::prelude::GtkWindowExt::set_focus(&self.window, gtk::Widget::NONE);
            child.child_focus(gtk::DirectionType::Down);
        }
    }

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
                ui.start_scan();
            }) as Rc<dyn Fn()>
        };
        let reschedule = {
            let ui = Rc::clone(self);
            // Refresh settings change nothing in the grid, so this re-arms the
            // timer and updates the status line, and stops there.
            Rc::new(move || {
                // A pass under way was started with the settings as they
                // were. Switching fast-forward off, or the refresh off, while
                // the status bar reads "fetching 3/44" has to mean something
                // for the other forty-one; the next pass reads the settings
                // afresh.
                ui.cancel_refresh();
                ui.schedule_refresh();
            }) as Rc<dyn Fn()>
        };
        let restyle = {
            let ui = Rc::clone(self);
            Rc::new(move || ui.apply_theme()) as Rc<dyn Fn()>
        };
        ui::dialogs::settings(
            &self.window,
            Rc::clone(&self.cfg),
            rescan,
            reschedule,
            restyle,
        );
    }

    fn update_selection_label(&self) {
        let selected = self.selected.borrow().len();
        if selected > 0 {
            self.flow.add_css_class("om-has-selection");
        } else {
            self.flow.remove_css_class("om-has-selection");
        }
        self.contrib_content.set_label(&if selected > 0 {
            format!("Contributions ({selected})")
        } else {
            "Contributions".to_string()
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
                eprintln!("omarepos: could not write the config file: {err}");
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
        let ui = Rc::clone(self);
        let on_open: Rc<dyn Fn(PathBuf, String, Option<String>)> =
            Rc::new(move |path: PathBuf, name: String, commit: Option<String>| {
                ui.accept(&path);
                ui.nav
                    .push(&ui::graph::page(&path, &name, commit, ui.graph_host(&path)));
            });
        self.nav.push(&ui::search::page(repos, scope, on_open));
    }

    /// What a graph page of the repository at `path` reaches the window with.
    fn graph_host(self: &Rc<Self>, path: &Path) -> ui::graph::Host {
        let (detail_open, on_detail_toggled) = self.detail_prefs();
        let nav = self.nav.clone();
        let on_contributions: Rc<dyn Fn(PathBuf)> = Rc::new(move |path: PathBuf| {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| path.display().to_string());
            nav.push(&ui::stats::page(vec![path], name));
        });
        let on_pull: Rc<dyn Fn(bool, Rc<dyn Fn(bool)>)> = {
            let ui = Rc::clone(self);
            let path = path.to_path_buf();
            Rc::new(move |fast_forward, done| ui.pull_one(path.clone(), fast_forward, Some(done)))
        };
        let on_written: Rc<dyn Fn()> = {
            let ui = Rc::clone(self);
            let path = path.to_path_buf();
            Rc::new(move || {
                ui.stale.borrow_mut().insert(path.clone());
            })
        };
        let on_pushed: Rc<dyn Fn()> = {
            let ui = Rc::clone(self);
            let path = path.to_path_buf();
            Rc::new(move || {
                ui.stale.borrow_mut().insert(path.clone());
                ui.reload_graph_pages(&path);
            })
        };
        ui::graph::Host {
            detail_open,
            on_detail_toggled,
            on_terminal: self.terminal_opener(path),
            on_editor: self.editor_opener(path),
            on_contributions,
            on_pull,
            on_written,
            on_pushed,
            toasts: self.toasts.clone(),
        }
    }

    /// Fetch one repository, and fast-forward it too when asked, saying what
    /// happened: the pass the timer runs, narrowed to the repository in hand.
    /// The card's `p` and `f` come here, and so do a graph page's, which hands
    /// in `done` to hear whether the remote was reached.
    fn pull_one(
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
            let mut said = format!("{name}: nothing to do");
            let mut reached = false;
            while let Ok(event) = rx.recv().await {
                match event {
                    refresh::Event::Repo(report) => {
                        ui.note_outcome(&report);
                        reached |= report.fetched;
                        said = format!("{name}: {}", ui::outcome_text(&report.outcome));
                    }
                    refresh::Event::Done => break,
                }
            }
            ui.refreshing.set(false);
            ui.spinner.stop();
            ui.spinner.set_visible(false);
            crate::config::save_fetch_errors(&ui.fetch_errors.borrow());
            ui::say(&ui.toasts, &said);
            // This one card, drawn again whatever was read: a fetch that
            // failed changes what the card says without changing the
            // repository. The whole fleet used to be scanned to learn it.
            ui.reread(vec![path], true);
            if let Some(done) = done {
                done(reached);
            }
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
    /// back: those the app wrote to from another page, and those handed to a
    /// terminal or an editor. Nothing did this before. Commit and push in the
    /// app, press Escape, and the card still said dirty and "2 ahead" until
    /// the next rescan; with the timer off, which is how it comes, that was
    /// until Ctrl+R.
    ///
    /// `handed_too` is for a return to the window or to the grid, when the
    /// terminals have had their chance. The end of a scan only owes the
    /// repositories that were written to while it ran.
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
            ui::open_terminal(&ui.cfg.borrow(), &path)
        })
    }

    fn editor_opener(self: &Rc<Self>, path: &Path) -> Rc<dyn Fn()> {
        let ui = Rc::clone(self);
        let path = path.to_path_buf();
        Rc::new(move || {
            ui.handed.borrow_mut().insert(path.clone());
            ui::open_editor(&ui.cfg.borrow(), &path)
        })
    }

    fn open_contributions(&self, repos: Vec<PathBuf>, scope: String) {
        if repos.is_empty() {
            self.toasts
                .add_toast(ui::toast("No repositories to report on"));
            return;
        }
        self.nav.push(&ui::stats::page(repos, scope));
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
            Action::CopyPath => {
                let path = info.path.display().to_string();
                self.window.clipboard().set_text(&path);
                self.toasts
                    .add_toast(ui::toast(&format!("Copied {path}")));
            }
        }
    }
}

fn noun(n: usize, one: &'static str, many: &'static str) -> &'static str {
    if n == 1 { one } else { many }
}

/// A parsed filter: free text, plus the `tag:` and `lang:` terms pulled out of
/// it. Each kind of term is matched against its own field, so a word can only
/// match where the user aimed it.
#[derive(Default)]
struct Query {
    text: String,
    tags: Vec<String>,
    langs: Vec<String>,
}

fn split_query(raw: &str) -> Query {
    let mut words = Vec::new();
    let mut query = Query::default();
    for token in raw.split_whitespace() {
        if let Some(tag) = token.strip_prefix("tag:").filter(|t| !t.is_empty()) {
            query.tags.push(tag.to_lowercase());
        } else if let Some(lang) = token.strip_prefix("lang:").filter(|l| !l.is_empty()) {
            query.langs.push(lang.to_lowercase());
        } else {
            words.push(token.to_lowercase());
        }
    }
    query.text = words.join(" ");
    query
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
