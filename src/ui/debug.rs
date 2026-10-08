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
//!     --object-path /se/alphadev/Omarepos/window/1 \
//!     --method org.gtk.Actions.Activate dump '[]' '{}'
//! ```

use adw::prelude::*;
use gtk::gio;

pub fn install(window: &adw::ApplicationWindow) {
    let Some(path) = std::env::var_os("OMAREPOS_DUMP") else { return };
    let action = gio::SimpleAction::new("dump", None);
    let window_weak = window.downgrade();
    action.connect_activate(move |_, _| {
        let Some(window) = window_weak.upgrade() else { return };
        let mut out = String::new();
        // Said outright as well, since the walk below only finds a focus that
        // is on screen, and "the keyboard is nowhere" is worth knowing.
        match gtk::prelude::GtkWindowExt::focus(&window) {
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
        // How narrow the window can be made before something is cut off,
        // beside how wide it is.
        if let Some(content) = window.content() {
            let (least, _, _, _) = content.measure(gtk::Orientation::Horizontal, -1);
            out.push_str(&format!("width: {} (needs {least})\n", window.width()));
        }
        walk(window.upcast_ref(), 0, &mut out);
        if let Err(err) = std::fs::write(&path, out) {
            eprintln!("omarepos: could not write the dump: {err}");
        }
    });
    window.add_action(&action);

    // And a hand for the script: `press` with a string clicks the first
    // button on screen whose label or tooltip contains it.
    let press = gio::SimpleAction::new("press", Some(gtk::glib::VariantTy::STRING));
    let window_weak = window.downgrade();
    press.connect_activate(move |_, wanted| {
        let Some(window) = window_weak.upgrade() else { return };
        let Some(wanted) = wanted.and_then(|wanted| wanted.str()) else { return };
        match find_button(window.upcast_ref(), wanted) {
            Some(button) if button.is_sensitive() => button.emit_clicked(),
            Some(_) => eprintln!("omarepos: {wanted:?} is not sensitive"),
            None => eprintln!("omarepos: no button says {wanted:?}"),
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
        let Some(wanted) = wanted.and_then(|wanted| wanted.str()) else { return };
        let (mods, name) = match wanted.rsplit_once('+') {
            Some((mods, name)) => (mods, name),
            None => ("", wanted),
        };
        let Some(keyval) = gtk::gdk::Key::from_name(name) else {
            eprintln!("omarepos: no key is called {name:?}");
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
        eprintln!("omarepos: nothing took {wanted:?}");
    });
    window.add_action(&key);

    // Text, into the field the keyboard is in.
    let type_ = gio::SimpleAction::new("type", Some(gtk::glib::VariantTy::STRING));
    let window_weak = window.downgrade();
    type_.connect_activate(move |_, text| {
        let Some(window) = window_weak.upgrade() else { return };
        let Some(text) = text.and_then(|text| text.str()) else { return };
        match gtk::prelude::GtkWindowExt::focus(&window).and_downcast::<gtk::Editable>() {
            Some(field) => {
                let mut at = field.position();
                field.insert_text(text, &mut at);
                field.set_position(at);
            }
            None => eprintln!("omarepos: the keyboard is not in a text field"),
        }
    });
    window.add_action(&type_);
}

fn find_button(widget: &gtk::Widget, wanted: &str) -> Option<gtk::Button> {
    if !widget.is_mapped() {
        return None;
    }
    if let Some(button) = widget.downcast_ref::<gtk::Button>() {
        let mut words = say(widget).unwrap_or_default();
        words.push_str(&button.tooltip_text().unwrap_or_default());
        // A button with a child of its own (an icon and a label) says what
        // the labels inside it say.
        let mut inner = String::new();
        walk(widget, 0, &mut inner);
        words.push_str(&inner);
        if words.contains(wanted) {
            return Some(button.clone());
        }
    }
    let mut child = widget.first_child();
    while let Some(current) = child {
        if let Some(found) = find_button(&current, wanted) {
            return Some(found);
        }
        child = current.next_sibling();
    }
    None
}

fn walk(widget: &gtk::Widget, depth: usize, out: &mut String) {
    // What is not mapped is not on screen: a page under another, a folded
    // group, the pane when it is closed.
    if !widget.is_mapped() {
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
    let classes: Vec<String> = widget
        .css_classes()
        .iter()
        .filter(|class| class.starts_with("om-") || ["danger", "warn", "ok", "current"].contains(&class.as_str()))
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
        walk(&current, depth + 1, out);
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
    if let Some(banner) = widget.downcast_ref::<adw::Banner>() {
        // Mapped whether or not it is showing; say only what can be read.
        return banner.is_revealed().then(|| banner.title().to_string());
    }
    None
}
