//! The contributions page: who committed what, over the last N days, across a
//! set of repositories.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use gtk::pango;
use gtk::{gdk, gio, glib};

use crate::stats::{self, Author, Branches, Contributions, RepoCommits};
use crate::ui::arcade::{self, Arcade};
use crate::ui::{key_color, relative_time};

/// The two views, the one there has always been first.
const VIEWS: [(&str, Branches); 2] =
    [("Checked-out branches", Branches::CheckedOut), ("All branches", Branches::All)];
const BAR_H: i32 = 46;
/// The space between the list and who is set aside under it: enough to
/// stop a screenshot above, so that it shows nothing of them. It was twice
/// this, and on a tall window it read as the end of the page.
const GAP: i32 = 110;
const SPARK_W: f64 = 11.0;

/// What opens a commit from the page: its repository, what to call it, and
/// the commit.
pub type OnOpen = Rc<dyn Fn(PathBuf, String, Option<String>)>;

/// What searches the commits of some repositories, with what to call them.
pub type OnSearch = Rc<dyn Fn(Vec<PathBuf>, String)>;
/// What runs the race (`ui::race`) over what this page is showing.
pub type OnRace = Rc<dyn Fn(crate::ui::race::Field)>;

/// `scope` is shown as the page subtitle, e.g. "4 selected repos".
pub fn page(
    repos: Vec<PathBuf>,
    scope: String,
    on_open: OnOpen,
    on_search: OnSearch,
    on_race: OnRace,
) -> adw::NavigationPage {
    // The subtitle also says which commits are counted. The totals mean one
    // thing for the branch that is checked out and another for every branch
    // there is, and the page gave no way to tell which it was showing. It is
    // written by `load`, which knows which was asked for.
    let title = adw::WindowTitle::new("Contributions", &scope);
    // For the commit search and the race, which are of the same repositories.
    let search_scope = scope.clone();
    let race_scope = scope.clone();
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&title));

    let range = gtk::DropDown::from_strings(&crate::ui::periods().map(|(label, _)| label));
    range.set_selected(crate::ui::DEFAULT_PERIOD);
    range.add_css_class("om-sort");
    range.set_tooltip_text(Some("Time window"));
    header.pack_end(&range);

    // Which branches: the line of history each repository has checked out,
    // or every branch it has. Two views of one thing and a switch between
    // them, because neither can be both: the first counts a change once and
    // cannot see a branch nobody has checked out, the second sees every
    // branch and counts a change once for each copy of it.
    let view = gtk::DropDown::from_strings(&VIEWS.map(|(label, _)| label));
    view.add_css_class("om-sort");
    view.set_tooltip_text(Some("Which branches are counted"));
    header.pack_end(&view);

    let race = gtk::Button::from_icon_name("media-playback-start-symbolic");
    race.set_tooltip_text(Some("Race: authors' commits, day by day (r)"));
    race.set_action_name(Some("page.race"));
    header.pack_end(&race);

    let spinner = gtk::Spinner::new();
    spinner.start();
    let progress = gtk::Label::new(Some("Reading history…"));
    progress.add_css_class("om-meta");
    let loading = gtk::Box::new(gtk::Orientation::Vertical, 10);
    loading.set_valign(gtk::Align::Center);
    loading.set_halign(gtk::Align::Center);
    loading.append(&spinner);
    loading.append(&progress);

    let content = gtk::Box::new(gtk::Orientation::Vertical, 14);
    content.set_margin_top(16);
    content.set_margin_bottom(18);
    content.set_margin_start(16);
    content.set_margin_end(16);

    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&content)
        .build();
    scroller.add_css_class("om-grid");

    let empty = adw::StatusPage::builder().icon_name("system-users-symbolic").title("No commits").build();

    let stack = gtk::Stack::new();
    stack.add_named(&loading, Some("loading"));
    stack.add_named(&scroller, Some("content"));
    stack.add_named(&empty, Some("empty"));
    stack.set_vexpand(true);

    let status = gtk::Label::builder().xalign(0.0).ellipsize(pango::EllipsizeMode::End).build();
    status.add_css_class("om-statusbar");

    let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
    body.append(&stack);
    body.append(&status);

    let frame = adw::ToolbarView::new();
    frame.add_top_bar(&header);
    frame.set_content(Some(&body));

    let page = adw::NavigationPage::builder().title("Contributions").child(&frame).build();

    // A generation counter so a slow run that the user has already moved on
    // from cannot overwrite the results of the one they are waiting for.
    let generation = Rc::new(RefCell::new(0u64));
    let repos = Rc::new(repos);

    // The arcade (`ui::arcade`): what it is shown, and what the page shows
    // when there is no game on.
    let game = Game::default();

    // Who is set aside: listed last, after a gap, and out of the figures
    // above. Remembered between sessions and across pages, since it is
    // mostly oneself, for a picture of what the others did.
    let aside = Rc::new(RefCell::new(crate::config::load_set_aside()));
    let rows = Rc::new(Rows::default());

    let load: Rc<dyn Fn()> = {
        let generation = Rc::clone(&generation);
        let repos = Rc::clone(&repos);
        let stack = stack.clone();
        let content = content.clone();
        let scroller = scroller.clone();
        let empty = empty.clone();
        let status = status.clone();
        let progress = progress.clone();
        let (range, view) = (range.clone(), view.clone());
        let title = title.clone();
        let game = game.clone();
        let aside = Rc::clone(&aside);
        let rows = Rc::clone(&rows);
        let on_open = Rc::clone(&on_open);
        Rc::new(move || {
            let span = crate::ui::periods()
                .get(range.selected() as usize)
                .map_or(crate::ui::Span::Days(7), |(_, span)| *span);
            let branches =
                VIEWS.get(view.selected() as usize).map_or(Branches::CheckedOut, |(_, branches)| *branches);
            let (says, means) = counted(branches, repos.len());
            title.set_subtitle(&format!("{scope} · {says}"));
            title.set_tooltip_text(Some(means));

            let mine = {
                let mut counter = generation.borrow_mut();
                *counter += 1;
                *counter
            };
            game.rest(&stack, "loading");
            game.show(None);
            status.set_label("Reading history…");
            status.set_tooltip_text(None);
            // A new period or view starts with every row closed, as it did.
            rows.expanded.borrow_mut().clear();

            let rx = stats::spawn((*repos).clone(), span.window(), branches);
            let generation = Rc::clone(&generation);
            let stack = stack.clone();
            let content = content.clone();
            let scroller = scroller.clone();
            let empty = empty.clone();
            let status = status.clone();
            let progress = progress.clone();
            let on_open = Rc::clone(&on_open);
            let game = game.clone();
            let aside = Rc::clone(&aside);
            let rows = Rc::clone(&rows);
            glib::spawn_future_local(async move {
                // Everything but the lines comes first, and the page goes up
                // with it; the lines follow, and the page is put together
                // again around them. It keeps where it was scrolled to, which
                // rows were open, and the keyboard on the author it was on:
                // a year across a fleet takes a while to diff, and the page
                // is in use by then.
                let show = |result: Box<Contributions>| {
                    let result: Rc<Contributions> = Rc::from(result);
                    game.show(Some(Rc::clone(&result)));
                    let offset = scroller.vadjustment().value();
                    let had = rows.focused_author(&content);
                    while let Some(child) = content.first_child() {
                        content.remove(&child);
                    }
                    if result.total_commits == 0 {
                        let (title, description) = empty_state(&result);
                        empty.set_title(title);
                        // The description is markup, and what goes in it now
                        // includes paths and what libgit2 said.
                        empty.set_description(Some(&glib::markup_escape_text(&description)));
                        game.rest(&stack, "empty");
                    } else {
                        fill(&content, &result, &aside.borrow(), &on_open, &rows);
                        game.rest(&stack, "content");
                        if let Some(head) = had.and_then(|key| rows.head(&key)) {
                            head.grab_focus();
                        }
                        if offset > 0.0 {
                            let adjustment = scroller.vadjustment();
                            glib::timeout_add_local_once(std::time::Duration::from_millis(16), move || {
                                adjustment.set_value(offset);
                            });
                        }
                    }
                    status.set_label(&status_line(&result, &aside.borrow()));
                    // The label is one line and cut short when the window is
                    // narrow; the reasons are many lines.
                    status.set_tooltip_text(
                        (!result.errors.is_empty()).then(|| result.errors.join("\n")).as_deref(),
                    );
                };
                while let Ok(event) = rx.recv().await {
                    if *generation.borrow() != mine {
                        break;
                    }
                    match event {
                        stats::Event::Progress(name, done, total) => {
                            progress.set_label(&format!("{name}  ({done}/{total})"));
                        }
                        stats::Event::Counted(result) => show(result),
                        stats::Event::Done(result) => {
                            show(result);
                            break;
                        }
                    }
                }
            });
        })
    };

    for dropdown in [&range, &view] {
        let load = Rc::clone(&load);
        dropdown.connect_selected_notify(move |_| load());
    }
    load();

    // How the window and the rows reach the page. A row's button for setting
    // its author aside, or bringing them back, names them by the key the
    // figures are kept under; the page is put together again from what was
    // read, and nothing is read again. The commit search key searches the
    // repositories this page is of.
    let actions = gio::SimpleActionGroup::new();
    let search = gio::SimpleAction::new("search-commits", None);
    {
        let repos = Rc::clone(&repos);
        search.connect_activate(move |_, _| on_search((*repos).clone(), search_scope.clone()));
    }
    actions.add_action(&search);
    // The race is of this page as it stands: its repositories, the branches
    // it counts, and without whoever it has set aside.
    let run_race = gio::SimpleAction::new("race", None);
    {
        let (repos, aside, view, range) = (Rc::clone(&repos), Rc::clone(&aside), view.clone(), range.clone());
        run_race.connect_activate(move |_, _| {
            let branches =
                VIEWS.get(view.selected() as usize).map_or(Branches::CheckedOut, |(_, branches)| *branches);
            on_race(crate::ui::race::Field {
                repos: (*repos).clone(),
                scope: race_scope.clone(),
                branches,
                aside: aside.borrow().clone(),
                period: range.selected(),
            });
        });
    }
    actions.add_action(&run_race);
    let set_aside = gio::SimpleAction::new("set-aside", Some(glib::VariantTy::STRING));
    {
        let (content, status) = (content.clone(), status.clone());
        let latest = Rc::clone(&game.latest);
        set_aside.connect_activate(move |_, who| {
            let Some(who) = who.and_then(|who| who.str()) else { return };
            {
                let mut aside = aside.borrow_mut();
                let latest = latest.borrow();
                let authors = latest.as_deref().map_or(&[][..], |result| &result.authors[..]);
                // Brought back: every address of theirs that was set aside,
                // since any one of them keeps them all aside by name.
                match authors.iter().find(|author| author.key() == who) {
                    Some(author) if set_aside_in(authors, &aside)(author) => {
                        let name = author.name.to_lowercase();
                        for same in authors.iter().filter(|other| other.name.to_lowercase() == name) {
                            aside.remove(&same.key());
                        }
                        aside.remove(who);
                    }
                    _ => {
                        if !aside.remove(who) {
                            aside.insert(who.to_string());
                        }
                    }
                }
                crate::config::save_set_aside(&aside);
            }
            if let Some(result) = latest.borrow().as_deref().filter(|result| result.total_commits > 0) {
                fill(&content, result, &aside.borrow(), &on_open, &rows);
                status.set_label(&status_line(result, &aside.borrow()));
            }
        });
    }
    actions.add_action(&set_aside);
    page.insert_action_group("page", Some(&actions));

    // `r` for the race, from anywhere on the page.
    let race_key = gtk::EventControllerKey::new();
    race_key.connect_key_pressed(|controller, key, _, state| {
        if key != gdk::Key::r || !crate::ui::unmodified(state) {
            return glib::Propagation::Proceed;
        }
        if let Some(page) = controller.widget() {
            let _ = page.activate_action("page.race", None);
        }
        glib::Propagation::Stop
    });
    page.add_controller(race_key);

    // The code is watched for on the way down to whatever has the keyboard,
    // and every key goes on to it: the page works as it would without.
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    let konami = RefCell::new(arcade::Konami::default());
    let weak_page = page.downgrade();
    keys.connect_key_pressed(move |_, key, _, state| {
        if game.playing.get() || !konami.borrow_mut().press(key, state) {
            return glib::Propagation::Proceed;
        }
        if let Some(page) = weak_page.upgrade() {
            game.start(&page, &stack, &range);
        }
        glib::Propagation::Stop
    });
    page.add_controller(keys);

    page
}

