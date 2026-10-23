//! The arcade: the Contributions page as a high-score table.
//!
//! It is behind the Konami code, typed on that page: ↑ ↑ ↓ ↓ ← → ← → B A.
//! Nothing on screen says it is there and the list of keys leaves it out. It
//! reads nothing the page has not read already; it is the same figures,
//! scored, and Escape is game over.
//!
//! A player is a person, by address. Whoever a `Co-authored-by` trailer names
//! plays too, in co-op, and a commit counts for each of its players. Every
//! address commits are made under here (`user.email`, as each repository has
//! it) is the one player, 1UP. A robot plays as the CPU.
//!
//! A score is 100 a commit, 250 a day of the longest run of days with a
//! commit in each (the combo), and 50 a repository (a stage).

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::time::Duration;

use adw::prelude::*;
use gtk::{gdk, gio, glib};

use crate::stats::{Author, Contributions};

/// ↑ ↑ ↓ ↓ ← → ← → B A.
const KONAMI: [gdk::Key; 10] = [
    gdk::Key::Up,
    gdk::Key::Up,
    gdk::Key::Down,
    gdk::Key::Down,
    gdk::Key::Left,
    gdk::Key::Right,
    gdk::Key::Left,
    gdk::Key::Right,
    gdk::Key::b,
    gdk::Key::a,
];

/// Keys that are only ever held down for another: pressed on the way to a
/// capital B, they are not a wrong key.
const HELD: [gdk::Key; 12] = [
    gdk::Key::Shift_L,
    gdk::Key::Shift_R,
    gdk::Key::Caps_Lock,
    gdk::Key::Control_L,
    gdk::Key::Control_R,
    gdk::Key::Alt_L,
    gdk::Key::Alt_R,
    gdk::Key::Super_L,
    gdk::Key::Super_R,
    gdk::Key::Meta_L,
    gdk::Key::Meta_R,
    gdk::Key::ISO_Level3_Shift,
];

/// The last keys pressed, for telling when they were the code.
#[derive(Default)]
pub struct Konami {
    pressed: VecDeque<gdk::Key>,
}

impl Konami {
    /// One key pressed, with what was held: whether it ends the code. A key
    /// pressed with Ctrl, Alt or Super is a shortcut, and starts it again.
    pub fn press(&mut self, key: gdk::Key, state: gdk::ModifierType) -> bool {
        if HELD.contains(&key) {
            return false;
        }
        let chord =
            gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::ALT_MASK | gdk::ModifierType::SUPER_MASK;
        if state.intersects(chord) {
            self.pressed.clear();
            return false;
        }
        let key = match key {
            gdk::Key::KP_Up => gdk::Key::Up,
            gdk::Key::KP_Down => gdk::Key::Down,
            gdk::Key::KP_Left => gdk::Key::Left,
            gdk::Key::KP_Right => gdk::Key::Right,
            key => key.to_lower(),
        };
        self.pressed.push_back(key);
        if self.pressed.len() > KONAMI.len() {
            self.pressed.pop_front();
        }
        if self.pressed.iter().eq(KONAMI.iter()) {
            self.pressed.clear();
            return true;
        }
        false
    }
}

// ---- the players ----------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tag {
    /// The one whose machine this is.
    You,
    /// A robot.
    Cpu,
    /// Somebody with no commit of their own here, only commits a trailer
    /// names them on.
    CoOp,
    Nobody,
}

