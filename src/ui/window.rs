//! The main window: a searchable, sortable grid of repository cards.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::gio;
use gtk::glib;
use adw::prelude::*;

use crate::config::Config;
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
    cfg: Rc<Config>,
    window: adw::ApplicationWindow,
    provider: gtk::CssProvider,
    subtitle: gtk::Label,
    search: gtk::SearchEntry,
    sort: gtk::DropDown,
    dirty_only: gtk::ToggleButton,
    spinner: gtk::Spinner,
    stack: gtk::Stack,
    flow: gtk::FlowBox,
    status: gtk::Label,
    toasts: adw::ToastOverlay,
    repos: RefCell<Vec<RepoInfo>>,
    generation: Cell<u64>,
    expected: Cell<usize>,
    monitors: RefCell<Vec<gio::FileMonitor>>,
}

pub fn build(app: &adw::Application, cfg: Rc<Config>, provider: gtk::CssProvider) {
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
    let subtitle = gtk::Label::new(None);
    subtitle.add_css_class("om-subtitle");
    title_box.append(&title);
    title_box.append(&subtitle);
    header.set_title_widget(Some(&title_box));

    let refresh = gtk::Button::builder()
        .icon_name("view-refresh-symbolic")
        .tooltip_text("Rescan (Ctrl+R)")
        .has_frame(false)
        .build();
    header.pack_start(&refresh);

    let spinner = gtk::Spinner::new();
    spinner.set_visible(false);
    header.pack_start(&spinner);

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

    let status = gtk::Label::builder().xalign(0.0).build();
    status.add_css_class("om-statusbar");

    let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
    body.append(&stack);
    body.append(&status);

    let toasts = adw::ToastOverlay::new();
    toasts.set_child(Some(&body));

    let view = adw::ToolbarView::new();
    view.add_top_bar(&header);
    view.set_content(Some(&toasts));
    window.set_content(Some(&view));

    let ui = Rc::new(Ui {
        cfg,
        window: window.clone(),
        provider,
        subtitle,
        search: search.clone(),
        sort: sort.clone(),
        dirty_only: dirty_only.clone(),
        spinner,
        stack,
        flow,
        status,
        toasts,
        repos: RefCell::new(Vec::new()),
        generation: Cell::new(0),
        expected: Cell::new(0),
        monitors: RefCell::new(Vec::new()),
    });

    // --- wiring ---------------------------------------------------------
    {
        let ui = Rc::clone(&ui);
        refresh.connect_clicked(move |_| ui.start_scan());
    }
    {
        let ui = Rc::clone(&ui);
        search.connect_search_changed(move |_| ui.rebuild());
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

    ui.apply_theme();
    ui.watch_theme();
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
            .roots
            .iter()
            .map(|r| r.trim().to_string())
            .collect::<Vec<_>>()
            .join("  ");
        self.subtitle
            .set_label(&format!("{}  ·  {}", roots, palette.name));
    }

    /// Restyle whenever Omarchy's current-theme directory changes.
    fn watch_theme(self: &Rc<Self>) {
        if !self.cfg.follow_omarchy_theme {
            return;
        }
        let dir = crate::theme::state_dir();
        let file = gio::File::for_path(&dir);
        let Ok(monitor) = file.monitor_directory(
            gio::FileMonitorFlags::WATCH_MOVES,
            gio::Cancellable::NONE,
        ) else {
            return;
        };
        let ui = Rc::clone(self);
        monitor.connect_changed(move |_, _, _, _| ui.apply_theme());
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

        let rx = scan::spawn(self.cfg.roots(), self.cfg.max_depth.max(1));
        let ui = Rc::clone(self);
        glib::spawn_future_local(async move {
            while let Ok(event) = rx.recv().await {
                if ui.generation.get() != generation {
                    break;
                }
                match event {
                    Event::Discovered(n) => {
                        ui.expected.set(n);
                        ui.update_status();
                    }
                    Event::Repo(info) => {
                        ui.repos.borrow_mut().push(info);
                        ui.rebuild();
                    }
                    Event::Done => {
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
        let query = self.search.text().trim().to_lowercase();
        let dirty_only = self.dirty_only.is_active();
        let mut repos: Vec<RepoInfo> = self
            .repos
            .borrow()
            .iter()
            .filter(|r| !dirty_only || Self::needs_attention(r))
            .filter(|r| query.is_empty() || r.haystack().contains(&query))
            .cloned()
            .collect();

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
        while let Some(child) = self.flow.first_child() {
            self.flow.remove(&child);
        }

        let repos = self.visible();
        let ui = Rc::clone(self);
        let on_action: Rc<dyn Fn(Action, &RepoInfo)> =
            Rc::new(move |action, info| ui.run_action(action, info));

        for info in &repos {
            self.flow.append(&card::build(info, Rc::clone(&on_action)));
        }

        let scanning = self.spinner.is_visible();
        let page = if repos.is_empty() && !scanning { "empty" } else { "grid" };
        self.stack.set_visible_child_name(page);
        self.update_status();
    }

    fn update_status(&self) {
        let repos = self.repos.borrow();
        let total = repos.len();
        let dirty = repos.iter().filter(|r| !r.tree.is_clean()).count();
        let ahead = repos.iter().filter(|r| r.ahead > 0).count();
        let behind = repos.iter().filter(|r| r.behind > 0).count();
        let broken = repos.iter().filter(|r| r.error.is_some()).count();

        let mut parts = vec![format!("{total} repos")];
        if self.spinner.is_visible() {
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
        self.status.set_label(&parts.join("  ·  "));
    }

    fn run_action(&self, action: Action, info: &RepoInfo) {
        match action {
            Action::OpenTerminal => ui::open_terminal(&self.cfg, &info.path),
            Action::OpenEditor => ui::open_editor(&self.cfg, &info.path),
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