/// The arcade, from the page's side.
#[derive(Clone, Default)]
struct Game {
    /// Made the first time the code is typed.
    arcade: Rc<RefCell<Option<Rc<Arcade>>>>,
    playing: Rc<Cell<bool>>,
    /// The latest figures, for a game begun after they were read.
    latest: Rc<RefCell<Option<Rc<Contributions>>>>,
    /// What the page shows with no game on: "loading", "content" or "empty".
    resting: Rc<Cell<&'static str>>,
    /// Where the keyboard was when the game began, to go back to.
    before: Rc<RefCell<glib::WeakRef<gtk::Widget>>>,
}

impl Game {
    /// Show `name` on the page, or keep it for after the game.
    fn rest(&self, stack: &gtk::Stack, name: &'static str) {
        self.resting.set(name);
        if !self.playing.get() {
            stack.set_visible_child_name(name);
        }
    }

    fn show(&self, result: Option<Rc<Contributions>>) {
        self.latest.replace(result.clone());
        if let Some(arcade) = self.arcade.borrow().as_ref() {
            arcade.show(result);
        }
    }

    fn start(&self, page: &adw::NavigationPage, stack: &gtk::Stack, range: &gtk::DropDown) {
        let arcade = Rc::clone(self.arcade.borrow_mut().get_or_insert_with(|| {
            arcade::load_font(page);
            // Left and right are the time window's, a step either way round.
            let on_range = {
                let range = range.clone();
                move |by: i32| {
                    let ranges = crate::ui::periods().len() as i32;
                    range.set_selected((range.selected() as i32 + by).rem_euclid(ranges) as u32);
                }
            };
            // Not the whole of `self`, which holds the arcade that holds this.
            let on_quit = {
                let (playing, resting) = (Rc::clone(&self.playing), Rc::clone(&self.resting));
                let before = Rc::clone(&self.before);
                let stack = stack.downgrade();
                move || {
                    playing.set(false);
                    if let Some(stack) = stack.upgrade() {
                        stack.set_visible_child_name(resting.get());
                        let before = before.borrow().upgrade();
                        match before.filter(|widget| widget.is_mapped()) {
                            Some(widget) => widget.grab_focus(),
                            None => stack.grab_focus(),
                        };
                    }
                }
            };
            let arcade = Arcade::new(on_range, on_quit);
            stack.add_named(arcade.widget(), Some("arcade"));
            arcade
        }));
        let before = page.root().and_then(|root| root.focus());
        self.before.replace(before.map(|widget| widget.downgrade()).unwrap_or_default());
        self.playing.set(true);
        arcade.show(self.latest.borrow().clone());
        stack.set_visible_child_name("arcade");
        arcade.insert_coin();
    }
}

