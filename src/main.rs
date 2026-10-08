//! omarepos — an Omarchy-styled overview of every git repo on your machine.

/// Say something on stderr, and do not mind if nobody is there to hear it.
/// `eprintln!` panics when the write fails, and stderr fails for dull reasons:
/// the terminal the program was started from has been closed, or the pipe it
/// was started into has stopped reading. Most of these are said from inside a
/// callback, where a panic ends the program, and in the middle of a pass
/// across the fleet, where it left the window saying "fetching 1/44" for good.
macro_rules! note {
    ($($arg:tt)*) => {{
        use std::io::Write as _;
        let _ = writeln!(std::io::stderr(), $($arg)*);
    }};
}

mod commit;
mod config;
mod git;
mod gitcmd;
mod graph;
mod lang;
mod meta;
mod refresh;
mod scan;
mod search;
mod stats;
#[cfg(test)]
mod testkit;
mod theme;
mod ui;

use std::cell::RefCell;
use std::rc::Rc;

use gtk::gdk;
use gtk::glib;
use gtk::prelude::*;

const APP_ID: &str = "se.alphadev.Omarepos";
const USAGE: &str = "\
omarepos — Omarchy-styled overview of your git repos

Usage: omarepos [options]

Options:
  -r, --root <PATH>   Directory to scan (repeatable; overrides the config file)
  -d, --depth <N>     How deep below each root to look for repos
  -h, --help          Show this help
  -V, --version       Show the version

Config: ~/.config/omarepos/config.toml
";

fn main() -> glib::ExitCode {
    // Before anything else, and before there is a second thread: see
    // `gitcmd::REPOSITORY_ENV`.
    for name in gitcmd::REPOSITORY_ENV {
        // SAFETY: nothing else is running yet to read the environment.
        unsafe { std::env::remove_var(name) };
    }
    let mut cfg = config::Config::load();
    let mut roots: Vec<String> = Vec::new();
    // Lossily: `std::env::args` panics on an argument that is not Unicode,
    // and a path may be anything.
    let mut args = std::env::args_os().skip(1).map(|arg| arg.to_string_lossy().into_owned());
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                out(USAGE);
                return glib::ExitCode::SUCCESS;
            }
            "-V" | "--version" => {
                out(concat!("omarepos ", env!("CARGO_PKG_VERSION"), "\n"));
                return glib::ExitCode::SUCCESS;
            }
            "-r" | "--root" => match args.next() {
                Some(path) => roots.push(path),
                None => {
                    note!("omarepos: --root needs a path");
                    return glib::ExitCode::FAILURE;
                }
            },
            "-d" | "--depth" => match args.next().and_then(|n| n.parse::<usize>().ok()) {
                Some(depth) => cfg.depth_override = Some(depth.max(1)),
                None => {
                    note!("omarepos: --depth needs a number");
                    return glib::ExitCode::FAILURE;
                }
            },
            other => {
                note!("omarepos: unknown argument {other}\n");
                out(USAGE);
                return glib::ExitCode::FAILURE;
            }
        }
    }
    if !roots.is_empty() {
        cfg.roots_override = Some(roots.into_iter().map(config::RootSpec::Simple).collect());
    }
    // An application with an id is single-instance: a second `omarepos` hands
    // a bare "activate" to the one already running and exits, and whatever was
    // on its command line goes with it. A run that was told where to look is
    // therefore its own process, or `-r` would do nothing whenever a window
    // happened to be open.
    let flags = if cfg.one_off() {
        gtk::gio::ApplicationFlags::NON_UNIQUE
    } else {
        gtk::gio::ApplicationFlags::empty()
    };
    let cfg = Rc::new(RefCell::new(cfg));

    let app = adw::Application::builder().application_id(APP_ID).flags(flags).build();

    app.connect_activate(move |app| {
        // One window to a process. "Activate" arrives again every time the
        // launcher key is pressed while the app is open on another workspace,
        // and building a second window built a second everything: its own
        // copy of the tags, to be saved over the first's, and its own refresh
        // timer, fetching the same repositories alongside it.
        if let Some(window) = app.active_window().or_else(|| app.windows().into_iter().next()) {
            window.present();
            // The launcher key is pressed to find a repository, by someone
            // who cannot see from the key whether a window was open already.
            // The keyboard goes where it is at a first launch: in the filter.
            let _ = WidgetExt::activate_action(&window, "win.launched-again", None);
            // The launcher key after a reinstall brings this window forward,
            // and this is the build from before. It looked exactly like the
            // new one having started.
            if replaced_on_disk() {
                let _ = WidgetExt::activate_action(&window, "win.updated-on-disk", None);
            }
            return;
        }
        // One provider for the whole app; the theme watcher reloads it in place.
        let provider = gtk::CssProvider::new();
        if let Some(display) = gdk::Display::default() {
            gtk::style_context_add_provider_for_display(
                &display,
                &provider,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
        }
        ui::window::build(app, Rc::clone(&cfg), provider);
    });

    let quit = gtk::gio::SimpleAction::new("quit", None);
    {
        // By closing the windows, not by `app.quit()`. Quit ends the process
        // there and then, under whatever git is in the middle of. A window
        // that closes gets to hold the application until git is done (see
        // the close handler in ui/window.rs), and with no window and nothing
        // held the application ends by itself.
        let app = app.clone();
        quit.connect_activate(move |_, _| {
            for window in app.windows() {
                window.close();
            }
        });
    }
    app.add_action(&quit);
    app.set_accels_for_action("app.quit", &["<Ctrl>q"]);

    // Said on the way out when another process answers: this one's build and
    // what it read from the config file are not what the user is about to
    // look at.
    if app.register(gtk::gio::Cancellable::NONE).is_ok() && app.is_remote() {
        note!("omarepos: already running; that window has been brought forward");
    }

    // GTK's own argument parsing would choke on the flags handled above.
    let argv: [&str; 0] = [];
    app.run_with_args(&argv)
}

/// Standard output, for the two things asked for there, written without
/// minding a reader that has gone (`omarepos --help | head -1`).
fn out(text: &str) {
    use std::io::Write as _;
    let _ = write!(std::io::stdout(), "{text}");
}

/// Whether the file this process was started from has since been replaced,
/// which is what installing a new build does. The kernel marks the link.
fn replaced_on_disk() -> bool {
    std::fs::read_link("/proc/self/exe")
        .is_ok_and(|exe| exe.to_string_lossy().ends_with(" (deleted)"))
}
