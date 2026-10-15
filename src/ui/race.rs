//! The race: authors' commits over a period, as a bar chart race. Each
//! author's running total is a bar; the bars grow day by day and pass one
//! another, and the date counts up in the corner.
//!
//! Opened from Contributions with `r`, over the same repositories, for the
//! last 90 days, this year or the last 12 months. It reads what Contributions reads but for the line counts,
//! which are most of what reading history costs: a year across a fleet would
//! take a while with them. Authors set aside on Contributions stay out of it.
//!
//! The bars are ordinary widgets in a `gtk::Fixed`, moved on each frame, so
//! that they are written in the app's own fonts and colours. Frames are asked
//! for only while the race runs or a bar is still on its way to its place.

use std::cell::{Cell, RefCell};
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::rc::{Rc, Weak};

use adw::prelude::*;
use gtk::{gdk, glib, pango};

use crate::stats::{self, Branches, Contributions, Lines};
use crate::ui::stats::{plural, thousands};
use crate::ui::{day_window, key_color};

/// What a race can be of, as the drop-down says it, and how many days it
/// goes back from today. This year is from the first of January, and so as
/// long as today is far into it.
fn periods() -> [(&'static str, usize); 3] {
    let this_year = glib::DateTime::now_local().map_or(365, |now| now.day_of_year().max(1) as usize);
    [("Last 90 days", 90), ("This year", this_year), ("Last 12 months", 365)]
}
/// How many bars are in the race at once.
const TOP: usize = 10;
/// From one bar to the next, and how tall a bar is.
const ROW: f64 = 36.0;
const BAR: i32 = 24;
/// Room for a name in front of its bar, and for the total after it.
const NAME: i32 = 190;
const TOTAL: i32 = 70;
/// How long a race lasts at 1×, whatever its period: a day a frame, a year
/// would take six seconds and thirty days half a second.
const LENGTH: f64 = 20.0;
const SPEEDS: [f64; 3] = [1.0, 2.0, 4.0];
/// How quickly a bar slides to a new place: in this many seconds it has gone
/// most (63%) of the way.
const SLIDE: f64 = 0.12;

/// One author in the race.
#[derive(Debug, Clone)]
pub struct Racer {
    pub name: String,
    pub color: &'static str,
    /// Their total after each day: `running[d]` is the first `d` days', so
    /// `running[0]` is nought and the last is everything.
    pub running: Vec<usize>,
}

#[derive(Debug, Clone)]
pub struct Race {
    pub racers: Vec<Racer>,
    pub days: usize,
    /// What each day is called in the corner: "15 Oct 2026".
    pub dates: Vec<String>,
}

impl Race {
    /// Everybody with a commit in the period, but whoever is set aside.
    pub fn of(result: &Contributions, aside: &BTreeSet<String>) -> Race {
        let racers = result
            .authors
            .iter()
            .filter(|author| author.commits > 0 && !aside.contains(&author.key()))
            .map(|author| {
                let mut running = Vec::with_capacity(author.per_day.len() + 1);
                running.push(0);
                for day in &author.per_day {
                    running.push(running.last().copied().unwrap_or(0) + day);
                }
                // The colour of their dot on Contributions.
                let color = key_color(if author.email.is_empty() { &author.name } else { &author.email });
                Racer { name: author.name.clone(), color, running }
            })
            .collect();
        let dates = result
            .window
            .day_starts
            .iter()
            .map(|start| {
                glib::DateTime::from_unix_local(*start)
                    .ok()
                    .and_then(|day| day.format("%-d %b %Y").ok())
                    .map(|day| day.to_string())
                    .unwrap_or_default()
            })
            .collect();
        Race { racers, days: result.window.days(), dates }
    }

    /// A racer's total `t` days in, `t` from nought to `days`. Between two
    /// days it is a straight line from one day's total to the next, so that a
    /// bar grows rather than jumps.
    pub fn total(&self, racer: usize, t: f64) -> f64 {
        let running = &self.racers[racer].running;
        let t = t.clamp(0.0, self.days as f64);
        let day = (t.floor() as usize).min(running.len().saturating_sub(2));
        let (from, to) = (running[day] as f64, running.get(day + 1).copied().unwrap_or(running[day]) as f64);
        from + (to - from) * (t - day as f64)
    }

    /// Who is where `t` days in, best first, with their totals. Two level
    /// with each other go by name, so that they do not trade places on every
    /// frame.
    pub fn standings(&self, t: f64) -> Vec<(usize, f64)> {
        let mut standings: Vec<(usize, f64)> = (0..self.racers.len()).map(|racer| (racer, self.total(racer, t))).collect();
        standings.sort_by(|a, b| {
            b.1.total_cmp(&a.1).then_with(|| {
                self.racers[a.0].name.to_lowercase().cmp(&self.racers[b.0].name.to_lowercase())
            })
        });
        standings
    }

    /// The day `t` days in is part of.
    pub fn date(&self, t: f64) -> &str {
        let day = (t.max(0.0).floor() as usize).min(self.days.saturating_sub(1));
        self.dates.get(day).map_or("", String::as_str)
    }
}

// ---- the page --------------------------------------------------------------

/// A racer's row: name, bar and total, and where it is on its way to.
struct Lane {
    row: gtk::Box,
    bar: gtk::DrawingArea,
    total: gtk::Label,
    y: Cell<f64>,
    shown: Cell<f64>,
}

/// The race on screen.
struct Track {
    fixed: gtk::Fixed,
    lanes: RefCell<Vec<Lane>>,
    race: RefCell<Option<Race>>,
    /// How far in, in days.
    t: Cell<f64>,
    playing: Cell<bool>,
    speed: Cell<usize>,
    /// When the last frame was, in the frame clock's microseconds.
    last: Cell<Option<i64>>,
    ticking: Cell<bool>,
    /// How wide the track is, as last laid out.
    width: Cell<i32>,
    /// Whether the desktop wants things to move; with it off, bars go
    /// straight to their places and a race opens at its end.
    animate: bool,
    date: gtk::Label,
    sum: gtk::Label,
    play: gtk::Button,
    progress: gtk::Scale,
    /// Set while the progress bar is being moved by the race, not by hand.
    moving: Cell<bool>,
}

impl Track {
    fn lane(&self, racer: &Racer) -> Lane {
        let name = gtk::Label::builder()
            .label(&racer.name)
            .xalign(1.0)
            .ellipsize(pango::EllipsizeMode::End)
            .width_request(NAME)
            .build();
        name.add_css_class("om-race-name");
        let bar = gtk::DrawingArea::builder().content_height(BAR).content_width(4).valign(gtk::Align::Center).build();
        let (r, g, b) = crate::theme::Palette::rgb(racer.color);
        bar.set_draw_func(move |_, cr, width, height| {
            let (w, h) = (width as f64, height as f64);
            let radius = 4.0_f64.min(w / 2.0);
            cr.new_sub_path();
            cr.arc(w - radius, radius, radius, -std::f64::consts::FRAC_PI_2, 0.0);
            cr.arc(w - radius, h - radius, radius, 0.0, std::f64::consts::FRAC_PI_2);
            cr.arc(radius, h - radius, radius, std::f64::consts::FRAC_PI_2, std::f64::consts::PI);
            cr.arc(radius, radius, radius, std::f64::consts::PI, 3.0 * std::f64::consts::FRAC_PI_2);
            cr.close_path();
            cr.set_source_rgb(r, g, b);
            let _ = cr.fill();
        });
        let total = gtk::Label::new(None);
        total.add_css_class("om-race-total");
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        row.append(&name);
        row.append(&bar);
        row.append(&total);
        row.set_opacity(0.0);
        let y = TOP as f64 * ROW;
        self.fixed.put(&row, 0.0, y);
        Lane { row, bar, total, y: Cell::new(y), shown: Cell::new(0.0) }
    }

    /// A race to run, from its start, or from its end where nothing is to
    /// move.
    fn load(self: &Rc<Self>, race: Race) {
        for lane in self.lanes.borrow_mut().drain(..) {
            self.fixed.remove(&lane.row);
        }
        let lanes: Vec<Lane> = race.racers.iter().map(|racer| self.lane(racer)).collect();
        self.moving.set(true);
        self.progress.set_range(0.0, race.days.max(1) as f64);
        self.moving.set(false);
        *self.lanes.borrow_mut() = lanes;
        let days = race.days as f64;
        *self.race.borrow_mut() = Some(race);
        if self.animate {
            self.t.set(0.0);
            self.set_playing(true);
        } else {
            self.t.set(days);
            self.set_playing(false);
        }
        self.render(true, 0.0);
    }

    fn set_playing(self: &Rc<Self>, playing: bool) {
        self.playing.set(playing);
        let (icon, says) = if playing { ("media-playback-pause-symbolic", "Pause (Space)") } else { ("media-playback-start-symbolic", "Play (Space)") };
        self.play.set_icon_name(icon);
        self.play.set_tooltip_text(Some(says));
        if playing {
            self.last.set(None);
            self.tick();
        }
    }

    fn toggle(self: &Rc<Self>) {
        let days = self.race.borrow().as_ref().map_or(0.0, |race| race.days as f64);
        if !self.playing.get() && self.t.get() >= days {
            self.t.set(0.0);
        }
        self.set_playing(!self.playing.get());
    }

    /// To `t` days in by hand, which stops the race there.
    fn seek(self: &Rc<Self>, t: f64) {
        let days = self.race.borrow().as_ref().map_or(0.0, |race| race.days as f64);
        self.set_playing(false);
        self.t.set(t.clamp(0.0, days));
        self.render(!self.animate, 0.0);
        self.tick();
    }

    /// One day on or back, to the day's end or start.
    fn step(self: &Rc<Self>, by: f64) {
        let t = self.t.get();
        let next = if by > 0.0 { (t + 1.0).floor() } else { (t - 1.0).ceil() };
        self.seek(next);
    }

    /// Ask for frames until the race is paused and every bar is in place.
    fn tick(self: &Rc<Self>) {
        if self.ticking.replace(true) {
            return;
        }
        let weak: Weak<Track> = Rc::downgrade(self);
        self.fixed.add_tick_callback(move |_, clock| {
            let Some(track) = weak.upgrade() else { return glib::ControlFlow::Break };
            let now = clock.frame_time();
            let dt = track.last.replace(Some(now)).map_or(0.0, |last| (now - last) as f64 / 1e6).min(0.25);
            if track.playing.get() {
                let days = track.race.borrow().as_ref().map_or(0.0, |race| race.days as f64);
                let speed = SPEEDS[track.speed.get()];
                let t = (track.t.get() + dt * days / LENGTH * speed).min(days);
                track.t.set(t);
                if t >= days {
                    track.set_playing(false);
                }
            }
            let settled = track.render(!track.animate, dt);
            if track.playing.get() || !settled {
                glib::ControlFlow::Continue
            } else {
                track.ticking.set(false);
                track.last.set(None);
                glib::ControlFlow::Break
            }
        });
    }

    /// Put everything where it is `t` days in, the bars `dt` seconds further
    /// on their way to their places, or straight there. Whether every bar is
    /// in place.
    fn render(&self, snap: bool, dt: f64) -> bool {
        let race = self.race.borrow();
        let Some(race) = race.as_ref() else { return true };
        let t = self.t.get();
        let standings = race.standings(t);
        let lead = standings.first().map_or(0.0, |(_, total)| *total).max(1.0);
        let room = (self.width.get() - NAME - TOTAL - 20).max(120) as f64;
        let lanes = self.lanes.borrow();
        let ease = if snap { 1.0 } else { 1.0 - (-dt / SLIDE).exp() };
        let mut settled = true;
        let mut sum = 0.0;
        for (place, (racer, total)) in standings.iter().enumerate() {
            sum += total;
            let lane = &lanes[*racer];
            let racing = place < TOP && *total > 0.0;
            let (to_y, to_shown) = if racing { (place as f64 * ROW, 1.0) } else { (TOP as f64 * ROW, 0.0) };
            let y = lane.y.get() + (to_y - lane.y.get()) * ease;
            let shown = lane.shown.get() + (to_shown - lane.shown.get()) * ease;
            settled &= (to_y - y).abs() < 0.5 && (to_shown - shown).abs() < 0.01;
            lane.y.set(y);
            lane.shown.set(shown);
            self.fixed.move_(&lane.row, 0.0, y);
            lane.row.set_opacity(shown);
            lane.row.set_visible(shown > 0.01);
            lane.bar.set_content_width(((total / lead) * room).round().max(4.0) as i32);
            lane.total.set_label(&thousands(total.round() as usize));
        }
        self.date.set_label(race.date(t));
        let sum = sum.round() as usize;
        self.sum.set_label(&format!("{} {}", thousands(sum), plural(sum, "commit", "commits")));
        self.moving.set(true);
        self.progress.set_value(t);
        self.moving.set(false);
        settled
    }
}

/// What a race is of: the Contributions page's selection as it stands when
/// the race is asked for. Its repositories, what they are called, which of
/// their branches are counted, and who is set aside, who does not race.
pub struct Field {
    pub repos: Vec<PathBuf>,
    pub scope: String,
    pub branches: Branches,
    pub aside: BTreeSet<String>,
}

pub fn page(field: Field) -> adw::NavigationPage {
    let Field { repos, scope, branches, aside } = field;
    let title = adw::WindowTitle::new("Contributions race", &scope);
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&title));
    let period = gtk::DropDown::from_strings(&periods().map(|(label, _)| label));
    period.add_css_class("om-sort");
    period.set_tooltip_text(Some("Period"));
    header.pack_end(&period);

    let fixed = gtk::Fixed::new();
    fixed.set_hexpand(true);
    fixed.set_size_request(-1, (TOP as f64 * ROW) as i32);
    let date = gtk::Label::new(None);
    date.add_css_class("om-race-date");
    let sum = gtk::Label::new(None);
    sum.add_css_class("om-meta");
    sum.set_xalign(1.0);
    let corner = gtk::Box::new(gtk::Orientation::Vertical, 2);
    corner.set_halign(gtk::Align::End);
    corner.set_valign(gtk::Align::End);
    corner.set_can_target(false);
    corner.append(&sum);
    corner.append(&date);
    // Says when the track changes width, which a paused race must be drawn
    // again for: its bars are as long as the leader's is wide.
    let probe = gtk::DrawingArea::new();
    probe.set_can_target(false);
    // The bars and the date in their corner, as tall as ten bars and the
    // date, in the middle of whatever height the page has.
    let chart = gtk::Overlay::new();
    chart.set_child(Some(&fixed));
    chart.add_overlay(&probe);
    chart.add_overlay(&corner);
    chart.set_size_request(-1, (TOP as f64 * ROW) as i32 + 56);
    chart.set_valign(gtk::Align::Center);
    let stage = gtk::Box::new(gtk::Orientation::Vertical, 0);
    stage.append(&chart);
    stage.set_vexpand(true);
    stage.set_valign(gtk::Align::Fill);
    stage.set_margin_top(18);
    stage.set_margin_start(16);
    stage.set_margin_end(20);
    chart.set_vexpand(true);

    let play = gtk::Button::from_icon_name("media-playback-start-symbolic");
    let again = gtk::Button::from_icon_name("media-skip-backward-symbolic");
    again.set_tooltip_text(Some("From the start (Home)"));
    let progress = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 1.0, 1.0);
    progress.set_hexpand(true);
    progress.set_draw_value(false);
    progress.set_tooltip_text(Some("Drag to any day; ← → a day at a time"));
    let speed = gtk::Button::with_label("1×");
    speed.set_tooltip_text(Some("Speed"));
    for button in [&play, &again, &speed] {
        button.add_css_class("flat");
    }
    let controls = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    controls.set_margin_start(12);
    controls.set_margin_end(12);
    controls.set_margin_top(6);
    controls.append(&play);
    controls.append(&again);
    controls.append(&progress);
    controls.append(&speed);

    // Which repositories the race is of, by name, so that a picture or a
    // recording of it says.
    let covered = gtk::Label::builder().xalign(0.0).wrap(true).wrap_mode(pango::WrapMode::WordChar).build();
    covered.add_css_class("om-meta");
    covered.set_margin_start(18);
    covered.set_margin_end(18);
    covered.set_margin_top(4);
    covered.set_margin_bottom(10);
    let mut names = stats::labels(&repos);
    names.sort_by_key(|name| name.to_lowercase());
    covered.set_label(&format!(
        "{} {}: {}",
        names.len(),
        plural(names.len(), "repository", "repositories"),
        names.join("  ·  ")
    ));

    let running = gtk::Box::new(gtk::Orientation::Vertical, 0);
    running.append(&stage);
    running.append(&controls);
    running.append(&covered);

    let spinner = gtk::Spinner::new();
    spinner.start();
    let reading = gtk::Label::new(Some("Reading history…"));
    reading.add_css_class("om-meta");
    let loading = gtk::Box::new(gtk::Orientation::Vertical, 10);
    loading.set_valign(gtk::Align::Center);
    loading.set_halign(gtk::Align::Center);
    loading.append(&spinner);
    loading.append(&reading);
    let empty = adw::StatusPage::builder().icon_name("system-users-symbolic").title("No commits").build();
    let stack = gtk::Stack::new();
    stack.add_named(&loading, Some("loading"));
    stack.add_named(&running, Some("race"));
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
    let page = adw::NavigationPage::builder().title("Contributions race").child(&frame).build();

    let animate = gtk::Settings::default().is_none_or(|settings| settings.is_gtk_enable_animations());
    let track = Rc::new(Track {
        fixed,
        lanes: RefCell::new(Vec::new()),
        race: RefCell::new(None),
        t: Cell::new(0.0),
        playing: Cell::new(false),
        speed: Cell::new(0),
        last: Cell::new(None),
        ticking: Cell::new(false),
        width: Cell::new(0),
        animate,
        date,
        sum,
        play: play.clone(),
        progress: progress.clone(),
        moving: Cell::new(false),
    });

    {
        let track = Rc::downgrade(&track);
        play.connect_clicked(move |_| {
            if let Some(track) = track.upgrade() {
                track.toggle();
            }
        });
    }
    {
        let track = Rc::downgrade(&track);
        again.connect_clicked(move |_| {
            if let Some(track) = track.upgrade() {
                track.seek(0.0);
                track.set_playing(track.animate);
            }
        });
    }
    {
        let track = Rc::downgrade(&track);
        progress.connect_value_changed(move |scale| {
            let Some(track) = track.upgrade() else { return };
            if !track.moving.get() {
                track.seek(scale.value());
            }
        });
    }
    {
        let track = Rc::downgrade(&track);
        speed.connect_clicked(move |button| {
            let Some(track) = track.upgrade() else { return };
            track.speed.set((track.speed.get() + 1) % SPEEDS.len());
            button.set_label(&format!("{}×", SPEEDS[track.speed.get()]));
        });
    }

    {
        let track = Rc::downgrade(&track);
        probe.connect_resize(move |_, width, _| {
            let Some(track) = track.upgrade() else { return };
            if track.width.replace(width) != width {
                // Not now: this is the middle of laying the page out.
                glib::idle_add_local_once(move || {
                    track.render(true, 0.0);
                });
            }
        });
    }

    // The race's keys, ahead of whatever has the keyboard: Space plays and
    // pauses wherever it is, and the arrows step a day. The controller holds
    // the track, and so the track lives as long as the page; everything else
    // holds it weakly.
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    {
        let track = Rc::clone(&track);
        keys.connect_key_pressed(move |_, key, _, state| {
            if !crate::ui::unmodified(state) || track.race.borrow().is_none() {
                return glib::Propagation::Proceed;
            }
            let days = track.race.borrow().as_ref().map_or(0.0, |race| race.days as f64);
            match key {
                gdk::Key::space => track.toggle(),
                gdk::Key::Left | gdk::Key::KP_Left => track.step(-1.0),
                gdk::Key::Right | gdk::Key::KP_Right => track.step(1.0),
                gdk::Key::Home => {
                    track.seek(0.0);
                    track.set_playing(track.animate);
                }
                gdk::Key::End => track.seek(days),
                _ => return glib::Propagation::Proceed,
            }
            glib::Propagation::Stop
        });
    }
    page.add_controller(keys);

    // Read the period, without line counts. A newer read wins over an older
    // one still on its way.
    let generation = Rc::new(Cell::new(0u64));
    let repos = Rc::new(repos);
    let load: Rc<dyn Fn()> = {
        let (period, stack, status, reading) = (period.clone(), stack.clone(), status.clone(), reading.clone());
        let title = title.clone();
        let track = Rc::downgrade(&track);
        Rc::new(move || {
            let periods = periods();
            let (label, days) = periods.get(period.selected() as usize).copied().unwrap_or(periods[0]);
            let counted = if branches == Branches::All { " · all branches" } else { "" };
            title.set_subtitle(&format!("{scope} · {}{counted}", label.to_lowercase()));
            generation.set(generation.get() + 1);
            let mine = generation.get();
            stack.set_visible_child_name("loading");
            status.set_label("Reading history…");
            let rx = stats::spawn_with((*repos).clone(), day_window(days), branches, Lines::Skipped);
            let (generation, stack, status, reading) =
                (Rc::clone(&generation), stack.clone(), status.clone(), reading.clone());
            let track = track.clone();
            let aside = aside.clone();
            glib::spawn_future_local(async move {
                while let Ok(event) = rx.recv().await {
                    if generation.get() != mine {
                        return;
                    }
                    match event {
                        stats::Event::Progress(name, done, total) => {
                            reading.set_label(&format!("{name}  ({done}/{total})"));
                        }
                        stats::Event::Done(result) => {
                            let race = Race::of(&result, &aside);
                            status.set_label(&status_line(&result, &race));
                            if race.racers.is_empty() {
                                stack.set_visible_child_name("empty");
                            } else if let Some(track) = track.upgrade() {
                                stack.set_visible_child_name("race");
                                track.load(race);
                            }
                            return;
                        }
                    }
                }
            });
        })
    };
    {
        let load = Rc::clone(&load);
        period.connect_selected_notify(move |_| load());
    }
    load();

    page
}