/// The author rows as last put on the page: which are open, and the head of
/// each, by the key the author is counted under. The page is put together
/// again when the lines arrive, and again when somebody is set aside, and a
/// row is found again by its author.
#[derive(Default)]
struct Rows {
    expanded: RefCell<BTreeSet<String>>,
    heads: RefCell<HashMap<String, glib::WeakRef<gtk::Button>>>,
}

impl Rows {
    fn head(&self, key: &str) -> Option<gtk::Button> {
        self.heads.borrow().get(key).and_then(|head| head.upgrade())
    }

    /// Whose row the keyboard is in, on the head or in the commits under it.
    fn focused_author(&self, content: &gtk::Box) -> Option<String> {
        let focus = content.root()?.focus()?;
        if !focus.is_ancestor(content) {
            return None;
        }
        let row = focus.ancestor(gtk::Box::static_type()).and_then(|mut widget| {
            while !widget.has_css_class("om-stat-row") {
                widget = widget.parent()?;
            }
            Some(widget)
        })?;
        self.heads
            .borrow()
            .iter()
            .find(|(_, head)| head.upgrade().is_some_and(|head| head.is_ancestor(&row)))
            .map(|(key, _)| key.clone())
    }
}

/// What a view counts: in a word or two for the subtitle, and in full.
fn counted(branches: Branches, repos: usize) -> (&'static str, &'static str) {
    match branches {
        Branches::CheckedOut => (
            if repos == 1 { "checked-out branch" } else { "checked-out branches" },
            "Counts the branch each repository has checked out and the branch it tracks.\n\
             Other branches are left out, so that work which was squashed, rebased or amended\n\
             is not counted once for every copy of it.",
        ),
        Branches::All => (
            "all branches",
            "Counts every branch, a repository's own and the ones it has fetched. \
             Work that was squashed, rebased or amended is counted once for each copy of it, \
             so read this as who is busy where, and not as a tally.",
        ),
    }
}

/// Who among `authors` is set aside: whoever's address is in `aside`, and
/// whoever else goes by one of their names. One person committing from a
/// second address (another machine, a forge's no-reply one) is a second
/// author here, and with only one of them set aside the other was still on
/// the page and in the race.
pub(crate) fn set_aside_in(authors: &[Author], aside: &BTreeSet<String>) -> impl Fn(&Author) -> bool {
    let names: BTreeSet<String> = authors
        .iter()
        .filter(|author| aside.contains(&author.key()))
        .map(|author| author.name.to_lowercase())
        .collect();
    let aside = aside.clone();
    move |author| aside.contains(&author.key()) || names.contains(&author.name.to_lowercase())
}

