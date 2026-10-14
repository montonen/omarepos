//! A way to read the window without looking at it. Debug builds only.
//!
//! Started with `OMAREPOS_DUMP=<file>`, the window gains a `dump` action that
//! writes what is on screen to that file as text: every mapped widget that
//! says something, with where the keyboard is and what is selected; a `press`
//! action that clicks a button by what it says; and a `key` action that hands
//! a key to the application's own handlers. It is how a change to a page gets
//! checked from a script, with the window on a workspace nobody is looking
//! at, and without a single event passing through the compositor:
//!
//! ```text
//! gdbus call --session --dest <bus name> \
//!     --object-path /se/montonen/Omarepos/window/1 \
//!     --method org.gtk.Actions.Activate dump '[]' '{}'
//! ```

use adw::prelude::*;
use gtk::gio;

thread_local! {
    /// Set while `press` clicks a button it was told to click with Ctrl held
    /// ("ctrl+" in front of what the button says), and to "not held" while
    /// it clicks any other: the keyboard of a window nobody is using says
    /// nothing worth going by.
    static CTRL: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

/// Whether the click being handled was made with Ctrl held, when it was the
/// driver's. `None` for a click of anybody else's.
pub fn ctrl_held() -> Option<bool> {
    CTRL.with(|ctrl| ctrl.get())
}

/// Whether this window is being driven by a script rather than a person.
/// What would leave the window (a browser being opened) asks, and says what
/// it would have done instead of doing it.
pub fn driven() -> bool {
    std::env::var_os("OMAREPOS_DUMP").is_some()
}

pub fn install(window: &adw::ApplicationWindow) {
    let Some(path) = std::env::var_os("OMAREPOS_DUMP") else { return };
    // A window's actions are on the bus only where GTK puts them there,
    // which is under Wayland and X11. The application's own are there on any
    // display, so one of those passes a window action on: `app.window`,
    // with the action's name and then its argument if it takes one. (A list
    // of strings, since the bus has no way to say "a string or nothing".)
    // It is what lets a window be driven on a display of its own, GTK's
    // Broadway, which sends frames to a window nobody is looking at.
    if let Some(app) = window.application() {
        let pass = gio::SimpleAction::new("window", Some(gtk::glib::VariantTy::STRING_ARRAY));
        let window_weak = window.downgrade();
        pass.connect_activate(move |_, wanted| {
            let Some(window) = window_weak.upgrade() else { return };
            let words = wanted.and_then(|wanted| wanted.get::<Vec<String>>()).unwrap_or_default();
            let Some(name) = words.first() else { return };
            let argument = words.get(1).map(|argument| argument.to_variant());
            gio::prelude::ActionGroupExt::activate_action(&window, name, argument.as_ref());
        });
        app.add_action(&pass);
    }
    // `dump` writes what is on screen; `dump-all` everything there is, with
    // what is not on screen included, which is the only way to read a dialog
    // in a window that gets no frames.
    for (name, everything) in [("dump", false), ("dump-all", true)] {
        let action = gio::SimpleAction::new(name, None);
        let window_weak = window.downgrade();
        let path = path.clone();
        action.connect_activate(move |_, _| {
            if let Some(window) = window_weak.upgrade() {
                write_dump(&window, &path, everything);
            }
        });
        window.add_action(&action);
    }

    // And a hand for the script: `press` with a string clicks the first
    // button on screen whose label or tooltip contains it, and "ctrl+" in
    // front clicks it with Ctrl held.
    let press = gio::SimpleAction::new("press", Some(gtk::glib::VariantTy::STRING));
    let window_weak = window.downgrade();
    press.connect_activate(move |_, wanted| {
        let Some(window) = window_weak.upgrade() else { return };
        let Some(wanted) = wanted.and_then(|wanted| wanted.str()) else { return };
        let (wanted, ctrl) = match wanted.strip_prefix("ctrl+") {
            Some(rest) => (rest, true),
            None => (wanted, false),
        };
        // A dialog in a window that is sent no frames is never laid out,
        // and so never on screen. "!" in front reaches into it anyway.
        let (wanted, unseen) = match wanted.strip_prefix('!') {
            Some(rest) => (rest, true),
            None => (wanted, false),
        };
        match find_button(window.upcast_ref(), wanted, unseen) {
            Some(button) if button.is_sensitive() => {
                CTRL.with(|held| held.set(Some(ctrl)));
                button.emit_clicked();
                CTRL.with(|held| held.set(None));
            }
            Some(_) => note!("omarepos: {wanted:?} is not sensitive"),
            None => note!("omarepos: no button says {wanted:?}"),
        }
    });
    window.add_action(&press);

    // And a key, handed straight to the application's own key handlers: the
    // controller on the widget with the keyboard, then on each widget above
    // it, until one takes it. `key` takes the key's name with any modifiers
    // in front: "p", "Tab", "shift+question", "ctrl+d". Nothing goes through
    // the compositor, which matters: keys injected there take the keyboard
    // away from whatever the person at the desk is typing in. What this does
    // not reach is GTK's own handling (arrow keys in a list) and the window's
    // accelerators, which are actions and can be activated as such.
    let key = gio::SimpleAction::new("key", Some(gtk::glib::VariantTy::STRING));
    let window_weak = window.downgrade();
    key.connect_activate(move |_, wanted| {
        let Some(window) = window_weak.upgrade() else { return };
        // A key that is really pressed makes GTK show where the keyboard is,
        // which the stylesheet's `:focus-visible` rules wait for. This one
        // never reaches GTK, so it says so itself.
        window.set_focus_visible(true);
        let Some(wanted) = wanted.and_then(|wanted| wanted.str()) else { return };
        let (mods, name) = match wanted.rsplit_once('+') {
            Some((mods, name)) => (mods, name),
            None => ("", wanted),
        };
        let Some(keyval) = gtk::gdk::Key::from_name(name) else {
            note!("omarepos: no key is called {name:?}");
            return;
        };
        let mut state = gtk::gdk::ModifierType::empty();
        for held in mods.split('+').filter(|held| !held.is_empty()) {
            state |= match held {
                "shift" => gtk::gdk::ModifierType::SHIFT_MASK,
                "ctrl" => gtk::gdk::ModifierType::CONTROL_MASK,
                "alt" => gtk::gdk::ModifierType::ALT_MASK,
                _ => gtk::gdk::ModifierType::empty(),
            };
        }
        let focus = gtk::prelude::GtkWindowExt::focus(&window);
        let mut at = focus.clone();
        while let Some(widget) = at {
            let controllers = widget.observe_controllers();
            for index in 0..controllers.n_items() {
                let Some(keys) = controllers.item(index).and_downcast::<gtk::EventControllerKey>()
                else {
                    continue;
                };
                let taken = keys.emit_by_name::<bool>(
                    "key-pressed",
                    &[&gtk::glib::translate::IntoGlib::into_glib(keyval), &0u32, &state],
                );
                if taken {
                    return;
                }
            }
            at = widget.parent();
        }
        // Enter, where no handler of ours wanted it, is what GTK would make
        // of it: the widget's own activation (a card, an entry, a button).
        if matches!(name, "Return" | "KP_Enter") {
            let mut at = focus;
            while let Some(widget) = at {
                // A text field's Enter is a signal of its own.
                if widget.is::<gtk::SearchEntry>() || widget.is::<gtk::Entry>() {
                    widget.emit_by_name::<()>("activate", &[]);
                    return;
                }
                if widget.activate() {
                    return;
                }
                at = widget.parent().filter(|_| widget.is::<gtk::Text>());
            }
        }
        note!("omarepos: nothing took {wanted:?}");
    });
    window.add_action(&key);

    // A settings row, by its title: `row` with "Title=value". A switch takes
    // on, off or toggle; a spin row a number; an entry row its text, applied.
    // Rows are looked for whether or not they are on screen.
    let row = gio::SimpleAction::new("row", Some(gtk::glib::VariantTy::STRING));
    let window_weak = window.downgrade();
    row.connect_activate(move |_, wanted| {
        let Some(window) = window_weak.upgrade() else { return };
        let Some((title, value)) = wanted.and_then(|w| w.str()).and_then(|w| w.split_once('=')) else {
            return;
        };
        match find_row(window.upcast_ref(), title) {
            Some(found) => set_row(&found, value),
            None => note!("omarepos: no row is titled {title:?}"),
        }
    });
    window.add_action(&row);

    // An answer to the dialog that is up: `respond` with the response's id
    // ("save", "remove", "cancel"), for the dialogs that ask a question.
    let respond = gio::SimpleAction::new("respond", Some(gtk::glib::VariantTy::STRING));
    let window_weak = window.downgrade();
    respond.connect_activate(move |_, id| {
        let Some(window) = window_weak.upgrade() else { return };
        let Some(id) = id.and_then(|id| id.str()) else { return };
        match find_alert(window.upcast_ref()) {
            Some(alert) => {
                alert.emit_by_name::<()>("response", &[&id]);
                alert.close();
            }
            None => note!("omarepos: no dialog is asking anything"),
        }
    });
    window.add_action(&respond);

    // A choice in a drop-down: `choose` with "n", for the first drop-down on
    // screen (the sort order on the grid, the window on Contributions).
    let choose = gio::SimpleAction::new("choose", Some(gtk::glib::VariantTy::STRING));
    let window_weak = window.downgrade();
    choose.connect_activate(move |_, wanted| {
        let Some(window) = window_weak.upgrade() else { return };
        let Some(wanted) = wanted.and_then(|wanted| wanted.str()) else { return };
        // "2" is the third entry of the first drop-down on screen. A page
        // with two of them names the one it means by its tooltip:
        // "Time window=2".
        let (which, n) = match wanted.rsplit_once('=') {
            Some((which, n)) => (which, n),
            None => ("", wanted),
        };
        let Ok(n) = n.parse::<u32>() else { return };
        match find_dropdown(window.upcast_ref(), which) {
            Some(dropdown) => dropdown.set_selected(n),
            None => note!("omarepos: no drop-down on screen says {which:?}"),
        }
    });
    window.add_action(&choose);

    // A row of a list: `pick` with "list:row", both counted from nought among
    // the lists on screen, selects that row and puts the keyboard on it, as
    // the arrow keys would (those are GTK's own, and `key` does not reach
    // them). "list:row!" activates it as well, as Enter would.
    let pick = gio::SimpleAction::new("pick", Some(gtk::glib::VariantTy::STRING));
    let window_weak = window.downgrade();
    pick.connect_activate(move |_, which| {
        let Some(window) = window_weak.upgrade() else { return };
        let Some(which) = which.and_then(|which| which.str()) else { return };
        let (which, activate) = match which.strip_suffix('!') {
            Some(rest) => (rest, true),
            None => (which, false),
        };
        let Some((list, row)) = which.split_once(':') else { return };
        let (Ok(list), Ok(row)) = (list.parse::<usize>(), row.parse::<i32>()) else { return };
        let mut lists = Vec::new();
        find_lists(window.upcast_ref(), &mut lists);
        match lists.get(list).and_then(|list| list.row_at_index(row).map(|row| (list.clone(), row))) {
            Some((list, row)) => {
                if row.is_selectable() {
                    list.select_row(Some(&row));
                }
                row.grab_focus();
                if activate {
                    row.activate();
                }
            }
            None => note!("omarepos: no row {row} in list {list} ({} lists on screen)", lists.len()),
        }
    });
    window.add_action(&pick);

    // Draw, into nothing. A window that is sent no frames never runs a draw
    // function, and the lanes, the bars and the dots are drawn by code that
    // can fail like any other. `paint` gives every drawing area on screen the
    // size it asks for and takes a snapshot of it, which runs its draw
    // function.
    let paint = gio::SimpleAction::new("paint", None);
    let window_weak = window.downgrade();
    paint.connect_activate(move |_, _| {
        let Some(window) = window_weak.upgrade() else { return };
        let snapshot = gtk::Snapshot::new();
        let mut areas = 0;
        paint_areas(window.upcast_ref(), &snapshot, &mut areas);
        let drawn = snapshot.to_node().is_some();
        note!("omarepos: painted {areas} drawing areas: {}", if drawn { "ok" } else { "nothing came of it" });
    });
    window.add_action(&paint);

    // A size for the window, as "1000x700". Only a display that lets a
    // window choose its own size obeys (GTK's Broadway does, a tiling
    // compositor does not), and only one that sends frames finishes what a
    // change of width starts: a breakpoint takes the content off screen
    // until the next frame.
    //
    // A display also has a size of its own that a window is kept within,
    // and Broadway's, with no browser attached, is 1024 by 768. The one
    // thing that outranks it is the least a window may be, so a size past
    // the display's is asked for as that; a size within it puts the least
    // back to what the window was built with, and the window can shrink
    // again.
    let resize = gio::SimpleAction::new("resize", Some(gtk::glib::VariantTy::STRING));
    let window_weak = window.downgrade();
    let least = window.size_request();
    resize.connect_activate(move |_, wanted| {
        let Some(window) = window_weak.upgrade() else { return };
        let size = wanted
            .and_then(|wanted| wanted.str())
            .and_then(|wanted| wanted.split_once('x'))
            .and_then(|(width, height)| Some((width.parse::<i32>().ok()?, height.parse::<i32>().ok()?)));
        let Some((width, height)) = size else {
            note!("omarepos: resize wants a size like 1000x700");
            return;
        };
        let display = gtk::prelude::WidgetExt::display(&window);
        let screen = display
            .monitors()
            .item(0)
            .and_downcast::<gtk::gdk::Monitor>()
            .map(|monitor| monitor.geometry());
        match screen {
            Some(screen) if width > screen.width() || height > screen.height() => {
                window.set_size_request(width.max(least.0), height.max(least.1));
            }
            _ => window.set_size_request(least.0, least.1),
        }
        window.set_default_size(width, height);
    });
    window.add_action(&resize);

    // A picture of the window, written as a PNG to the path given. The dump
    // says what is on a card and where the keyboard is; it cannot say what a
    // card looks like, and that is what a change to one is judged by. Drawn
    // by a renderer of its own, into memory, so it asks nothing of the
    // display. It needs the window to have been laid out, which a window on
    // a hidden workspace is only once.
    //
    // "2:" in front of the path draws it at twice the size, which is what a
    // picture for a page wants: text and everything the window draws itself
    // are drawn again at that size, not stretched. A path starts with "/",
    // so the two cannot be mistaken for each other.
    let shot = gio::SimpleAction::new("shot", Some(gtk::glib::VariantTy::STRING));
    let window_weak = window.downgrade();
    shot.connect_activate(move |_, wanted| {
        let Some(window) = window_weak.upgrade() else { return };
        let Some(wanted) = wanted.and_then(|wanted| wanted.str()) else { return };
        let (scale, path) = match wanted.split_once(':') {
            Some((scale, path)) if !scale.is_empty() && scale.chars().all(|c| c.is_ascii_digit()) => {
                (scale.parse::<f32>().unwrap_or(1.0).clamp(1.0, 4.0), path)
            }
            _ => (1.0, wanted),
        };
        let (width, height) = (window.width() as f64, window.height() as f64);
        let snapshot = gtk::Snapshot::new();
        snapshot.scale(scale, scale);
        gtk::WidgetPaintable::new(Some(&window)).snapshot(&snapshot, width, height);
        let Some(node) = snapshot.to_node() else {
            note!("omarepos: there is nothing to take a picture of");
            return;
        };
        let renderer = gtk::gsk::CairoRenderer::new();
        if let Err(err) = renderer.realize(gtk::gdk::Surface::NONE) {
            note!("omarepos: no picture: {err}");
            return;
        }
        let texture = renderer.render_texture(&node, None);
        renderer.unrealize();
        match texture.save_to_png(path) {
            Ok(()) => note!("omarepos: picture in {path}"),
            Err(err) => note!("omarepos: no picture: {err}"),
        }
    });
    window.add_action(&shot);

    // Where the divider of a split page stands, in pixels from its top: the
    // commit graph above, what a commit changed below. A person drags it; a
    // page opened where frames are slow takes its share of a height it does
    // not have yet, and a picture of it wants the divider put somewhere.
    let split = gio::SimpleAction::new("split", Some(gtk::glib::VariantTy::STRING));
    let window_weak = window.downgrade();
    split.connect_activate(move |_, wanted| {
        let Some(window) = window_weak.upgrade() else { return };
        let at = wanted.and_then(|wanted| wanted.str()).and_then(|wanted| wanted.parse::<i32>().ok());
        match (first_split(window.upcast_ref()), at) {
            (Some(split), Some(at)) => split.set_position(at),
            (None, _) => note!("omarepos: there is no split on screen"),
            (_, None) => note!("omarepos: split wants a number of pixels"),
        }
    });
    window.add_action(&split);

    // The pointer, put over a card: `point` with the card's number on the
    // grid, counted from nought, or "off" to take it away. No pointer reaches
    // a window nobody is looking at, so this says what one would: the card is
    // marked as being under it, and the grid's own handler is told where it
    // is. Once is a pointer that is there. Again, on another card, is a
    // pointer that has been moved. `wheel` is the wheel turned where it is.
    let point = gio::SimpleAction::new("point", Some(gtk::glib::VariantTy::STRING));
    let window_weak = window.downgrade();
    point.connect_activate(move |_, wanted| {
        let Some(window) = window_weak.upgrade() else { return };
        let Some(wanted) = wanted.and_then(|wanted| wanted.str()) else { return };
        let Some(flow) = first_flow(window.upcast_ref()) else {
            note!("omarepos: there is no grid to point at");
            return;
        };
        let over = gtk::StateFlags::PRELIGHT;
        let mut index = 0;
        while let Some(child) = flow.child_at_index(index) {
            child.unset_state_flags(over);
            if let Some(card) = child.child() {
                card.unset_state_flags(over);
            }
            index += 1;
        }
        if wanted == "off" {
            return;
        }
        let Some(child) = wanted.parse::<i32>().ok().and_then(|n| flow.child_at_index(n)) else {
            note!("omarepos: no card {wanted} on a grid of {index}");
            return;
        };
        child.set_state_flags(over, false);
        if let Some(card) = child.child() {
            card.set_state_flags(over, false);
        }
        // The middle of the card, in the grid's own coordinates. A window
        // that is sent no frames lays a card out once at most, and one that
        // has never been laid out is nowhere: such cards are told apart by
        // their numbers, which is all the handler needs of a place.
        let (x, y) = match child.compute_bounds(&flow) {
            Some(bounds) if bounds.width() > 0.0 => (
                f64::from(bounds.x() + bounds.width() / 2.0),
                f64::from(bounds.y() + bounds.height() / 2.0),
            ),
            _ => (f64::from(child.index()) * 40.0 + 20.0, 20.0),
        };
        let controllers = flow.observe_controllers();
        for index in 0..controllers.n_items() {
            if let Some(motion) = controllers.item(index).and_downcast::<gtk::EventControllerMotion>() {
                motion.emit_by_name::<()>("motion", &[&x, &y]);
            }
        }
    });
    window.add_action(&point);

    let wheel = gio::SimpleAction::new("wheel", None);
    let window_weak = window.downgrade();
    wheel.connect_activate(move |_, _| {
        let Some(window) = window_weak.upgrade() else { return };
        let Some(flow) = first_flow(window.upcast_ref()) else {
            note!("omarepos: there is no grid to turn the wheel over");
            return;
        };
        let controllers = flow.observe_controllers();
        for index in 0..controllers.n_items() {
            if let Some(wheel) = controllers.item(index).and_downcast::<gtk::EventControllerScroll>() {
                wheel.emit_by_name::<bool>("scroll", &[&0f64, &1f64]);
            }
        }
    });
    window.add_action(&wheel);

    // Text, into the field the keyboard is in.
    let type_ = gio::SimpleAction::new("type", Some(gtk::glib::VariantTy::STRING));
    let window_weak = window.downgrade();
    type_.connect_activate(move |_, text| {
        let Some(window) = window_weak.upgrade() else { return };
        let Some(text) = text.and_then(|text| text.str()) else { return };
        // The field the keyboard is in, or failing that the first entry on
        // screen that is not a filter: a page has at most one of those.
        let field = gtk::prelude::GtkWindowExt::focus(&window)
            .and_downcast::<gtk::Editable>()
            .or_else(|| first_entry(window.upcast_ref()).map(|entry| entry.upcast()));
        match field {
            Some(field) => {
                // As a keyboard does it: what is selected is typed over.
                field.delete_selection();
                let mut at = field.position();
                field.insert_text(text, &mut at);
                field.set_position(at);
            }
            None => note!("omarepos: there is no text field to type into"),
        }
    });
    window.add_action(&type_);
}

fn write_dump(window: &adw::ApplicationWindow, path: &std::ffi::OsStr, everything: bool) {
    // A window nobody is looking at is sent no frames, and libadwaita
    // finishes a change of breakpoint on the next frame, keeping the
    // content off screen until then. Here that frame never comes. On a
    // display of its own it does come, if slowly, and putting the content
    // back by hand there gets in ahead of libadwaita doing it.
    let frames_come = gtk::prelude::WidgetExt::display(window).type_().name() == "GdkBroadwayDisplay";
    if let Some(content) = window.content().filter(|content| !frames_come && !content.is_child_visible()) {
        content.set_child_visible(true);
    }
    let mut out = String::new();
    // Said outright as well, since the walk below only finds a focus that
    // is on screen, and "the keyboard is nowhere" is worth knowing.
    match gtk::prelude::GtkWindowExt::focus(window) {
        Some(focus) => {
            out.push_str(&format!(
                "keyboard: {} {:?} mapped={}\n",
                focus.type_().name(),
                focus.widget_name().as_str(),
                focus.is_mapped()
            ));
            // Why not, when it is not: the first thing up the tree that
            // is hidden.
            let mut up = Some(focus);
            while let Some(widget) = up {
                if !widget.is_visible() || !widget.is_child_visible() {
                    out.push_str(&format!(
                        "  hidden at: {} {:?} visible={} child_visible={}\n",
                        widget.type_().name(),
                        widget.widget_name().as_str(),
                        widget.get_visible(),
                        widget.is_child_visible()
                    ));
                    break;
                }
                up = widget.parent();
            }
        }
        None => out.push_str("keyboard: nowhere\n"),
    }
    out.push_str(&format!("dialog: {}\n", window.visible_dialog().is_some()));
    // What is choosing a card on the grid, as the stylesheet is told it: the
    // keyboard, a pointer that has been moved, or nothing yet.
    if let Some(flow) = first_flow(window.upcast_ref()) {
        let choosing = match (flow.has_css_class("om-keyboard"), flow.has_css_class("om-pointer")) {
            (true, true) => "keyboard and pointer",
            (true, false) => "keyboard",
            (false, true) => "pointer",
            (false, false) => "nothing",
        };
        out.push_str(&format!("choosing: {choosing}\n"));
    }
    // How many frames the display has asked this window for. A number that
    // stands still is a window nobody is looking at, where nothing that
    // waits for a frame (a change of breakpoint, an animation) ever ends.
    if let Some(clock) = window.frame_clock() {
        out.push_str(&format!("frames: {}\n", clock.frame_counter()));
    }
    // How narrow the window can be made before something is cut off,
    // beside how wide it is.
    if let Some(content) = window.content() {
        let (least, _, _, _) = content.measure(gtk::Orientation::Horizontal, -1);
        out.push_str(&format!("width: {} (needs {least})\n", window.width()));
    }
    walk(window.upcast_ref(), 0, everything, &mut out);
    if let Err(err) = std::fs::write(&path, out) {
        note!("omarepos: could not write the dump: {err}");
    }
}

fn find_row(widget: &gtk::Widget, title: &str) -> Option<adw::PreferencesRow> {
    if let Some(row) = widget.downcast_ref::<adw::PreferencesRow>() {
        if row.title().contains(title) {
            return Some(row.clone());
        }
    }
    let mut child = widget.first_child();
    while let Some(current) = child {
        if let Some(found) = find_row(&current, title) {
            return Some(found);
        }
        child = current.next_sibling();
    }
    None
}

fn set_row(row: &adw::PreferencesRow, value: &str) {
    if let Some(switch) = row.downcast_ref::<adw::SwitchRow>() {
        switch.set_active(match value {
            "on" => true,
            "off" => false,
            _ => !switch.is_active(),
        });
    } else if let Some(spin) = row.downcast_ref::<adw::SpinRow>() {
        match value.parse::<f64>() {
            Ok(number) => spin.set_value(number),
            Err(_) => note!("omarepos: {value:?} is not a number"),
        }
    } else if let Some(entry) = row.downcast_ref::<adw::EntryRow>() {
        entry.set_text(value);
        entry.emit_by_name::<()>("apply", &[]);
    } else if let Some(expander) = row.downcast_ref::<adw::ExpanderRow>() {
        expander.set_expanded(value != "off");
    } else {
        note!("omarepos: do not know how to set a {}", row.type_().name());
    }
}

fn paint_areas(widget: &gtk::Widget, snapshot: &gtk::Snapshot, areas: &mut usize) {
    if !widget.is_mapped() {
        return;
    }
    if let (Some(area), Some(parent)) = (widget.downcast_ref::<gtk::DrawingArea>(), widget.parent()) {
        let width = area.content_width().max(area.width()).max(24);
        let height = area.content_height().max(area.height()).max(24);
        area.allocate(width, height, -1, None);
        parent.snapshot_child(area, snapshot);
        *areas += 1;
    }
    let mut child = widget.first_child();
    while let Some(current) = child {
        paint_areas(&current, snapshot, areas);
        child = current.next_sibling();
    }
}

fn find_lists(widget: &gtk::Widget, lists: &mut Vec<gtk::ListBox>) {
    if !widget.is_mapped() {
        return;
    }
    if let Some(list) = widget.downcast_ref::<gtk::ListBox>() {
        lists.push(list.clone());
    }
    let mut child = widget.first_child();
    while let Some(current) = child {
        find_lists(&current, lists);
        child = current.next_sibling();
    }
}

fn find_alert(widget: &gtk::Widget) -> Option<adw::AlertDialog> {
    if let Some(alert) = widget.downcast_ref::<adw::AlertDialog>() {
        return Some(alert.clone());
    }
    let mut child = widget.first_child();
    let mut last = None;
    while let Some(current) = child {
        // The last one found: a question asked from inside another dialog
        // comes after it.
        if let Some(found) = find_alert(&current) {
            last = Some(found);
        }
        child = current.next_sibling();
    }
    last
}

fn find_dropdown(widget: &gtk::Widget, says: &str) -> Option<gtk::DropDown> {
    if !widget.is_mapped() {
        return None;
    }
    if let Some(dropdown) = widget.downcast_ref::<gtk::DropDown>() {
        if dropdown.tooltip_text().unwrap_or_default().contains(says) {
            return Some(dropdown.clone());
        }
    }
    let mut child = widget.first_child();
    while let Some(current) = child {
        if let Some(found) = find_dropdown(&current, says) {
            return Some(found);
        }
        child = current.next_sibling();
    }
    None
}

/// The grid of cards, whether or not it is the page in front. By its class:
/// the tag editor lays its tags out in a flow box as well.
fn first_flow(widget: &gtk::Widget) -> Option<gtk::FlowBox> {
    if let Some(flow) = widget.downcast_ref::<gtk::FlowBox>().filter(|flow| flow.has_css_class("om-flow")) {
        return Some(flow.clone());
    }
    let mut child = widget.first_child();
    while let Some(current) = child {
        if let Some(found) = first_flow(&current) {
            return Some(found);
        }
        child = current.next_sibling();
    }
    None
}

fn first_split(widget: &gtk::Widget) -> Option<gtk::Paned> {
    if !widget.is_mapped() {
        return None;
    }
    if let Some(split) = widget.downcast_ref::<gtk::Paned>() {
        return Some(split.clone());
    }
    let mut child = widget.first_child();
    while let Some(current) = child {
        if let Some(found) = first_split(&current) {
            return Some(found);
        }
        child = current.next_sibling();
    }
    None
}

fn first_entry(widget: &gtk::Widget) -> Option<gtk::Entry> {
    if !widget.is_mapped() {
        return None;
    }
    if let Some(entry) = widget.downcast_ref::<gtk::Entry>() {
        return Some(entry.clone());
    }
    let mut child = widget.first_child();
    while let Some(current) = child {
        if let Some(found) = first_entry(&current) {
            return Some(found);
        }
        child = current.next_sibling();
    }
    None
}

fn find_button(widget: &gtk::Widget, wanted: &str, unseen: bool) -> Option<gtk::Button> {
    if !unseen && !widget.is_mapped() {
        return None;
    }
    if let Some(button) = widget.downcast_ref::<gtk::Button>() {
        let mut words = say(widget).unwrap_or_default();
        words.push_str(&button.tooltip_text().unwrap_or_default());
        // A menu button is a button inside the widget that has the tooltip,
        // and on a narrow window the tooltip is all it says.
        if let Some(menu) = widget.parent().and_downcast::<gtk::MenuButton>() {
            words.push_str(&menu.tooltip_text().unwrap_or_default());
        }
        // A button with a child of its own (an icon and a label) says what
        // the labels inside it say.
        let mut inner = String::new();
        walk(widget, 0, unseen, &mut inner);
        words.push_str(&inner);
        if words.contains(wanted) {
            return Some(button.clone());
        }
    }
    let mut child = widget.first_child();
    while let Some(current) = child {
        if let Some(found) = find_button(&current, wanted, unseen) {
            return Some(found);
        }
        child = current.next_sibling();
    }
    None
}

fn walk(widget: &gtk::Widget, depth: usize, unseen: bool, out: &mut String) {
    // What is not mapped is not on screen: a page under another, a folded
    // group, the pane when it is closed. Left out unless asked for.
    if !unseen && !widget.is_mapped() {
        return;
    }
    let said = say(widget);
    let mut flags = Vec::new();
    // Within the window, whether or not the window is the one being typed
    // into: a window on a workspace nobody is looking at never is.
    if widget.is_focus() {
        flags.push("focus".to_string());
    }
    if !widget.is_sensitive() {
        flags.push("insensitive".to_string());
    }
    if let Some(row) = widget.downcast_ref::<gtk::ListBoxRow>() {
        if row.is_selected() {
            flags.push("selected".to_string());
        }
    }
    if let Some(toggle) = widget.downcast_ref::<gtk::ToggleButton>() {
        if toggle.is_active() {
            flags.push("active".to_string());
        }
    }
    if let Some(check) = widget.downcast_ref::<gtk::CheckButton>() {
        if check.is_active() {
            flags.push("active".to_string());
        }
    }
    // The card the pointer is over, which says nothing of its own.
    if widget.has_css_class("om-card") && widget.state_flags().contains(gtk::StateFlags::PRELIGHT) {
        flags.push("pointer".to_string());
    }
    let classes: Vec<String> = widget
        .css_classes()
        .iter()
        .filter(|class| class.starts_with("om-") || ["danger", "warn", "ok", "current", "work", "bad", "word", "out", "new", "detached"].contains(&class.as_str()))
        .map(|class| class.to_string())
        .collect();
    let tooltip = widget.tooltip_text().filter(|_| said.is_some() || !flags.is_empty());
    if said.is_some() || !flags.is_empty() {
        out.push_str(&"  ".repeat(depth));
        out.push_str(widget.type_().name().trim_start_matches("Gtk").trim_start_matches("Adw"));
        if let Some(said) = &said {
            out.push_str(&format!(" {said:?}"));
        }
        if !classes.is_empty() {
            out.push_str(&format!(" .{}", classes.join(".")));
        }
        if !flags.is_empty() {
            out.push_str(&format!(" [{}]", flags.join(", ")));
        }
        if let Some(tooltip) = tooltip {
            out.push_str(&format!(" tip={:?}", tooltip.as_str()));
        }
        out.push('\n');
    }
    let mut child = widget.first_child();
    while let Some(current) = child {
        walk(&current, depth + 1, unseen, out);
        child = current.next_sibling();
    }
}

/// The words a widget shows, for the kinds that show any of their own.
fn say(widget: &gtk::Widget) -> Option<String> {
    if let Some(label) = widget.downcast_ref::<gtk::Label>() {
        return Some(label.text().to_string());
    }
    if let Some(editable) = widget.downcast_ref::<gtk::Editable>() {
        // An entry wraps a text widget that is editable too; say it once.
        if widget.parent().is_some_and(|parent| parent.is::<gtk::Editable>()) {
            return None;
        }
        return Some(editable.text().to_string());
    }
    if let Some(view) = widget.downcast_ref::<gtk::TextView>() {
        let buffer = view.buffer();
        let text = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false);
        return Some(text.chars().take(400).collect());
    }
    if let Some(button) = widget.downcast_ref::<gtk::Button>() {
        return Some(
            button
                .label()
                .map(|label| label.to_string())
                .or_else(|| button.icon_name().map(|icon| format!("({icon})")))
                .unwrap_or_default(),
        );
    }
    if let Some(title) = widget.downcast_ref::<adw::WindowTitle>() {
        return Some(format!("{} | {}", title.title(), title.subtitle()));
    }
    if let Some(page) = widget.downcast_ref::<adw::StatusPage>() {
        return Some(format!(
            "{} | {}",
            page.title(),
            page.description().unwrap_or_default()
        ));
    }
    if let Some(switch) = widget.downcast_ref::<adw::SwitchRow>() {
        return Some(format!(
            "{} = {}{}",
            switch.title(),
            if switch.is_active() { "on" } else { "off" },
            if switch.is_sensitive() { "" } else { " (greyed)" }
        ));
    }
    if let Some(spin) = widget.downcast_ref::<adw::SpinRow>() {
        return Some(format!("{} = {}", spin.title(), spin.value()));
    }
    if let Some(entry) = widget.downcast_ref::<adw::EntryRow>() {
        return Some(format!("{} = {:?}", entry.title(), entry.text().as_str()));
    }
    if let Some(row) = widget.downcast_ref::<adw::PreferencesRow>() {
        let subtitle = widget
            .downcast_ref::<adw::ActionRow>()
            .and_then(|row| row.subtitle())
            .or_else(|| widget.downcast_ref::<adw::ExpanderRow>().map(|row| row.subtitle()))
            .unwrap_or_default();
        return Some(format!("{} | {}", row.title(), subtitle));
    }
    if let Some(banner) = widget.downcast_ref::<adw::Banner>() {
        // Mapped whether or not it is showing; say only what can be read.
        return banner.is_revealed().then(|| banner.title().to_string());
    }
    None
}