impl Tag {
    fn says(self) -> &'static str {
        match self {
            Tag::You => "1UP",
            Tag::Cpu => "CPU",
            Tag::CoOp => "CO-OP",
            Tag::Nobody => "",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Player {
    pub name: String,
    pub initials: String,
    pub tag: Tag,
    pub commits: usize,
    pub insertions: usize,
    pub deletions: usize,
    pub per_day: Vec<usize>,
    /// Repository -> what it is called and the commits there.
    pub stages: BTreeMap<PathBuf, (String, usize)>,
    /// When each commit the page lists was written, and where: the page
    /// keeps the newest hundred of a person's commits in each repository.
    pub written: Vec<(i64, PathBuf)>,
}

impl Player {
    /// The longest run of days with a commit in each.
    pub fn combo(&self) -> usize {
        let (mut best, mut run) = (0, 0);
        for &day in &self.per_day {
            run = if day > 0 { run + 1 } else { 0 };
            best = best.max(run);
        }
        best
    }

    pub fn score(&self) -> usize {
        self.commits * 100 + self.combo() * 250 + self.stages.len() * 50
    }

    /// Where most of the commits are.
    pub fn top_stage(&self) -> &str {
        self.stages
            .values()
            .max_by(|a, b| a.1.cmp(&b.1).then_with(|| b.0.cmp(&a.0)))
            .map_or("", |(name, _)| name.as_str())
    }
}

/// Everybody who committed or co-authored, best score first.
pub fn players(result: &Contributions) -> Vec<Player> {
    let mut players: Vec<Player> = Vec::new();
    // Which player each address is, and how many commits of their own the
    // spelling of their name in front has: the name a player goes by is the
    // one on most of their own commits.
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut own: Vec<usize> = Vec::new();
    let mut named_by: Vec<usize> = Vec::new();
    let you = "\u{0}you";
    for (author, is_own) in result
        .authors
        .iter()
        .map(|author| (author, true))
        .chain(result.co_authors.iter().map(|author| (author, false)))
    {
        let key = if result.mine.contains(&author.email) {
            you.to_string()
        } else if author.email.is_empty() {
            author.name.to_lowercase()
        } else {
            author.email.clone()
        };
        let at = *index.entry(key.clone()).or_insert_with(|| {
            players.push(Player {
                name: author.name.clone(),
                initials: String::new(),
                tag: if key == you {
                    Tag::You
                } else if is_robot(author) {
                    Tag::Cpu
                } else {
                    Tag::Nobody
                },
                commits: 0,
                insertions: 0,
                deletions: 0,
                per_day: vec![0; author.per_day.len()],
                stages: BTreeMap::new(),
                written: Vec::new(),
            });
            own.push(0);
            named_by.push(0);
            players.len() - 1
        });
        let player = &mut players[at];
        if is_own {
            own[at] += author.commits;
            if author.commits > named_by[at] {
                named_by[at] = author.commits;
                player.name = author.name.clone();
            }
        }
        add(player, author);
    }
    for (player, own) in players.iter_mut().zip(own) {
        if own == 0 && player.tag == Tag::Nobody {
            player.tag = Tag::CoOp;
        }
    }
    players.sort_by(|a, b| {
        b.score()
            .cmp(&a.score())
            .then_with(|| b.commits.cmp(&a.commits))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    let initials = initials(&players.iter().map(|player| player.name.as_str()).collect::<Vec<_>>());
    for (player, initials) in players.iter_mut().zip(initials) {
        player.initials = initials;
    }
    players
}

fn add(player: &mut Player, author: &Author) {
    player.commits += author.commits;
    player.insertions += author.insertions;
    player.deletions += author.deletions;
    for (total, day) in player.per_day.iter_mut().zip(&author.per_day) {
        *total += day;
    }
    for (path, repo) in &author.repos {
        let stage = player.stages.entry(path.clone()).or_insert_with(|| (repo.name.clone(), 0));
        stage.1 += repo.total;
        player.written.extend(repo.commits.iter().map(|commit| (commit.when, path.clone())));
    }
}

/// GitHub's apps commit as `name[bot]`.
fn is_robot(author: &Author) -> bool {
    author.name.to_lowercase().ends_with("[bot]") || author.email.contains("[bot]@")
}

/// Three letters for each name, as on an arcade's table: the first three of
/// the first name. Where two would be the same, those two are the first
/// letter of the first name and the first two of the last.
pub fn initials(names: &[&str]) -> Vec<String> {
    let choices: Vec<(String, String)> = names
        .iter()
        .map(|name| {
            let words: Vec<String> = name
                .split(|c: char| !c.is_alphabetic())
                .filter(|word| !word.is_empty())
                .map(str::to_uppercase)
                .collect();
            let Some(first) = words.first() else {
                return ("???".to_string(), "???".to_string());
            };
            let three: String = first.chars().take(3).collect();
            let other = match words.last().filter(|_| words.len() > 1) {
                Some(last) => first.chars().take(1).chain(last.chars().take(2)).collect(),
                None => three.clone(),
            };
            (three, other)
        })
        .collect();
    let mut times: HashMap<&str, usize> = HashMap::new();
    for (three, _) in &choices {
        *times.entry(three.as_str()).or_default() += 1;
    }
    choices
        .iter()
        .map(|(three, other)| if times[three.as_str()] > 1 { other.clone() } else { three.clone() })
        .collect()
}

/// Every repository with commits in it, most commits first.
pub fn stages(result: &Contributions) -> Vec<(String, usize)> {
    let mut by_path: BTreeMap<&Path, (&str, usize)> = BTreeMap::new();
    for author in &result.authors {
        for (path, repo) in &author.repos {
            by_path.entry(path.as_path()).or_insert((repo.name.as_str(), 0)).1 += repo.total;
        }
    }
    let mut stages: Vec<(String, usize)> =
        by_path.into_values().map(|(name, commits)| (name.to_string(), commits)).collect();
    stages.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    stages
}

/// When a commit was written, on this machine's clock.
#[derive(Debug, Clone, Copy)]
pub struct Local {
    /// Which day: any number that is one more the next day.
    pub day: i64,
    /// 1 for Monday to 7 for Sunday.
    pub weekday: u32,
    pub hour: u32,
}

pub struct Badge {
    pub name: &'static str,
    pub how: &'static str,
    pub won: bool,
}

/// What a player has done that an arcade would give a badge for.
pub fn badges(player: &Player, local: impl Fn(i64) -> Local) -> Vec<Badge> {
    let times: Vec<(Local, &PathBuf)> =
        player.written.iter().map(|(when, path)| (local(*when), path)).collect();
    let any = |test: &dyn Fn(&Local) -> bool| times.iter().any(|(at, _)| test(at));
    let mut per_day: HashMap<i64, BTreeSet<&PathBuf>> = HashMap::new();
    for (at, path) in &times {
        per_day.entry(at.day).or_default().insert(path);
    }
    let badge = |name, how, won| Badge { name, how, won };
    vec![
        badge("NIGHT OWL", "A COMMIT AFTER MIDNIGHT", any(&|at| at.hour < 5)),
        badge("EARLY BIRD", "A COMMIT BEFORE 7", any(&|at| (5..7).contains(&at.hour))),
        badge(
            "WEEKEND WARRIOR",
            "SATURDAY AND SUNDAY",
            any(&|at| at.weekday == 6) && any(&|at| at.weekday == 7),
        ),
        badge("FRIDAY DEPLOY", "FRIDAY AFTER 4 PM", any(&|at| at.weekday == 5 && at.hour >= 16)),
        badge("BOSS FIGHT", "DELETED MORE THAN ADDED", player.deletions > player.insertions),
        badge("COMBO ×5", "FIVE DAYS IN A ROW", player.combo() >= 5),
        badge("WORLD TOUR", "FIVE STAGES IN A DAY", per_day.values().any(|stages| stages.len() >= 5)),
        badge("CENTURY", "100 COMMITS", player.commits >= 100),
    ]
}

fn local(when: i64) -> Local {
    match glib::DateTime::from_unix_local(when) {
        Ok(at) => Local {
            day: i64::from(at.year()) * 400 + i64::from(at.day_of_year()),
            weekday: at.day_of_week() as u32,
            hour: at.hour() as u32,
        },
        Err(_) => Local { day: when / 86_400, weekday: 0, hour: 12 },
    }
}

/// 1ST, 2ND, 3RD, 4TH, … 11TH, 12TH, 13TH, … 21ST.
pub fn ordinal(n: usize) -> String {
    let suffix = match (n % 10, n % 100) {
        (_, 11..=13) => "TH",
        (1, _) => "ST",
        (2, _) => "ND",
        (3, _) => "RD",
        _ => "TH",
    };
    format!("{n}{suffix}")
}

/// What fits in `chars`, as an arcade's table cuts a name.
fn cut(text: &str, chars: usize) -> String {
    text.to_uppercase().chars().take(chars).collect()
}

// ---- the screen -----------------------------------------------------------

/// Rows on the table: an arcade's ten, and never fewer than five, the rest
/// waiting for a coin.
const ROWS: usize = 10;
const AT_LEAST: usize = 5;
const SCREENS: [(&str, &str); 3] =
    [("PLAYERS", "HIGH SCORES"), ("STAGES", "STAGE SELECT"), ("BADGES", "HALL OF FAME")];
/// How long the coin and game over screens stay up.
const COIN: Duration = Duration::from_millis(1400);
const OVER: Duration = Duration::from_millis(1200);
/// The longest stage bar, in pixels.
const BAR: f64 = 260.0;
/// How tall the table is at the least: the heading, ten rows and one more.
const BODY: i32 = 330;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Coin,
    Playing,
    Over,
}

struct State {
    result: Option<Rc<Contributions>>,
    players: Vec<Player>,
    stages: Vec<(String, usize)>,
    screen: usize,
    /// The chosen row on the players' table and on the stages'.
    player: usize,
    stage: usize,
    mode: Mode,
}

pub struct Arcade {
    root: gtk::Overlay,
    screen: gtk::Box,
    frames: gtk::Stack,
    banner: gtk::Label,
    banner_small: gtk::Label,
    one_up: gtk::Label,
    hi: gtk::Label,
    title: gtk::Label,
    range: gtk::Label,
    body: gtk::Box,
    tabs: Vec<gtk::Label>,
    foot: gtk::Label,
    state: RefCell<State>,
    /// What changes the window of days, by one either way.
    on_range: Box<dyn Fn(i32)>,
    /// What puts the page back once the game is over.
    on_quit: Box<dyn Fn()>,
}

fn label(text: &str, classes: &[&str]) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    for class in classes {
        label.add_css_class(class);
    }
    label
}

/// A cell of a table row, `chars` wide: the font has every letter the same
/// width, so the columns line up without a grid.
fn cell(text: &str, chars: i32, xalign: f32) -> gtk::Label {
    let cell = gtk::Label::new(Some(text));
    cell.set_width_chars(chars);
    cell.set_max_width_chars(chars);
    cell.set_xalign(xalign);
    cell
}

fn clear(container: &gtk::Box) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
}

impl Arcade {
    pub fn new(on_range: impl Fn(i32) + 'static, on_quit: impl Fn() + 'static) -> Rc<Arcade> {
        let one_up = label("", &[]);
        let hi = label("", &["om-arcade-hi"]);
        let credit = label("CREDIT 01", &["om-arcade-dim"]);
        let top = gtk::CenterBox::new();
        one_up.set_xalign(0.0);
        credit.set_xalign(1.0);
        top.set_start_widget(Some(&one_up));
        top.set_center_widget(Some(&hi));
        top.set_end_widget(Some(&credit));
        top.add_css_class("om-arcade-small");

        let title = label("", &["om-arcade-title"]);
        title.set_margin_top(18);
        let range = label("", &["om-arcade-small", "om-arcade-dim"]);
        range.set_margin_bottom(10);

        // Room for a full table, so that the tabs under it stay where they
        // are from one screen to the next.
        let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
        body.set_size_request(-1, BODY);

        let tab_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        tab_row.set_halign(gtk::Align::Center);
        tab_row.set_margin_top(14);
        let tabs: Vec<gtk::Label> = SCREENS
            .iter()
            .map(|(tab, _)| {
                let tab = label(tab, &["om-arcade-tab", "om-arcade-small"]);
                tab_row.append(&tab);
                tab
            })
            .collect();

        let foot = label("", &["om-arcade-small", "om-arcade-dim"]);
        foot.set_margin_top(10);
        foot.set_wrap(true);
        foot.set_justify(gtk::Justification::Center);

        let table = gtk::Box::new(gtk::Orientation::Vertical, 4);
        let parts = [top.upcast_ref::<gtk::Widget>(), title.upcast_ref(), range.upcast_ref(), body.upcast_ref()];
        for part in parts {
            table.append(part);
        }
        table.append(&tab_row);
        table.append(&foot);

        let banner = label("", &["om-arcade-title"]);
        let banner_small = label("", &["om-arcade-blink"]);
        let banner_box = gtk::Box::new(gtk::Orientation::Vertical, 28);
        banner_box.set_valign(gtk::Align::Center);
        banner_box.append(&banner);
        banner_box.append(&banner_small);

        let frames = gtk::Stack::new();
        frames.add_named(&table, Some("table"));
        frames.add_named(&banner_box, Some("banner"));

        // One widget takes the keyboard for the whole screen, so the keys
        // are the game's and not GTK's way round a page of labels.
        let screen = gtk::Box::new(gtk::Orientation::Vertical, 0);
        screen.set_focusable(true);
        screen.add_css_class("om-arcade-screen");
        screen.set_halign(gtk::Align::Center);
        screen.set_valign(gtk::Align::Center);
        screen.set_margin_top(20);
        screen.set_margin_bottom(20);
        screen.set_margin_start(24);
        screen.set_margin_end(24);
        screen.append(&frames);

        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .vexpand(true)
            .child(&screen)
            .build();
        let scanlines = gtk::Box::new(gtk::Orientation::Vertical, 0);
        scanlines.add_css_class("om-arcade-scanlines");
        scanlines.set_can_target(false);
        let root = gtk::Overlay::new();
        root.add_css_class("om-arcade");
        root.set_child(Some(&scroller));
        root.add_overlay(&scanlines);

        let arcade = Rc::new(Arcade {
            root,
            screen,
            frames,
            banner,
            banner_small,
            one_up,
            hi,
            title,
            range,
            body,
            tabs,
            foot,
            state: RefCell::new(State {
                result: None,
                players: Vec::new(),
                stages: Vec::new(),
                screen: 0,
                player: 0,
                stage: 0,
                mode: Mode::Over,
            }),
            on_range: Box::new(on_range),
            on_quit: Box::new(on_quit),
        });

        let keys = gtk::EventControllerKey::new();
        let weak = Rc::downgrade(&arcade);
        keys.connect_key_pressed(move |_, key, _, state| match weak.upgrade() {
            Some(arcade) if arcade.key(key, state) => glib::Propagation::Stop,
            _ => glib::Propagation::Proceed,
        });
        arcade.screen.add_controller(keys);

        // The tabs are the keyboard's, and take a click as well.
        for (n, tab) in arcade.tabs.iter().enumerate() {
            let click = gtk::GestureClick::new();
            let weak = Rc::downgrade(&arcade);
            click.connect_released(move |_, _, _, _| {
                let Some(arcade) = weak.upgrade() else { return };
                if arcade.state.borrow().mode != Mode::Playing {
                    return;
                }
                arcade.state.borrow_mut().screen = n;
                arcade.draw();
                arcade.screen.grab_focus();
            });
            tab.add_controller(click);
        }

        // Escape is the window's, which backs out of the page with it. With
        // the keyboard in here it asks for this first.
        let actions = gio::SimpleActionGroup::new();
        let game_over = gio::SimpleAction::new("game-over", None);
        let weak = Rc::downgrade(&arcade);
        game_over.connect_activate(move |_, _| {
            if let Some(arcade) = weak.upgrade() {
                arcade.game_over();
            }
        });
        actions.add_action(&game_over);
        arcade.screen.insert_action_group("arcade", Some(&actions));

        arcade
    }

