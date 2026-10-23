pub mod arcade;
pub mod card;
pub mod commit;
#[cfg(debug_assertions)]
pub mod debug;
pub mod dialogs;
pub mod graph;
pub mod race;
pub mod search;
pub mod shortcuts;
pub mod stats;
pub mod window;

use adw::prelude::*;

use std::path::Path;
use std::process::{Command, Stdio};

use crate::config::Config;

#[derive(Debug, Clone, Copy)]
pub enum Action {
    /// Drill into this repository's commit graph.
    CommitGraph,
    /// Contributions scoped to this one repository.
    Contributions,
    /// Edit this repository's tags.
    EditTags,
    OpenTerminal,
    OpenEditor,
    OpenFolder,
    /// The repository's page on the web, where its remote has one.
    OpenWeb,
    CopyPath,
}

/// A toast that shows `text` as it is. A toast's title is Pango markup unless
/// told otherwise, and most of what this application says in one came from
/// somewhere else: git's complaint, a hook's output, a branch or directory
/// name. An ampersand in any of those left the toast blank, which lost the one
/// sentence explaining why a fetch had failed.
pub fn toast(text: &str) -> adw::Toast {
    adw::Toast::builder().title(text).use_markup(false).build()
}

/// Say what git said. One line goes in a toast. More than one, which is what a
/// hook that refuses usually has to say, puts the first line in the toast with
/// a button that shows the rest: a toast holds a single line, and the lines
/// after the first are where a linter lists what it objected to.
pub fn say(overlay: &adw::ToastOverlay, text: &str) {
    let text = text.trim();
    let first = text.lines().next().unwrap_or_default();
    let toast = toast(first);
    if text.lines().count() > 1 {
        toast.set_button_label(Some("Show all"));
        toast.set_timeout(0);
        let overlay_for_dialog = overlay.clone();
        let whole = text.to_string();
        toast.connect_button_clicked(move |_| {
            let dialog = adw::AlertDialog::new(Some("What git said"), None);
            let body = gtk::Label::builder()
                .label(&whole)
                .selectable(true)
                .wrap(true)
                .xalign(0.0)
                .build();
            body.add_css_class("monospace");
            let scroller = gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Never)
                .propagate_natural_height(true)
                .max_content_height(360)
                .min_content_width(520)
                .child(&body)
                .build();
            dialog.set_extra_child(Some(&scroller));
            dialog.add_response("close", "Close");
            dialog.set_default_response(Some("close"));
            dialog.set_close_response("close");
            dialog.present(Some(&overlay_for_dialog));
        });
    }
    overlay.add_toast(toast);
}

/// A toast for something that went wrong. It stays until it is dismissed: five
/// seconds is not long enough to read why a fetch failed when the eye was
/// on the terminal next door, and when it had gone there was nothing left on
/// screen to say that anything had been.
pub fn complain(overlay: &adw::ToastOverlay, text: &str) {
    let text = text.trim();
    if text.lines().count() > 1 {
        // Already stays up, with a button for the rest.
        say(overlay, text);
        return;
    }
    let toast = toast(text);
    toast.set_timeout(0);
    overlay.add_toast(toast);
}

/// What fetching one repository came to, worded to follow the repository's
/// name: "billing: fetched, 3 behind". One wording, because the card and the
/// graph page ask the same question and used to answer it in two sets of
/// words.
pub fn outcome_text(outcome: &crate::refresh::Outcome) -> String {
    use crate::refresh::Outcome;
    match outcome {
        Outcome::UpToDate => "fetched, nothing new".to_string(),
        Outcome::Behind(n) => format!("fetched, {n} behind"),
        Outcome::FastForwarded(1) => "1 commit pulled".to_string(),
        Outcome::FastForwarded(n) => format!("{n} commits pulled"),
        Outcome::Skipped(skip) if skip.after_fetch() => {
            format!("fetched, not updated: {}", skip.reason())
        }
        Outcome::Skipped(skip) => format!("not fetched: {}", skip.reason()),
        Outcome::Refused(why) => format!("fetched, not updated. {why}"),
        Outcome::Failed(err) => err.clone(),
    }
}

/// How long after a press the same key is still the same press. A held key
/// repeats: once after the keyboard's delay, four to six tenths of a second,
/// and then some thirty times a second.
const SAME_PRESS: std::time::Duration = std::time::Duration::from_millis(700);

