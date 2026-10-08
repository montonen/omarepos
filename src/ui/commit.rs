//! The commit detail pane: what the selected commit changed.

use std::cell::{Cell, OnceCell, RefCell};
use std::collections::HashMap;
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
    // backgrounds are mixed from the theme's own colours: here, so that the
    // tags exist, and again each time a diff is drawn.
    Tints::of(&crate::theme::Palette::load()).apply(&buffer);
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
    // What the last write here came to: "Committing…" while one runs, and a
    // refusal in git's or a hook's own words. On a line of its own, which the
    // loader never writes to. These used to go to the totals line under the
    // pane, and the loader writes that on every read: "Committed" was put
    // there and replaced in the same turn of the main loop, so it was never
    // drawn at all, and a hook's reason for refusing lasted until the window
    // was next come back to, which is the way back from fixing what the hook
    // complained about.
    let outcome = gtk::Label::builder()
        .xalign(0.0)
        .ellipsize(pango::EllipsizeMode::End)
        .visible(false)
        .build();
    outcome.add_css_class("om-summary");
    let commit_line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    commit_line.append(&message);
    commit_line.append(&who);
    commit_line.append(&commit_button);
    let commit_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
    commit_box.add_css_class("om-commit");
    commit_box.append(&outcome);
    commit_box.append(&commit_line);
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
    // The commit asked for last. A read takes a moment, so this is not always
    // the one whose files are on screen: the viewer knows that one.
    let showing: Rc<RefCell<Option<(PathBuf, Target)>>> = Rc::new(RefCell::new(None));
    let viewer = Rc::new(Viewer {
        buffer: buffer.clone(),
        listed: RefCell::new(None),
        rendered: RefCell::new(None),
        reading: RefCell::new(None),
        generation: Cell::new(0),
    });
    // Set while the loader empties the list and fills it again. The list
    // reports each step of that as a change of selection: to nothing, then to
    // whichever row it took for itself. Neither is the user's doing, and the
    // loader says what to show once it has put the selection back.
    let rebuilding = Rc::new(Cell::new(false));
    // How many times the list has been rebuilt. What is put right a moment
    // after a rebuild is dropped if another has happened since, and only
    // then: a later read that changes nothing leaves this one's rows there.
    let rebuilds = Rc::new(Cell::new(0u64));
    // Which groups are folded, for as long as the same commit is on show.
    let folds: Rc<RefCell<HashMap<Group, bool>>> = Rc::new(RefCell::new(HashMap::new()));
    // The adjustment the list scrolls to its focused row with, which GTK gives
    // it from the viewport it is put in. Taken away for a moment each time
    // the list is rebuilt in place, and remembered here to be given back.
    let follows: Rc<OnceCell<Option<gtk::Adjustment>>> = Rc::new(OnceCell::new());
    let staged_count = Rc::new(Cell::new(0usize));
    // One write at a time. git reads the index again after a pre-commit hook
    // has run, so a file staged while "Committing…" was on screen went into a
    // commit whose message had been written without it.
    let busy = Rc::new(Cell::new(false));

    // Reloading after a write: the same path the pane already uses, so there is
    // one way for its contents to arrive.
    let reload: Rc<RefCell<Option<Rc<dyn Fn()>>>> = Rc::new(RefCell::new(None));

    {
        let viewer = Rc::clone(&viewer);
        let staged_count = Rc::clone(&staged_count);
        let reload = Rc::clone(&reload);
        let message = message.clone();
        let outcome = outcome.clone();
        let on_changed = Rc::clone(&on_changed);
        let busy = Rc::clone(&busy);
        let files_for_commit = files.clone();
        commit_button.connect_clicked(move |button| {
            let Some(repo) = viewer.working() else { return };
            let text = message.text().to_string();
            if let Some(reason) = commit::why_not_commit(&repo, staged_count.get(), &text) {
                report(&outcome, &reason);
                return;
            }
            if busy.replace(true) {
                report(&outcome, "Still staging. Try again in a moment.");
                return;
            }
            button.set_sensitive(false);
            files_for_commit.set_sensitive(false);
            report(&outcome, "Committing…");

            let rx = commit::spawn_commit(repo, text);
            let button = button.clone();
            let busy = Rc::clone(&busy);
            let files_for_commit = files_for_commit.clone();
            let message = message.clone();
            let outcome = outcome.clone();
            let reload = Rc::clone(&reload);
            let on_changed = Rc::clone(&on_changed);
            glib::spawn_future_local(async move {
                let result = rx.recv().await;
                button.set_sensitive(true);
                files_for_commit.set_sensitive(true);
                busy.set(false);
                // Switching off the button that was pressed takes the
                // keyboard away from it, and switching it on again does not
                // give it back: after a refusal the keyboard was nowhere.
                if button.root().is_some_and(|root| root.focus().is_none()) {
                    button.grab_focus();
                }
                let reread = || {
                    if let Some(reload) = reload.borrow().clone() {
                        reload();
                    }
                };
                match result {
                    Ok(Ok(sha)) => {
                        message.set_text("");
                        let done = match sha.as_str() {
                            "" => "Committed".to_string(),
                            sha => format!("Committed {sha}"),
                        };
                        // As a toast, where a push and a pull report theirs:
                        // after the commit that leaves the tree clean, this
                        // box and its line are no longer on screen.
                        report(&outcome, if announce(&button, &done) { "" } else { &done });
                        reread();
                        on_changed(true);
                    }
                    // A hook that refuses is quoted rather than paraphrased.
                    // The line holds the first of what it said; the rest is in
                    // the tooltip. And the tree is read again: a hook that
                    // refuses has often rewritten a file first, which is then
                    // staged as it was and modified as it is, and the list
                    // went on showing neither that nor a `+` to stage the fix.
                    Ok(Err(err)) => {
                        report(&outcome, &err);
                        reread();
                    }
                    Err(_) => report(&outcome, "The commit could not be started"),
                }
            });
        });
    }

    {
        // Enter in the message commits, as it does in every box with one
        // button beside it. It did nothing: the only way from the message to
        // the commit was Tab, past the name, and then Enter. Not while a
        // commit is running, when the button is switched off; a click sent
        // from here would not ask.
        // Weakly: the button's own handler holds the message, to read it, and
        // a handler on the message that held the button would make a ring of
        // the two, each kept alive by a closure on the other, and with them
        // the file list the button's handler also holds.
        let commit_button = commit_button.downgrade();
        message.connect_activate(move |_| {
            let Some(commit_button) = commit_button.upgrade() else { return };
            if commit_button.is_sensitive() {
                commit_button.emit_clicked();
            }
        });
    }

    {
        let viewer = Rc::clone(&viewer);
        let rebuilding = Rc::clone(&rebuilding);
        files.connect_row_selected(move |_, row| {
            if rebuilding.get() {
                return;
            }
            // With no row, too: a group folded over the selected file takes
            // the selection away, and its diff should not stay behind.
            viewer.show(row);
        });
    }

    let load: Rc<dyn Fn(PathBuf, Target)> = {
        let generation = Rc::clone(&generation);
        let running = Rc::clone(&running);
        let showing = Rc::clone(&showing);
        let reload = Rc::clone(&reload);
        let staged_count = Rc::clone(&staged_count);
        let on_changed = Rc::clone(&on_changed);
        let viewer = Rc::clone(&viewer);
        Rc::new(move |repo: PathBuf, target: Target| {
            let mine = {
                let mut counter = generation.borrow_mut();
                *counter += 1;
                *counter
            };
            if let Some(cancel) = running.borrow_mut().take() {
                cancel.stop();
            }
            // Asked for what was asked for last: a re-read, after a write
            // here or on the way back to the window. There is no cursor to
            // wait for then, and nothing on screen is replaced before there
            // is something to replace it with.
            let asked = (repo.clone(), target.clone());
            let again = showing.borrow().as_ref() == Some(&asked);
            *showing.borrow_mut() = Some(asked);
            if !again {
                tell(&totals, "Reading…");
            }

            let generation = Rc::clone(&generation);
            let running = Rc::clone(&running);
            let subject = subject.clone();
            let meta = meta.clone();
            let body = body.clone();
            let body_scroller = body_scroller.clone();
            let files = files.clone();
            let scroller = scroller.clone();
            let totals = totals.clone();
            let commit_box = commit_box.clone();
            let outcome = outcome.clone();
            let staged_count = Rc::clone(&staged_count);
            let reload = Rc::clone(&reload);
            let on_changed = Rc::clone(&on_changed);
            let busy = Rc::clone(&busy);
            let who = who.clone();
            let viewer = Rc::clone(&viewer);
            let rebuilding = Rc::clone(&rebuilding);
            let rebuilds = Rc::clone(&rebuilds);
            let follows = Rc::clone(&follows);
            let folds = Rc::clone(&folds);

            glib::spawn_future_local(async move {
                if !again {
                    glib::timeout_future(std::time::Duration::from_millis(SETTLE_MS)).await;
                }
                // Asked again since, whether or not there was a wait: this
                // runs a turn of the main loop after it was asked for, and two
                // requests in one turn would otherwise each start a read.
                if *generation.borrow() != mine {
                    return;
                }

                let (rx, cancel) = commit::spawn(repo.clone(), target.clone());
                *running.borrow_mut() = Some(cancel);
                let Ok(result) = rx.recv().await else { return };
                if *generation.borrow() != mine {
                    return;
                }

                let working = target == Target::Working;
                if working {
                    let who = who.clone();
                    let repo = repo.clone();
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
                if let Ok(detail) = &result {
                    // From the counts taken before the list was capped:
                    // Commit takes everything staged, listed or not.
                    staged_count.set(detail.counts.staged);
                }

                // Whether the rows on screen are this commit's already, and
                // whether they still say what is true of it.
                let (same, unchanged) = match viewer.listed.borrow().as_ref() {
                    Some(listed) if listed.repo == repo && listed.target == target => {
                        (true, listed.result == result)
                    }
                    _ => (false, false),
                };
                if unchanged {
                    // Coming back to the window re-reads the working tree, and
                    // most of the time finds what it left. The list used to be
                    // emptied and rebuilt regardless, which put the selection
                    // on the first file, the diff at its top and the groups
                    // back as they started, every time the pointer returned.
                    // A file can have changed without its counts changing, so
                    // the diff on show is read again: quietly, and redrawn
                    // only if it differs.
                    tell(&totals, &result.as_ref().map(|detail| summary(detail)).unwrap_or_default());
                    viewer.show(files.selected_row().as_ref());
                    return;
                }

                // A re-read that did find something new, which after a stage
                // or an unstage is every time. The user is somewhere in this
                // list, and is put back there once it has been rebuilt.
                let place = same.then(|| Place::of(&files));
                let offset = scroller.vadjustment().value();
                if !same {
                    folds.borrow_mut().clear();
                    // What a write came to was said about the list this one
                    // replaces.
                    report(&outcome, "");
                    // And another commit's files start at the top, wherever
                    // the last one's were scrolled to.
                    scroller.vadjustment().set_value(0.0);
                }
                // The message box goes when this is not the working tree, and
                // after the commit that emptied the tree the keyboard is in it.
                let typing = focus_inside(&commit_box).is_some();

                *viewer.listed.borrow_mut() = Some(Listed {
                    repo: repo.clone(),
                    target: target.clone(),
                    result: result.clone(),
                });
                rebuilding.set(true);
                rebuilds.set(rebuilds.get() + 1);
                let built = rebuilds.get();
                files.remove_all();
                commit_box.set_visible(working);

                // Staging only exists for the working tree; a commit's files
                // are already committed.
                let writer: Option<Rc<dyn Fn(Write, String)>> = working.then(|| {
                    let viewer = Rc::clone(&viewer);
                    let reload = Rc::clone(&reload);
                    let on_changed = Rc::clone(&on_changed);
                    let outcome = outcome.clone();
                    let busy = Rc::clone(&busy);
                    Rc::new(move |what: Write, file: String| {
                        let Some(repo) = viewer.working() else { return };
                        if busy.replace(true) {
                            return;
                        }
                        let busy = Rc::clone(&busy);
                        let rx = commit::spawn_write(repo, what, vec![file]);
                        let reload = Rc::clone(&reload);
                        let on_changed = Rc::clone(&on_changed);
                        let outcome = outcome.clone();
                        glib::spawn_future_local(async move {
                            let result = rx.recv().await;
                            busy.set(false);
                            let reread = || {
                                if let Some(reload) = reload.borrow().clone() {
                                    reload();
                                }
                            };
                            match result {
                                Ok(Ok(())) => {
                                    // Whatever the line said was about the
                                    // index as it was before this.
                                    report(&outcome, "");
                                    reread();
                                    on_changed(false);
                                }
                                // git refusing to stage a file it was shown
                                // as able to stage means the list is out of
                                // date: the file is gone, or something else
                                // has the index. So it is read again here too.
                                Ok(Err(err)) => {
                                    report(&outcome, &err);
                                    reread();
                                }
                                Err(_) => {}
                            }
                        });
                    }) as Rc<dyn Fn(Write, String)>
                });

                match &result {
                    Ok(detail) => {
                        subject.set_label(&detail.summary);
                        // Who wrote it and its whole id, for a commit. The
                        // working tree has neither, and its tooltip read
                        // "Local changes", an empty pair of brackets, and a
                        // blank line.
                        subject.set_tooltip_text(
                            (!detail.id.is_empty())
                                .then(|| {
                                    format!(
                                        "{}\n{} <{}>\n{}",
                                        detail.summary, detail.author, detail.email, detail.id
                                    )
                                })
                                .as_deref(),
                        );
                        meta.set_label(&describe(detail));
                        meta.set_tooltip_text(None);
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
                            // The folders above the row being made, so that a
                            // folder's row can be named by its whole path:
                            // `src/ui` and `tests/ui` are both `ui`, and the
                            // keyboard is put back on a row by its name.
                            let mut above: Vec<String> = Vec::new();
                            for node in commit::tree(&in_group) {
                                let row = row(&node, writer.as_ref());
                                // The files of a group that holds nothing
                                // back step back with its heading. Only the
                                // heading did, which left the untracked files
                                // drawn as loudly as the changes that matter.
                                if group != Group::Committed && !group.blocks_update() {
                                    row.add_css_class("om-aside");
                                }
                                if let Node::Dir { name, depth } = &node {
                                    above.truncate(*depth);
                                    above.push(name.clone());
                                    row.set_widget_name(&format!(
                                        "dir:{}:{}",
                                        side_token(group),
                                        above.join("/")
                                    ));
                                }
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
                                let heading =
                                    group_heading(group, in_group.len(), total, members, &folds);
                                files.insert(&heading, position);
                                added += 1;
                            }
                            position += added;
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
                            // The arrow keys stop here as well.
                            row.set_widget_name("more:");
                            files.append(&row);
                        }
                        tell(&totals, &summary(detail));
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
                        tell(&meta, err);
                        body_scroller.set_visible(false);
                        tell(&totals, "");
                    }
                }

                // Which row to be on. For a commit that was not on show
                // before, the first file there is. A row that is folded away
                // does not count: a working tree of nothing but a dozen
                // untracked files opens with its one group folded, and used
                // to open with a hidden row selected, its diff on show under
                // no name, and Tab and Space going to a file never seen.
                // Coming back to the same commit, it is the row the user was
                // on, or the nearest to it of those that are left.
                let rows: HashMap<String, gtk::ListBoxRow> = named_rows(&files).into_iter().collect();
                let chosen = match &place {
                    // Nothing selected because nothing could be: every group
                    // folded, or a list that could not be read. That is not
                    // the user choosing to have no selection, and the first
                    // file to show up is selected as on a first load.
                    Some(place) if !place.had_a_file => first_row(&files, is_shown_file),
                    Some(place) => place.back(place.selected, &rows, is_shown_file),
                    None => first_row(&files, is_shown_file),
                };
                match &chosen {
                    Some(row) => files.select_row(Some(row)),
                    // Nor whichever row the list may have taken for itself
                    // while it was being filled. Selecting no row, and not
                    // unselecting the one: that is only done "if the
                    // selection mode allows it", and this is the call that
                    // does not ask.
                    None => files.select_row(gtk::ListBoxRow::NONE),
                }
                rebuilding.set(false);
                viewer.show(chosen.as_ref());

                // And the keyboard, if it was in the list, or in the message
                // box that has just gone. On the selected row it stays with
                // the selection, which is what the arrow keys keep it with;
                // on a heading, a folder or another file's button it goes
                // back there, or as near as there still is. Straight away,
                // not once the list has been laid out: a key pressed in
                // between would go nowhere.
                let keyboard = place.as_ref().and_then(|place| {
                    let at = place.focused?;
                    Some(if place.selected == Some(at) {
                        chosen.clone()
                    } else {
                        place.back(Some(at), &rows, is_shown).or_else(|| chosen.clone())
                    })
                });
                let back_to = match &keyboard {
                    Some(row) => Some(row.clone()),
                    None if typing && !working => Some(chosen.clone()),
                    None => None,
                };
                // A row that has just been made has no place in the list
                // yet, and until the list is laid out it measures as sitting
                // at the very top. The list and the viewport around it both
                // scroll to whatever takes the focus, so focusing such a row
                // would send a list that had been scrolled down back to its
                // top. Neither follows the focus until the rows have places.
                let viewport = scroller.child().and_downcast::<gtk::Viewport>();
                if same {
                    let _ = follows.get_or_init(|| files.adjustment());
                    files.set_adjustment(gtk::Adjustment::NONE);
                    if let Some(viewport) = &viewport {
                        viewport.set_scroll_to_focus(false);
                    }
                }
                if let (Some(row), true) = (back_to, files.is_mapped()) {
                    match row.or_else(|| first_row(&files, is_shown)) {
                        Some(row) => row.grab_focus(),
                        None => files.grab_focus(),
                    };
                }

                // Once they have: the list goes back to following the focus,
                // is put where it was scrolled to, as far as its new length
                // allows, and the row the keyboard is on is brought into view
                // if that left it out.
                if same {
                    let follows = Rc::clone(&follows);
                    glib::timeout_add_local_once(std::time::Duration::from_millis(50), move || {
                        files.set_adjustment(follows.get().and_then(Option::as_ref));
                        if let Some(viewport) = &viewport {
                            viewport.set_scroll_to_focus(true);
                        }
                        // A later rebuild has its own idea of where the list
                        // is, and its own turn at this.
                        if rebuilds.get() != built {
                            return;
                        }
                        scroller.vadjustment().set_value(offset);
                        if let (Some(viewport), Some(row)) = (&viewport, keyboard.flatten()) {
                            // Up or down only: a long path makes a row wider
                            // than the pane, and that is not a reason to move
                            // the list sideways.
                            let how = gtk::ScrollInfo::new();
                            how.set_enable_horizontal(false);
                            viewport.scroll_to(&row, Some(how));
                        }
                    });
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
                // Mapped, and not `get_visible()`: that reads the entry's own
                // flag, which nothing ever clears. It is the box around the
                // entry that is hidden, for every commit that is not the
                // working tree, and GTK will give the focus to a widget it is
                // not showing. `c` on an ordinary commit's files sent the
                // keyboard into an entry nobody could see.
                if message.is_mapped() {
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
            // Not into a pane that is not on screen. The list is filled
            // whether or not the pane is open, and a row in a hidden pane
            // takes the focus as readily as any other: the focus ring
            // vanished and the arrow keys moved a selection nobody could see.
            if !files.is_mapped() {
                return;
            }
            // Browse selection keeps a row current, so focusing the list lands
            // on the file already being shown. Failing that, on the first row
            // there is, which is a heading when every group is folded: Space
            // there unfolds it. Never on a row that is folded away.
            let row = files
                .selected_row()
                .filter(|row| row.get_visible())
                .or_else(|| first_row(&files, |row| row.get_visible()));
            match row {
                Some(row) => row.grab_focus(),
                None => files.grab_focus(),
            };
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
    folds: &Rc<RefCell<HashMap<Group, bool>>>,
) -> gtk::ListBoxRow {
    let line = gtk::Box::new(gtk::Orientation::Horizontal, 8);

    // As it was left, if this commit has been on show before. The list is
    // rebuilt after every stage and whenever the working tree is found
    // changed, and each time a group the user had opened shut again. Once
    // drawn, a group stays as it is until the user says otherwise: it does not
    // fold itself either, when an eleventh untracked file turns up.
    let open = *folds
        .borrow_mut()
        .entry(group)
        .or_insert(group.blocks_update() || count <= FOLD_UNTRACKED_PAST);
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
    let shown = Cell::new(open);
    let folds = Rc::clone(folds);
    toggle.connect_clicked(move |_| {
        let now = !shown.get();
        shown.set(now);
        folds.borrow_mut().insert(group, now);
        chevron.set_icon_name(Some(if now {
            "pan-down-symbolic"
        } else {
            "pan-end-symbolic"
        }));
        for member in &members {
            // A row folded away does not stay selected. It would go on being
            // the file the diff is of, and the one Tab comes back to.
            if !now && member.is_selected() {
                if let Some(list) = member.parent().and_downcast::<gtk::ListBox>() {
                    list.select_row(gtk::ListBoxRow::NONE);
                }
            }
            member.set_visible(now);
        }
    });

    let holder = gtk::Box::new(gtk::Orientation::Vertical, 0);
    holder.set_margin_top(10);
    holder.append(&toggle);

    let row = gtk::ListBoxRow::builder().child(&holder).build();
    row.set_selectable(false);
    row.set_activatable(false);
    // So that the keyboard can be put back on it after the list is rebuilt.
    row.set_widget_name(&format!("group:{}", side_token(group)));
    if !group.blocks_update() {
        row.add_css_class("om-aside");
    }
    row
}

/// What the file list on screen was built from. A row carries its path and
/// which half of the file it stands for, and no more.
struct Listed {
    repo: PathBuf,
    target: Target,
    result: Result<Box<Detail>, String>,
}

/// One file's diff as it stands in the view, or the reason there is none.
struct Rendered {
    key: (PathBuf, Target, Option<Group>, String),
    diff: Result<Box<commit::FileDiff>, String>,
}

/// The diff view, and the reading of what goes into it.
struct Viewer {
    buffer: gtk::TextBuffer,
    listed: RefCell<Option<Listed>>,
    rendered: RefCell<Option<Rendered>>,
    /// The read in flight, so that it can be abandoned.
    reading: RefCell<Option<Cancel>>,
    generation: Cell<u64>,
}

impl Viewer {
    /// The repository, when it is its working tree that is listed.
    fn working(&self) -> Option<PathBuf> {
        match self.listed.borrow().as_ref() {
            Some(Listed { repo, target: Target::Working, .. }) => Some(repo.clone()),
            _ => None,
        }
    }

    /// Show the diff of the file `row` stands for, or nothing for no row.
    ///
    /// Asked for the file that is already on show, which is what a re-read of
    /// the list amounts to, it reads quietly: the view keeps what it has until
    /// the new diff is in, and is redrawn only if that differs. Redrawing
    /// starts the view again at its top, which is where it used to land every
    /// time the window was come back to.
    fn show(self: &Rc<Self>, row: Option<&gtk::ListBoxRow>) {
        self.generation.set(self.generation.get() + 1);
        let mine = self.generation.get();
        if let Some(cancel) = self.reading.borrow_mut().take() {
            cancel.stop();
        }

        let wanted = row.and_then(row_file).and_then(|(side, path)| {
            let listed = self.listed.borrow();
            let listed = listed.as_ref()?;
            // Where the list says the file came from. Without it the diff of a
            // renamed file is read as that of a new one, every line added.
            let from = listed.result.as_ref().ok().and_then(|detail| {
                let group = side.unwrap_or(Group::Committed);
                let file = detail.files.iter().find(|file| file.group == group && file.path == path)?;
                file.from.clone()
            });
            Some((listed.repo.clone(), listed.target.clone(), side, path, from))
        });
        let Some((repo, target, side, path, from)) = wanted else {
            self.buffer.set_text("");
            *self.rendered.borrow_mut() = None;
            return;
        };

        let key = (repo.clone(), target.clone(), side, path.clone());
        let again = self.rendered.borrow().as_ref().is_some_and(|rendered| rendered.key == key);
        if !again {
            self.buffer.set_text("Reading…");
            *self.rendered.borrow_mut() = None;
        }

        let (rx, cancel) = commit::spawn_patch(repo, target, path.clone(), from, side);
        *self.reading.borrow_mut() = Some(cancel);
        let viewer = Rc::clone(self);
        glib::spawn_future_local(async move {
            let Ok(result) = rx.recv().await else { return };
            if viewer.generation.get() != mine {
                return;
            }
            let same = viewer
                .rendered
                .borrow()
                .as_ref()
                .is_some_and(|rendered| rendered.key == key && rendered.diff == result);
            if same {
                return;
            }
            match &result {
                Ok(diff) => {
                    // The palette as it is now, and not as it was when the
                    // pane was built: the theme can be changed while a graph
                    // page is open, and the view's own background follows it.
                    let tints = Tints::of(&crate::theme::Palette::load());
                    render_diff(&viewer.buffer, diff, &path, &tints);
                }
                Err(err) => viewer.buffer.set_text(err),
            }
            // Kept for an error too: the next read of the same file is then
            // as quiet as any other, and does not blank the reason to show it
            // again.
            *viewer.rendered.borrow_mut() = Some(Rendered { key, diff: result });
        });
    }
}

/// Where the user was in the file list, taken down before the list is emptied
/// so that they can be put back when it has been filled again.
struct Place {
    /// Every row that can be found again, in order, by the name it was given:
    /// files, folders, group headings and the line that counts what is not
    /// listed.
    names: Vec<String>,
    selected: Option<usize>,
    /// The row the keyboard was on or in, if it was in the list at all.
    focused: Option<usize>,
    /// Whether there was a file's row on show, to be selected or not.
    had_a_file: bool,
}

impl Place {
    fn of(files: &gtk::ListBox) -> Self {
        let selected = files.selected_row();
        let focused = focus_inside(files).and_then(|widget| {
            // The row itself, or the stage button inside it.
            widget.clone().downcast::<gtk::ListBoxRow>().ok().or_else(|| {
                widget
                    .ancestor(gtk::ListBoxRow::static_type())
                    .and_downcast::<gtk::ListBoxRow>()
            })
        });
        let mut place = Place { names: Vec::new(), selected: None, focused: None, had_a_file: false };
        for (name, row) in named_rows(files) {
            place.had_a_file |= is_shown_file(&row);
            if selected.as_ref() == Some(&row) {
                place.selected = Some(place.names.len());
            }
            if focused.as_ref() == Some(&row) {
                place.focused = Some(place.names.len());
            }
            place.names.push(name);
        }
        place
    }

    /// The row of the rebuilt list to go back to from the one that was at
    /// `from`, among those `wanted` accepts.
    fn back(
        &self,
        from: Option<usize>,
        rows: &HashMap<String, gtk::ListBoxRow>,
        wanted: impl Fn(&gtk::ListBoxRow) -> bool,
    ) -> Option<gtk::ListBoxRow> {
        let name = way_back(&self.names, from?, |name| rows.get(name).is_some_and(&wanted))?;
        rows.get(&name).cloned()
    }
}

/// A row that is not folded away. Its own flag, and not `is_visible()`, which
/// is false for every row while the pane itself is closed: the list is filled
/// then too.
fn is_shown(row: &gtk::ListBoxRow) -> bool {
    row.get_visible()
}

/// A file's row, not folded away: one that can be the selection.
fn is_shown_file(row: &gtk::ListBoxRow) -> bool {
    row.is_selectable() && row.get_visible()
}

/// The widget that has the keyboard, when it is somewhere inside `within`.
/// Asked of the window, which is the one that knows.
fn focus_inside(within: &impl IsA<gtk::Widget>) -> Option<gtk::Widget> {
    within.root()?.focus().filter(|widget| widget.is_ancestor(within))
}

/// The rows of the list that carry a name of their own, in order.
fn named_rows(files: &gtk::ListBox) -> Vec<(String, gtk::ListBoxRow)> {
    (0..)
        .map_while(|index| files.row_at_index(index))
        .filter_map(|row| {
            let name = row.widget_name();
            ["file:", "dir:", "group:", "more:"]
                .iter()
                .any(|kind| name.starts_with(kind))
                .then(|| (name.to_string(), row))
        })
        .collect()
}

/// The first row of the list that `wanted` accepts.
fn first_row(files: &gtk::ListBox, wanted: impl Fn(&gtk::ListBoxRow) -> bool) -> Option<gtk::ListBoxRow> {
    (0..).map_while(|index| files.row_at_index(index)).find(|row| wanted(row))
}

/// The row to go back to once the list has been rebuilt, by name: `names` is
/// the list as it was, `from` the row the user was on, and `there` says
/// whether a row of that name is in the new list to be gone to.
///
/// The same row, if it is still there. A file that has just been staged is
/// not: it has left its group. Then the next file of that group, below or
/// failing that above, which is the next one to stage, and where the cursor
/// stays in lazygit. When the group has run out, the same file in the group
/// it went to, so that the key that staged it would unstage it and nothing
/// else. And only then whatever is nearest.
fn way_back(names: &[String], from: usize, there: impl Fn(&str) -> bool) -> Option<String> {
    let was = names.get(from)?.as_str();
    if there(was) {
        return Some(was.to_string());
    }
    let around = || names[from + 1..].iter().chain(names[..from].iter().rev()).map(String::as_str);
    if let Some((token, path)) = was.strip_prefix("file:").and_then(|rest| rest.split_once(':')) {
        let own = format!("file:{token}:");
        if let Some(next) = around().find(|name| name.starts_with(&own) && there(name)) {
            return Some(next.to_string());
        }
        let moved = ["c", "s", "u", "n"]
            .iter()
            .filter(|other| **other != token)
            .map(|other| format!("file:{other}:{path}"))
            .find(|name| there(name));
        if moved.is_some() {
            return moved;
        }
    }
    around().find(|name| there(name)).map(str::to_string)
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

/// Say what a write came to on the line kept for that, which takes no room
/// while it has nothing to say.
fn report(outcome: &gtk::Label, text: &str) {
    tell(outcome, text);
    outcome.set_visible(!text.trim().is_empty());
}

/// Raise a toast on the page `from` is part of. Whether there was a page to
/// raise it on: the pane does not own one, and is not told which it is in.
fn announce(from: &impl IsA<gtk::Widget>, text: &str) -> bool {
    let overlay = from
        .ancestor(adw::ToastOverlay::static_type())
        .and_downcast::<adw::ToastOverlay>();
    match overlay {
        Some(overlay) => {
            overlay.add_toast(crate::ui::toast(text));
            true
        }
        None => false,
    }
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

/// Red, green and blue, each from 0 to 1.
type Rgb = (f64, f64, f64);

/// How much of the theme's green and red is mixed into the background behind
/// an added and a removed line. It was 0.20. The band is what the lines a
/// diff is read for are drawn on, and the text on it is coloured by a syntax
/// theme made for one background of its own: at 0.20 the review measured a
/// comment on an added line at under 3 to 1 on all but one of the twenty-two
/// themes installed. Fainter than this and the band stops marking the line;
/// the `+` and `-` in the margin say the same thing either way.
const BAND: f64 = 0.12;
/// The same, for the theme's accent behind a hunk header.
const HUNK_BAND: f64 = 0.14;
/// The contrast every piece of text in a diff is brought up to against what it
/// is drawn on. 3 to 1 and not the 4.5 asked of body text: the syntax themes
/// do not manage 3 for a comment on their own backgrounds, and at 4.5 a
/// comment on the paler themes comes out the colour of the code around it.
const LEGIBLE: f64 = 3.0;

/// The colours a diff is drawn in, worked out from the theme's palette.
///
/// The view has the theme's background and the bands are mixed from the
/// theme's green and red, but the colour of each token comes from one of two
/// syntax themes, each drawn for a single background that is not this one.
/// Laid over twenty-two themes as they were, comments, which are a fifth of
/// the characters in this repository's own diffs, were measured at 1.7 to 1
/// on the worst of them. So a token keeps its colour where that can be read,
/// and is moved towards the theme's own text colour by as little as makes it
/// readable where it cannot.
struct Tints {
    dark: bool,
    fg: Rgb,
    added: Rgb,
    removed: Rgb,
    hunk: Rgb,
    hunk_text: Rgb,
    /// Everything a token may be drawn on: the view itself and the two bands.
    /// One answer for all three, so that a word is the same colour on a line
    /// that was added as on the line above it.
    surfaces: [Rgb; 3],
}

impl Tints {
    fn of(palette: &crate::theme::Palette) -> Self {
        let rgb = crate::theme::Palette::rgb;
        let (bg, fg) = (rgb(&palette.bg), rgb(&palette.fg));
        let added = mix(bg, rgb(&palette.green), BAND);
        let removed = mix(bg, rgb(&palette.red), BAND);
        let hunk = mix(bg, rgb(&palette.accent), HUNK_BAND);
        Tints {
            dark: palette.dark,
            fg,
            added,
            removed,
            hunk,
            hunk_text: legible(rgb(&palette.fg_dim), &[hunk], fg, LEGIBLE),
            surfaces: [bg, added, removed],
        }
    }

    /// Give the buffer's line tags these colours, making the tags the first
    /// time. Text tag backgrounds ignore alpha, which is why each band is a
    /// colour mixed beforehand and not the theme's colour laid on thinly.
    fn apply(&self, buffer: &gtk::TextBuffer) {
        for (name, band) in [("added", self.added), ("removed", self.removed), ("hunk", self.hunk)] {
            let tag = buffer.tag_table().lookup(name).unwrap_or_else(|| {
                let tag = gtk::TextTag::builder().name(name).build();
                buffer.tag_table().add(&tag);
                tag
            });
            tag.set_paragraph_background_rgba(Some(&rgba(band)));
            if name == "hunk" {
                tag.set_foreground_rgba(Some(&rgba(self.hunk_text)));
            }
        }
    }

    /// A syntax theme's colour for a token, as it is drawn here.
    fn token(&self, colour: syntect::highlighting::Color) -> Rgb {
        let channel = |value: u8| f64::from(value) / 255.0;
        legible(
            (channel(colour.r), channel(colour.g), channel(colour.b)),
            &self.surfaces,
            self.fg,
            LEGIBLE,
        )
    }
}

fn rgba((r, g, b): Rgb) -> gtk::gdk::RGBA {
    gtk::gdk::RGBA::new(r as f32, g as f32, b as f32, 1.0)
}

/// `amount` of `over` mixed into `base`.
fn mix(base: Rgb, over: Rgb, amount: f64) -> Rgb {
    let part = |a: f64, b: f64| a + (b - a) * amount;
    (part(base.0, over.0), part(base.1, over.1), part(base.2, over.2))
}

/// How light a colour is, as the eye weighs it: WCAG's relative luminance.
fn luminance((r, g, b): Rgb) -> f64 {
    let linear = |c: f64| if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) };
    0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b)
}

/// WCAG's contrast ratio between two colours: 1 for the same colour, 21 for
/// black on white.
fn contrast(a: Rgb, b: Rgb) -> f64 {
    let (a, b) = (luminance(a), luminance(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

/// `colour`, moved towards `fg` by as little as gives it a contrast of `floor`
/// against every one of `surfaces`. Left alone when it has that already. When
/// not even `fg` has it, the answer is whichever of the two reads better:
/// the theme's own text is as clear as this asks anything to be, and a colour
/// that was clearer than that is not made worse.
fn legible(colour: Rgb, surfaces: &[Rgb], fg: Rgb, floor: f64) -> Rgb {
    const STEPS: u32 = 16;
    let least = |colour: Rgb| {
        surfaces.iter().map(|surface| contrast(colour, *surface)).fold(f64::INFINITY, f64::min)
    };
    (0..STEPS)
        .map(|step| mix(colour, fg, f64::from(step) / f64::from(STEPS)))
        .find(|mixed| least(*mixed) >= floor)
        .unwrap_or(if least(fg) >= least(colour) { fg } else { colour })
}

/// A foreground tag for one colour, created once per colour and reused — a
/// diff can mention the same colour thousands of times.
fn colour_tag(buffer: &gtk::TextBuffer, colour: Rgb) -> gtk::TextTag {
    let byte = |value: f64| (value.clamp(0.0, 1.0) * 255.0).round() as u8;
    let name = format!("fg{:02x}{:02x}{:02x}", byte(colour.0), byte(colour.1), byte(colour.2));
    if let Some(existing) = buffer.tag_table().lookup(&name) {
        return existing;
    }
    let tag = gtk::TextTag::builder().name(&name).build();
    tag.set_foreground_rgba(Some(&rgba(colour)));
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

fn render_diff(buffer: &gtk::TextBuffer, diff: &commit::FileDiff, path: &str, tints: &Tints) {
    buffer.set_text("");
    tints.apply(buffer);
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
        .get(if tints.dark { "base16-ocean.dark" } else { "InspiredGitHub" })
        .or_else(|| themes.themes.values().next());
    let syntax = syntax_for(syntaxes, path);
    // A syntax theme has a dozen colours and a diff has thousands of tokens:
    // each colour is worked out and looked up once.
    let mut tags: HashMap<(u8, u8, u8), gtk::TextTag> = HashMap::new();

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

        // "\ No newline at end of file". libgit2 hands it over as a line of
        // its own, with one of these three origins and a newline in front of
        // the words. It is a remark about the line above, not a line of the
        // file: drawn as one, it was a blank context line and then a line of
        // source reading "\ No newline…", coloured as code.
        if matches!(line.origin, '=' | '>' | '<') {
            let mut end = buffer.end_iter();
            buffer.insert(&mut end, &format!("{}\n", line.text.trim_start_matches('\n')));
            buffer.apply_tag_by_name(
                "hunk",
                &buffer.iter_at_offset(start),
                &buffer.end_iter(),
            );
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
            // The highlighter is somebody else's parser run over whatever is
            // in the file, inside a callback, where a panic ends the program.
            // Caught here, a line it cannot take costs the colours for the
            // rest of this diff and nothing else.
            let coloured = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                highlighter.as_mut().and_then(|h| h.highlight_line(&owned, syntaxes).ok())
            }));
            if coloured.is_err() {
                highlighter = None;
            }
            coloured
                .ok()
                .flatten()
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
                    let colour = style.foreground;
                    let tag = tags
                        .entry((colour.r, colour.g, colour.b))
                        .or_insert_with(|| colour_tag(buffer, tints.token(colour)));
                    buffer.apply_tag(
                        &*tag,
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
                let how = if file.change == commit::Change::Copied { "copied" } else { "renamed" };
                tip = format!("{tip}\n{how} from {from}");
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

// Nothing here builds a widget or needs a display: these are the parts of
// the pane that are reasoning about names, and can be checked as such.
#[cfg(test)]
mod tests {
    use super::*;

    fn list(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| name.to_string()).collect()
    }

    /// Where the selection goes when the list `before` is rebuilt as `after`,
    /// with the user on `on`.
    fn back(before: &[&str], on: &str, after: &[&str]) -> Option<String> {
        let before = list(before);
        let from = before.iter().position(|name| name == on).expect("the row was in the list");
        way_back(&before, from, |name| name.starts_with("file:") && after.contains(&name))
    }

    const BEFORE: [&str; 9] = [
        "group:s",
        "file:s:done.rs",
        "group:u",
        "dir:u:0:src",
        "file:u:src/a.rs",
        "file:u:src/b.rs",
        "file:u:src/c.rs",
        "group:n",
        "file:n:notes.txt",
    ];

    #[test]
    fn a_row_that_is_still_there_is_gone_back_to() {
        let after = BEFORE;
        assert_eq!(back(&BEFORE, "file:u:src/b.rs", &after).as_deref(), Some("file:u:src/b.rs"));
    }

    /// Space, Space, Space down a group: each stage takes its file out of the
    /// group, and the cursor is on the next one.
    #[test]
    fn after_a_stage_the_cursor_is_on_the_next_file_to_stage() {
        let after = [
            "group:s",
            "file:s:done.rs",
            "file:s:src/b.rs",
            "group:u",
            "file:u:src/a.rs",
            "file:u:src/c.rs",
            "group:n",
            "file:n:notes.txt",
        ];
        assert_eq!(back(&BEFORE, "file:u:src/b.rs", &after).as_deref(), Some("file:u:src/c.rs"));
    }

    /// The last of its group: the one above it, not the first file of the
    /// group below, whose button does something else.
    #[test]
    fn the_last_file_of_a_group_gives_way_to_the_one_above_it() {
        let after = [
            "group:s",
            "file:s:done.rs",
            "file:s:src/c.rs",
            "group:u",
            "file:u:src/a.rs",
            "file:u:src/b.rs",
            "group:n",
            "file:n:notes.txt",
        ];
        assert_eq!(back(&BEFORE, "file:u:src/c.rs", &after).as_deref(), Some("file:u:src/b.rs"));
    }

    /// When the group has run out the cursor follows the file to where it
    /// went, so that the same key puts it back and touches nothing else.
    #[test]
    fn the_only_file_of_a_group_is_followed_into_the_group_it_went_to() {
        let before = ["group:s", "file:s:done.rs", "group:u", "file:u:only.rs", "group:n", "file:n:notes.txt"];
        let after = ["group:s", "file:s:done.rs", "file:s:only.rs", "group:n", "file:n:notes.txt"];
        assert_eq!(back(&before, "file:u:only.rs", &after).as_deref(), Some("file:s:only.rs"));

        // And back again, from a Staged group that held nothing else.
        let staged = ["group:s", "file:s:only.rs", "group:n", "file:n:notes.txt"];
        let unstaged = ["group:u", "file:u:only.rs", "group:n", "file:n:notes.txt"];
        assert_eq!(back(&staged, "file:s:only.rs", &unstaged).as_deref(), Some("file:u:only.rs"));
    }

    /// A file that went away altogether, reverted in an editor: whatever is
    /// nearest, below first.
    #[test]
    fn a_file_that_is_gone_gives_way_to_its_nearest_neighbour() {
        let before = ["group:s", "file:s:done.rs", "group:u", "file:u:only.rs", "group:n", "file:n:notes.txt"];
        let after = ["group:s", "file:s:done.rs", "group:n", "file:n:notes.txt"];
        assert_eq!(back(&before, "file:u:only.rs", &after).as_deref(), Some("file:n:notes.txt"));
        let after = ["group:s", "file:s:done.rs"];
        assert_eq!(back(&before, "file:u:only.rs", &after).as_deref(), Some("file:s:done.rs"));
        assert_eq!(back(&before, "file:u:only.rs", &[]), None);
    }

    /// A path may hold colons, and a name that is not a file's has no group
    /// to keep to.
    #[test]
    fn names_are_taken_apart_at_the_first_colon_after_the_group() {
        let before = ["file:u:a:b.txt", "file:u:z.txt"];
        let after = ["file:s:a:b.txt"];
        assert_eq!(back(&before, "file:u:a:b.txt", &after).as_deref(), Some("file:s:a:b.txt"));

        let rows = list(&["group:u", "file:u:a.rs"]);
        assert_eq!(way_back(&rows, 0, |name| name == "file:u:a.rs").as_deref(), Some("file:u:a.rs"));
        assert_eq!(way_back(&rows, 5, |_| true), None, "no such row to start from");
    }

    // ---- colours that have to be read on a background they were not made for

    fn hex(colour: &str) -> Rgb {
        crate::theme::Palette::rgb(colour)
    }

    fn token(colour: &str) -> syntect::highlighting::Color {
        let (r, g, b) = hex(colour);
        let byte = |value: f64| (value * 255.0).round() as u8;
        syntect::highlighting::Color { r: byte(r), g: byte(g), b: byte(b), a: 255 }
    }

    /// A palette with these four colours and the built-in one's for the rest.
    fn palette(dark: bool, bg: &str, fg: &str, green: &str, red: &str) -> crate::theme::Palette {
        crate::theme::Palette {
            dark,
            bg: bg.into(),
            fg: fg.into(),
            green: green.into(),
            red: red.into(),
            ..crate::theme::Palette::fallback()
        }
    }

    /// The comment colours of the two syntax themes the diff is highlighted
    /// with: the dark one's and the light one's.
    const COMMENT_ON_DARK: &str = "#65737e";
    const COMMENT_ON_LIGHT: &str = "#969896";

    #[test]
    fn contrast_is_the_ratio_wcag_defines() {
        let (black, white) = (hex("#000000"), hex("#ffffff"));
        assert!((contrast(black, white) - 21.0).abs() < 1e-9);
        assert!((contrast(white, black) - 21.0).abs() < 1e-9, "either way round");
        assert!((contrast(white, white) - 1.0).abs() < 1e-9);
        // A pair with a published figure: #767676 is the grey that just makes
        // 4.5 to 1 on white.
        assert!((contrast(hex("#767676"), white) - 4.54).abs() < 0.01);
    }

    #[test]
    fn a_colour_that_can_be_read_is_left_as_it_is() {
        let tints = Tints::of(&palette(true, "#14161b", "#dcd7ba", "#98bb6c", "#e46876"));
        let string = token("#a3be8c");
        assert_eq!(tints.token(string), hex("#a3be8c"));
    }

    /// Two of the themes on which a comment on an added line could not be
    /// read: a dark one of low contrast and a light one.
    #[test]
    fn a_comment_can_be_read_on_every_line_of_a_diff() {
        let cases = [
            ("a muted dark theme", palette(true, "#2e3440", "#d8dee9", "#a3be8c", "#bf616a"), COMMENT_ON_DARK),
            ("a pale light theme", palette(false, "#eff1f5", "#4c4f69", "#40a02b", "#d20f39"), COMMENT_ON_LIGHT),
        ];
        for (name, palette, comment) in cases {
            let tints = Tints::of(&palette);
            let least = |colour: Rgb| {
                tints.surfaces.iter().map(|surface| contrast(colour, *surface)).fold(f64::INFINITY, f64::min)
            };
            let before = least(hex(comment));
            let drawn = tints.token(token(comment));
            assert!(before < LEGIBLE, "{name}: the fixture has to start unreadable, and is {before:.2}");
            assert!(least(drawn) >= LEGIBLE, "{name}: {:.2}", least(drawn));
            // By moving it part of the way, not by making it body text.
            assert!(drawn != tints.fg, "{name}");
            assert!(contrast(drawn, tints.fg) > 1.5, "{name}: a comment still reads as one");
        }
    }

    #[test]
    fn the_band_is_drawn_from_the_theme_and_weaker_than_it_was() {
        let tints = Tints::of(&palette(true, "#2e3440", "#d8dee9", "#a3be8c", "#bf616a"));
        let bg = hex("#2e3440");
        assert_eq!(tints.surfaces[0], bg);
        assert_eq!(tints.added, mix(bg, hex("#a3be8c"), BAND));
        assert_eq!(tints.removed, mix(bg, hex("#bf616a"), BAND));
        let comment = hex(COMMENT_ON_DARK);
        assert!(
            contrast(comment, tints.added) > contrast(comment, mix(bg, hex("#a3be8c"), 0.20)),
            "what lowering the band was for"
        );
    }

    /// A theme whose own text does not reach the floor: its text colour is
    /// the best there is, and the loop ends there.
    #[test]
    fn nothing_is_asked_to_be_clearer_than_the_themes_own_text() {
        let (bg, fg) = (hex("#808080"), hex("#909090"));
        assert!(contrast(fg, bg) < LEGIBLE);
        assert_eq!(legible(hex("#858585"), &[bg], fg, LEGIBLE), fg);
        // And a colour that was clearer than that text is not pulled down to it.
        assert_eq!(legible(hex("#b0b0b0"), &[bg], fg, LEGIBLE), hex("#b0b0b0"));
    }
}
