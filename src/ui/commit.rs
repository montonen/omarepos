//! The commit detail pane: what the selected commit changed.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use gtk::pango;

use crate::commit::{self, Cancel, Detail, Group, Node, Target, Write};
use crate::ui::{absolute_time, relative_time};

/// Holding Down through a graph walks a commit per frame. Waiting for the
/// cursor to settle means one diff is read rather than a hundred.
const SETTLE_MS: u64 = 120;

/// Build the pane. The returned closure loads a commit into it; calling it
/// again abandons whatever was still loading.
/// `on_changed` fires after a write: `true` when a commit was made, which means
/// the history the caller is showing is now out of date.
/// Returns the pane, a loader, and a way to put the keyboard into it — a
/// two-pane layout where Tab walks five hundred commit rows instead of moving
/// to the next region is not navigable.
pub fn pane(
    on_changed: Rc<dyn Fn(bool)>,
) -> (gtk::Widget, Rc<dyn Fn(PathBuf, Target)>, Rc<dyn Fn()>) {
    let header = gtk::Box::new(gtk::Orientation::Vertical, 2);
    header.set_margin_top(10);
    header.set_margin_start(14);
    header.set_margin_end(14);

    let subject = gtk::Label::builder()
        .xalign(0.0)
        .wrap(true)
        .wrap_mode(pango::WrapMode::WordChar)
        .lines(2)
        .ellipsize(pango::EllipsizeMode::End)
        .build();
    subject.add_css_class("om-name");
    header.append(&subject);

    let meta = gtk::Label::builder()
        .xalign(0.0)
        .ellipsize(pango::EllipsizeMode::End)
        .build();
    meta.add_css_class("om-meta");
    header.append(&meta);

    let body = gtk::Label::builder()
        .xalign(0.0)
        .yalign(0.0)
        .wrap(true)
        .wrap_mode(pango::WrapMode::WordChar)
        .selectable(true)
        .build();
    body.add_css_class("om-summary");
    // The message is held to a height, in a scroller, and not to a number of
    // lines. A label's line limit is per paragraph, and a message wrapped at
    // 72 columns is one paragraph to a line: the limit never applied, a long
    // message made the pane as tall as itself, and the split moved to make
    // room and stayed there. Where the limit did apply, the rest of the
    // message could not be reached at all.
    let body_scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_height(true)
        .max_content_height(120)
        .visible(false)
        .child(&body)
        .build();
    body_scroller.set_margin_top(4);
    header.append(&body_scroller);

    let files = gtk::ListBox::new();
    let files_for_focus = files.clone();
    let files_for_keys = files.clone();
    files.set_selection_mode(gtk::SelectionMode::Browse);
    files.add_css_class("om-results");
    files.set_margin_top(10);
    files.set_margin_bottom(12);
    files.set_margin_start(14);
    files.set_margin_end(8);

    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .width_request(320)
        .child(&files)
        .build();
    scroller.add_css_class("om-grid");

    // The diff itself: a text view rather than a list of rows, because a
    // thousand-line patch is a thousand rows otherwise, and because the text
    // should be selectable and copy as a real diff.
    let diff_view = gtk::TextView::builder()
        .editable(false)
        .cursor_visible(false)
        .monospace(true)
        .left_margin(10)
        .right_margin(10)
        .top_margin(8)
        .bottom_margin(8)
        .build();
    diff_view.add_css_class("om-diff");
    let buffer = diff_view.buffer();
    // Nobody undoes a diff, and keeping the history of every insert is a cost
    // paid on each line.
    buffer.set_enable_undo(false);
    // Text tags are styled through properties rather than CSS, so the line
    // backgrounds are mixed from the theme's own colours here.
    let palette = crate::theme::Palette::load();
    let dark = palette.dark;
    for (name, colour, weight) in [
        ("added", palette.green.as_str(), 0.20),
        ("removed", palette.red.as_str(), 0.20),
        ("hunk", palette.accent.as_str(), 0.14),
    ] {
        let tag = gtk::TextTag::builder().name(name).build();
        tag.set_paragraph_background_rgba(Some(&blend(&palette.bg, colour, weight)));
        if name == "hunk" {
            tag.set_foreground_rgba(Some(&rgba(&palette.fg_dim)));
        }
        buffer.tag_table().add(&tag);
    }
    let diff_scroller = gtk::ScrolledWindow::builder()
        .hexpand(true)
        .vexpand(true)
        .child(&diff_view)
        .build();
    diff_scroller.add_css_class("om-grid");

    // Side by side within the pane: the pane is as wide as the window even
    // when the window is half a screen, so this split can be vertical where
    // the outer one could not.
    let inner = gtk::Paned::builder()
        .orientation(gtk::Orientation::Horizontal)
        .end_child(&diff_scroller)
        .resize_start_child(false)
        .shrink_start_child(false)
        .position(380)
        .build();

    let totals = gtk::Label::builder()
        .xalign(0.0)
        .ellipsize(pango::EllipsizeMode::End)
        .build();
    totals.add_css_class("om-statusbar");

    // The message box belongs with the files it commits, not with the diff.
    let message = gtk::Entry::builder()
        .placeholder_text("Commit message…")
        .hexpand(true)
        .build();
    message.add_css_class("om-search");
    let commit_button = gtk::Button::with_label("Commit");
    commit_button.add_css_class("suggested-action");
    // Who the commit will be recorded as, before it is made. With an identity
    // per client directory, the wrong one is otherwise found out afterwards,
    // on the remote.
    let who = gtk::Label::builder()
        .ellipsize(pango::EllipsizeMode::Middle)
        .max_width_chars(30)
        .build();
    who.add_css_class("om-meta");
    let commit_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    commit_box.add_css_class("om-commit");
    commit_box.append(&message);
    commit_box.append(&who);
    commit_box.append(&commit_button);
    commit_box.set_visible(false);

    let left = gtk::Box::new(gtk::Orientation::Vertical, 0);
    left.append(&scroller);
    left.append(&commit_box);
    inner.set_start_child(Some(&left));

    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.add_css_class("om-detail");
    root.append(&header);
    root.append(&inner);
    root.append(&totals);

    let generation = Rc::new(RefCell::new(0u64));
    let running: Rc<RefCell<Option<Cancel>>> = Rc::new(RefCell::new(None));
    // Which commit the file list belongs to, so selecting a file knows what to
    // diff, and a patch load of its own that can be abandoned the same way.
    let showing: Rc<RefCell<Option<(PathBuf, Target)>>> = Rc::new(RefCell::new(None));
    let patching: Rc<RefCell<Option<Cancel>>> = Rc::new(RefCell::new(None));
    let patch_generation = Rc::new(RefCell::new(0u64));
    // What the rows on screen were built from. A row carries its path and no
    // more, and the diff for a renamed file needs to know where it came from.
    let listed: Rc<RefCell<Option<Box<Detail>>>> = Rc::new(RefCell::new(None));
    let staged_count = Rc::new(std::cell::Cell::new(0usize));
    // One write at a time. git reads the index again after a pre-commit hook
    // has run, so a file staged while "Committing…" was on screen went into a
    // commit whose message had been written without it.
    let busy = Rc::new(std::cell::Cell::new(false));

    // Reloading after a write: the same path the pane already uses, so there is
    // one way for its contents to arrive.
    let reload: Rc<RefCell<Option<Rc<dyn Fn()>>>> = Rc::new(RefCell::new(None));

    {
        let showing = Rc::clone(&showing);
        let staged_count = Rc::clone(&staged_count);
        let reload = Rc::clone(&reload);
        let message = message.clone();
        let totals = totals.clone();
        let on_changed = Rc::clone(&on_changed);
        let busy = Rc::clone(&busy);
        let files_for_commit = files.clone();
        commit_button.connect_clicked(move |button| {
            let Some((repo, Target::Working)) = showing.borrow().clone() else { return };
            let text = message.text().to_string();
            if let Some(reason) = commit::why_not_commit(&repo, staged_count.get(), &text) {
                tell(&totals, &reason);
                return;
            }
            if busy.replace(true) {
                tell(&totals, "Still staging. Try again in a moment.");
                return;
            }
            button.set_sensitive(false);
            files_for_commit.set_sensitive(false);
            tell(&totals, "Committing…");

            let rx = commit::spawn_commit(repo, text);
            let button = button.clone();
            let busy = Rc::clone(&busy);
            let files_for_commit = files_for_commit.clone();
            let message = message.clone();
            let totals = totals.clone();
            let reload = Rc::clone(&reload);
            let on_changed = Rc::clone(&on_changed);
            glib::spawn_future_local(async move {
                let result = rx.recv().await;
                button.set_sensitive(true);
                files_for_commit.set_sensitive(true);
                busy.set(false);
                match result {
                    Ok(Ok(sha)) => {
                        message.set_text("");
                        tell(&totals, &format!("Committed {sha}"));
                        if let Some(reload) = reload.borrow().clone() {
                            reload();
                        }
                        on_changed(true);
                    }
                    // A hook that refuses is quoted rather than paraphrased.
                    // The label holds one line; the rest is in its tooltip.
                    Ok(Err(err)) => tell(&totals, &err),
                    Err(_) => totals.set_label("The commit could not be started"),
                }
            });
        });
    }

    {
        let showing = Rc::clone(&showing);
        let patching = Rc::clone(&patching);
        let patch_generation = Rc::clone(&patch_generation);
        let buffer = buffer.clone();
        let listed = Rc::clone(&listed);
        files.connect_row_selected(move |_, row| {
            let Some(row) = row else { return };
            let Some((side, path)) = row_file(row) else { return };
            let path_for_render = path.clone();
            let Some((repo, target)) = showing.borrow().clone() else { return };
            // Where the list says the file came from. Without it the diff of a
            // renamed file is read as that of a new one, every line added.
            let from = listed.borrow().as_ref().and_then(|detail| {
                let group = side.unwrap_or(Group::Committed);
                let file = detail.files.iter().find(|file| file.group == group && file.path == path)?;
                file.from.clone()
            });

            let mine = {
                let mut counter = patch_generation.borrow_mut();
                *counter += 1;
                *counter
            };
            if let Some(cancel) = patching.borrow_mut().take() {
                cancel.stop();
            }
            buffer.set_text("Reading…");

            let patch_generation = Rc::clone(&patch_generation);
            let patching = Rc::clone(&patching);
            let buffer = buffer.clone();
            glib::spawn_future_local(async move {
                let path = path_for_render;
                let (rx, cancel) = commit::spawn_patch(repo, target, path.clone(), from, side);
                *patching.borrow_mut() = Some(cancel);
                let Ok(result) = rx.recv().await else { return };
                if *patch_generation.borrow() != mine {
                    return;
                }
                match result {
                    Ok(diff) => render_diff(&buffer, &diff, &path, dark),
                    Err(err) => buffer.set_text(&err),
                }
            });
        });
    }

    let load: Rc<dyn Fn(PathBuf, Target)> = {
        let generation = Rc::clone(&generation);
        let running = Rc::clone(&running);
        let showing = Rc::clone(&showing);
        let reload = Rc::clone(&reload);
        let staged_count = Rc::clone(&staged_count);
        let on_changed = Rc::clone(&on_changed);
        Rc::new(move |repo: PathBuf, target: Target| {
            let mine = {
                let mut counter = generation.borrow_mut();
                *counter += 1;
                *counter
            };
            if let Some(cancel) = running.borrow_mut().take() {
                cancel.stop();
            }
            tell(&totals, "Reading…");

            let generation = Rc::clone(&generation);
            let running = Rc::clone(&running);
            let subject = subject.clone();
            let meta = meta.clone();
            let body = body.clone();
            let body_scroller = body_scroller.clone();
            let files = files.clone();
            let totals = totals.clone();
            let showing = Rc::clone(&showing);
            let buffer = buffer.clone();
            let commit_box = commit_box.clone();
            let staged_count = Rc::clone(&staged_count);
            let reload = Rc::clone(&reload);
            let on_changed = Rc::clone(&on_changed);
            let busy = Rc::clone(&busy);
            let who = who.clone();
            let listed = Rc::clone(&listed);

            glib::spawn_future_local(async move {
                glib::timeout_future(std::time::Duration::from_millis(SETTLE_MS)).await;
                if *generation.borrow() != mine {
                    return;
                }

                *showing.borrow_mut() = Some((repo.clone(), target.clone()));
                let (rx, cancel) = commit::spawn(repo, target);
                *running.borrow_mut() = Some(cancel);
                let Ok(result) = rx.recv().await else { return };
                if *generation.borrow() != mine {
                    return;
                }

                // Before the rows: selecting one, below, reads this.
                *listed.borrow_mut() = result.as_ref().ok().cloned();
                files.remove_all();
                buffer.set_text("");

                let working = matches!(*showing.borrow(), Some((_, Target::Working)));
                commit_box.set_visible(working);
                if working {
                    if let Some((repo, _)) = showing.borrow().clone() {
                        let who = who.clone();
                        glib::spawn_future_local(async move {
                            match commit::spawn_identity(repo).recv().await {
                                Ok(Ok(identity)) => {
                                    who.set_label(&format!("as {identity}"));
                                    who.set_tooltip_text(Some(&format!(
                                        "A commit made here is recorded as {identity}"
                                    )));
                                }
                                Ok(Err(why)) => {
                                    who.set_label("no identity");
                                    who.set_tooltip_text(Some(&why));
                                }
                                Err(_) => {}
                            }
                        });
                    }
                }
                // Staging only exists for the working tree; a commit's files
                // are already committed.
                let writer: Option<Rc<dyn Fn(Write, String)>> = working.then(|| {
                    let showing = Rc::clone(&showing);
                    let reload = Rc::clone(&reload);
                    let on_changed = Rc::clone(&on_changed);
                    let totals = totals.clone();
                    let busy = Rc::clone(&busy);
                    Rc::new(move |what: Write, file: String| {
                        let Some((repo, _)) = showing.borrow().clone() else { return };
                        if busy.replace(true) {
                            return;
                        }
                        let busy = Rc::clone(&busy);
                        let rx = commit::spawn_write(repo, what, vec![file]);
                        let reload = Rc::clone(&reload);
                        let on_changed = Rc::clone(&on_changed);
                        let totals = totals.clone();
                        glib::spawn_future_local(async move {
                            let result = rx.recv().await;
                            busy.set(false);
                            match result {
                                Ok(Ok(())) => {
                                    if let Some(reload) = reload.borrow().clone() {
                                        reload();
                                    }
                                    on_changed(false);
                                }
                                Ok(Err(err)) => tell(&totals, &err),
                                Err(_) => {}
                            }
                        });
                    }) as Rc<dyn Fn(Write, String)>
                });

                match result {
                    Ok(detail) => {
                        // From the counts taken before the list was capped:
                        // Commit takes everything staged, listed or not.
                        staged_count.set(detail.counts.staged);
                        subject.set_label(&detail.summary);
                        subject.set_tooltip_text(Some(&format!(
                            "{}\n{} <{}>\n{}",
                            detail.summary, detail.author, detail.email, detail.id
                        )));
                        meta.set_label(&describe(&detail));
                        // A commit has its message here. The working tree has
                        // none, and where its list is known to differ from
                        // what git would say, this is the place that says so:
                        // above the rows it is about.
                        let said = if detail.body.is_empty() {
                            detail.caveats.join("\n\n")
                        } else {
                            detail.body.clone()
                        };
                        body_scroller.set_visible(!said.is_empty());
                        body.set_label(&said);
                        // A new commit's message starts at its top.
                        body_scroller.vadjustment().set_value(0.0);
                        let mut position = 0i32;
                        for group in [
                            Group::Committed,
                            Group::Conflicted,
                            Group::Staged,
                            Group::Unstaged,
                            Group::Untracked,
                        ] {
                            let in_group: Vec<_> = detail
                                .files
                                .iter()
                                .filter(|f| f.group == group)
                                .cloned()
                                .collect();
                            // How many there are, as against how many are
                            // listed: the list is capped, the count is not.
                            let total = if group == Group::Committed {
                                in_group.len()
                            } else {
                                detail.counts.of(group)
                            };
                            if in_group.is_empty() && total == 0 {
                                continue;
                            }
                            let mut members = Vec::new();
                            for node in commit::tree(&in_group) {
                                let row = row(&node, writer.as_ref());
                                members.push(row.clone());
                                files.append(&row);
                            }
                            // Count the rows that actually went in, not the
                            // files: the tree emits a row per directory too,
                            // and counting files put every heading in the
                            // wrong place.
                            let mut added = members.len() as i32;
                            if group != Group::Committed {
                                // Inserted above its rows once they exist, so
                                // the heading has something to hide.
                                let heading = group_heading(group, in_group.len(), total, members);
                                files.insert(&heading, position);
                                added += 1;
                            }
                            position += added;
                        }
                        // Browse mode will not pick a starting row on its own
                        // when the first one is not selectable, so the first
                        // actual file is chosen here.
                        let mut index = 0;
                        while let Some(candidate) = files.row_at_index(index) {
                            if candidate.is_selectable() {
                                files.select_row(Some(&candidate));
                                break;
                            }
                            index += 1;
                        }
                        if detail.total_files > detail.files.len() {
                            let more = gtk::Label::builder()
                                .label(format!(
                                    "+{} more files not listed",
                                    detail.total_files - detail.files.len()
                                ))
                                .xalign(0.0)
                                .build();
                            more.add_css_class("om-meta");
                            more.set_margin_top(6);
                            let row = gtk::ListBoxRow::builder().child(&more).build();
                            row.set_selectable(false);
                            row.set_activatable(false);
                            files.append(&row);
                        }
                        tell(&totals, &summary(&detail));
                    }
                    Err(err) => {
                        subject.set_label(if working {
                            "Could not read the working tree"
                        } else {
                            "Could not read this commit"
                        });
                        // Not the commit shown before this one.
                        subject.set_tooltip_text(None);
                        // The reason can be a sentence, and the line holds a
                        // few words of one.
                        tell(&meta, &err);
                        body_scroller.set_visible(false);
                        tell(&totals, "");
                    }
                }
            });
        })
    };

    {
        // Reloading is just loading again, so there is one way for the pane's
        // contents to arrive.
        // Weakly: the loader itself holds this slot, so a strong reference
        // back to it would be a ring nothing could ever free.
        let load = Rc::downgrade(&load);
        let showing = Rc::clone(&showing);
        *reload.borrow_mut() = Some(Rc::new(move || {
            let current = showing.borrow().clone();
            if let (Some((repo, target)), Some(load)) = (current, load.upgrade()) {
                load(repo, target);
            }
        }));
    }

    {
        // Space stages the focused file and `c` moves to the message, the two
        // reflexes anyone arriving from lazygit already has. The controller is
        // on the list, so neither fires while the message is being typed.
        let message = message.clone();
        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed(move |controller, key, _, state| {
            if !crate::ui::unmodified(state) {
                return glib::Propagation::Proceed;
            }
            if key == gtk::gdk::Key::c {
                if message.get_visible() {
                    message.grab_focus();
                    return glib::Propagation::Stop;
                }
                return glib::Propagation::Proceed;
            }
            if key != gtk::gdk::Key::space {
                return glib::Propagation::Proceed;
            }
            let focused = controller
                .widget()
                .and_downcast::<gtk::ListBox>()
                .and_then(|list| list.focus_child());
            match focused.as_ref().and_then(find_button) {
                Some(button) => {
                    button.emit_clicked();
                    glib::Propagation::Stop
                }
                None => glib::Propagation::Proceed,
            }
        });
        files_for_keys.add_controller(keys);
    }

    // No Tab handler on the file list: the natural order already runs row,
    // stage button, next row, and on to the message box. Intercepting Tab here
    // jumped straight to the message and made the stage buttons unreachable
    // from the keyboard entirely.

    let focus_files: Rc<dyn Fn()> = {
        let files = files_for_focus;
        Rc::new(move || {
            // Browse selection keeps a row current, so focusing the list lands
            // on the file already being shown.
            if let Some(row) = files.selected_row() {
                row.grab_focus();
            } else {
                files.grab_focus();
            }
        })
    };

    (root.upcast(), load, focus_files)
}