/// Lets a key that starts something outside the window (a terminal, an editor,
/// a fetch) do it once for each time it is pressed. GTK delivers a held key as
/// a run of presses, and a key held a second too long, or one that sticks,
/// opened a terminal per repeat and started as many fetches of one repository.
#[derive(Default)]
pub struct OncePerPress {
    last: std::cell::Cell<Option<(gtk::gdk::Key, std::time::Instant)>>,
}

impl OncePerPress {
    /// Whether this is a new press of `key`, and not the last one still held.
    pub fn first(&self, key: gtk::gdk::Key) -> bool {
        self.first_at(key, std::time::Instant::now())
    }

    /// Every press moves the mark, taken or not, so a key held for a minute is
    /// still one press at the end of it.
    fn first_at(&self, key: gtk::gdk::Key, now: std::time::Instant) -> bool {
        let held = self
            .last
            .get()
            .is_some_and(|(last, when)| last == key && now.duration_since(when) < SAME_PRESS);
        self.last.set(Some((key, now)));
        !held
    }
}

/// Human-friendly age of a unix timestamp, in the style git itself uses.
pub fn relative_time(when: i64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(when);
    // Saturating: a commit can carry any date at all, and one from the far
    // side of the calendar must not be what stops a page from drawing.
    let delta = now.saturating_sub(when);
    if delta < 0 {
        return "in the future".into();
    }
    const MINUTE: i64 = 60;
    const HOUR: i64 = 60 * MINUTE;
    const DAY: i64 = 24 * HOUR;
    const FORTNIGHT: i64 = 14 * DAY;
    const MONTH: i64 = 30 * DAY;
    const YEAR: i64 = 365 * DAY;
    let (n, unit) = match delta {
        d if d < MINUTE => return "just now".into(),
        d if d < HOUR => (d / MINUTE, "minute"),
        d if d < DAY => (d / HOUR, "hour"),
        // Days up to a fortnight: "1 week ago" for thirteen days throws away
        // most of what the reader wanted to know.
        d if d < FORTNIGHT => (d / DAY, "day"),
        d if d < MONTH => (d / (7 * DAY), "week"),
        d if d < YEAR => (d / MONTH, "month"),
        d => (d / YEAR, "year"),
    };
    if n == 1 {
        format!("1 {unit} ago")
    } else {
        format!("{n} {unit}s ago")
    }
}

/// Open a terminal in `dir`, running whatever the config wants started there
/// first — lazygit by default, since a terminal opened on a repository is
/// nearly always opened to do something with git.
///
/// The shell is left behind when that command exits, so the terminal is still
/// a terminal afterwards rather than closing out from under you.
///
/// An error is a sentence for the window to show. A terminal that could not
/// be started used to be said on stderr only, which under a desktop launcher
/// is nowhere: the key or the menu item did nothing, and said nothing.
pub fn open_terminal(cfg: &Config, dir: &Path) -> Result<(), String> {
    let term = cfg.terminal_command();
    match cfg.terminal_runs_command() {
        Some(command) => run_in_terminal(&term, dir, &format!("{command}; exec \"$SHELL\"")),
        // Terminals inherit the working directory, so the cwd alone is enough
        // and we avoid per-emulator `--working-directory` flags.
        None => spawn(Command::new(&term).current_dir(dir), &term),
    }
}

/// Open `$EDITOR` inside a terminal at `dir`.
pub fn open_editor(cfg: &Config, dir: &Path) -> Result<(), String> {
    let term = cfg.terminal_command();
    run_in_terminal(&term, dir, &editor_script(&cfg.editor_command()))
}

/// What to run for "open in editor". The repository is handed over as `.`,
/// which is what makes nvim, helix, emacs or code open a project. An editor
/// with no view of a directory answers that with an error and an empty
/// buffer, so those are started bare, in the repository.
fn editor_script(editor: &str) -> String {
    let program = editor.split_whitespace().next().unwrap_or_default();
    let name = Path::new(program)
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default();
    if matches!(name.as_str(), "nano" | "pico" | "micro" | "ed") {
        editor.to_string()
    } else {
        format!("{editor} .")
    }
}