    pub fn widget(&self) -> &gtk::Overlay {
        &self.root
    }

    /// The figures to play with, or nothing while the page reads new ones.
    pub fn show(&self, result: Option<Rc<Contributions>>) {
        {
            let mut state = self.state.borrow_mut();
            state.players = result.as_deref().map(players).unwrap_or_default();
            state.stages = result.as_deref().map(stages).unwrap_or_default();
            state.player = state.player.min(state.players.len().saturating_sub(1));
            state.stage = state.stage.min(state.stages.len().saturating_sub(1));
            state.result = result;
        }
        self.draw();
    }

    /// The game begins: a credit, then the table.
    pub fn insert_coin(self: &Rc<Self>) {
        self.state.borrow_mut().mode = Mode::Coin;
        self.banner.set_label("CREDIT 01");
        self.banner_small.set_label("PLAYER 1 READY");
        self.frames.set_visible_child_name("banner");
        self.screen.grab_focus();
        let weak: Weak<Arcade> = Rc::downgrade(self);
        glib::timeout_add_local_once(COIN, move || {
            let Some(arcade) = weak.upgrade() else { return };
            if arcade.state.borrow().mode != Mode::Coin {
                return;
            }
            arcade.state.borrow_mut().mode = Mode::Playing;
            arcade.frames.set_visible_child_name("table");
            arcade.draw();
        });
    }

