//! Where the keyboard is, and whose turn it is to choose a card: the
//! keyboard's or the pointer's. Everything here is bookkeeping around the two
//! being taken away — by a rebuild, a page, a dialog, a change of width —
//! and given back to where they belong.

use std::path::PathBuf;
use std::rc::Rc;

use gtk::glib;
use adw::prelude::*;

use super::Ui;

impl Ui {
    pub(super) fn enter_keyboard_nav(&self) {
        self.last_key.set(Some(std::time::Instant::now()));
        if !self.keyboard_nav.replace(true) {
            self.flow.add_css_class("om-keyboard");
        }
        self.pointer_rests();
    }

    /// The pointer is over the grid, at `at` in the window. That is not yet
    /// the pointer moving: GTK says so again, from the same place, each time
    /// the cards move under it, and a scroll, a rescan and a change of width
    /// all move them. The first sighting is no movement either. A window
    /// opens under a pointer more often than not.
    pub(super) fn pointer_seen(&self, at: Option<(f64, f64)>) {
        let Some(at) = at else { return };
        let was = self.pointer_at.get();
        if was.is_some_and(|was| !pointer_moved(was, at)) {
            return;
        }
        self.pointer_at.set(Some(at));
        // Not in the moment after a key, as with leaving the keyboard's turn
        // below: a hand on the arrow keys is not a hand on the mouse.
        if was.is_some() && !self.key_just_pressed() {
            self.pointer_chooses();
        }
    }

    /// Whether a key went to the grid within the last moment.
    fn key_just_pressed(&self) -> bool {
        self.last_key
            .get()
            .is_some_and(|when| when.elapsed() < std::time::Duration::from_millis(300))
    }

    /// The pointer has been moved, or the wheel turned: it is what chooses a
    /// card now, and the card under it shows what the one with the keyboard
    /// shows. The stylesheet does it, told by the class.
    pub(super) fn pointer_chooses(&self) {
        if self.keyboard_nav.replace(false) {
            self.flow.remove_css_class("om-keyboard");
        }
        if !self.pointer_nav.replace(true) {
            self.flow.add_css_class("om-pointer");
        }
    }

    /// The keyboard is in use, and the pointer is wherever it was left.
    pub(super) fn pointer_rests(&self) {
        if self.pointer_nav.replace(false) {
            self.flow.remove_css_class("om-pointer");
        }
    }

    pub(super) fn leave_keyboard_nav(&self) {
        if !self.keyboard_nav.get() {
            return;
        }
        // Motion arriving right after a keypress is the grid scrolling beneath
        // a pointer that has not actually moved.
        if self.key_just_pressed() {
            return;
        }
        self.keyboard_nav.set(false);
        self.flow.remove_css_class("om-keyboard");
    }

    /// Which repository the focused widget belongs to, if the focus is in the
    /// grid at all.
    pub(super) fn focused_repo(&self) -> Option<PathBuf> {
        let mut widget = gtk::prelude::GtkWindowExt::focus(&self.window);
        while let Some(current) = widget {
            if let Ok(child) = current.clone().downcast::<gtk::FlowBoxChild>() {
                // The window goes on naming a card as its focus for a while
                // after the card has been taken out of the grid. Such a card
                // must not be asked for its index: the flow box frees the
                // slot it kept for it and leaves the card pointing there, so
                // the answer is read out of freed memory. That crashed the
                // app twice in an afternoon.
                let in_grid = child
                    .parent()
                    .is_some_and(|parent| &parent == self.flow.upcast_ref::<gtk::Widget>());
                if !in_grid {
                    return None;
                }
                let index = child.index();
                if index >= 0 {
                    return self
                        .shown
                        .borrow()
                        .get(index as usize)
                        .map(|repo| repo.path.clone());
                }
            }
            widget = current.parent();
        }
        None
    }

    /// Back from a page: the keyboard belongs on the card that was opened,
    /// if it is not on a card already.
    pub(super) fn back_on_the_grid(&self) {
        if !self.on_grid() {
            return;
        }
        // Wherever the pointer was put while another page was in front, it
        // has not been moved here.
        self.pointer_at.set(None);
        self.pointer_rests();
        let focus = gtk::prelude::GtkWindowExt::focus(&self.window);
        let in_grid = focus.as_ref().is_some_and(|focus| focus.is_ancestor(&self.flow));
        let typing = focus.is_some_and(|focus| focus.is::<gtk::Editable>());
        if in_grid || typing {
            return;
        }
        let index = self.opened.borrow().as_ref().and_then(|opened| {
            self.shown.borrow().iter().position(|repo| &repo.path == opened)
        });
        match index.and_then(|index| self.flow.child_at_index(index as i32)) {
            // Through the box, as in `rebuild`, so its cursor comes too.
            Some(child) => {
                gtk::prelude::GtkWindowExt::set_focus(&self.window, gtk::Widget::NONE);
                child.child_focus(gtk::DirectionType::Down);
            }
            None => self.focus_grid(true),
        }
    }