fn status_line(result: &Contributions, race: &Race) -> String {
    let mut parts = Vec::new();
    if !result.errors.is_empty() {
        parts.push(format!("{} unreadable", result.errors.len()));
    }
    let commits: usize = race.racers.iter().map(|racer| racer.running.last().copied().unwrap_or(0)).sum();
    parts.push(format!("{} {}", thousands(commits), plural(commits, "commit", "commits")));
    parts.push(format!("{} {}", race.racers.len(), plural(race.racers.len(), "author", "authors")));
    parts.push(format!("{}/{} repos active", result.repos_active, result.repos_scanned));
    let aside = result.authors.len() - race.racers.len();
    if aside > 0 {
        parts.push(format!("{aside} set aside"));
    }
    parts.join("  ·  ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stats::{Author, Window};

    fn author(name: &str, per_day: &[usize]) -> Author {
        Author {
            name: name.to_string(),
            email: format!("{}@example.invalid", name.to_lowercase()),
            commits: per_day.iter().sum(),
            merges: 0,
            insertions: 0,
            deletions: 0,
            files: 0,
            uncounted: 0,
            per_day: per_day.to_vec(),
            repos: Default::default(),
            last_seen: 0,
        }
    }

    fn result(authors: Vec<Author>) -> Contributions {
        let days = authors.first().map_or(0, |author| author.per_day.len());
        Contributions {
            window: Window {
                day_starts: (0..days as i64).map(|day| day * 86_400).collect(),
                day_labels: vec![String::new(); days],
                day_titles: vec![String::new(); days],
            },
            branches: Branches::CheckedOut,
            authors,
            co_authors: Vec::new(),
            mine: Default::default(),
            per_day_total: vec![0; days],
            repos_scanned: 1,
            repos_active: 1,
            total_commits: 0,
            total_insertions: 0,
            total_deletions: 0,
            uncounted: 0,
            future: 0,
            errors: Vec::new(),
        }
    }

    #[test]
    fn a_total_grows_from_one_day_to_the_next() {
        let race = Race::of(&result(vec![author("Ada", &[2, 0, 4])]), &BTreeSet::new());
        assert_eq!(race.racers[0].running, [0, 2, 2, 6]);
        assert_eq!(race.total(0, 0.0), 0.0);
        assert_eq!(race.total(0, 0.5), 1.0);
        assert_eq!(race.total(0, 1.0), 2.0);
        assert_eq!(race.total(0, 2.25), 3.0);
        assert_eq!(race.total(0, 3.0), 6.0);
        // Before the start and past the end it is the start and the end.
        assert_eq!((race.total(0, -1.0), race.total(0, 9.0)), (0.0, 6.0));
    }

    #[test]
    fn the_lead_changes_hands_and_a_tie_goes_by_name() {
        let race = Race::of(
            &result(vec![author("Bo", &[3, 0, 0]), author("ada", &[0, 1, 5]), author("Cy", &[1, 2, 0])]),
            &BTreeSet::new(),
        );
        let order = |t: f64| race.standings(t).iter().map(|(racer, _)| race.racers[*racer].name.clone()).collect::<Vec<_>>();
        assert_eq!(order(1.0), ["Bo", "Cy", "ada"]);
        // Day two: Bo 3, Cy 3, ada 1. Level, and Bo before Cy by name.
        assert_eq!(order(2.0), ["Bo", "Cy", "ada"]);
        assert_eq!(order(3.0), ["ada", "Bo", "Cy"]);
    }

    #[test]
    fn whoever_is_set_aside_or_has_nothing_does_not_race() {
        let aside: BTreeSet<String> = ["me@example.invalid".to_string()].into();
        let race = Race::of(&result(vec![author("Me", &[5, 5]), author("Them", &[1, 0]), author("Idle", &[0, 0])]), &aside);
        let names: Vec<&str> = race.racers.iter().map(|racer| racer.name.as_str()).collect();
        assert_eq!(names, ["Them"]);
    }

    #[test]
    fn the_date_is_the_day_the_race_is_in() {
        let mut race = Race::of(&result(vec![author("Ada", &[1, 1, 1])]), &BTreeSet::new());
        race.dates = vec!["1".into(), "2".into(), "3".into()];
        assert_eq!([race.date(0.0), race.date(1.5), race.date(3.0), race.date(-2.0)], ["1", "2", "3", "1"]);
    }
}