/// The page, from what was read: the overview of everybody not set aside,
/// their rows, and under a gap whoever is.
fn fill(
    content: &gtk::Box,
    result: &Contributions,
    aside: &BTreeSet<String>,
    on_open: &OnOpen,
    rows: &Rc<Rows>,
) {
    while let Some(child) = content.first_child() {
        content.remove(&child);
    }
    rows.heads.borrow_mut().clear();
    if !result.errors.is_empty() {
        content.append(&not_counted(&result.errors));
    }
    // Said on the page and not only in a tooltip: these figures are not the
    // other view's, and read as a tally they are too high.
    if result.branches == Branches::All {
        let (_, means) = counted(result.branches, result.repos_scanned);
        let note = gtk::Label::builder().label(means).xalign(0.0).wrap(true).build();
        note.add_css_class("om-meta");
        content.append(&note);
    }
    let is_aside = set_aside_in(&result.authors, aside);
    let (listed, set_aside): (Vec<&Author>, Vec<&Author>) =
        result.authors.iter().partition(|author| !is_aside(author));
    let drawn = Drawn::of(result);
    content.append(&overview(&Figures::of(result, &listed), &drawn, result.lines_pending));
    for author in &listed {
        content.append(&author_row(author, result, on_open, false, &drawn, rows));
    }
    if set_aside.is_empty() {
        return;
    }
    let gap = gtk::Box::new(gtk::Orientation::Vertical, 0);
    gap.set_size_request(-1, GAP);
    content.append(&gap);
    let heading = gtk::Label::builder()
        .label(format!(
            "Set aside, and not in the figures above: {} {}",
            thousands(set_aside.iter().map(|author| author.commits).sum()),
            plural(set_aside.iter().map(|author| author.commits).sum(), "commit", "commits"),
        ))
        .xalign(0.0)
        .wrap(true)
        .build();
    heading.add_css_class("om-meta");
    content.append(&heading);
    for author in &set_aside {
        content.append(&author_row(author, result, on_open, true, &drawn, rows));
    }
}

/// The overview's figures: those of the authors listed, which is every
/// author until somebody is set aside. Each commit is one author's, so these
/// add up to what was counted.
#[derive(Debug, PartialEq, Eq)]
struct Figures {
    commits: usize,
    authors: usize,
    repos_active: usize,
    insertions: usize,
    deletions: usize,
    per_day: Vec<usize>,
}

impl Figures {
    fn of(result: &Contributions, listed: &[&Author]) -> Figures {
        let mut per_day = vec![0; result.per_day_total.len()];
        let mut repos = BTreeSet::new();
        for author in listed {
            for (total, day) in per_day.iter_mut().zip(&author.per_day) {
                *total += day;
            }
            repos.extend(author.repos.keys());
        }
        Figures {
            commits: listed.iter().map(|author| author.commits).sum(),
            authors: listed.len(),
            repos_active: repos.len(),
            insertions: listed.iter().map(|author| author.insertions).sum(),
            deletions: listed.iter().map(|author| author.deletions).sum(),
            per_day,
        }
    }
}

fn status_line(result: &Contributions, aside: &BTreeSet<String>) -> String {
    let mut parts = Vec::new();
    // First, where a narrow window cuts off something else. It used to be
    // last, and was the only sign on the page that a repository had been left
    // out of every figure before it.
    if !result.errors.is_empty() {
        parts.push(format!("{} unreadable", result.errors.len()));
    }
    parts.extend([
        format!("{} commits", thousands(result.total_commits)),
        // Authors, not people: the count is of names commits are signed with,
        // and a robot has one and a person may have two.
        format!("{} {}", result.authors.len(), plural(result.authors.len(), "author", "authors")),
        format!("{}/{} repos active", result.repos_active, result.repos_scanned),
    ]);
    if result.lines_pending {
        parts.push("counting lines…".to_string());
    } else {
        parts.push(format!("+{} −{}", thousands(result.total_insertions), thousands(result.total_deletions)));
        if result.uncounted > 0 {
            parts.push(format!("{} without line counts", thousands(result.uncounted)));
        }
    }
    if result.future > 0 {
        parts.push(format!("{} dated in the future", thousands(result.future)));
    }
    // The bar keeps everybody's figures; the page above says how many are
    // not in its own.
    let is_aside = set_aside_in(&result.authors, aside);
    let set_aside = result.authors.iter().filter(|author| is_aside(author)).count();
    if set_aside > 0 {
        parts.push(format!("{set_aside} set aside"));
    }
    parts.join("  ·  ")
}

/// What the page says when it has no commit to show: a title, and a
/// description in plain text.
///
/// It used to say "Nothing was committed … across 3 repositories" of three
/// repositories none of which could be opened. One that was not read is not
/// one where nothing happened, so it is not counted among those, and it is
/// named with the reason.
fn empty_state(result: &Contributions) -> (&'static str, String) {
    let unread = result.errors.len();
    let read = result.repos_scanned.saturating_sub(unread);
    if read == 0 && unread > 0 {
        return ("Nothing could be read", result.errors.join("\n"));
    }
    let mut description = format!(
        "Nothing was committed in the last {} days across {} {}.",
        result.window.days(),
        read,
        plural(read, "repository", "repositories"),
    );
    if result.future > 0 {
        description.push_str(&format!(
            "\n\n{} {} dated in the future and not counted.",
            thousands(result.future),
            plural(result.future, "commit is", "commits are"),
        ));
    }
    if unread > 0 {
        description.push_str(&format!("\n\n{unread} more could not be read:\n{}", result.errors.join("\n")));
    }
    ("No commits", description)
}

/// The repositories that are in none of the figures below, and why. The
/// reasons were collected and never shown: all the page had was a count at the
/// end of the status bar.
fn not_counted(errors: &[String]) -> gtk::Widget {
    let card = gtk::Box::new(gtk::Orientation::Vertical, 4);
    card.add_css_class("om-card");

    let heading = gtk::Label::builder()
        .label(format!(
            "{} {} could not be read and {} not counted",
            errors.len(),
            plural(errors.len(), "repository", "repositories"),
            plural(errors.len(), "is", "are"),
        ))
        .xalign(0.0)
        .build();
    heading.add_css_class("om-summary");
    card.append(&heading);

    for error in errors {
        let reason = gtk::Label::builder()
            .label(error)
            .xalign(0.0)
            .wrap(true)
            .wrap_mode(pango::WrapMode::WordChar)
            .build();
        reason.add_css_class("om-meta");
        card.append(&reason);
    }

    card.upcast()
}

/// Totals, plus a bar per day across every author.
/// How the days are drawn on the page: as they are, or a week or a month to
/// a column (`ui::buckets`), and the busiest any author had one of them,
/// which every sparkline is drawn against so that rows can be compared.
struct Drawn {
    buckets: Vec<crate::ui::Bucket>,
    peak: usize,
}