/// A heading above each kind of change. Untracked is marked as not blocking an
/// update, because the question people bring to this view is usually "what do I
/// have to clean up", and untracked files are not the answer.
/// A heading above each kind of change, which folds its files away. Untracked
/// files are often the bulk of a list and the least of anyone's concerns, so
/// past a handful the group starts folded.
const FOLD_UNTRACKED_PAST: usize = 10;

fn group_heading(
    group: Group,
    count: usize,
    total: usize,
    members: Vec<gtk::ListBoxRow>,
) -> gtk::ListBoxRow {
    let line = gtk::Box::new(gtk::Orientation::Horizontal, 8);

    let open = group.blocks_update() || count <= FOLD_UNTRACKED_PAST;
    let chevron = gtk::Image::from_icon_name(if open {
        "pan-down-symbolic"
    } else {
        "pan-end-symbolic"
    });
    line.append(&chevron);

    let title = gtk::Label::builder()
        .label(if total > count {
            format!("{}  ({count} of {total} shown)", group.heading())
        } else {
            format!("{}  ({count})", group.heading())
        })
        .xalign(0.0)
        .hexpand(true)
        .build();
    title.add_css_class("om-name");
    line.append(&title);

    if !group.blocks_update() {
        let note = gtk::Label::new(Some("does not block an update"));
        note.add_css_class("om-meta");
        line.append(&note);
    }

    for member in &members {
        member.set_visible(open);
    }

    // A button, so the group folds from the keyboard as well as the pointer.
    let toggle = gtk::Button::builder().child(&line).build();
    toggle.add_css_class("om-stat-head");
    toggle.set_tooltip_text(Some(if group.blocks_update() {
        "These changes stop the background refresh from updating this repository"
    } else {
        "Untracked files do not hold an update back. Where an incoming commit adds a file of \
         the same name, git refuses rather than overwrite it, and the card says which"
    }));
    let shown = std::cell::Cell::new(open);
    toggle.connect_clicked(move |_| {
        let now = !shown.get();
        shown.set(now);
        chevron.set_icon_name(Some(if now {
            "pan-down-symbolic"
        } else {
            "pan-end-symbolic"
        }));
        for member in &members {
            member.set_visible(now);
        }
    });

    let holder = gtk::Box::new(gtk::Orientation::Vertical, 0);
    holder.set_margin_top(10);
    holder.append(&toggle);

    let row = gtk::ListBoxRow::builder().child(&holder).build();
    row.set_selectable(false);
    row.set_activatable(false);
    if !group.blocks_update() {
        row.add_css_class("om-aside");
    }
    row
}

