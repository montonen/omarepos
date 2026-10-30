//! The tag chrome: the chips in the header, the Tags menu, the way to the
//! editor, and what a click on a chip does to the filter. The tags themselves
//! live in `crate::meta`; this is only how the window wears them.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use gtk::gdk;

use crate::ui;

use super::{HEADER_BARE, HEADER_FULL, SPARE, TAGS_LABEL, Ui, below};

/// How many tags get a button each in the header. Up to this many are one
/// click to filter by; more than this and the Tags menu lists them. It was
/// two, with the widths below fixed for two short names. They are worked out
/// from the buttons that are there now (`fit_header`), so the number is a
/// matter of taste and not of room.
pub(super) const QUICK_TAGS: usize = 5;

impl Ui {
    /// The tags on the repositories of the grid, with how many carry each.
    /// Not the tags in the store, which also remembers repositories no root
    /// reaches any more: counted over that, a tag none of whose repositories
    /// was still on the grid kept its chip and its old count, and filtering
    /// by it emptied the grid.
    pub(super) fn grid_tags(&self) -> Vec<(String, usize)> {
        let repos = self.repos.borrow();
        self.meta.borrow().tags_among(repos.iter().map(|repo| repo.path.as_path()))
    }

    /// Keep the tag chrome in step with the tags that exist and the filter in
    /// force. Below three tags they become toggles in the header, which is one
    /// click instead of three; past that they would crowd out the title, so the
    /// popover takes over.
    pub(super) fn sync_tag_controls(self: &Rc<Self>) {
        let tags = self.grid_tags();
        let quick = !tags.is_empty() && tags.len() <= QUICK_TAGS;
        let signature =
            tags.iter().map(|(tag, count)| format!("{tag}\u{1f}{count}")).collect::<Vec<_>>().join("\u{1e}");

        if signature != *self.tag_signature.borrow() {
            *self.tag_signature.borrow_mut() = signature;
            self.build_quick_tags(&tags, quick);
            self.fit_header();
        }

        // Reflect the live filter without the writes bouncing back as toggles.
        self.tag_syncing.set(true);
        for (tag, button) in self.tag_quick_buttons.borrow().iter() {
            button.set_active(self.tag_filter.borrow().contains(&tag.to_lowercase()));
        }
        self.tag_syncing.set(false);

        // The buttons are there to be pressed only while there is room for
        // them. Without it the menu does the filtering, and says how much of
        // it is on: it used to go on saying nothing, as it does beside the
        // buttons, with the buttons gone.
        let buttons = self.tag_buttons_showing();
        self.tag_quick.set_visible(buttons);
        let active = self.tag_filter.borrow().len() + usize::from(self.untagged_only.get());
        self.tags_content.set_label(&match (buttons, self.narrow.get(), active) {
            // The buttons carry the filtering, so this one is only the menu.
            (true, _, _) => String::new(),
            (false, true, 0) => String::new(),
            (false, true, n) => n.to_string(),
            (false, false, 0) => "Tags".to_string(),
            (false, false, n) => format!("Tags ({n})"),
        });
        self.tags_button.set_tooltip_text(Some(if buttons {
            "Tag the selected repos, or filter by untagged"
        } else {
            "Filter by tag, or tag the selected repos"
        }));
    }

    /// Whether the header has a button for each tag at the moment: there are
    /// few enough of them, and the window is wide enough.
    fn tag_buttons_showing(&self) -> bool {
        let tags = self.grid_tags().len();
        (1..=QUICK_TAGS).contains(&tags) && !self.cramped.get()
    }

    /// Put the two widths at which the header gives something up where what
    /// is in it now needs them.
    ///
    /// They were two numbers, right for two tag buttons with two-letter
    /// names. A button is as wide as its tag's name, and there can be five
    /// of them, so the widths are worked out from the buttons as they stand:
    /// what the header needs without any, and the buttons on top of that.
    fn fit_header(&self) {
        let buttons = self.tag_quick_buttons.borrow();
        // Each button asked on its own. The row they are in is hidden when
        // the window is too narrow for it, and a hidden row asks for nothing:
        // measured as a whole it would call itself narrow enough to show
        // again the moment it had been put away.
        let wide: f64 = buttons
            .iter()
            .map(|(_, button)| {
                let (_, natural, _, _) = button.measure(gtk::Orientation::Horizontal, -1);
                f64::from(natural + self.tag_quick.spacing())
            })
            .sum();
        let label = if buttons.is_empty() { 0.0 } else { TAGS_LABEL };
        if let Some([narrow, cramped]) = self.breakpoints.borrow().as_ref() {
            narrow.set_condition(Some(&below(HEADER_FULL - label + wide + SPARE)));
            cramped.set_condition(Some(&below(HEADER_BARE + wide + SPARE)));
        }
    }