impl Drawn {
    fn of(result: &Contributions) -> Drawn {
        let buckets = crate::ui::buckets(&result.window);
        let peak = result
            .authors
            .iter()
            .flat_map(|author| buckets.iter().map(|bucket| in_bucket(&author.per_day, bucket)))
            .max()
            .unwrap_or(0)
            .max(1);
        Drawn { buckets, peak }
    }
}

/// What a day-by-day count comes to over a bucket's days.
fn in_bucket(per_day: &[usize], bucket: &crate::ui::Bucket) -> usize {
    per_day.get(bucket.days.clone()).map_or(0, |days| days.iter().sum())
}

fn overview(figures: &Figures, drawn: &Drawn, lines_pending: bool) -> gtk::Widget {
    let card = gtk::Box::new(gtk::Orientation::Vertical, 12);
    card.add_css_class("om-card");
    // Not +0 while the lines are being counted: nothing has been added up yet.
    let lines = |sign: &str, n: usize| {
        if lines_pending { format!("{sign}…") } else { format!("{sign}{}", thousands(n)) }
    };

    let numbers = gtk::Box::new(gtk::Orientation::Horizontal, 26);
    numbers.append(&big_number(&thousands(figures.commits), plural(figures.commits, "commit", "commits")));
    numbers.append(&big_number(&figures.authors.to_string(), plural(figures.authors, "author", "authors")));
    numbers.append(&big_number(&figures.repos_active.to_string(), "active repos"));
    numbers.append(&big_number(&lines("+", figures.insertions), "insertions"));
    numbers.append(&big_number(&lines("−", figures.deletions), "deletions"));
    card.append(&numbers);

    let counts: Vec<usize> = drawn.buckets.iter().map(|bucket| in_bucket(&figures.per_day, bucket)).collect();
    let peak = counts.iter().copied().max().unwrap_or(0).max(1);
    let days = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    days.set_homogeneous(true);
    for (bucket, count) in drawn.buckets.iter().zip(&counts) {
        let column = gtk::Box::new(gtk::Orientation::Vertical, 4);

        let bar = gtk::DrawingArea::builder().content_height(BAR_H).hexpand(true).build();
        let (count, peak) = (*count, peak);
        bar.set_draw_func(move |_, cr, width, height| {
            let w = width as f64;
            let h = height as f64;
            let (r, g, b) = crate::theme::Palette::rgb(crate::ui::accent());
            // A faint track, so empty days are still legible as days.
            cr.set_source_rgba(r, g, b, 0.12);
            cr.rectangle(0.0, 0.0, w, h);
            let _ = cr.fill();
            if count > 0 {
                let filled = (count as f64 / peak as f64).clamp(0.06, 1.0) * h;
                cr.set_source_rgb(r, g, b);
                cr.rectangle(0.0, h - filled, w, filled);
                let _ = cr.fill();
            }
        });
        column.append(&bar);

        let value = gtk::Label::new(Some(&count.to_string()));
        value.add_css_class("om-summary");
        column.append(&value);

        let label = gtk::Label::new(Some(&bucket.label));
        label.add_css_class("om-meta");
        column.append(&label);

        column.set_tooltip_text(Some(&format!(
            "{} · {count} {}",
            bucket.title,
            plural(count, "commit", "commits")
        )));
        days.append(&column);
    }
    card.append(&days);

    card.upcast()
}

fn big_number(value: &str, caption: &str) -> gtk::Box {
    let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let value = gtk::Label::builder().label(value).xalign(0.0).build();
    value.add_css_class("om-big");
    let caption = gtk::Label::builder().label(caption).xalign(0.0).build();
    caption.add_css_class("om-meta");
    column.append(&value);
    column.append(&caption);
    column
}