/// The group travels on the row with the path, so that the diff asked for is
/// the half the row stands for. One letter, then a colon: a path may have
/// colons of its own, and is everything after the first.
fn side_token(group: Group) -> char {
    match group {
        Group::Staged => 's',
        Group::Unstaged => 'u',
        Group::Untracked => 'n',
        Group::Conflicted => 'c',
        Group::Committed => '-',
    }
}

/// The file a row stands for, and which half of its changes.
fn row_file(row: &gtk::ListBoxRow) -> Option<(Option<Group>, String)> {
    let name = row.widget_name();
    let (token, path) = name.strip_prefix("file:")?.split_once(':')?;
    let side = match token {
        "s" => Some(Group::Staged),
        "u" => Some(Group::Unstaged),
        "n" => Some(Group::Untracked),
        "c" => Some(Group::Conflicted),
        _ => None,
    };
    Some((side, path.to_string()))
}

/// Put a message in the pane's one status line. The line holds a single
/// ellipsized row, and some of what lands in it is longer than that: a refusal
/// that explains itself, or several lines from a hook. The whole of it goes in
/// the tooltip, so it can be read.
fn tell(label: &gtk::Label, text: &str) {
    let first = text.lines().next().unwrap_or_default();
    label.set_label(first);
    let more = text.lines().count() > 1 || first.chars().count() > 60;
    label.set_tooltip_text(more.then_some(text));
}