    fn game_over(self: &Rc<Self>) {
        if self.state.borrow().mode == Mode::Over {
            return;
        }
        self.state.borrow_mut().mode = Mode::Over;
        self.banner.set_label("GAME OVER");
        self.banner_small.set_label("");
        self.frames.set_visible_child_name("banner");
        let weak = Rc::downgrade(self);
        glib::timeout_add_local_once(OVER, move || {
            if let Some(arcade) = weak.upgrade() {
                (arcade.on_quit)();
            }
        });
    }

    /// A key on the screen; whether it was the game's.
    fn key(&self, key: gdk::Key, state: gdk::ModifierType) -> bool {
        let mode = self.state.borrow().mode;
        if mode != Mode::Playing {
            // Between screens every key is swallowed, so none of them is
            // GTK's and moves the keyboard off the screen.
            return mode == Mode::Coin;
        }
        let shift = state.contains(gdk::ModifierType::SHIFT_MASK);
        match key {
            gdk::Key::Left | gdk::Key::KP_Left => (self.on_range)(-1),
            gdk::Key::Right | gdk::Key::KP_Right => (self.on_range)(1),
            gdk::Key::Up | gdk::Key::KP_Up => self.choose(-1),
            gdk::Key::Down | gdk::Key::KP_Down => self.choose(1),
            gdk::Key::Tab | gdk::Key::ISO_Left_Tab => {
                let back = shift || key == gdk::Key::ISO_Left_Tab;
                let mut state = self.state.borrow_mut();
                state.screen = (state.screen + if back { SCREENS.len() - 1 } else { 1 }) % SCREENS.len();
                drop(state);
                self.draw();
            }
            _ => return false,
        }
        true
    }

