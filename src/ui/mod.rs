pub mod card;
pub mod commit;
#[cfg(debug_assertions)]
pub mod debug;
pub mod dialogs;
pub mod graph;
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
    CopyPath,
}

/// A toast that shows `text` as it is. A toast's title is Pango markup unless
/// told otherwise, and most of what this application says in one came from
/// somewhere else: git's complaint, a hook's output, a branch or directory
/// name. An ampersand in any of those left the toast blank, which lost the one
/// sentence explaining why a push was refused.
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
/// seconds is not long enough to read why a push was refused when the eye was
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

/// What fetching or updating one repository came to, worded to follow the
/// repository's name: "billing: 3 commits pulled". One wording, because the
/// card and the graph page ask the same question and used to answer it in two
/// sets of words.
pub fn outcome_text(outcome: &crate::refresh::Outcome) -> String {
    use crate::refresh::Outcome;
    match outcome {
        Outcome::FastForwarded(1) => "1 commit pulled".to_string(),
        Outcome::FastForwarded(n) => format!("{n} commits pulled"),
        Outcome::UpToDate => "up to date".to_string(),
        Outcome::Behind(n) => format!("fetched, {n} behind"),
        Outcome::Skipped(skip) => format!("left alone: {}", skip.reason()),
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
    let delta = now - when;
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
pub fn open_terminal(cfg: &Config, dir: &Path) {
    let term = cfg.terminal_command();
    match cfg.terminal_runs_command() {
        Some(command) => run_in_terminal(&term, dir, &format!("{command}; exec \"$SHELL\"")),
        // Terminals inherit the working directory, so the cwd alone is enough
        // and we avoid per-emulator `--working-directory` flags.
        None => spawn(Command::new(&term).current_dir(dir), &term),
    }
}

/// Open `$EDITOR` inside a terminal at `dir`.
pub fn open_editor(cfg: &Config, dir: &Path) {
    let term = cfg.terminal_command();
    run_in_terminal(&term, dir, &editor_script(&cfg.editor_command()));
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
fn run_in_terminal(term: &str, dir: &Path, script: &str) {
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
        _ => {
            cmd.args(["-e", "sh", "-c", script]);
        }
    }
    spawn(&mut cmd, term);
}

pub fn open_folder(dir: &Path) {
    let uri = format!("file://{}", dir.display());
    if let Err(err) = gtk::gio::AppInfo::launch_default_for_uri(
        &uri,
        None::<&gtk::gio::AppLaunchContext>,
    ) {
        eprintln!("omarepos: could not open {uri}: {err}");
    }
}

/// Start something that outlives the click that started it: a terminal, an
/// editor. In a process group of its own, so that Ctrl+C in the terminal
/// omarepos was started from does not take every terminal it opened with it,
/// and waited for on a thread, so that it does not sit in the process table
/// as a zombie from the moment it is closed until omarepos is.
fn spawn(cmd: &mut Command, what: &str) {
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
        }
        Err(err) => eprintln!("omarepos: could not launch {what}: {err}"),
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
    fn a_different_key_is_a_different_press() {
        let presses = OncePerPress::default();
        let start = Instant::now();
        assert!(presses.first_at(gtk::gdk::Key::f, start));
        assert!(presses.first_at(gtk::gdk::Key::p, start + Duration::from_millis(100)));
        assert!(presses.first_at(gtk::gdk::Key::f, start + Duration::from_millis(200)));
    }
}