fn describe(detail: &Detail) -> String {
    if detail.author.is_empty() {
        let blocking = if detail.counts.blocks_update() {
            // Not "until these are committed": on a branch that is behind, the
            // commit is what makes the update impossible.
            "the background refresh leaves this repository alone while these are here"
        } else {
            "nothing here blocks an update"
        };
        return format!("uncommitted  ·  {blocking}");
    }
    let mut parts = vec![
        detail.author.clone(),
        relative_time(detail.when),
        absolute_time(detail.when),
        detail.short_id.clone(),
    ];
    if let Some(committer) = &detail.committer {
        parts.push(format!("committed by {committer}"));
    }
    if detail.merge {
        parts.push(format!("merge of {}", detail.parents.join(" + ")));
    }
    parts.join("  ·  ")
}

fn summary(detail: &Detail) -> String {
    let files = detail.total_files;
    let mut parts = vec![format!(
        "{files} {} changed",
        if files == 1 { "file" } else { "files" }
    )];
    parts.push(format!("+{} −{}", detail.insertions, detail.deletions));
    if detail.unreadable > 0 {
        // The counts are of what could be read. Without this they are a
        // total, and in a partial clone the total was "+0 −0".
        parts.push(format!(
            "{} could not be read and {} not counted",
            detail.unreadable,
            if detail.unreadable == 1 { "is" } else { "are" }
        ));
    }
    if detail.merge {
        // Against the first parent, so this is what the merge brought in
        // rather than everything on both sides.
        parts.push("against the first parent".to_string());
    }
    if detail.boundary {
        // git marks such a commit "grafted" and shows the same numbers.
        parts.push(
            "its parent is outside this shallow clone, so every file reads as added".to_string(),
        );
    }
    parts.join("  ·  ")
}

