//! The keyboard, written down.
//!
//! Every binding in the app used to live in a tooltip or the README. A
//! keyboard-first application whose keys are undiscoverable is keyboard-first
//! only for the person who wrote it.

use adw::prelude::*;
use gtk::pango;

/// Section, then (keys, what it does). Keys are written the way they are typed.
const SECTIONS: &[(&str, &[(&str, &str)])] = &[
    (
        "Anywhere",
        &[
            ("F1  or  ?", "This list (? on the grid and in a commit graph)"),
            ("Ctrl+,", "Settings"),
            ("Ctrl+Q", "Quit"),
            ("Esc", "Back out: the text, then the page, then the selection, then the filter"),
            ("Alt+←", "Back"),
        ],
    ),
    (
        "The grid",
        &[
            ("↑ ↓ ← →", "Move between repositories"),
            ("Enter", "Open the commit graph"),
            ("Space", "Tick this repository"),
            ("Ctrl+A", "Tick everything shown, again to untick"),
            ("Ctrl+Shift+A", "Clear the selection"),
            ("Ctrl+L", "Put the keyboard back in the grid"),
            ("Ctrl+F  or  /", "Filter — try tag:work or lang:rust. It has the keyboard at launch"),
            ("Enter in the filter", "Open the top hit"),
            ("↓ in the filter", "Into the repositories"),
        ],
    ),
    (
        "A repository",
        &[
            ("l", "Open a terminal there — lazygit"),
            ("e", "Open it in your editor"),
            ("o", "Open its page on the web — GitHub, or wherever the remote is"),
            ("p", "Fetch, and fast-forward it if it is clean and only behind"),
            ("f", "Fetch it — your branches and files stay as they are"),
        ],
    ),
    (
        "The commit graph",
        &[
            ("↑ ↓", "Move; the pane below follows"),
            ("Enter", "Copy the sha (so does a double click)"),
            ("Tab", "Into the changed files"),
            ("Ctrl+D", "Show or hide the pane"),
            ("Ctrl+R  or  F5", "Read this repository again"),
        ],
    ),
    (
        "Changed files",
        &[
            ("↑ ↓", "Move; the diff follows"),
        ],
    ),
    (
        "Across repositories",
        &[
            ("Ctrl+G", "Contributions"),
            ("Ctrl+Shift+F", "Search commits"),
            ("Ctrl+R  or  F5", "Rescan what is on disk"),
            ("Ctrl+Shift+R", "Fetch every repository in scope, now"),
        ],
    ),
];

pub fn dialog() -> adw::Dialog {
    let body = gtk::Box::new(gtk::Orientation::Vertical, 18);
    body.set_margin_top(18);
    body.set_margin_bottom(24);
    body.set_margin_start(20);
    body.set_margin_end(20);

    for (title, rows) in SECTIONS {
        let section = gtk::Box::new(gtk::Orientation::Vertical, 6);

        let heading = gtk::Label::builder().label(*title).xalign(0.0).build();
        heading.add_css_class("om-name");
        section.append(&heading);

        let grid = gtk::Grid::builder().column_spacing(18).row_spacing(4).build();
        for (index, (keys, what)) in rows.iter().enumerate() {
            let key = gtk::Label::builder()
                .label(*keys)
                .xalign(1.0)
                .width_chars(18)
                .build();
            key.add_css_class("om-sha");
            grid.attach(&key, 0, index as i32, 1, 1);

            let text = gtk::Label::builder()
                .label(*what)
                .xalign(0.0)
                .hexpand(true)
                .wrap(true)
                .wrap_mode(pango::WrapMode::WordChar)
                .build();
            text.add_css_class("om-summary");
            grid.attach(&text, 1, index as i32, 1, 1);
        }
        section.append(&grid);
        body.append(&section);
    }

    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&body)
        .build();

    let view = adw::ToolbarView::new();
    view.add_top_bar(&adw::HeaderBar::new());
    view.set_content(Some(&scroller));

    adw::Dialog::builder()
        .title("Keyboard")
        .content_width(560)
        .content_height(680)
        .child(&view)
        .build()
}