    /// Up or down the table on screen. The badges are the chosen player's.
    fn choose(&self, by: isize) {
        {
            let mut state = self.state.borrow_mut();
            let (rows, at) = match state.screen {
                0 => (state.players.len(), &mut state.player),
                1 => (state.stages.len().min(ROWS), &mut state.stage),
                _ => return,
            };
            if rows > 0 {
                *at = at.saturating_add_signed(by).min(rows - 1);
            }
        }
        self.draw();
    }

    fn draw(&self) {
        let state = self.state.borrow();
        let you = state.players.iter().find(|player| player.tag == Tag::You);
        self.one_up.set_label(&format!("1UP {:06}", you.map_or(0, Player::score)));
        self.hi.set_label(&format!("HI-SCORE {:06}", state.players.first().map_or(0, Player::score)));
        self.title.set_label(SCREENS[state.screen].1);
        for (n, tab) in self.tabs.iter().enumerate() {
            if n == state.screen {
                tab.add_css_class("om-on");
            } else {
                tab.remove_css_class("om-on");
            }
        }
        clear(&self.body);
        let Some(result) = state.result.as_deref() else {
            self.range.set_label("PLEASE WAIT");
            self.range.add_css_class("om-arcade-blink");
            self.foot.set_label("");
            return;
        };
        self.range.remove_css_class("om-arcade-blink");
        // The period by its name, as the page's drop-down has it.
        let days = result.window.days();
        let period = crate::ui::periods()
            .iter()
            .find(|(_, of)| of.days() == days)
            .map_or_else(|| format!("LAST {days} DAYS"), |(name, _)| name.to_uppercase());
        self.range.set_label(&format!("◀ {period} ▶"));
        self.foot.set_label(match state.screen {
            2 => "← → DAYS · TAB SCREEN · ESC GAME OVER",
            _ => "← → DAYS · ↑ ↓ CHOOSE · TAB SCREEN · ESC GAME OVER",
        });
        match state.screen {
            0 => self.draw_players(&state),
            1 => self.draw_stages(&state),
            _ => self.draw_badges(&state),
        }
    }

    fn row(&self, classes: &[&str], cells: &[gtk::Label]) -> gtk::Box {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 16);
        row.add_css_class("om-arcade-row");
        for class in classes {
            row.add_css_class(class);
        }
        for cell in cells {
            row.append(cell);
        }
        self.body.append(&row);
        row
    }

