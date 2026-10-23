//! Saved views (ADR-0012): the filter, the tags, the warning toggle and the
//! sort the grid is showing, kept under a name in `config.toml` and opened
//! again with one key. The questions an overseer asks are the same ones day
//! after day (one client's fleet, everything behind, everything with work in
//! it), and each was three or four settings put back by hand, gone again
//! with the window.
//!
//! A view is a saved question and nothing more. It writes nothing about any
//! repository, and opening one sets the same controls a person would.

use std::rc::Rc;

use adw::prelude::*;

use crate::config::View;
use crate::ui;

use super::Ui;
use super::query::SORT_LABELS;

/// How many views have a key of their own: Ctrl+1 to Ctrl+9.
pub(super) const KEYED: usize = 9;

impl Ui {
    /// What the grid is showing now, as a view called `name`.
    fn view_now(&self, name: &str) -> View {
        View {
            name: name.to_string(),
            filter: self.search.text().trim().to_string(),
            tags: self.tag_filter.borrow().iter().cloned().collect(),
            untagged: self.untagged_only.get(),
            attention: self.dirty_only.is_active(),
            sort: SORT_LABELS.get(self.sort.selected() as usize).map(|label| label.to_string()),
        }
    }

    /// Show the view numbered `number`, counting from 1, as the list and
    /// the keys number them.
    pub(super) fn open_view(self: &Rc<Self>, number: usize) {
        let view = number.checked_sub(1).and_then(|at| self.cfg.borrow().views.get(at).cloned());
        let Some(view) = view else {
            self.toasts.add_toast(ui::toast(&format!(
                "There is no view {number}: set the grid up, then save it from the views menu"
            )));
            return;
        };
        // A sort the list no longer has leaves the sort as it is, rather
        // than putting the grid in some other order and calling it the view.
        if let Some(at) = view.sort.as_deref().and_then(|sort| SORT_LABELS.iter().position(|label| *label == sort)) {
            self.sort.set_selected(at as u32);
        }
        *self.tag_filter.borrow_mut() = view.tags.iter().map(|tag| tag.to_lowercase()).collect();
        self.untagged_only.set(view.untagged);
        self.dirty_only.set_active(view.attention);
        self.search.set_text(&view.filter);
        self.rebuild();
        self.toasts.add_toast(ui::toast(&format!("View: {}", view.name)));
    }

    /// Keep what the grid is showing as the view `name`, in place of one of
    /// that name or after the others.
    pub(super) fn save_view(self: &Rc<Self>, name: &str) {
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        let view = self.view_now(name);
        let before = self.cfg.borrow().views.clone();
        let number = {
            let mut cfg = self.cfg.borrow_mut();
            match cfg.views.iter().position(|kept| kept.name.eq_ignore_ascii_case(name)) {
                Some(at) => {
                    cfg.views[at] = view;
                    at + 1
                }
                None => {
                    cfg.views.push(view);
                    cfg.views.len()
                }
            }
        };
        self.keep_views(before, &format!("Saved the view {name}{}", key_of(number)));
    }

    /// Forget the view numbered `number`. The ones after it move up a key.
    pub(super) fn forget_view(self: &Rc<Self>, number: usize) {
        let before = self.cfg.borrow().views.clone();
        let Some(at) = number.checked_sub(1).filter(|at| *at < before.len()) else { return };
        let name = self.cfg.borrow_mut().views.remove(at).name;
        self.keep_views(before, &format!("Forgot the view {name}"));
    }

    /// Write the views out. One that cannot be written is not kept in the
    /// window either: the file is what the next launch reads, and a view
    /// that seems saved and is gone tomorrow is worse than one refused now.
    fn keep_views(self: &Rc<Self>, before: Vec<View>, said: &str) {
        let saved = self.cfg.borrow().save();
        match saved {
            Ok(()) => self.toasts.add_toast(ui::toast(said)),
            Err(err) => {
                self.cfg.borrow_mut().views = before;
                ui::complain(&self.toasts, &format!("The view was {err}"));
            }
        }
    }

    /// The header's views button: its menu is built each time it opens, from
    /// the views as the configuration has them then, so an edit to the file
    /// read in by a rescan is there too.
    pub(super) fn wire_views(self: &Rc<Self>, button: &gtk::MenuButton) {
        let ui = Rc::clone(self);
        button.set_create_popup_func(move |button| button.set_popover(Some(&ui.views_menu())));
    }