    /// What has the keyboard: the window's focus, or, while a change of
    /// width has taken it, the widget it is about to be given back to.
    fn keyboard(&self) -> Option<gtk::Widget> {
        gtk::prelude::GtkWindowExt::focus(&self.window)
            .or_else(|| self.owed.upgrade().filter(|had| had.is_ancestor(&self.window)))
    }

    /// The content is leaving the screen: note what has the keyboard, so
    /// that it can be given back when the content returns.
    ///
    /// A change of breakpoint takes the whole content off screen for a
    /// frame, and GTK moves the keyboard off a widget that leaves the
    /// screen. libadwaita notes what had it and means to give it back, but
    /// GTK's move reaches it as a request for the keyboard, which it takes
    /// to be somebody's wish and honours instead: when the content comes
    /// back, the keyboard is on a button in the header. So any
    /// resize across one of the two widths, which on a tiling desktop is
    /// what opening a window beside this one does, took the keyboard out of
    /// the filter or off the card it was on, and no key did what it should
    /// until it had been put back by hand.
    ///
    /// GTK moves it at the end of the frame, so here it is still where the
    /// person left it.
    pub(super) fn note_keyboard(&self) {
        // A second change of width can begin before the first has been made
        // good, with the keyboard on that button. The first note stands.
        if self.owed.upgrade().is_none() {
            self.owed.set(gtk::prelude::GtkWindowExt::focus(&self.window).as_ref());
        }
    }

    /// The content is back on screen: give the keyboard back to what
    /// `note_keyboard` noted.
    ///
    /// A moment later, not now. libadwaita puts the content back and then
    /// hands the keyboard to that button, in one go, and this is called in
    /// the middle of it. Ahead of anything typed, though, so that no key
    /// reaches the button.
    pub(super) fn give_keyboard_back(self: &Rc<Self>) {
        if self.owed.upgrade().is_none() {
            return;
        }
        let ui = Rc::clone(self);
        glib::idle_add_local_full(glib::Priority::HIGH, move || {
            // Off screen again already, by another change of width: the
            // note is kept for when it comes back from that one.
            if !ui.window.content().is_some_and(|content| content.is_mapped()) {
                return glib::ControlFlow::Break;
            }
            let had = ui.owed.upgrade();
            ui.owed.set(None::<&gtk::Widget>);
            // Not if it has gone meanwhile, or is on a page that has been
            // left: the keyboard stays where GTK put it.
            let Some(had) = had.filter(|had| had.is_mapped()) else {
                return glib::ControlFlow::Break;
            };
            if gtk::prelude::GtkWindowExt::focus(&ui.window).as_ref() == Some(&had) {
                return glib::ControlFlow::Break;
            }
            // Not `grab_focus` for a text field. That selects what is in
            // it, and the next key typed would replace a filter that was
            // only being added to.
            match had.downcast_ref::<gtk::Text>() {
                Some(text) => {
                    text.grab_focus_without_selecting();
                }
                None => {
                    had.grab_focus();
                }
            }
            glib::ControlFlow::Break
        });
    }

    /// Give the filter the keyboard, with whatever is in it selected, so
    /// that what is typed next is the filter. This is where the keyboard
    /// starts: the window is opened to find a repository, the way a launcher
    /// is, and the letters that would name one mean other things on a card —
    /// `f` fetches the repository under the keyboard, `l` opens a terminal.
    /// The grid is one Down away.
    ///
    /// Only with the grid in front and nothing over it. Behind a dialog or
    /// another page the filter is out of sight, and the keyboard would go
    /// where nobody can see what it types.
    pub(super) fn focus_filter(&self) {
        if !self.on_grid() || self.window.visible_dialog().is_some() {
            return;
        }
        self.search.grab_focus();
        self.search.select_region(0, -1);
    }

    /// With nothing to list there is nothing to filter, so the keyboard goes
    /// to the one thing on the page that leads somewhere, and Enter opens
    /// Settings. From an empty filter or from nowhere only: a keyboard that
    /// has been taken to a button in the header stays there, so does one in
    /// a dialog, and so does one in a filter that is being typed in. The
    /// fleet can go from under a sentence (a disk unmounted), and the space
    /// that came next would have opened Settings.
    pub(super) fn offer_settings(&self) {
        if !self.on_grid() || self.window.visible_dialog().is_some() {
            return;
        }
        let free = match self.keyboard() {
            None => true,
            Some(had) => {
                !had.is_mapped() || (had.is_ancestor(&self.search) && self.search.text().is_empty())
            }
        };
        if free {
            self.empty_action.grab_focus();
        }
    }