    fn draw_players(&self, state: &State) {
        let head = ["RANK", "SCORE", "NAME", "STAGE", "COMBO", ""];
        let widths = [4, 6, 3, 10, 5, 5];
        let cells: Vec<gtk::Label> =
            head.iter().zip(widths).map(|(text, chars)| cell(text, chars, 0.0)).collect();
        // In the rows' own size: the columns are counted in letters.
        self.row(&["om-arcade-dim"], &cells);

        let line = |rank: usize, player: &Player| {
            let tag = cell(player.tag.says(), widths[5], 0.0);
            if player.tag == Tag::You {
                tag.add_css_class("om-arcade-blink");
            } else {
                tag.add_css_class("om-arcade-dim");
            }
            let combo = if player.combo() > 0 { format!("×{}", player.combo()) } else { String::new() };
            let cells = [
                cell(&ordinal(rank + 1), widths[0], 0.0),
                cell(&format!("{:06}", player.score()), widths[1], 1.0),
                cell(&player.initials, widths[2], 0.0),
                cell(&cut(player.top_stage(), 10), widths[3], 0.0),
                cell(&combo, widths[4], 0.0),
                tag,
            ];
            let rank_class = format!("om-rank-{}", rank % 5);
            let row = self.row(&[rank_class.as_str()], &cells);
            row.set_tooltip_text(Some(&player.name));
            if rank == state.player {
                row.add_css_class("om-chosen");
            }
        };
        let shown = state.players.len().min(ROWS);
        for (rank, player) in state.players.iter().take(shown).enumerate() {
            line(rank, player);
        }
        for rank in shown..AT_LEAST {
            let cells = [
                cell(&ordinal(rank + 1), widths[0], 0.0),
                cell("000000", widths[1], 1.0),
                cell("---", widths[2], 0.0),
                cell("INSERT COIN", widths[3] + widths[4], 0.0),
            ];
            self.row(&["om-arcade-dim"], &cells);
        }
        // Further down than the table goes: the chosen player, or 1UP, under
        // it with their place.
        let below = if state.player >= ROWS {
            Some(state.player)
        } else {
            state.players.iter().position(|player| player.tag == Tag::You).filter(|at| *at >= ROWS)
        };
        if let Some(rank) = below {
            self.row(&["om-arcade-dim"], &[cell("…", 4, 0.0)]);
            line(rank, &state.players[rank]);
        }
    }

    fn draw_stages(&self, state: &State) {
        let most = state.stages.first().map_or(1, |(_, commits)| (*commits).max(1));
        for (rank, (name, commits)) in state.stages.iter().take(ROWS).enumerate() {
            let bar = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            bar.add_css_class("om-arcade-bar");
            bar.set_valign(gtk::Align::Center);
            bar.set_width_request(((*commits as f64 / most as f64) * BAR).round().max(12.0) as i32);
            let track = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            track.set_width_request(BAR as i32);
            track.append(&bar);
            let rank_label = cell(&ordinal(rank + 1), 4, 0.0);
            rank_label.add_css_class(&format!("om-rank-{}", rank % 5));
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 16);
            row.add_css_class("om-arcade-row");
            row.append(&rank_label);
            row.append(&cell(&cut(name, 14), 14, 0.0));
            row.append(&track);
            row.append(&cell(&commits.to_string(), 5, 1.0));
            row.set_tooltip_text(Some(name));
            if rank == state.stage {
                row.add_css_class("om-chosen");
            }
            self.body.append(&row);
        }
    }

    fn draw_badges(&self, state: &State) {
        let Some(player) = state.players.get(state.player) else { return };
        let badges = badges(player, local);
        let won = badges.iter().filter(|badge| badge.won).count();
        let who = label(
            &format!("{} · {} OF {} UNLOCKED", player.initials, won, badges.len()),
            &["om-arcade-small", "om-arcade-dim"],
        );
        who.set_margin_bottom(12);
        who.set_tooltip_text(Some(&player.name));
        self.body.append(&who);

        let grid = gtk::Grid::new();
        grid.set_column_spacing(10);
        grid.set_row_spacing(10);
        grid.set_column_homogeneous(true);
        for (n, badge) in badges.iter().enumerate() {
            let card = gtk::Box::new(gtk::Orientation::Vertical, 8);
            card.add_css_class("om-arcade-badge");
            let (mark, how) = if badge.won { ("★", badge.how) } else { ("☆", "???") };
            let name = label(&format!("{mark} {}", badge.name), &["om-arcade-small", "om-arcade-badge-name"]);
            name.set_xalign(0.0);
            let how = label(how, &["om-arcade-small", "om-arcade-dim"]);
            how.set_xalign(0.0);
            card.append(&name);
            card.append(&how);
            if badge.won {
                card.add_css_class("om-won");
            }
            grid.attach(&card, (n % 2) as i32, (n / 2) as i32, 1, 1);
        }
        self.body.append(&grid);
    }
}

// ---- the font -------------------------------------------------------------

/// Press Start 2P, under the SIL Open Font License (`data/fonts/OFL.txt`).
const FONT: &[u8] = include_bytes!("../../data/fonts/PressStart2P-Regular.ttf");
const FONT_FILE: &str = "PressStart2P-Regular.ttf";