/// Run a shell command inside a terminal emulator, in `dir`. The flag that
/// takes a command differs per emulator, hence the arms.
fn run_in_terminal(term: &str, dir: &Path, script: &str) -> Result<(), String> {
    let base = Path::new(term)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| term.to_string());

    let mut cmd = Command::new(term);
    cmd.current_dir(dir);
    match base.as_str() {
        // The default-terminal launcher takes the command directly. It is what
        // $TERMINAL points at on a spec-following desktop, and passing it `-e`
        // silently does the wrong thing.
        "xdg-terminal-exec" => {
            cmd.args(["--", "sh", "-c", script]);
        }
        "kitty" => {
            cmd.args(["sh", "-c", script]);
        }
        "wezterm" => {
            cmd.args(["start", "--", "sh", "-c", script]);
        }
        // These two take the command after `--`, and take `-e` for one word.
        "gnome-terminal" | "ptyxis" => {
            cmd.args(["--", "sh", "-c", script]);
        }
        _ => {
            cmd.args(["-e", "sh", "-c", script]);
        }
    }
    spawn(&mut cmd, term)
}

pub fn open_folder(dir: &Path) {
    let uri = format!("file://{}", dir.display());
    if let Err(err) = gtk::gio::AppInfo::launch_default_for_uri(
        &uri,
        None::<&gtk::gio::AppLaunchContext>,
    ) {
        note!("omarepos: could not open {uri}: {err}");
    }
}

/// Send the desktop's browser to `page`, an address from
/// [`crate::git::web_url`].
///
/// With the display's launch context, which is what lets the compositor
/// bring the browser forward. A browser that is already running takes the
/// address as a new tab, and without the context that tab opens on a
/// workspace nobody is looking at.
pub fn open_web(page: &str) -> Result<(), String> {
    // A walk presses every key there is, and nothing it presses may leave
    // the window. It reads what would have been opened out of the log.
    #[cfg(debug_assertions)]
    if debug::driven() {
        note!("omarepos: would open {page}");
        return Ok(());
    }
    let context = gtk::gdk::Display::default().map(|display| display.app_launch_context());
    gtk::gio::AppInfo::launch_default_for_uri(page, context.as_ref())
        .map_err(|err| format!("Could not open {page}: {err}"))
}

/// Start something that outlives the click that started it: a terminal, an
/// editor. In a process group of its own, so that Ctrl+C in the terminal
/// omarepos was started from does not take every terminal it opened with it,
/// and waited for on a thread, so that it does not sit in the process table
/// as a zombie from the moment it is closed until omarepos is.
fn spawn(cmd: &mut Command, what: &str) -> Result<(), String> {
    use std::os::unix::process::CommandExt;
    let result = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn();
    match result {
        Ok(mut child) => {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
            Ok(())
        }
        Err(err) => Err(format!(
            "Could not start {what}: {err}. Set a terminal in Settings (Ctrl+,)."
        )),
    }
}

/// A small filled circle in an arbitrary colour. GTK4 has no inline styles, so
/// per-language colours are drawn rather than set through CSS.
pub fn color_dot(color: &str) -> gtk::DrawingArea {
    let area = gtk::DrawingArea::builder()
        .content_width(8)
        .content_height(8)
        .valign(gtk::Align::Center)
        .build();
    let (r, g, b) = crate::theme::Palette::rgb(color);
    area.set_draw_func(move |_, cr, width, height| {
        let radius = (width.min(height) as f64) / 2.0;
        cr.set_source_rgb(r, g, b);
        cr.arc(width as f64 / 2.0, height as f64 / 2.0, radius, 0.0, std::f64::consts::TAU);
        let _ = cr.fill();
    });
    area
}


/// Is this an unmodified keypress?
///
/// Not `state.is_empty()`: GDK reports lock bits — NumLock, CapsLock, ScrollLock
/// — in the same field, so a plain letter looks modified to anyone with NumLock
/// on. Only the modifiers that change what a key means are consulted.
pub fn unmodified(state: gtk::gdk::ModifierType) -> bool {
    !state.intersects(
        gtk::gdk::ModifierType::CONTROL_MASK
            | gtk::gdk::ModifierType::ALT_MASK
            | gtk::gdk::ModifierType::SUPER_MASK
            | gtk::gdk::ModifierType::SHIFT_MASK,
    )
}

/// A fixed accent ramp for graph lanes and author colours. Deliberately not
/// taken from the Omarchy palette: these have to stay distinguishable from each
/// other in every theme, which a theme's own handful of hues cannot promise.
const ACCENTS: [&str; 8] = [
    "#7e9cd8", "#98bb6c", "#dca561", "#e46876", "#957fb8", "#7aa89f", "#d27e99", "#c0a36e",
];

