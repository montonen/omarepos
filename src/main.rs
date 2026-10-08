//! omarepos — an Omarchy-styled overview of every git repo on your machine.

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
    let mut cfg = config::Config::load();
    let mut roots: Vec<String> = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                return glib::ExitCode::SUCCESS;
            }
            "-V" | "--version" => {
                println!("omarepos {}", env!("CARGO_PKG_VERSION"));
                return glib::ExitCode::SUCCESS;
            }
            "-r" | "--root" => match args.next() {
                Some(path) => roots.push(path),
                None => {
                    eprintln!("omarepos: --root needs a path");
                    return glib::ExitCode::FAILURE;
                }
            },
            "-d" | "--depth" => match args.next().and_then(|n| n.parse::<usize>().ok()) {
                Some(depth) => cfg.depth_override = Some(depth.max(1)),
                None => {
                    eprintln!("omarepos: --depth needs a number");
                    return glib::ExitCode::FAILURE;
                }
            },
            other => {
                eprintln!("omarepos: unknown argument {other}\n");
                print!("{USAGE}");
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
        let app = app.clone();
        quit.connect_activate(move |_, _| app.quit());
    }
    app.add_action(&quit);
    app.set_accels_for_action("app.quit", &["<Ctrl>q"]);

    // Said on the way out when another process answers: this one's build and
    // what it read from the config file are not what the user is about to
    // look at.
    if app.register(gtk::gio::Cancellable::NONE).is_ok() && app.is_remote() {
        eprintln!("omarepos: already running; that window has been brought forward");
    }

    // GTK's own argument parsing would choke on the flags handled above.
    let argv: [&str; 0] = [];
    app.run_with_args(&argv)
}

/// Whether the file this process was started from has since been replaced,
/// which is what installing a new build does. The kernel marks the link.
fn replaced_on_disk() -> bool {
    std::fs::read_link("/proc/self/exe")
        .is_ok_and(|exe| exe.to_string_lossy().ends_with(" (deleted)"))
}