/// Make the arcade's font one that `widget` can draw with. Done the first
/// time the code is typed, and not before: until then the app writes
/// nothing for it and loads nothing.
///
/// Pango takes a font from a file, so the one built into the binary is
/// written out to the runtime directory first, which is in memory and gone at
/// the end of the session. The call that loads it came in Pango 1.56 and is
/// looked up by name, so an older Pango builds and runs the app as before and
/// the arcade is in plain monospace there.
pub fn load_font(widget: &impl IsA<gtk::Widget>) {
    thread_local!(static TRIED: Cell<bool> = const { Cell::new(false) });
    if TRIED.replace(true) {
        return;
    }
    let Some(map) = widget.as_ref().pango_context().font_map() else { return };
    let dir = glib::user_runtime_dir().join("omarepos");
    let path = dir.join(FONT_FILE);
    if std::fs::read(&path).ok().as_deref() != Some(FONT) {
        // Written beside it and renamed over it: another copy of the app may
        // have the one there open.
        let spare = dir.join(format!("{FONT_FILE}.{}", std::process::id()));
        let written = std::fs::create_dir_all(&dir)
            .and_then(|()| std::fs::write(&spare, FONT))
            .and_then(|()| std::fs::rename(&spare, &path));
        if let Err(err) = written {
            note!("omarepos: could not write the arcade's font to {}: {err}", path.display());
            return;
        }
    }
    if !add_font_file(&map, &path) {
        note!("omarepos: Pango did not take the arcade's font; it is in monospace");
    }
}