/// The same ramp for a light background. The one above is pastel, tuned for
/// dark themes, and was used on light ones unchanged: six or seven of its
/// eight colours sat under 3:1 against the page, as lines 1.6 px wide. These
/// measure 4.49:1 or better on the five stock light backgrounds.
const ACCENTS_ON_LIGHT: [&str; 8] = [
    "#3b5fb0", "#4f7a1f", "#9a5b00", "#b3263a", "#6b46a8", "#1f7a6d", "#a8386a", "#5c6470",
];

/// Which of the two ramps is in use. Set when the theme is applied, and read
/// where a colour is drawn: asking the palette there would read two files per
/// stroke.
static ON_LIGHT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Tell the ramps what kind of background they are drawn on.
pub fn accents_for(dark: bool) {
    ON_LIGHT.store(!dark, std::sync::atomic::Ordering::Relaxed);
}

fn accents() -> &'static [&'static str; 8] {
    if ON_LIGHT.load(std::sync::atomic::Ordering::Relaxed) {
        &ACCENTS_ON_LIGHT
    } else {
        &ACCENTS
    }
}

/// The first colour of the ramp, for a mark that is not one of several.
pub fn accent() -> &'static str {
    accents()[0]
}

pub fn lane_color(lane: usize) -> &'static str {
    accents()[lane % ACCENTS.len()]
}

/// Stable colour per author, so the same person keeps their colour between runs.
pub fn key_color(key: &str) -> &'static str {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in key.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    accents()[(hash % ACCENTS.len() as u64) as usize]
}

/// Absolute local timestamp, for tooltips.
pub fn absolute_time(when: i64) -> String {
    gtk::glib::DateTime::from_unix_local(when)
        .ok()
        .and_then(|dt| dt.format("%Y-%m-%d %H:%M").ok())
        .map(|s| s.to_string())
        .unwrap_or_else(|| when.to_string())
}

/// Build the day buckets for a contributions window, in the local timezone.
/// Index 0 is the oldest day; the last entry is today.
/// How far back a period reaches: whole days back to a local midnight, or
/// hours back from this moment. "Last 24 hours" is not "today": at nine in
/// the morning today is nine hours long, and last night's work is in
/// yesterday.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Span {
    Days(usize),
    /// A whole number of days' worth: the window is cut into 24-hour columns.
    Hours(usize),
}

impl Span {
    /// How many columns of a day the window has.
    pub fn days(self) -> usize {
        match self {
            Span::Days(days) => days,
            Span::Hours(hours) => hours.div_ceil(24),
        }
    }

    pub fn window(self) -> crate::stats::Window {
        match self {
            Span::Days(days) => day_window(days),
            Span::Hours(hours) => hour_window(hours),
        }
    }
}

/// The periods Contributions and its race are read over, as their
/// drop-downs say them. The same eight on both, since both read the same
/// history. This year runs from the first of January.
pub fn periods() -> [(&'static str, Span); 8] {
    let this_year = gtk::glib::DateTime::now_local().map_or(365, |now| now.day_of_year().max(1) as usize);
    [
        ("Last 24 hours", Span::Hours(24)),
        ("Last 48 hours", Span::Hours(48)),
        ("Last 7 days", Span::Days(7)),
        ("Last 14 days", Span::Days(14)),
        ("Last 30 days", Span::Days(30)),
        ("Last 90 days", Span::Days(90)),
        ("This year", Span::Days(this_year)),
        ("Last 12 months", Span::Days(365)),
    ]
}

/// Which of [`periods`] a page opens on: the last 7 days, as it always has.
pub const DEFAULT_PERIOD: u32 = 2;

/// Days drawn as one: a column of the overview, a step of a sparkline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bucket {
    pub days: std::ops::Range<usize>,
    /// Under its column: "Wed", "6 Oct", "Oct".
    pub label: String,
    /// In its tooltip: "Wed 15 Oct", "6 Oct – 12 Oct", "October 2026".
    pub title: String,
}

