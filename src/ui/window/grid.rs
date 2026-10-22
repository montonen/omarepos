//! The grid, kept up to date by touching only the cards that changed.
//!
//! `rebuild` used to empty the flow box and build a card for every visible
//! repository, on every keystroke in the filter, every tick, every theme
//! flip and every card a streaming scan added: the whole grid paid for any
//! change, the scroll position and the focused card were destroyed with it
//! and put back by hand, and at a few hundred repositories every keystroke
//! was felt. Now the list the grid shows is compared with the list it is
//! about to show, and only what differs is touched: a card whose repository
//! reads differently is built again, one that moved is moved, and one that
//! says the same stands untouched — keyboard, scroll and all.
//!
//! What a card is built from is written down beside it as text (the reading,
//! its tags, a failed fetch, whether it is new here), the same comparison
//! `reread` makes. What is *not* in that text is set straight in place
//! instead of costing a build: the tick, the top-hit mark. And when every
//! card really must be drawn again although nothing it says has changed —
//! the accent ramp flipped with the theme, or the ages the cards show ("2h")
//! are due to be read off the clock anew — the epoch in the text is bumped,
//! and nothing matches.

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk::glib;
use adw::prelude::*;

use crate::git::RepoInfo;
use crate::ui::{Action, card};

use super::{Ui, find_check};

/// How old the ages on a card may grow before every card is built afresh at
/// the next change to the grid. A card left standing keeps the "2h" it was
/// built with; once a minute is often enough that no age is more than a
/// minute out when anything happens, and seldom enough that typing in the
/// filter does not pay for the whole grid twice.
const CARD_AGES: Duration = Duration::from_secs(60);

/// One touch of the flow box. The indexes are live: each step means what it
/// means with every step before it already taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Step {
    /// The card at `at` is not in the new grid, or no longer says what its
    /// repository now says: take it out.
    Remove { at: usize },
    /// The card now at `from` is wanted, unchanged, at `to`: move it whole.
    Move { from: usize, to: usize },
    /// Build the card for the new grid's entry `at`, and put it in there.
    Insert { at: usize },
}

/// The steps that turn a grid showing `old` into one showing `new`, where
/// each entry is a repository and the text its card was, or will be, built
/// from. Removals come first, back to front so the indexes hold; then the new
/// grid is walked once, and every card already in its place is passed over in
/// silence.
pub(super) fn reconcile(old: &[(PathBuf, String)], new: &[(PathBuf, String)]) -> Vec<Step> {
    let wanted: HashMap<&PathBuf, &String> = new.iter().map(|(path, sig)| (path, sig)).collect();
    let keep = |path: &PathBuf, sig: &String| wanted.get(path).is_some_and(|want| *want == sig);

    let mut steps = Vec::new();
    // What still stands once the removals are done, in the order it stands.
    let mut order: Vec<&PathBuf> = old
        .iter()
        .filter(|(path, sig)| keep(path, sig))
        .map(|(path, _)| path)
        .collect();
    for (at, (path, sig)) in old.iter().enumerate().rev() {
        if !keep(path, sig) {
            steps.push(Step::Remove { at });
        }
    }

    for (to, (path, _)) in new.iter().enumerate() {
        if order.get(to).is_some_and(|have| *have == path) {
            continue;
        }
        // A reusable card that is not already in place can only be further
        // down: everything above `to` has been settled, and a repository is
        // on one card at most.
        match order.iter().skip(to).position(|have| *have == path) {
            Some(offset) => {
                let from = to + offset;
                steps.push(Step::Move { from, to });
                let moved = order.remove(from);
                order.insert(to, moved);
            }
            None => {
                steps.push(Step::Insert { at: to });
                order.insert(to, path);
            }
        }
    }
    steps
}

/// Add or take away one style class, whichever way `on` says.
fn set_class(widget: &impl IsA<gtk::Widget>, class: &str, on: bool) {
    if on {
        widget.add_css_class(class);
    } else {
        widget.remove_css_class(class);
    }
}

impl Ui {
    /// Build every card again, whatever it says: for what the cards draw
    /// once and keep, like the colours of the dots on tags.
    pub(super) fn redraw_cards(self: &Rc<Self>) {
        self.drawn_at.set(None);
        self.rebuild();
    }

