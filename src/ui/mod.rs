pub mod card;
pub mod window;

use std::path::Path;
use std::process::{Command, Stdio};

use crate::config::Config;

#[derive(Debug, Clone, Copy)]
pub enum Action {
    OpenTerminal,
    OpenEditor,
    OpenFolder,
    CopyPath,
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
    const WEEK: i64 = 7 * DAY;
    const MONTH: i64 = 30 * DAY;
    const YEAR: i64 = 365 * DAY;
    let (n, unit) = match delta {
        d if d < MINUTE => return "just now".into(),
        d if d < HOUR => (d / MINUTE, "minute"),
        d if d < DAY => (d / HOUR, "hour"),
        d if d < WEEK => (d / DAY, "day"),
        d if d < MONTH => (d / WEEK, "week"),
        d if d < YEAR => (d / MONTH, "month"),
        d => (d / YEAR, "year"),
    };
    if n == 1 {
        format!("1 {unit} ago")
    } else {
        format!("{n} {unit}s ago")
    }
}

/// Open a terminal in `dir`. Terminals inherit the working directory, so the
/// cwd alone is enough and we avoid per-emulator `--working-directory` flags.
pub fn open_terminal(cfg: &Config, dir: &Path) {
    let term = cfg.terminal_command();
    spawn(Command::new(&term).current_dir(dir), &term);
}

/// Open `$EDITOR` inside a terminal at `dir`. The flag that takes a command
/// differs per emulator, hence the per-terminal arms.
pub fn open_editor(cfg: &Config, dir: &Path) {
    let term = cfg.terminal_command();
    let editor = cfg.editor_command();
    let base = Path::new(&term)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| term.clone());

    let mut cmd = Command::new(&term);
    cmd.current_dir(dir);
    match base.as_str() {
        "kitty" => {
            cmd.args([editor.as_str(), "."]);
        }
        "wezterm" => {
            cmd.args(["start", "--", editor.as_str(), "."]);
        }
        _ => {
            cmd.args(["-e", editor.as_str(), "."]);
        }
    }
    spawn(&mut cmd, &term);
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

fn spawn(cmd: &mut Command, what: &str) {
    let result = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    if let Err(err) = result {
        eprintln!("omarepos: could not launch {what}: {err}");
    }
}