/// A window's days as they are drawn: one at a time up to a month, a week at
/// a time up to three months, and a month at a time beyond. A column a day
/// over a year was 365 columns of the overview and a sparkline 4,000 pixels
/// wide. Weeks run Monday to Sunday and months are the calendar's, so the
/// first and the last can be short.
pub fn buckets(window: &crate::stats::Window) -> Vec<Bucket> {
    use gtk::glib::DateTime;
    let days = window.days();
    if days <= 31 {
        return (0..days)
            .map(|day| Bucket {
                days: day..day + 1,
                label: window.day_labels.get(day).cloned().unwrap_or_default(),
                title: window.day_titles.get(day).cloned().unwrap_or_default(),
            })
            .collect();
    }
    let weekly = days <= 92;
    let dates: Vec<Option<DateTime>> =
        window.day_starts.iter().map(|start| DateTime::from_unix_local(*start).ok()).collect();
    let group = |date: &DateTime| {
        if weekly { (date.week_numbering_year(), date.week_of_year()) } else { (date.year(), date.month()) }
    };
    let said = |date: Option<&DateTime>, how: &str| {
        date.and_then(|date| date.format(how).ok()).map(|said| said.to_string()).unwrap_or_default()
    };
    let mut buckets = Vec::new();
    let mut start = 0;
    for day in 1..=days {
        let ends = day == days
            || match (&dates[day - 1], &dates[day]) {
                (Some(before), Some(now)) => group(before) != group(now),
                _ => true,
            };
        if !ends {
            continue;
        }
        let (first, last) = (dates[start].as_ref(), dates[day - 1].as_ref());
        let (label, title) = if weekly {
            (said(first, "%-d %b"), format!("{} – {}", said(first, "%-d %b"), said(last, "%-d %b")))
        } else {
            (said(first, "%b"), said(first, "%B %Y"))
        };
        buckets.push(Bucket { days: start..day, label, title });
        start = day;
    }
    buckets
}

/// The last `hours` back from now, as columns of 24 hours, the newest last.
/// Each is labelled by how long ago it ends, since a column of "Wed" would
/// say a calendar day it is not.
fn hour_window(hours: usize) -> crate::stats::Window {
    const DAY: i64 = 86_400;
    let columns = hours.div_ceil(24).max(1) as i64;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let mut window = crate::stats::Window { day_starts: Vec::new(), day_labels: Vec::new(), day_titles: Vec::new() };
    for back in (1..=columns).rev() {
        let start = now - back * DAY;
        window.day_starts.push(start);
        window.day_labels.push(if back == 1 {
            "Last 24 h".to_string()
        } else {
            format!("{}–{} h ago", (back - 1) * 24, back * 24)
        });
        let since = gtk::glib::DateTime::from_unix_local(start)
            .ok()
            .and_then(|at| at.format("%a %-d %b %H:%M").ok())
            .map(|at| at.to_string())
            .unwrap_or_default();
        window.day_titles.push(format!("24 hours from {since}"));
    }
    window
}