/// The syntax and theme sets, loaded once. Both are sizeable and parsing them
/// per diff would be the slowest thing the pane does.
fn highlighter() -> &'static (syntect::parsing::SyntaxSet, syntect::highlighting::ThemeSet) {
    static ONCE: std::sync::OnceLock<(
        syntect::parsing::SyntaxSet,
        syntect::highlighting::ThemeSet,
    )> = std::sync::OnceLock::new();
    ONCE.get_or_init(|| {
        (
            syntect::parsing::SyntaxSet::load_defaults_newlines(),
            syntect::highlighting::ThemeSet::load_defaults(),
        )
    })
}

fn rgba(hex: &str) -> gtk::gdk::RGBA {
    let (r, g, b) = crate::theme::Palette::rgb(hex);
    gtk::gdk::RGBA::new(r as f32, g as f32, b as f32, 1.0)
}

/// Mix `over` into `base`. Text tag backgrounds ignore alpha, so the blend has
/// to be done rather than layered.
fn blend(base: &str, over: &str, amount: f64) -> gtk::gdk::RGBA {
    let (br, bg, bb) = crate::theme::Palette::rgb(base);
    let (or, og, ob) = crate::theme::Palette::rgb(over);
    let mix = |a: f64, b: f64| (a + (b - a) * amount) as f32;
    gtk::gdk::RGBA::new(mix(br, or), mix(bg, og), mix(bb, ob), 1.0)
}