    pub(super) fn rebuild(self: &Rc<Self>) {
        // A background refresh reshapes the grid under the user's hands, so
        // the scroll position has to survive it. Restoring on the next frame
        // rather than immediately gives the flow box time to allocate, without
        // which the adjustment clamps the value back to zero.
        let offset = self.scroller.vadjustment().value();
        // A card that is taken out or moved takes the keyboard with it; one
        // that is left alone keeps it, so the focus is only put back when the
        // card that had it was touched.
        let focused = self.focused_repo();
        let holder: Option<gtk::Widget> = gtk::prelude::GtkWindowExt::focus(&self.window)
            .and_then(|focus| focus.ancestor(gtk::FlowBoxChild::static_type()))
            .filter(|child| child.parent().as_ref() == Some(self.flow.upcast_ref()));
        let mut touched = false;

        if self.drawn_at.get().is_none_or(|at| at.elapsed() >= CARD_AGES) {
            self.card_epoch.set(self.card_epoch.get() + 1);
            self.drawn_at.set(Some(Instant::now()));
        }

        // A filter on a tag that nothing on the grid carries matches nothing,
        // and the controls that could take it off are built from the tags
        // that are there: the grid stayed empty until the app was restarted.
        // Not while the first scan is still bringing cards in, when a tag's
        // repositories may simply not have arrived yet.
        if !self.scanning.get() {
            let live: BTreeSet<String> = self
                .grid_tags()
                .into_iter()
                .map(|(tag, _)| tag.to_lowercase())
                .collect();
            self.tag_filter.borrow_mut().retain(|tag| live.contains(tag));
        }

        let repos = self.visible();

        // Everything each card is built from, as text.
        let epoch = self.card_epoch.get();
        let mut sigs = Vec::with_capacity(repos.len());
        let mut tags_by_card = Vec::with_capacity(repos.len());
        let mut failed_by_card = Vec::with_capacity(repos.len());
        let mut unseen_by_card = Vec::with_capacity(repos.len());
        {
            let meta = self.meta.borrow();
            let errors = self.fetch_errors.borrow();
            for info in &repos {
                let tags = meta.tags_for(&info.path).to_vec();
                let failed = errors.get(&info.path.display().to_string()).cloned();
                let unseen = !self.is_known(&info.path);
                sigs.push(format!(
                    "{epoch}\u{1f}{info:?}\u{1f}{tags:?}\u{1f}{failed:?}\u{1f}{unseen}"
                ));
                tags_by_card.push(tags);
                failed_by_card.push(failed);
                unseen_by_card.push(unseen);
            }
        }

        let old: Vec<(PathBuf, String)> = self
            .shown
            .borrow()
            .iter()
            .map(|repo| repo.path.clone())
            .zip(self.signatures.borrow().iter().cloned())
            .collect();
        let new: Vec<(PathBuf, String)> = repos
            .iter()
            .map(|repo| repo.path.clone())
            .zip(sigs.iter().cloned())
            .collect();

        let ui = Rc::clone(self);
        let on_action: Rc<dyn Fn(Action, &RepoInfo)> =
            Rc::new(move |action, info| ui.run_action(action, info));
        let ui = Rc::clone(self);
        let on_select: Rc<dyn Fn(&RepoInfo, bool)> = Rc::new(move |info, selected| {
            {
                let mut set = ui.selected.borrow_mut();
                if selected {
                    set.insert(info.path.clone());
                } else {
                    set.remove(&info.path);
                }
            }
            ui.update_selection_label();
            ui.update_status();
        });

        for step in reconcile(&old, &new) {
            match step {
                Step::Remove { at } => {
                    if let Some(child) = self.flow.child_at_index(at as i32) {
                        touched |= holder.as_ref() == Some(child.upcast_ref());
                        self.flow.remove(&child);
                    }
                }
                Step::Move { from, to } => {
                    if let Some(child) = self.flow.child_at_index(from as i32) {
                        touched |= holder.as_ref() == Some(child.upcast_ref());
                        self.flow.remove(&child);
                        self.flow.insert(&child, to as i32);
                    }
                }
                Step::Insert { at } => {
                    let info = &repos[at];
                    let ctx = card::Context {
                        tags: &tags_by_card[at],
                        selected: self.selected.borrow().contains(&info.path),
                        fetch_failed: failed_by_card[at].as_deref(),
                        unseen: unseen_by_card[at],
                        top_hit: at == 0,
                    };
                    self.flow.insert(
                        &card::build(info, &ctx, Rc::clone(&on_action), Rc::clone(&on_select)),
                        at as i32,
                    );
                }
            }
        }

        *self.shown.borrow_mut() = repos.clone();
        *self.signatures.borrow_mut() = sigs;

        // What is not worth a build is set straight in place: the tick and
        // its ring, the mark on the one card Enter would open, and the one on
        // a card that still shows the last run's reading. The flags are taken
        // first — setting a checkbox answers back through `on_select`, which
        // wants the selection for itself.
        let marks: Vec<(bool, bool)> = {
            let selected = self.selected.borrow();
            let earlier = self.from_last_run.borrow();
            repos
                .iter()
                .map(|repo| (selected.contains(&repo.path), earlier.contains(&repo.path)))
                .collect()
        };
        for (index, (ticked, earlier)) in marks.into_iter().enumerate() {
            let Some(child) = self.flow.child_at_index(index as i32) else { break };
            if let Some(card) = child.child() {
                set_class(&card, "om-selected", ticked);
                set_class(&card, "om-top-hit", index == 0);
                set_class(&card, "om-earlier", earlier);
            }
            if let Some(check) = find_check(child.upcast_ref()) {
                if check.is_active() != ticked {
                    check.set_active(ticked);
                }
            }
        }

        self.update_selection_label();
        self.sync_tag_controls();

        let page = if repos.is_empty() && !self.scanning.get() { "empty" } else { "grid" };
        // Nothing to list at all, as against nothing that passes the filter.
        let nothing_at_all = self.repos.borrow().is_empty();
        if page == "empty" {
            // Why, where it can be said. "No repositories found" over a
            // filter that hides forty of them, or over a root that has been
            // moved, sent the reader looking in the wrong place.
            let amiss = self.roots_amiss();
            let (title, why) = if !nothing_at_all {
                ("Nothing matches", "No repository on the grid passes the filter in force.".to_string())
            } else if amiss.is_empty() {
                ("No repositories found", "No folder is set to be scanned.".to_string())
            } else {
                ("No repositories found", amiss.join("\n"))
            };
            self.empty.set_title(title);
            self.empty.set_description(Some(&glib::markup_escape_text(&why)));
            // A filter that hides everything is undone in the filter. A
            // fleet with nothing in it is put right in Settings.
            self.empty_action.set_visible(nothing_at_all);
        }
        // Taken before the page changes: the button leaves the screen with
        // the page, and the keyboard with the button.
        let leaving_the_way = page != "empty" && self.empty_action.has_focus();
        self.stack.set_visible_child_name(page);
        self.update_status();
        if page == "empty" && nothing_at_all {
            self.unstarted.set(true);
            self.offer_settings();
        } else if nothing_at_all {
            // A scan has begun and found nothing yet. Left where it was,
            // the keyboard would be moved by GTK to whatever comes next,
            // which is a button in the header.
            if leaving_the_way {
                self.focus_filter();
            }
        } else if self.unstarted.get() {
            self.start_keyboard();
        }

        // Only when the card that had the keyboard was taken out or moved on
        // the way: one that stood untouched has kept it, which no full
        // rebuild could let it do. A moved card has to be given it again as
        // well. GTK lets go of the keyboard of anything taken out of the
        // window at the end of the frame, by handing it to what the widget
        // was in, and a card put back in the box is still in the box: left
        // to that, the keyboard went to the box and not to the card.
        if touched {
            if let Some(path) = focused {
                if let Some(index) = repos.iter().position(|repo| repo.path == path) {
                    if let Some(child) = self.flow.child_at_index(index as i32) {
                        // Through the flow box's own focus path, not `grab_focus`.
                        // The box keeps a cursor of its own, which is where the
                        // arrow keys start from, and `grab_focus` on a card moves
                        // the window's focus without moving that. The cursor went
                        // on naming a card this rebuild had just removed, and the
                        // next arrow key started from it: from the wrong place at
                        // best, from freed memory at worst. Let go of first, as in
                        // `focus_grid`: GTK leaves alone a card that already
                        // holds the focus, and a moved card does.
                        gtk::prelude::GtkWindowExt::set_focus(&self.window, gtk::Widget::NONE);
                        child.child_focus(gtk::DirectionType::Down);
                    }
                }
            }
        }

        if offset > 0.0 {
            let adjustment = self.scroller.vadjustment();
            glib::timeout_add_local_once(std::time::Duration::from_millis(16), move || {
                adjustment.set_value(offset);
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid(cards: &[(&str, &str)]) -> Vec<(PathBuf, String)> {
        cards
            .iter()
            .map(|(path, sig)| (PathBuf::from(path), sig.to_string()))
            .collect()
    }

    /// Carry the steps out on a plain list, the way `rebuild` carries them
    /// out on the flow box, and say what was done: the paths at the end, and
    /// how many cards were built, moved and taken out.
    fn apply(old: &[(PathBuf, String)], new: &[(PathBuf, String)]) -> (Vec<PathBuf>, usize, usize, usize) {
        let mut stands: Vec<PathBuf> = old.iter().map(|(path, _)| path.clone()).collect();
        let (mut built, mut moved, mut removed) = (0, 0, 0);
        for step in reconcile(old, new) {
            match step {
                Step::Remove { at } => {
                    stands.remove(at);
                    removed += 1;
                }
                Step::Move { from, to } => {
                    let card = stands.remove(from);
                    stands.insert(to, card);
                    moved += 1;
                }
                Step::Insert { at } => {
                    stands.insert(at, new[at].0.clone());
                    built += 1;
                }
            }
        }
        (stands, built, moved, removed)
    }

    /// Whatever the steps are, they have to end at the new grid.
    fn ends_right(old: &[(PathBuf, String)], new: &[(PathBuf, String)]) -> (usize, usize, usize) {
        let (stands, built, moved, removed) = apply(old, new);
        let wanted: Vec<PathBuf> = new.iter().map(|(path, _)| path.clone()).collect();
        assert_eq!(stands, wanted);
        (built, moved, removed)
    }

    #[test]
    fn a_grid_that_says_the_same_is_left_alone() {
        let cards = grid(&[("/a", "1"), ("/b", "1"), ("/c", "1")]);
        assert!(reconcile(&cards, &cards).is_empty());
        assert!(reconcile(&grid(&[]), &grid(&[])).is_empty());
    }

    #[test]
    fn only_the_card_that_reads_differently_is_built_again() {
        let old = grid(&[("/a", "1"), ("/b", "1"), ("/c", "1")]);
        let new = grid(&[("/a", "1"), ("/b", "2"), ("/c", "1")]);
        let (built, moved, removed) = ends_right(&old, &new);
        assert_eq!((built, moved, removed), (1, 0, 1));
    }

    #[test]
    fn a_new_sort_moves_cards_and_builds_none() {
        let old = grid(&[("/a", "1"), ("/b", "1"), ("/c", "1"), ("/d", "1")]);
        let new = grid(&[("/d", "1"), ("/c", "1"), ("/b", "1"), ("/a", "1")]);
        let (built, _, removed) = ends_right(&old, &new);
        assert_eq!((built, removed), (0, 0));
    }

    #[test]
    fn a_filter_takes_cards_out_and_builds_none() {
        let old = grid(&[("/a", "1"), ("/b", "1"), ("/c", "1"), ("/d", "1")]);
        let new = grid(&[("/b", "1"), ("/d", "1")]);
        let (built, moved, removed) = ends_right(&old, &new);
        assert_eq!((built, moved, removed), (0, 0, 2));
    }

    #[test]
    fn a_card_arriving_mid_grid_moves_nothing_around_it() {
        let old = grid(&[("/a", "1"), ("/c", "1")]);
        let new = grid(&[("/a", "1"), ("/b", "1"), ("/c", "1")]);
        let (built, moved, removed) = ends_right(&old, &new);
        assert_eq!((built, moved, removed), (1, 0, 0));
    }

    #[test]
    fn an_emptied_grid_and_a_filled_one_both_come_out_right() {
        let full = grid(&[("/a", "1"), ("/b", "1")]);
        let (built, moved, removed) = ends_right(&full, &grid(&[]));
        assert_eq!((built, moved, removed), (0, 0, 2));
        let (built, moved, removed) = ends_right(&grid(&[]), &full);
        assert_eq!((built, moved, removed), (2, 0, 0));
    }

    #[test]
    fn changed_moved_new_and_gone_all_at_once_end_at_the_new_grid() {
        let old = grid(&[("/a", "1"), ("/b", "1"), ("/c", "1"), ("/d", "1"), ("/e", "1")]);
        // /a changed, /b gone, /f new, the rest reshuffled.
        let new = grid(&[("/e", "1"), ("/a", "2"), ("/f", "1"), ("/c", "1"), ("/d", "1")]);
        let (built, _, removed) = ends_right(&old, &new);
        assert_eq!((built, removed), (2, 2));
    }
}