    fn views_menu(self: &Rc<Self>) -> gtk::Popover {
        let popover = gtk::Popover::builder().has_arrow(false).build();
        let body = gtk::Box::new(gtk::Orientation::Vertical, 6);
        body.set_width_request(260);

        let views = self.cfg.borrow().views.clone();
        if views.is_empty() {
            let hint = gtk::Label::builder()
                .label("No saved views yet.\n\nSet the filter, the tags and the\nsort, then save them here.")
                .justify(gtk::Justification::Center)
                .build();
            hint.add_css_class("om-meta");
            body.append(&hint);
        }
        let list = gtk::Box::new(gtk::Orientation::Vertical, 2);
        for (at, view) in views.iter().enumerate() {
            let number = at + 1;
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 4);
            let open = gtk::Button::builder()
                .label(format!("{}{}", view.name, key_of(number)))
                .has_frame(false)
                .hexpand(true)
                .tooltip_text(describe(view))
                .build();
            if let Some(label) = open.child().and_downcast::<gtk::Label>() {
                label.set_xalign(0.0);
                label.set_ellipsize(gtk::pango::EllipsizeMode::End);
            }
            {
                let (ui, popover) = (Rc::clone(self), popover.downgrade());
                open.connect_clicked(move |_| {
                    if let Some(popover) = popover.upgrade() {
                        popover.popdown();
                    }
                    ui.open_view(number);
                });
            }
            let forget = gtk::Button::builder()
                .icon_name("user-trash-symbolic")
                .has_frame(false)
                .tooltip_text(format!("Forget the view {}", view.name))
                .build();
            {
                let (ui, popover) = (Rc::clone(self), popover.downgrade());
                forget.connect_clicked(move |_| {
                    if let Some(popover) = popover.upgrade() {
                        popover.popdown();
                    }
                    ui.forget_view(number);
                });
            }
            row.append(&open);
            row.append(&forget);
            list.append(&row);
        }
        body.append(&list);
        body.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

        let save = gtk::Button::builder().label("Save this view…").build();
        {
            let (ui, popover) = (Rc::clone(self), popover.downgrade());
            save.connect_clicked(move |_| {
                if let Some(popover) = popover.upgrade() {
                    popover.popdown();
                }
                ui.ask_view_name();
            });
        }
        body.append(&save);
        popover.set_child(Some(&body));
        popover
    }

    /// Ask what to call the view the grid is showing, and keep it.
    fn ask_view_name(self: &Rc<Self>) {
        let dialog = adw::AlertDialog::new(
            Some("Save this view"),
            Some("The filter, the tags, the warning toggle and the sort, under a name. A view of the same name is replaced."),
        );
        let entry = gtk::Entry::builder().placeholder_text("Name").activates_default(true).build();
        dialog.set_extra_child(Some(&entry));
        dialog.add_response("cancel", "Cancel");
        dialog.add_response("save", "Save");
        dialog.set_response_appearance("save", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("save"));
        dialog.set_close_response("cancel");
        let ui = Rc::clone(self);
        dialog.connect_response(Some("save"), move |_, _| ui.save_view(&entry.text()));
        dialog.present(Some(&self.window));
    }
}

/// The key that opens view `number`, as a list or a toast says it.
fn key_of(number: usize) -> String {
    if number <= KEYED { format!("  (Ctrl+{number})") } else { String::new() }
}

/// What a view shows, for its tooltip.
fn describe(view: &View) -> String {
    let mut said = Vec::new();
    if !view.filter.is_empty() {
        said.push(format!("filter: {}", view.filter));
    }
    if !view.tags.is_empty() {
        said.push(format!("tagged {}", view.tags.join(" or ")));
    }
    if view.untagged {
        said.push("untagged".to_string());
    }
    if view.attention {
        said.push("only what needs something".to_string());
    }
    if let Some(sort) = &view.sort {
        said.push(format!("sorted by {}", sort.to_lowercase()));
    }
    if said.is_empty() { "Everything, as it comes".to_string() } else { said.join("\n") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_view_says_what_it_shows_and_its_key() {
        let view = View {
            name: "behind".into(),
            filter: "lang:rust".into(),
            tags: vec!["acme".into(), "work".into()],
            attention: true,
            sort: Some("Unpushed first".into()),
            ..View::default()
        };
        assert_eq!(
            describe(&view),
            "filter: lang:rust\ntagged acme or work\nonly what needs something\nsorted by unpushed first"
        );
        assert_eq!(describe(&View::default()), "Everything, as it comes");
        assert_eq!(key_of(1), "  (Ctrl+1)");
        assert_eq!(key_of(9), "  (Ctrl+9)");
        assert_eq!(key_of(10), "");
    }
}