/// A foreground tag for one syntect colour, created once per colour and reused
/// — a diff can mention the same colour thousands of times.
fn colour_tag(buffer: &gtk::TextBuffer, colour: syntect::highlighting::Color) -> gtk::TextTag {
    let name = format!("fg{:02x}{:02x}{:02x}", colour.r, colour.g, colour.b);
    if let Some(existing) = buffer.tag_table().lookup(&name) {
        return existing;
    }
    let tag = gtk::TextTag::builder().name(&name).build();
    tag.set_foreground_rgba(Some(&gtk::gdk::RGBA::new(
        colour.r as f32 / 255.0,
        colour.g as f32 / 255.0,
        colour.b as f32 / 255.0,
        1.0,
    )));
    buffer.tag_table().add(&tag);
    tag
}

/// The stage button inside a file row. Rows are a handful of widgets wide, so
/// walking them is cheaper than threading a reference out to the key handler.
fn find_button(widget: &gtk::Widget) -> Option<gtk::Button> {
    if let Ok(button) = widget.clone().downcast::<gtk::Button>() {
        return Some(button);
    }
    let mut child = widget.first_child();
    while let Some(current) = child {
        if let Some(found) = find_button(&current) {
            return Some(found);
        }
        child = current.next_sibling();
    }
    None
}