    fn build_quick_tags(self: &Rc<Self>, tags: &[(String, usize)], quick: bool) {
        while let Some(child) = self.tag_quick.first_child() {
            self.tag_quick.remove(&child);
        }
        self.tag_quick_buttons.borrow_mut().clear();
        if !quick {
            return;
        }

        for (tag, count) in tags {
            let content = gtk::Box::new(gtk::Orientation::Horizontal, 5);
            content.append(&ui::color_dot(ui::key_color(&tag.to_lowercase())));
            content.append(&gtk::Label::new(Some(tag)));

            let button = gtk::ToggleButton::builder().child(&content).build();
            button.add_css_class("om-quick-tag");
            button.set_tooltip_text(Some(&format!(
                "{count} {}. Click to see this tag alone, Ctrl+click to add it or take it away.",
                if *count == 1 { "repository" } else { "repositories" }
            )));

            // A click, and not a toggle: the button has flipped by the time
            // this runs, and what it flipped to is not the answer. What the
            // filter becomes is worked out from what it was, and every
            // button, this one too, is then set from that by the rebuild.
            let ui = Rc::clone(self);
            let key = tag.to_lowercase();
            button.connect_clicked(move |button| {
                if ui.tag_syncing.get() {
                    return;
                }
                let (chosen, untagged) =
                    after_tag_click(&ui.tag_filter.borrow(), ui.untagged_only.get(), &key, ctrl_held(button));
                *ui.tag_filter.borrow_mut() = chosen;
                ui.untagged_only.set(untagged);
                ui.rebuild();
            });

            self.tag_quick.append(&button);
            self.tag_quick_buttons.borrow_mut().push((tag.clone(), button));
        }
    }

    /// Rebuilt on every open, because the set of tags changes under it.
    pub(super) fn tag_popover(self: &Rc<Self>) -> gtk::Popover {
        let popover = gtk::Popover::builder().has_arrow(false).build();
        let body = gtk::Box::new(gtk::Orientation::Vertical, 6);
        body.set_width_request(230);

        let known = self.grid_tags();
        if self.tag_buttons_showing() {
            // The header's buttons already do the filtering; repeating it
            // here would be two controls for one setting. Only while they
            // are showing, though: on a window too narrow for them this
            // menu was all there was, and it did not list a single tag.
            let untagged =
                gtk::CheckButton::builder().label("Untagged only").active(self.untagged_only.get()).build();
            let ui = Rc::clone(self);
            untagged.connect_toggled(move |check| {
                ui.untagged_only.set(check.is_active());
                ui.rebuild();
            });
            body.append(&untagged);
        } else if known.is_empty() {
            let hint = gtk::Label::builder()
                .label("No tags yet.\n\nTick some repos, then\n“Tag selected…”.")
                .justify(gtk::Justification::Center)
                .build();
            hint.add_css_class("om-meta");
            body.append(&hint);
        } else {
            let list = gtk::Box::new(gtk::Orientation::Vertical, 2);
            for (tag, count) in known {
                let key = tag.to_lowercase();
                let check = gtk::CheckButton::builder()
                    .label(format!("{tag}  ({count})"))
                    .active(self.tag_filter.borrow().contains(&key))
                    .build();
                let ui = Rc::clone(self);
                check.connect_toggled(move |check| {
                    {
                        let mut filter = ui.tag_filter.borrow_mut();
                        if check.is_active() {
                            filter.insert(key.clone());
                        } else {
                            filter.remove(&key);
                        }
                    }
                    ui.rebuild();
                });
                list.append(&check);
            }
            let scroller = gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Never)
                .propagate_natural_height(true)
                .max_content_height(280)
                .child(&list)
                .build();
            body.append(&scroller);

            let untagged =
                gtk::CheckButton::builder().label("Untagged only").active(self.untagged_only.get()).build();
            let ui = Rc::clone(self);
            untagged.connect_toggled(move |check| {
                ui.untagged_only.set(check.is_active());
                ui.rebuild();
            });
            body.append(&untagged);

            let clear = gtk::Button::builder().label("Clear tag filter").build();
            let ui = Rc::clone(self);
            let popover_for_clear = popover.clone();
            clear.connect_clicked(move |_| {
                ui.tag_filter.borrow_mut().clear();
                ui.untagged_only.set(false);
                popover_for_clear.popdown();
                ui.rebuild();
            });
            body.append(&clear);
        }