fn author_row(
    author: &Author,
    result: &Contributions,
    on_open: &OnOpen,
    set_aside: bool,
    drawn: &Drawn,
    rows: &Rc<Rows>,
) -> gtk::Widget {
    let card = gtk::Box::new(gtk::Orientation::Vertical, 0);
    card.add_css_class("om-card");
    card.add_css_class("om-stat-row");
    if set_aside {
        card.add_css_class("om-set-aside");
    }

    let head = gtk::Box::new(gtk::Orientation::Horizontal, 14);

    let chevron = gtk::Image::from_icon_name("pan-end-symbolic");
    chevron.set_valign(gtk::Align::Center);
    head.append(&chevron);

    let color = key_color(if author.email.is_empty() { &author.name } else { &author.email });
    head.append(&swatch(color));

    let who = gtk::Box::new(gtk::Orientation::Vertical, 2);
    who.set_hexpand(true);
    let name =
        gtk::Label::builder().label(&author.name).xalign(0.0).ellipsize(pango::EllipsizeMode::End).build();
    name.add_css_class("om-name");
    who.append(&name);

    let mut repos: Vec<(&PathBuf, &RepoCommits)> = author.repos.iter().collect();
    repos.sort_by(|a, b| {
        b.1.total.cmp(&a.1.total).then_with(|| a.1.name.cmp(&b.1.name)).then_with(|| a.0.cmp(b.0))
    });
    let repo_line = repos
        .iter()
        .take(4)
        .map(|(_, in_repo)| format!("{} {}", in_repo.name, in_repo.total))
        .collect::<Vec<_>>()
        .join("  ·  ");
    let extra = repos.len().saturating_sub(4);
    let mut line = if extra > 0 { format!("{repo_line}  ·  +{extra} more") } else { repo_line };
    // Authors are keyed by e-mail, so the same display name can appear twice.
    // When it does, say which address each row is.
    let ambiguous = result.authors.iter().filter(|a| a.name.eq_ignore_ascii_case(&author.name)).count() > 1;
    if ambiguous && !author.email.is_empty() {
        line = format!("{}  ·  {line}", author.email);
    }
    let detail = gtk::Label::builder().label(line).xalign(0.0).ellipsize(pango::EllipsizeMode::End).build();
    detail.add_css_class("om-meta");
    who.append(&detail);
    head.append(&who);

    let counts = gtk::Box::new(gtk::Orientation::Vertical, 2);
    counts.set_halign(gtk::Align::End);
    let commits = gtk::Label::builder()
        .label(format!("{} {}", author.commits, plural(author.commits, "commit", "commits")))
        .xalign(1.0)
        .build();
    commits.add_css_class("om-summary");
    counts.append(&commits);

    let mut lines = format!("+{} −{}", thousands(author.insertions), thousands(author.deletions));
    // Somebody whose commits could not be diffed is not somebody who changed
    // nothing, which is what a bare +0 −0 said of them. Nor is somebody whose
    // commits have not been diffed yet.
    if result.lines_pending {
        lines = "counting lines…".to_string();
    } else if author.uncounted > 0 {
        lines.push_str(&format!(" and {} unread", thousands(author.uncounted)));
    }
    let churn = gtk::Label::builder().label(lines).xalign(1.0).build();
    churn.add_css_class("om-meta");
    counts.append(&churn);
    head.append(&counts);

    head.append(&sparkline(author, drawn, color));

    // A button rather than a click gesture, so the row can be opened from the
    // keyboard like anything else.
    let toggle = gtk::Button::builder().child(&head).hexpand(true).build();
    toggle.add_css_class("om-stat-head");
    toggle.set_tooltip_text(Some(&tooltip(author, result.lines_pending)));
    rows.heads.borrow_mut().insert(author.key(), toggle.downgrade());

    // Beside the row and not in it, so it is a stop of its own for the
    // keyboard. Out of sight until the row is pointed at or the keyboard
    // gets to it, so that a picture of the page does not show one on every
    // row.
    let (icon, says) = if set_aside {
        ("go-top-symbolic", "Bring back into the list and the figures")
    } else {
        ("go-bottom-symbolic", "Set aside: under the list, after a gap, and out of the figures above it")
    };
    let aside = gtk::Button::from_icon_name(icon);
    aside.add_css_class("om-stat-aside");
    aside.set_valign(gtk::Align::Center);
    aside.set_tooltip_text(Some(says));
    aside.set_action_name(Some("page.set-aside"));
    aside.set_action_target_value(Some(&author.key().to_variant()));

    let top = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    top.append(&toggle);
    top.append(&aside);
    card.append(&top);

    let open = rows.expanded.borrow().contains(&author.key());
    let revealer = gtk::Revealer::builder()
        .transition_type(gtk::RevealerTransitionType::SlideDown)
        .transition_duration(160)
        .child(&commit_list(&repos, on_open, &toggle, result.lines_pending))
        .reveal_child(open)
        .build();
    card.append(&revealer);
    let chevron_for = |open: bool| if open { "pan-down-symbolic" } else { "pan-end-symbolic" };
    chevron.set_icon_name(Some(chevron_for(open)));

    let key = author.key();
    let rows = Rc::downgrade(rows);
    toggle.connect_clicked(move |_| {
        let opening = !revealer.reveals_child();
        revealer.set_reveal_child(opening);
        chevron.set_icon_name(Some(chevron_for(opening)));
        if let Some(rows) = rows.upgrade() {
            let mut expanded = rows.expanded.borrow_mut();
            if opening {
                expanded.insert(key.clone());
            } else {
                expanded.remove(&key);
            }
        }
    });

    card.upcast()
}

fn tooltip(author: &Author, lines_pending: bool) -> String {
    let mut tip = format!(
        "{} <{}>\n{} commits",
        author.name,
        if author.email.is_empty() { "no e-mail" } else { &author.email },
        author.commits
    );
    if author.merges > 0 {
        tip.push_str(&format!("\n{} of them merges (not counted in lines)", author.merges));
    }
    if lines_pending {
        tip.push_str("\nTheir lines are still being counted");
    } else if author.uncounted > 0 {
        tip.push_str(&format!(
            "\n{} of them with lines that could not be read (not counted in lines)",
            author.uncounted
        ));
    }
    // Not "files touched": the figure is summed commit by commit, so ten
    // commits to one file are ten.
    tip.push_str(&format!(
        "\n{} file changes\nlast commit {}",
        thousands(author.files),
        relative_time(author.last_seen)
    ));
    tip
}