/// Pick a syntax for a path.
///
/// syntect's bundled set comes from Sublime's default packages, which have no
/// TypeScript, no JSX and no Bicep — three of the languages this is actually
/// pointed at. JavaScript is close enough for what highlighting buys on a
/// diff: strings, comments, keywords and braces. Types and decorators read as
/// plain text, which is a better answer than the whole file reading as plain
/// text.
fn syntax_for<'a>(
    syntaxes: &'a syntect::parsing::SyntaxSet,
    path: &str,
) -> &'a syntect::parsing::SyntaxReference {
    let extension = path
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    let stand_in = match extension.as_str() {
        "ts" | "tsx" | "mts" | "cts" | "jsx" => Some("JavaScript"),
        // Bicep is braces, strings and `//` comments once you look past the
        // keywords; JavaScript carries those.
        "bicep" => Some("JavaScript"),
        _ => None,
    };
    stand_in
        .and_then(|name| syntaxes.find_syntax_by_name(name))
        .or_else(|| syntaxes.find_syntax_by_extension(&extension))
        .unwrap_or_else(|| syntaxes.find_syntax_plain_text())
}

/// Paint a unified diff into the buffer: the file's own language highlighted,
/// with added and removed lines marked by a line background rather than by
/// colouring the text, so both facts are readable at once.
/// A line longer than this goes in uncoloured. Highlighting is paid for per
/// coloured span, by the highlighter and again by the text buffer, and a long
/// line of minified code is thousands of spans.
const HIGHLIGHT_MAX_LINE: usize = 2_000;
/// And past this much text in one diff the rest goes in uncoloured too: four
/// thousand lines each just under the limit above are still millions of spans.
const HIGHLIGHT_BUDGET: usize = 256 * 1024;

fn render_diff(buffer: &gtk::TextBuffer, diff: &commit::FileDiff, path: &str, dark: bool) {
    buffer.set_text("");
    if diff.binary {
        buffer.set_text("Binary file.");
        return;
    }
    if diff.lines.is_empty() {
        // "No textual changes" is not all there is to say about a file that
        // was moved, and the worker says the rest.
        buffer.set_text(diff.note.as_deref().unwrap_or("No textual changes."));
        return;
    }

    let (syntaxes, themes) = highlighter();
    let theme = themes
        .themes
        .get(if dark { "base16-ocean.dark" } else { "InspiredGitHub" })
        .or_else(|| themes.themes.values().next());
    let syntax = syntax_for(syntaxes, path);

    // A diff skips between hunks, so highlighting state cannot carry across
    // one. Restarting per hunk is approximate at the top of each — the honest
    // alternative would be to read the whole file.
    let mut highlighter = theme.map(|theme| {
        syntect::easy::HighlightLines::new(syntax, theme)
    });
    let mut highlighted = 0usize;
    // Set when a line went in without the highlighter seeing it. Its idea of
    // where it is in the syntax is then out of date, the same as after a hunk
    // boundary, and it starts again.
    let mut stale = false;

    for line in &diff.lines {
        let start = buffer.end_iter().offset();

        if line.origin == '@' {
            let mut end = buffer.end_iter();
            buffer.insert(&mut end, &format!("{}\n", line.text));
            buffer.apply_tag_by_name(
                "hunk",
                &buffer.iter_at_offset(start),
                &buffer.end_iter(),
            );
            if let Some(theme) = theme {
                highlighter = Some(syntect::easy::HighlightLines::new(syntax, theme));
            }
            continue;
        }

        let mut end = buffer.end_iter();
        buffer.insert(&mut end, prefix(line.origin));

        let spans = if line.text.len() > HIGHLIGHT_MAX_LINE || highlighted > HIGHLIGHT_BUDGET {
            stale = true;
            None
        } else {
            if stale {
                if let Some(theme) = theme {
                    highlighter = Some(syntect::easy::HighlightLines::new(syntax, theme));
                }
                stale = false;
            }
            highlighted += line.text.len();
            let owned = format!("{}\n", line.text);
            highlighter
                .as_mut()
                .and_then(|h| h.highlight_line(&owned, syntaxes).ok())
                // The pieces borrow from `owned`, which does not outlive this
                // block; keep them as text of their own.
                .map(|spans| {
                    spans
                        .into_iter()
                        .map(|(style, piece)| (style, piece.to_string()))
                        .collect::<Vec<_>>()
                })
        };
        match spans {
            Some(spans) => {
                for (style, piece) in &spans {
                    let piece = piece.trim_end_matches('\n');
                    if piece.is_empty() {
                        continue;
                    }
                    let at = buffer.end_iter().offset();
                    let mut end = buffer.end_iter();
                    buffer.insert(&mut end, piece);
                    // The dark theme paints `invalid.illegal` in its own
                    // background colour, on a red background that this view
                    // does not draw. And a highlighter restarted at every hunk
                    // calls a good deal of valid code invalid. So the token
                    // was dark on dark: closing braces, C# method signatures
                    // and the colons in JSON were simply not there. Untagged,
                    // it takes the view's own text colour.
                    if theme.and_then(|theme| theme.settings.background) == Some(style.foreground) {
                        continue;
                    }
                    let tag = colour_tag(buffer, style.foreground);
                    buffer.apply_tag(
                        &tag,
                        &buffer.iter_at_offset(at),
                        &buffer.end_iter(),
                    );
                }
            }
            None => {
                let mut end = buffer.end_iter();
                buffer.insert(&mut end, &line.text);
            }
        }
        let mut end = buffer.end_iter();
        buffer.insert(&mut end, "\n");

        if let Some(tag) = match line.origin {
            '+' => Some("added"),
            '-' => Some("removed"),
            _ => None,
        } {
            buffer.apply_tag_by_name(tag, &buffer.iter_at_offset(start), &buffer.end_iter());
        }
    }

    if diff.truncated {
        let mut end = buffer.end_iter();
        buffer.insert(&mut end, "\n… diff truncated\n");
    }
}

