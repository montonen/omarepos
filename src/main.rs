//! omarepos — an Omarchy-styled overview of every git repo on your machine.

mod config;
mod git;
mod graph;
mod lang;
mod meta;
mod scan;
mod stats;
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
                Some(depth) => cfg.max_depth = depth.max(1),
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
        cfg.roots = roots;
    }
    let cfg = Rc::new(RefCell::new(cfg));

    let app = adw::Application::builder().application_id(APP_ID).build();

    app.connect_activate(move |app| {
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

    // GTK's own argument parsing would choke on the flags handled above.
    let argv: [&str; 0] = [];
    app.run_with_args(&argv)
}