/// The author's commits, grouped by repository, busiest first. `head` is
/// the row they open from, which Up from the first commit goes back to.
fn commit_list(
    repos: &[(&PathBuf, &RepoCommits)],
    on_open: &OnOpen,
    head: &gtk::Button,
    lines_pending: bool,
) -> gtk::Box {
    let list = gtk::Box::new(gtk::Orientation::Vertical, 10);
    list.set_margin_top(12);
    list.set_margin_start(26);
    let mut groups: Vec<gtk::ListBox> = Vec::new();

    for (path, in_repo) in repos {
        let group = gtk::Box::new(gtk::Orientation::Vertical, 2);

        // The name is the folder's, lengthened where two repositories share
        // one; the tooltip says where the repository is in any case.
        let heading = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        heading.set_tooltip_text(Some(&crate::config::tildify(path)));
        heading.append(&crate::ui::color_dot(key_color(&in_repo.name)));
        let repo_name = gtk::Label::builder().label(&in_repo.name).xalign(0.0).build();
        repo_name.add_css_class("om-summary");
        heading.append(&repo_name);
        let count = gtk::Label::new(Some(&format!(
            "{} {}",
            in_repo.total,
            plural(in_repo.total, "commit", "commits")
        )));
        count.add_css_class("om-meta");
        heading.append(&count);
        group.append(&heading);

        let mut commits = in_repo.commits.clone();
        commits.sort_by_key(|commit| std::cmp::Reverse(commit.when));
        // A list, so that a commit can be reached with the keyboard and
        // opened. These were plain boxes: the page could show that a commit
        // existed and offered no way to it, not even its whole sha.
        let rows = gtk::ListBox::new();
        rows.set_selection_mode(gtk::SelectionMode::None);
        rows.add_css_class("om-results");
        {
            let ids: Vec<String> = commits.iter().map(|commit| commit.id.clone()).collect();
            let path = (*path).clone();
            let name = in_repo.name.clone();
            let on_open = Rc::clone(on_open);
            rows.connect_row_activated(move |_, row| {
                if let Some(id) = usize::try_from(row.index()).ok().and_then(|index| ids.get(index)) {
                    on_open(path.clone(), name.clone(), Some(id.clone()));
                }
            });
        }
        for commit in &commits {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);

            let sha = gtk::Label::builder().label(&commit.short_id).xalign(0.0).width_chars(7).build();
            sha.add_css_class("om-sha");
            row.append(&sha);

            let summary = gtk::Label::builder()
                .label(&commit.summary)
                .xalign(0.0)
                .hexpand(true)
                .ellipsize(pango::EllipsizeMode::End)
                .build();
            summary.add_css_class("om-summary");
            row.append(&summary);

            let churn = gtk::Label::builder()
                .label(match commit.lines {
                    _ if commit.merge => "merge".to_string(),
                    Some((insertions, deletions)) => {
                        format!("+{} −{}", thousands(insertions), thousands(deletions))
                    }
                    None if lines_pending => "…".to_string(),
                    // Not +0 −0, which is what it used to say: lines that
                    // could not be read are not lines that are not there.
                    None => "?".to_string(),
                })
                .xalign(1.0)
                .width_chars(14)
                .build();
            churn.add_css_class("om-meta");
            row.append(&churn);

            let when =
                gtk::Label::builder().label(relative_time(commit.when)).xalign(1.0).width_chars(13).build();
            when.add_css_class("om-meta");
            row.append(&when);

            let written = crate::ui::absolute_time(commit.when);
            let mut tip = format!("{}\n{}\n{written}", commit.id, commit.summary);
            // The row is dated by when the work was written. A rebase, an
            // amend or a cherry-pick makes the commit itself newer than that,
            // and the commit graph shows that other date.
            let committed = crate::ui::absolute_time(commit.committed);
            if committed != written {
                tip.push_str(&format!("\ncommitted {committed}"));
            }
            if commit.lines.is_none() && !lines_pending {
                tip.push_str(
                    "\nIts lines could not be counted: this clone does not hold \
                     everything the commit changed, or the commit it was made on.",
                );
            }
            tip.push_str("\n\nEnter, or a click, opens it in the commit graph.");
            let listed = gtk::ListBoxRow::builder().child(&row).build();
            listed.add_css_class("om-row");
            listed.set_tooltip_text(Some(&tip));
            rows.append(&listed);
        }
        group.append(&rows);
        groups.push(rows);

        if in_repo.total > commits.len() {
            let more = gtk::Label::builder()
                .label(format!("+{} older", in_repo.total - commits.len()))
                .xalign(0.0)
                .build();
            more.add_css_class("om-meta");
            more.set_margin_start(8);
            group.append(&more);
        }

        list.append(&group);
    }

    // Each repository's commits are a list of their own, and the arrow keys
    // stopped at the end of one with a beep: the next was a Tab away. Down
    // from the last commit of one goes on to the first of the next, Up the
    // other way, Up from the very first back to the author, and Down from
    // the very last on to the next author, as Tab would.
    for (n, rows) in groups.iter().enumerate() {
        let before = n.checked_sub(1).and_then(|before| groups.get(before)).cloned();
        let after = groups.get(n + 1).cloned();
        let head = head.downgrade();
        rows.connect_keynav_failed(move |rows, direction| {
            match direction {
                gtk::DirectionType::Down => match after.as_ref().and_then(|next| next.row_at_index(0)) {
                    Some(first) => {
                        first.grab_focus();
                    }
                    None => {
                        if let Some(root) = rows.root() {
                            root.child_focus(gtk::DirectionType::TabForward);
                        }
                    }
                },
                gtk::DirectionType::Up => match before.as_ref().and_then(|last| {
                    let mut at = 0;
                    while last.row_at_index(at + 1).is_some() {
                        at += 1;
                    }
                    last.row_at_index(at)
                }) {
                    Some(last) => {
                        last.grab_focus();
                    }
                    None => {
                        if let Some(head) = head.upgrade() {
                            head.grab_focus();
                        }
                    }
                },
                _ => return glib::Propagation::Proceed,
            }
            glib::Propagation::Stop
        });
    }

    list
}

fn swatch(color: &'static str) -> gtk::DrawingArea {
    let area =
        gtk::DrawingArea::builder().content_width(10).content_height(10).valign(gtk::Align::Center).build();
    area.set_draw_func(move |_, cr, width, height| {
        let (r, g, b) = crate::theme::Palette::rgb(color);
        cr.set_source_rgb(r, g, b);
        cr.arc(
            width as f64 / 2.0,
            height as f64 / 2.0,
            width.min(height) as f64 / 2.0,
            0.0,
            std::f64::consts::TAU,
        );
        let _ = cr.fill();
    });
    area
}

/// Per-day commits for one author, scaled against the busiest author-day so the
/// strips are comparable between rows.
fn sparkline(author: &Author, drawn: &Drawn, color: &'static str) -> gtk::DrawingArea {
    let counts: Vec<usize> = drawn.buckets.iter().map(|bucket| in_bucket(&author.per_day, bucket)).collect();
    let peak = drawn.peak;

    let area = gtk::DrawingArea::builder()
        .content_width((counts.len() as f64 * SPARK_W) as i32)
        .content_height(30)
        .valign(gtk::Align::Center)
        .build();

    area.set_tooltip_text(Some(
        &drawn
            .buckets
            .iter()
            .zip(&counts)
            .map(|(bucket, n)| format!("{}: {n}", bucket.title))
            .collect::<Vec<_>>()
            .join("\n"),
    ));
    area.set_draw_func(move |_, cr, width, height| {
        let h = height as f64;
        let step = width as f64 / counts.len().max(1) as f64;
        let (r, g, b) = crate::theme::Palette::rgb(color);
        for (i, count) in counts.iter().enumerate() {
            let x = i as f64 * step;
            cr.set_source_rgba(r, g, b, 0.10);
            cr.rectangle(x + 1.0, 0.0, step - 2.0, h);
            let _ = cr.fill();
            if *count > 0 {
                let filled = (*count as f64 / peak as f64).clamp(0.1, 1.0) * h;
                cr.set_source_rgb(r, g, b);
                cr.rectangle(x + 1.0, h - filled, step - 2.0, filled);
                let _ = cr.fill();
            }
        }
    });
    area
}

pub(crate) fn plural(n: usize, one: &'static str, many: &'static str) -> &'static str {
    if n == 1 { one } else { many }
}