fn add_font_file(map: &gtk::pango::FontMap, path: &Path) -> bool {
    use std::ffi::{CString, c_char, c_int, c_void};
    use std::os::unix::ffi::OsStrExt;

    type AddFontFile = unsafe extern "C" fn(*mut c_void, *const c_char, *mut *mut c_void) -> c_int;
    let Ok(path) = CString::new(path.as_os_str().as_bytes()) else { return false };
    // SAFETY: looking a symbol up does nothing else.
    let symbol = unsafe { libc::dlsym(libc::RTLD_DEFAULT, c"pango_font_map_add_font_file".as_ptr()) };
    if symbol.is_null() {
        return false;
    }
    // SAFETY: the symbol is Pango's `gboolean pango_font_map_add_font_file
    // (PangoFontMap *, const char *, GError **)`, which takes no error
    // pointer at all, and is handed a live font map and a C string that
    // outlive the call.
    unsafe {
        let add: AddFontFile = std::mem::transmute(symbol);
        add(map.as_ptr().cast(), path.as_ptr(), std::ptr::null_mut()) != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stats::{RepoCommits, Window};

    fn author(name: &str, email: &str, per_day: &[usize], repos: &[(&str, usize)]) -> Author {
        Author {
            name: name.to_string(),
            email: email.to_string(),
            commits: per_day.iter().sum(),
            merges: 0,
            insertions: 10,
            deletions: 2,
            files: 1,
            uncounted: 0,
            per_day: per_day.to_vec(),
            repos: repos
                .iter()
                .map(|(repo, total)| {
                    let commits = RepoCommits { name: repo.to_string(), total: *total, ..Default::default() };
                    (PathBuf::from(format!("/r/{repo}")), commits)
                })
                .collect(),
            last_seen: 0,
        }
    }

    fn result(authors: Vec<Author>, co_authors: Vec<Author>, mine: &[&str]) -> Contributions {
        let days = authors.first().map_or(7, |author| author.per_day.len());
        Contributions {
            window: Window {
                day_starts: (0..days as i64).map(|day| day * 86_400).collect(),
                day_labels: vec![String::new(); days],
                day_titles: vec![String::new(); days],
            },
            branches: crate::stats::Branches::CheckedOut,
            authors,
            co_authors,
            mine: mine.iter().map(|email| email.to_string()).collect(),
            per_day_total: vec![0; days],
            repos_scanned: 0,
            repos_active: 0,
            total_commits: 0,
            total_insertions: 0,
            total_deletions: 0,
            uncounted: 0,
            future: 0,
            errors: Vec::new(),
            lines_pending: false,
        }
    }

    fn typed(konami: &mut Konami, keys: &[gdk::Key]) -> bool {
        keys.iter().map(|key| konami.press(*key, gdk::ModifierType::empty())).last().unwrap_or(false)
    }

    #[test]
    fn the_code_opens_it_and_nothing_else_does() {
        use gdk::Key as K;
        let mut konami = Konami::default();
        assert!(typed(&mut konami, &KONAMI));
        // A key too many at the start is forgiven, as on the machines.
        let one_too_many = [K::Up, K::Up, K::Up, K::Down, K::Down, K::Left, K::Right, K::Left, K::Right, K::b, K::a];
        assert!(typed(&mut konami, &one_too_many));
        // Capitals, the keypad, and Shift pressed on the way to them.
        let shifted =
            [K::KP_Up, K::Up, K::Down, K::KP_Down, K::Left, K::Right, K::Left, K::Right, K::Shift_L, K::B, K::A];
        assert!(typed(&mut konami, &shifted));
        // One wrong key and it starts again.
        let wrong = [K::Up, K::Up, K::Down, K::Down, K::Left, K::Right, K::Left, K::x, K::Right, K::b, K::a];
        assert!(!typed(&mut konami, &wrong));
        assert!(!typed(&mut konami, &KONAMI[..9]));
        // Ctrl+A is a shortcut, not the last letter of the code.
        let mut konami = Konami::default();
        typed(&mut konami, &KONAMI[..9]);
        assert!(!konami.press(K::a, gdk::ModifierType::CONTROL_MASK));
        assert!(!konami.press(K::a, gdk::ModifierType::empty()));
    }

    #[test]
    fn a_score_is_commits_combo_and_stages() {
        let days = [1, 2, 0, 3, 1, 1, 0];
        let me = author("Jonas Montonen", "me@example.invalid", &days, &[("one", 5), ("two", 3)]);
        let players = players(&result(vec![me], Vec::new(), &[]));
        assert_eq!(players[0].combo(), 3);
        assert_eq!(players[0].score(), 8 * 100 + 3 * 250 + 2 * 50);
        assert_eq!(players[0].top_stage(), "one");
    }

    /// Every address of yours is one player; a co-author with no commits of
    /// their own plays in co-op, and is credited with what they co-wrote;
    /// a robot is the CPU.
    #[test]
    fn who_plays_as_what() {
        let work = author("Jonas M", "me@work.invalid", &[1, 0, 0, 0, 0, 0, 0], &[("job", 1)]);
        let home = author("Jonas Montonen", "me@home.invalid", &[2, 2, 0, 0, 0, 0, 0], &[("site", 4)]);
        let robot = "49699333+dependabot[bot]@users.noreply.github.com";
        let bot = author("dependabot[bot]", robot, &[1, 0, 0, 0, 0, 0, 0], &[("site", 1)]);
        let pair = author("Claude Opus 5.5", "noreply@anthropic.com", &[2, 2, 0, 0, 0, 0, 0], &[("site", 4)]);
        let colleague = author("Ada Lovelace", "ada@example.invalid", &[0, 0, 0, 0, 0, 0, 1], &[("job", 1)]);
        let helped = author("Ada Lovelace", "ada@example.invalid", &[0, 0, 0, 0, 0, 1, 0], &[("job", 1)]);
        let players = players(&result(
            vec![home, work, bot, colleague],
            vec![pair, helped],
            &["me@home.invalid", "me@work.invalid"],
        ));
        let by = |initials: &str| players.iter().find(|player| player.initials == initials).unwrap();
        assert_eq!(players.len(), 4);
        assert_eq!(players[0].tag, Tag::You);
        let you = by("JON");
        assert_eq!((you.name.as_str(), you.commits, you.stages.len()), ("Jonas Montonen", 5, 2));
        assert_eq!((by("CLA").tag, by("CLA").commits), (Tag::CoOp, 4));
        assert_eq!(by("DEP").tag, Tag::Cpu);
        // Ada has a commit of her own, so she is no co-op player, and the one
        // she co-wrote counts as well.
        assert_eq!((by("ADA").tag, by("ADA").commits, by("ADA").combo()), (Tag::Nobody, 2, 2));
    }

    #[test]
    fn initials_are_three_letters_and_two_people_do_not_share_them() {
        assert_eq!(
            initials(&[
                "Jonas Montonen",
                "Claude Opus 5.5",
                "dependabot[bot]",
                "Jonathan Ive",
                "Åsa Öberg",
                "42"
            ]),
            ["JMO", "CLA", "DEP", "JIV", "ÅSA", "???"]
        );
    }

    #[test]
    fn stages_add_up_every_author_there() {
        let a = author("A", "a@x.invalid", &[3], &[("one", 2), ("two", 1)]);
        let b = author("B", "b@x.invalid", &[4], &[("two", 4)]);
        let stages = stages(&result(vec![a, b], Vec::new(), &[]));
        assert_eq!(stages, [("two".to_string(), 5), ("one".to_string(), 2)]);
    }

    #[test]
    fn badges_go_by_the_clock_commits_were_written_on() {
        let mut player = players(&result(
            vec![author("A", "a@x.invalid", &[1, 1, 1, 1, 1, 0, 0], &[("one", 5)])],
            Vec::new(),
            &[],
        ))
        .remove(0);
        // Friday 23:00, Saturday 03:00, Sunday 06:00, by hours from a Monday 00:00.
        let hours = [4 * 24 + 23, 5 * 24 + 3, 6 * 24 + 6];
        player.written = hours.iter().map(|hour| (hour * 3600, PathBuf::from("/r/one"))).collect();
        let at = |when: i64| Local {
            day: when / 86_400,
            weekday: (when / 86_400) as u32 + 1,
            hour: (when % 86_400 / 3600) as u32,
        };
        let badges = badges(&player, at);
        let won: Vec<&str> = badges.iter().filter(|badge| badge.won).map(|badge| badge.name).collect();
        assert_eq!(won, ["NIGHT OWL", "EARLY BIRD", "WEEKEND WARRIOR", "FRIDAY DEPLOY", "COMBO ×5"]);
    }

    #[test]
    fn places_are_said_the_arcade_way() {
        let said: Vec<String> = [1, 2, 3, 4, 11, 12, 13, 21, 22, 101].into_iter().map(ordinal).collect();
        assert_eq!(said, ["1ST", "2ND", "3RD", "4TH", "11TH", "12TH", "13TH", "21ST", "22ND", "101ST"]);
    }
}