        body.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

        let selected = self.selected.borrow().len();
        let edit = gtk::Button::builder()
            .label(if selected > 0 {
                format!("Tag {selected} selected…")
            } else {
                "Tag the repos shown…".to_string()
            })
            .build();
        let ui = Rc::clone(self);
        let popover_for_edit = popover.clone();
        edit.connect_clicked(move |_| {
            popover_for_edit.popdown();
            let repos = if ui.selected.borrow().is_empty() {
                ui.shown.borrow().iter().map(|r| r.path.clone()).collect()
            } else {
                let mut paths: Vec<PathBuf> = ui.selected.borrow().iter().cloned().collect();
                paths.sort();
                paths
            };
            ui.edit_tags(repos);
        });
        body.append(&edit);

        popover.set_child(Some(&body));
        popover
    }

    pub(super) fn edit_tags(self: &Rc<Self>, repos: Vec<PathBuf>) {
        let ui = Rc::clone(self);
        ui::dialogs::edit_tags(
            &self.window,
            Rc::clone(&self.meta),
            repos,
            self.grid_tags(),
            Rc::new(move || ui.rebuild()),
            {
                let ui = Rc::clone(self);
                Rc::new(move |why: String| ui::complain(&ui.toasts, &why))
            },
        );
    }
}

/// What the tag buttons filter by once `tag`'s has been clicked: the tags to
/// show and whether "untagged only" stays on.
///
/// One tag at a time is the common case, so a click shows that tag and no
/// other, and a second click on it, shown alone, shows everything again. They
/// used to be switches each, and looking at one tag after another took a
/// click to turn the last one off every time. With Ctrl held a click adds the
/// tag to the ones shown, or takes it away, and leaves the rest alone.
fn after_tag_click(
    chosen: &BTreeSet<String>,
    untagged: bool,
    tag: &str,
    add: bool,
) -> (BTreeSet<String>, bool) {
    let mut next = chosen.clone();
    if add {
        if !next.remove(tag) {
            next.insert(tag.to_string());
        }
        return (next, untagged);
    }
    let alone = chosen.len() == 1 && chosen.contains(tag) && !untagged;
    next.clear();
    if !alone {
        next.insert(tag.to_string());
    }
    (next, false)
}

/// Whether Ctrl is held down, on the keyboard of the display `widget` is on.
fn ctrl_held(widget: &impl IsA<gtk::Widget>) -> bool {
    // A driven window is sent no keys, so the driver says it outright.
    #[cfg(debug_assertions)]
    if let Some(held) = ui::debug::ctrl_held() {
        return held;
    }
    widget
        .display()
        .default_seat()
        .and_then(|seat| seat.keyboard())
        .is_some_and(|keyboard| keyboard.modifier_state().contains(gdk::ModifierType::CONTROL_MASK))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A click on a tag shows it alone, again shows everything; Ctrl+click
    /// adds to what is shown or takes from it.
    #[test]
    fn a_tag_click_switches_and_a_ctrl_click_adds() {
        let set = |tags: &[&str]| tags.iter().map(|t| t.to_string()).collect::<BTreeSet<_>>();
        let none = set(&[]);

        // From nothing: that tag.
        assert_eq!(after_tag_click(&none, false, "work", false), (set(&["work"]), false));
        // Another: that one instead, not both.
        assert_eq!(after_tag_click(&set(&["work"]), false, "side", false), (set(&["side"]), false));
        // The one shown, again: everything.
        assert_eq!(after_tag_click(&set(&["work"]), false, "work", false), (none.clone(), false));
        // One of several, plainly: that one alone, the others and "untagged
        // only" gone with them.
        assert_eq!(after_tag_click(&set(&["work", "side"]), true, "work", false), (set(&["work"]), false));
        // Shown together with the untagged, it is not alone: it stays, alone.
        assert_eq!(after_tag_click(&set(&["work"]), true, "work", false), (set(&["work"]), false));

        // With Ctrl: added, and taken away again; the rest left as they were.
        assert_eq!(after_tag_click(&set(&["work"]), true, "side", true), (set(&["work", "side"]), true));
        assert_eq!(after_tag_click(&set(&["work", "side"]), false, "work", true), (set(&["side"]), false));
        assert_eq!(after_tag_click(&set(&["side"]), false, "side", true), (none, false));
    }
}
