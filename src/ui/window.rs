//! The main window: a searchable, sortable grid of repository cards.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeSet, HashSet};
use std::path::PathBuf;
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
    expected: Cell<usize>,
    /// Handle of the periodic refresh timer, so it can be rescheduled.
    refresh_source: RefCell<Option<glib::SourceId>>,
    /// Interval the live timer was armed with, so an unrelated settings change
    /// does not restart the countdown.
    refresh_armed: Cell<u64>,
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
        .tooltip_text("Rescan (Ctrl+R)  ·  fetch now (Ctrl+Shift+R)")
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

    let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
    body.append(&stack);
    body.append(&status);

    let toasts = adw::ToastOverlay::new();
    toasts.set_child(Some(&body));

    let view = adw::ToolbarView::new();
    view.add_top_bar(&header);
    view.set_content(Some(&toasts));

    let nav = adw::NavigationView::new();
    nav.add(
        &adw::NavigationPage::builder()
            .title("omarepos")
            .child(&view)
            .build(),
    );
    window.set_content(Some(&nav));

    let ui = Rc::new(Ui {
        cfg,
        meta: Rc::new(RefCell::new(Meta::load())),
        window: window.clone(),
        nav: nav.clone(),
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
        expected: Cell::new(0),
        refresh_source: RefCell::new(None),
        refresh_armed: Cell::new(0),
        refreshing: Cell::new(false),
        refresh_progress: Cell::new((0, 0)),
        last_refresh: Cell::new(None),
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
        keys.connect_key_pressed(move |controller, key, _, _| {
            nav.enter_keyboard_nav();
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
            let info = ui.shown.borrow().first().cloned();
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
        act_refresh.connect_activate(move |_, _| ui.start_scan());
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
        act_search_commits.connect_activate(move |_, _| ui.open_commit_search());
    }
    window.add_action(&act_search_commits);

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
            ui.search.grab_focus();
        });
    }
    window.add_action(&act_search);

    app.set_accels_for_action("win.refresh", &["<Ctrl>r", "F5"]);
    app.set_accels_for_action("win.search", &["<Ctrl>f", "slash"]);
    app.set_accels_for_action("win.contributions", &["<Ctrl>g"]);
    app.set_accels_for_action("win.settings", &["<Ctrl>comma"]);
    app.set_accels_for_action("win.fetch", &["<Ctrl><Shift>r"]);
    app.set_accels_for_action("win.select-all", &["<Ctrl>a"]);
    app.set_accels_for_action("win.escape", &["Escape"]);
    app.set_accels_for_action("win.search-commits", &["<Ctrl><Shift>f"]);
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
        let roots = self
            .cfg
            .borrow()
            .roots
            .iter()
            .map(|r| r.path_str().trim().to_string())
            .collect::<Vec<_>>()
            .join("  ");
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
        self.expected.set(0);
        self.repos.borrow_mut().clear();
        self.rebuild();
        self.spinner.set_visible(true);
        self.spinner.start();
        self.status.set_label("Scanning…");

        let (roots, extras, depth) = {
            let cfg = self.cfg.borrow();
            (cfg.roots(), cfg.extra_repos(), cfg.max_depth.max(1))
        };
        let rx = scan::spawn(roots, extras, depth);
        let ui = Rc::clone(self);
        glib::spawn_future_local(async move {
            // Roots that seed a tag write to the metadata store as repos come
            // in; saving once at the end keeps it to a single write.
            let mut seeded_any = false;
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
                        if let Some(seed) = seed {
                            let changed = ui.meta.borrow_mut().apply_root_tag(
                                &info.path,
                                &seed.root,
                                seed.tag.as_deref(),
                            );
                            seeded_any |= changed;
                        }
                        ui.repos.borrow_mut().push(info);
                        ui.rebuild();
                    }
                    Event::Done => {
                        // Pruning after the scan, so a repository that was just
                        // found is never a candidate for removal.
                        seeded_any |= ui.meta.borrow_mut().prune() > 0;
                        if seeded_any {
                            if let Err(err) = ui.meta.borrow().save() {
                                eprintln!("omarepos: could not write the tag file: {err}");
                            }
                        }
                        ui.spinner.stop();
                        ui.spinner.set_visible(false);
                        ui.rebuild();
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
                let key = |r: &RepoInfo| (r.ahead + r.behind, r.ahead);
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
        for info in &repos {
            let selected = self.selected.borrow().contains(&info.path);
            let tags = self.meta.borrow().tags_for(&info.path).to_vec();
            self.flow.append(&card::build(
                info,
                &tags,
                selected,
                Rc::clone(&on_action),
                Rc::clone(&on_select),
            ));
        }
        self.update_selection_label();
        self.sync_tag_controls();

        let scanning = self.spinner.is_visible();
        let page = if repos.is_empty() && !scanning { "empty" } else { "grid" };
        self.stack.set_visible_child_name(page);
        self.update_status();

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
        let ahead = repos.iter().filter(|r| r.ahead > 0).count();
        let behind = repos.iter().filter(|r| r.behind > 0).count();
        let broken = repos.iter().filter(|r| r.error.is_some()).count();

        let mut parts = vec![format!("{total} repos")];
        if self.refreshing.get() {
            let (done, of) = self.refresh_progress.get();
            parts = vec![format!("fetching {done}/{of}")];
        } else if self.spinner.is_visible() {
            let expected = self.expected.get();
            if expected > 0 {
                parts = vec![format!("scanning {total}/{expected}")];
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
        let settings = &self.cfg.borrow().refresh;
        if settings.enabled() {
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
        if !settings.enabled() {
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
    fn run_refresh(self: &Rc<Self>, manual: bool) {
        if self.refreshing.get() || self.spinner.is_visible() {
            if manual {
                self.toasts
                    .add_toast(adw::Toast::new("Already busy — try again in a moment"));
            }
            return;
        }

        let settings = self.cfg.borrow().refresh.clone();
        if !manual && !settings.will_fetch() {
            // Local-only refresh: just re-read what is on disk.
            self.start_scan();
            return;
        }

        let repos = self.refresh_scope();
        if repos.is_empty() {
            if manual {
                self.toasts
                    .add_toast(adw::Toast::new("Nothing in scope to fetch"));
            }
            return;
        }

        let total = repos.len();
        self.refreshing.set(true);
        self.refresh_progress.set((0, total));
        self.spinner.set_visible(true);
        self.spinner.start();
        self.update_status();

        let rx = refresh::spawn(
            repos,
            refresh::Options {
                fast_forward: settings.fast_forward,
                max_parallel: settings.max_parallel,
                timeout: std::time::Duration::from_secs(settings.timeout_seconds.clamp(5, 600)),
            },
        );

        let ui = Rc::clone(self);
        glib::spawn_future_local(async move {
            let mut summary = Summary::default();
            let mut done = 0usize;
            while let Ok(event) = rx.recv().await {
                match event {
                    refresh::Event::Repo(report) => {
                        done += 1;
                        ui.refresh_progress.set((done, total));
                        summary.add(&report.outcome);
                        match &report.outcome {
                            refresh::Outcome::Failed(err) => {
                                eprintln!(
                                    "omarepos: {} failed: {err} ({})",
                                    report.name,
                                    report.path.display()
                                );
                            }
                            refresh::Outcome::FastForwarded(n) => {
                                eprintln!(
                                    "omarepos: {} fast-forwarded {n} commits ({})",
                                    report.name,
                                    report.path.display()
                                );
                            }
                            refresh::Outcome::Skipped(skip) => {
                                eprintln!(
                                    "omarepos: {} left alone: {} ({})",
                                    report.name,
                                    skip.reason(),
                                    report.path.display()
                                );
                            }
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
            ui.last_refresh.set(Some(now_seconds()));
            if manual || summary.updated > 0 || summary.failed > 0 {
                ui.toasts.add_toast(adw::Toast::new(&summary.line()));
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

    /// Escape backs out of whatever is in the way, nearest first: the text you
    /// are typing, then the selection, then a filter left in the box.
    fn on_escape(self: &Rc<Self>) {
        if let Some(focused) = gtk::prelude::GtkWindowExt::focus(&self.window) {
            if let Ok(editable) = focused.dynamic_cast::<gtk::Editable>() {
                if !editable.text().is_empty() {
                    editable.set_text("");
                    return;
                }
            }
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

    fn select_all_shown(self: &Rc<Self>) {
        if let Some(focused) = gtk::prelude::GtkWindowExt::focus(&self.window) {
            if let Ok(editable) = focused.dynamic_cast::<gtk::Editable>() {
                editable.select_region(0, -1);
                return;
            }
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
                ui.schedule_refresh();
                ui.start_scan();
            }) as Rc<dyn Fn()>
        };
        let reschedule = {
            let ui = Rc::clone(self);
            // Refresh settings change nothing in the grid, so this re-arms the
            // timer and updates the status line, and stops there.
            Rc::new(move || ui.schedule_refresh()) as Rc<dyn Fn()>
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

    fn open_graph(&self, info: &RepoInfo) {
        if let Some(err) = &info.error {
            self.toasts
                .add_toast(adw::Toast::new(&format!("{}: {err}", info.name)));
            return;
        }
        let nav = self.nav.clone();
        let on_contributions: Rc<dyn Fn(PathBuf)> = Rc::new(move |path: PathBuf| {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| path.display().to_string());
            nav.push(&ui::stats::page(vec![path], name));
        });
        self.nav
            .push(&ui::graph::page(&info.path, &info.name, on_contributions));
    }

    /// Search commits across the ticked repositories, or everything shown.
    fn open_commit_search(self: &Rc<Self>) {
        let (repos, scope) = self.contribution_scope();
        if repos.is_empty() {
            self.toasts
                .add_toast(adw::Toast::new("No repositories to search"));
            return;
        }
        let nav = self.nav.clone();
        let on_open: Rc<dyn Fn(PathBuf, String)> = Rc::new(move |path: PathBuf, name: String| {
            let nav_inner = nav.clone();
            let on_contributions: Rc<dyn Fn(PathBuf)> = Rc::new(move |path: PathBuf| {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| path.display().to_string());
                nav_inner.push(&ui::stats::page(vec![path], name));
            });
            nav.push(&ui::graph::page(&path, &name, on_contributions));
        });
        self.nav.push(&ui::search::page(repos, scope, on_open));
    }

    fn open_contributions(&self, repos: Vec<PathBuf>, scope: String) {
        if repos.is_empty() {
            self.toasts
                .add_toast(adw::Toast::new("No repositories to report on"));
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
            Action::OpenTerminal => ui::open_terminal(&self.cfg.borrow(), &info.path),
            Action::OpenEditor => ui::open_editor(&self.cfg.borrow(), &info.path),
            Action::OpenFolder => ui::open_folder(&info.path),
            Action::CopyPath => {
                let path = info.path.display().to_string();
                self.window.clipboard().set_text(&path);
                self.toasts
                    .add_toast(adw::Toast::new(&format!("Copied {path}")));
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