pub(crate) fn thousands(n: usize) -> String {
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

// Only what the page says in words. Nothing here builds a widget.
#[cfg(test)]
mod tests {
    use super::*;

    /// A week over `scanned` repositories in which no commit was found, the
    /// ones in `errors` because they could not be read.
    fn nothing_found(scanned: usize, errors: &[&str]) -> Contributions {
        Contributions {
            window: stats::Window {
                day_starts: (0..7).map(|day| day * 86_400).collect(),
                day_labels: vec![String::new(); 7],
                day_titles: vec![String::new(); 7],
            },
            branches: stats::Branches::CheckedOut,
            authors: Vec::new(),
            co_authors: Vec::new(),
            mine: Default::default(),
            per_day_total: vec![0; 7],
            repos_scanned: scanned,
            repos_active: 0,
            total_commits: 0,
            total_insertions: 0,
            total_deletions: 0,
            uncounted: 0,
            future: 0,
            errors: errors.iter().map(|error| error.to_string()).collect(),
            lines_pending: false,
        }
    }

    #[test]
    fn a_repository_that_was_not_read_is_not_one_where_nothing_happened() {
        let (title, text) = empty_state(&nothing_found(3, &[]));
        assert_eq!(title, "No commits");
        assert_eq!(text, "Nothing was committed in the last 7 days across 3 repositories.");

        let unopened = "api: could not find repository at '/srv/api'";
        let (title, text) = empty_state(&nothing_found(1, &[unopened]));
        assert_eq!(title, "Nothing could be read");
        assert_eq!(text, unopened);

        let (title, text) = empty_state(&nothing_found(3, &["api: object not found"]));
        assert_eq!(title, "No commits");
        assert_eq!(
            text,
            "Nothing was committed in the last 7 days across 2 repositories.\n\n\
             1 more could not be read:\napi: object not found"
        );
    }

    #[test]
    fn the_status_line_leads_with_what_was_left_out() {
        assert_eq!(
            status_line(&nothing_found(3, &[]), &BTreeSet::new()),
            "0 commits  ·  0 authors  ·  0/3 repos active  ·  +0 −0"
        );

        let mut result = nothing_found(3, &["api: object not found"]);
        result.uncounted = 2;
        result.future = 1;
        let line = status_line(&result, &BTreeSet::new());
        assert!(line.starts_with("1 unreadable  ·  0 commits"), "{line}");
        assert!(line.ends_with("+0 −0  ·  2 without line counts  ·  1 dated in the future"), "{line}");

        let (_, text) = empty_state(&result);
        assert!(text.contains("\n\n1 commit is dated in the future and not counted."), "{text}");

        // Lines still being counted are neither +0 −0 nor without counts.
        result.lines_pending = true;
        let line = status_line(&result, &BTreeSet::new());
        assert!(line.ends_with("0/3 repos active  ·  counting lines…  ·  1 dated in the future"), "{line}");
    }

    /// The overview is of the authors listed, and with nobody set aside it
    /// is what was counted: every commit is one author's.
    #[test]
    fn the_figures_above_are_of_whoever_is_listed() {
        use crate::testkit::*;
        let _guard = isolate();
        let one = scratch("contributions-aside-one");
        let two = scratch("contributions-aside-two");
        for dir in [&one, &two] {
            git(dir, &["init", "-q", "-b", "main"]);
        }
        let commit = |dir: &std::path::Path, who: &str, file: &str, body: &str| {
            write(&dir.join(file), body);
            git(dir, &["add", "--", file]);
            let (name, email) =
                (format!("user.name={who}"), format!("user.email={}@example.invalid", who.to_lowercase()));
            git(dir, &["-c", &name, "-c", &email, "commit", "-q", "-m", file]);
        };
        commit(&one, "Me", "a.txt", "1\n2\n3\n");
        commit(&one, "Them", "b.txt", "1\n");
        commit(&two, "Me", "c.txt", "1\n2\n");
        let rx = stats::spawn(vec![one.clone(), two.clone()], crate::ui::day_window(7), Branches::CheckedOut);
        let result = loop {
            if let stats::Event::Done(result) = rx.recv_blocking().unwrap() {
                break *result;
            }
        };

        let everybody: Vec<&Author> = result.authors.iter().collect();
        let all = Figures::of(&result, &everybody);
        let counted = Figures {
            commits: result.total_commits,
            authors: result.authors.len(),
            repos_active: result.repos_active,
            insertions: result.total_insertions,
            deletions: result.total_deletions,
            per_day: result.per_day_total.clone(),
        };
        assert_eq!(all, counted);

        let me: BTreeSet<String> = ["me@example.invalid".to_string()].into();
        let others: Vec<&Author> =
            result.authors.iter().filter(|author| !me.contains(&author.key())).collect();
        let without = Figures::of(&result, &others);
        assert_eq!(
            (without.commits, without.authors, without.repos_active, without.insertions),
            (1, 1, 1, 1)
        );
        assert_eq!(without.per_day.iter().sum::<usize>(), 1);
        assert!(status_line(&result, &me).ends_with("  ·  1 set aside"), "{}", status_line(&result, &me));
    }

    /// One person from two addresses is two authors, and setting one aside
    /// sets aside the other, by name; somebody else is left alone.
    #[test]
    fn the_same_name_from_another_address_is_set_aside_with_them() {
        let author = |name: &str, email: &str| Author {
            name: name.to_string(),
            email: email.to_string(),
            commits: 1,
            merges: 0,
            insertions: 0,
            deletions: 0,
            files: 0,
            uncounted: 0,
            per_day: vec![1],
            repos: Default::default(),
            last_seen: 0,
        };
        let authors = vec![
            author("Ada Lind", "ada@work.invalid"),
            author("ada lind", "ada@home.invalid"),
            author("Bo", "bo@work.invalid"),
        ];
        let aside: BTreeSet<String> = ["ada@work.invalid".to_string()].into();
        let is_aside = set_aside_in(&authors, &aside);
        assert_eq!(authors.iter().map(is_aside).collect::<Vec<_>>(), [true, true, false]);
        assert!(!set_aside_in(&authors, &BTreeSet::new())(&authors[1]));
    }
}