    /// The first repositories have arrived in a window that had none to
    /// list. The keyboard starts in the filter, as it does at launch,
    /// wherever it was: on the button that led to Settings, or on the one in
    /// the header that started the scan. Not from under a dialog, which has
    /// the keyboard while it is open: then when it closes.
    pub(super) fn start_keyboard(&self) {
        if !self.on_grid() || self.window.visible_dialog().is_some() {
            return;
        }
        self.unstarted.set(false);
        self.focus_filter();
    }

    /// A dialog has closed, and the keyboard goes back to what had it before
    /// the dialog opened: libadwaita sees to that. Three times it does not
    /// come out right, and this is for those.
    ///
    /// On the page that says there is nothing to list, the keyboard belongs
    /// on the button there, which is the one thing on the page that leads
    /// anywhere. Closed before it had been drawn, Settings left the keyboard
    /// on a button in the header instead.
    ///
    /// When Settings has given the grid its first repositories, the button
    /// the keyboard was on has left the screen with its page, and the
    /// keyboard starts in the filter as it does at launch.
    ///
    /// And when what had the keyboard has gone for any other reason (a card
    /// that was rebuilt under the tag editor), it was nowhere, and no key did
    /// anything until the filter or a card had been clicked. The filter
    /// again.
    ///
    /// A beat later, so that libadwaita has had its turn first.
    pub(super) fn keyboard_after_dialog(self: &Rc<Self>) {
        let ui = Rc::clone(self);
        glib::timeout_add_local_once(std::time::Duration::from_millis(150), move || {
            if ui.window.visible_dialog().is_some() || !ui.on_grid() {
                return;
            }
            if ui.empty_action.is_mapped() {
                ui.empty_action.grab_focus();
            } else if ui.unstarted.get() {
                ui.start_keyboard();
            } else if !ui.keyboard().is_some_and(|had| had.is_mapped()) {
                ui.focus_filter();
            }
        });
    }

    /// Put the keyboard in the grid. `force` takes focus even from a text
    /// field, which is what Ctrl+L is for; without it the grid is only claimed
    /// when nothing else holds the focus, so a scan finishing mid-sentence
    /// does not swallow what is being typed.
    pub(super) fn focus_grid(&self, force: bool) {
        // Not only the window's focus: a keyboard that has just been taken
        // from the filter by a change of width is still the filter's, and is
        // about to be given back to it.
        let current = self.keyboard();
        if !self.on_grid() {
            return;
        }
        // Unforced, the grid stands back for a keyboard that is in use:
        // typing, already on a card, or in a menu or a dialog. It used to
        // stand back for any focus at all, and a window always has one by the
        // time a scan ends, on the first button of the header. So this never
        // once ran, and the arrow keys did nothing until Tab had been found.
        if !force {
            if self.window.visible_dialog().is_some() {
                return;
            }
            if let Some(focus) = &current {
                let typing = focus.is::<gtk::Editable>();
                let in_grid = focus.is_ancestor(&self.flow);
                let in_menu = focus.ancestor(gtk::Popover::static_type()).is_some();
                if typing || in_grid || in_menu {
                    return;
                }
            }
        }
        if let Some(child) = self.flow.child_at_index(0) {
            // Asked for with a key, which makes it the keyboard's turn.
            if force {
                self.pointer_rests();
            }
            // As in `rebuild`: through the box, so its cursor comes too. GTK
            // leaves the cursor alone for a card that already holds the focus,
            // so the focus is let go of first.
            gtk::prelude::GtkWindowExt::set_focus(&self.window, gtk::Widget::NONE);
            child.child_focus(gtk::DirectionType::Down);
        }
    }
}

/// Whether a pointer last seen at `was` and now at `at`, both in the window,
/// has been moved. By a few pixels: where it is comes through a float each
/// way, so the same place does not always come back as the same number, and
/// a mouse on a desk that is being typed at is not being moved either.
fn pointer_moved(was: (f64, f64), at: (f64, f64)) -> bool {
    const SLACK: f64 = 3.0;
    (was.0 - at.0).abs() >= SLACK || (was.1 - at.1).abs() >= SLACK
}

#[cfg(test)]
mod tests {
    use super::*;

    /// GTK reports a pointer again whenever the cards move under it, from the
    /// place it was already in. Only a report from somewhere else is the
    /// pointer having been moved.
    #[test]
    fn a_pointer_seen_again_in_the_same_place_has_not_moved() {
        let at = (412.0, 388.5);
        assert!(!pointer_moved(at, at));
        // What a place loses and gains on its way through a float.
        assert!(!pointer_moved(at, (412.0004, 388.4997)));
        // A mouse that shivers where it lies.
        assert!(!pointer_moved(at, (413.0, 387.5)));
        assert!(pointer_moved(at, (416.0, 388.5)));
        assert!(pointer_moved(at, (412.0, 380.0)));
        assert!(pointer_moved(at, (30.0, 900.0)));
    }
}