pub fn day_window(days: usize) -> crate::stats::Window {
    let days = days.max(1);
    let mut day_starts = Vec::with_capacity(days);
    let mut day_labels = Vec::with_capacity(days);
    let mut day_titles = Vec::with_capacity(days);

    if let Ok(now) = gtk::glib::DateTime::now_local() {
        if let Ok(midnight) = gtk::glib::DateTime::from_local(
            now.year(),
            now.month(),
            now.day_of_month(),
            0,
            0,
            0.0,
        ) {
            for back in (0..days as i32).rev() {
                let Ok(day) = midnight.add_days(-back) else { continue };
                day_starts.push(day.to_unix());
                day_labels.push(
                    day.format("%a")
                        .map(|s| s.to_string())
                        .unwrap_or_default(),
                );
                day_titles.push(
                    day.format("%a %-d %b")
                        .map(|s| s.to_string())
                        .unwrap_or_default(),
                );
            }
        }
    }

    if day_starts.is_empty() {
        // No local timezone to be had; fall back to rolling 24-hour buckets.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let today = now - now.rem_euclid(86_400);
        for back in (0..days as i64).rev() {
            day_starts.push(today - back * 86_400);
            day_labels.push(format!("-{back}d"));
            day_titles.push(format!("{back} days ago"));
        }
    }

    crate::stats::Window { day_starts, day_labels, day_titles }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn a_held_key_is_one_press_however_long_it_is_held() {
        let presses = OncePerPress::default();
        let start = Instant::now();
        let key = gtk::gdk::Key::p;
        assert!(presses.first_at(key, start));
        // The keyboard's delay, then thirty repeats a second for two seconds.
        let mut at = start + Duration::from_millis(400);
        for _ in 0..60 {
            assert!(!presses.first_at(key, at), "a repeat was taken for a press");
            at += Duration::from_millis(33);
        }
        // Let go, and pressed again a second later.
        assert!(presses.first_at(key, at + Duration::from_secs(1)));
    }

    #[test]
    fn an_editor_is_handed_the_repository_only_if_it_can_show_one() {
        assert_eq!(editor_script("nvim"), "nvim .");
        assert_eq!(editor_script("code --wait"), "code --wait .");
        assert_eq!(editor_script("nano"), "nano");
        assert_eq!(editor_script("/usr/bin/micro -softwrap"), "/usr/bin/micro -softwrap");
    }

    #[test]
    fn the_ramp_follows_the_background() {
        accents_for(true);
        assert_eq!(lane_color(0), "#7e9cd8");
        assert_eq!(lane_color(8), lane_color(0), "it wraps");
        accents_for(false);
        assert_eq!(lane_color(0), "#3b5fb0");
        assert_eq!(accent(), "#3b5fb0");
        assert_ne!(key_color("fixture@example.invalid"), "");
        accents_for(true);
    }

    #[test]
    fn any_date_at_all_has_an_age() {
        assert_eq!(relative_time(i64::MIN), relative_time(i64::MIN + 1));
        assert_eq!(relative_time(i64::MAX), "in the future");
        assert_eq!(relative_time(0).split(' ').last(), Some("ago"));
    }

    #[test]
    fn a_different_key_is_a_different_press() {
        let presses = OncePerPress::default();
        let start = Instant::now();
        assert!(presses.first_at(gtk::gdk::Key::f, start));
        assert!(presses.first_at(gtk::gdk::Key::p, start + Duration::from_millis(100)));
        assert!(presses.first_at(gtk::gdk::Key::f, start + Duration::from_millis(200)));
    }

    /// Every day of a period is in one bucket and in order, a day at a time
    /// up to a month, a week up to three months, and a month beyond.
    #[test]
    fn a_period_is_drawn_by_the_day_the_week_or_the_month() {
        for (days, most, fewest, longest) in [(7, 7, 7, 1), (30, 30, 30, 1), (90, 15, 13, 7), (365, 13, 12, 31)] {
            let buckets = buckets(&day_window(days));
            let mut next = 0;
            for bucket in &buckets {
                assert_eq!(bucket.days.start, next, "{days} days: a gap or an overlap at {next}");
                assert!(!bucket.days.is_empty() && bucket.days.len() <= longest, "{days} days: {:?}", bucket.days);
                assert!(!bucket.label.is_empty() && !bucket.title.is_empty());
                next = bucket.days.end;
            }
            assert_eq!(next, days, "{days} days: not every day is drawn");
            assert!((fewest..=most).contains(&buckets.len()), "{days} days in {} buckets", buckets.len());
        }
    }

    #[test]
    fn the_periods_run_from_a_day_to_twelve_months() {
        let periods = periods();
        let spans: Vec<Span> = periods.iter().map(|(_, span)| *span).collect();
        assert_eq!(spans[..2], [Span::Hours(24), Span::Hours(48)]);
        let days: Vec<usize> = spans[2..].iter().map(|span| span.days()).collect();
        assert_eq!(days[..4], [7, 14, 30, 90]);
        assert!((1..=366).contains(&days[4]), "this year is {} days", days[4]);
        assert_eq!(days[5], 365);
        assert_eq!(periods[DEFAULT_PERIOD as usize].0, "Last 7 days");
    }

    /// The hours are counted back from now, a column of 24 to each day's
    /// worth, and nothing written in the last moment is left out.
    #[test]
    fn the_last_hours_are_columns_of_twenty_four_ending_now() {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
        let window = Span::Hours(48).window();
        assert_eq!(window.days(), 2);
        let starts = &window.day_starts;
        assert!((now - 48 * 3600 - starts[0]).abs() <= 2, "{starts:?}");
        assert_eq!(starts[1] - starts[0], 24 * 3600);
        assert_eq!(window.day_labels, ["24–48 h ago", "Last 24 h"]);
        assert_eq!(buckets(&window).len(), 2);
        assert_eq!(Span::Hours(24).window().days(), 1);
    }
}