fn prefix(origin: char) -> &'static str {
    match origin {
        '+' => "+",
        '-' => "-",
        _ => " ",
    }
}

fn row(node: &Node, on_write: Option<&Rc<dyn Fn(Write, String)>>) -> gtk::ListBoxRow {
    match node {
        Node::Dir { name, depth } => {
            let label = gtk::Label::builder()
                .label(name)
                .xalign(0.0)
                .ellipsize(pango::EllipsizeMode::Start)
                .margin_start((*depth as i32) * 16)
                .margin_top(6)
                .build();
            label.add_css_class("om-meta");
            let row = gtk::ListBoxRow::builder().child(&label).build();
            row.set_selectable(false);
            row.set_activatable(false);
            row
        }
        Node::File { file, display, depth } => {
            let line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            line.set_margin_start((*depth as i32) * 16);
            line.add_css_class("om-row");

            let status = gtk::Label::new(Some(file.change.letter()));
            status.add_css_class("om-status");
            status.add_css_class(file.change.css_class());
            line.append(&status);

            let name = gtk::Label::builder()
                .label(display)
                .xalign(0.0)
                .hexpand(true)
                .ellipsize(pango::EllipsizeMode::Middle)
                .build();
            name.add_css_class("om-summary");
            line.append(&name);

            // A rename with no edit is not "nothing changed", and a binary file
            // has no line count to give. Nor has one that could not be read:
            // its count is not zero, it is not known.
            let churn = gtk::Label::builder()
                .label(if file.unreadable {
                    "unreadable".to_string()
                } else if file.binary {
                    "binary".to_string()
                } else if file.insertions == 0 && file.deletions == 0 {
                    String::new()
                } else {
                    format!("+{} −{}", file.insertions, file.deletions)
                })
                .xalign(1.0)
                .width_chars(12)
                .build();
            churn.add_css_class("om-meta");
            line.append(&churn);

            // A conflicted file gets no button, and so no Space either: to
            // stage it is to tell git the conflict is settled.
            if let Some(on_write) = on_write.filter(|_| file.group != Group::Conflicted) {
                // Whole files only. Staging a hunk is a different tool.
                let staged = file.group == Group::Staged;
                let button = gtk::Button::builder()
                    .icon_name(if staged {
                        "list-remove-symbolic"
                    } else {
                        "list-add-symbolic"
                    })
                    .tooltip_text(if staged { "Unstage this file" } else { "Stage this file" })
                    .has_frame(false)
                    .valign(gtk::Align::Center)
                    .build();
                let on_write = Rc::clone(on_write);
                let path = file.path.clone();
                button.connect_clicked(move |_| {
                    on_write(
                        if staged { Write::Unstage } else { Write::Stage },
                        path.clone(),
                    );
                });
                line.append(&button);
            }

            let mut tip = file.path.clone();
            if let Some(from) = &file.from {
                tip = format!("{tip}\nrenamed from {from}");
            }
            if file.group == Group::Conflicted {
                tip.push_str("\nConflicted — resolve it in a terminal");
            }
            line.set_tooltip_text(Some(&tip));

            let row = gtk::ListBoxRow::builder().child(&line).build();
            // The full path travels on the row, since the label only carries
            // the part worth showing.
            row.set_widget_name(&format!("file:{}:{}", side_token(file.group), file.path));
            row
        }
    }
}
