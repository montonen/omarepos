//! The commit detail pane: what the selected commit changed.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use gtk::pango;

use crate::commit::{self, Cancel, Detail, Node};
use crate::ui::{absolute_time, relative_time};

/// Holding Down through a graph walks a commit per frame. Waiting for the
/// cursor to settle means one diff is read rather than a hundred.
const SETTLE_MS: u64 = 120;

/// Build the pane. The returned closure loads a commit into it; calling it
/// again abandons whatever was still loading.
pub fn pane() -> (gtk::Widget, Rc<dyn Fn(PathBuf, String)>) {
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
        .wrap(true)
        .wrap_mode(pango::WrapMode::WordChar)
        .lines(6)
        .ellipsize(pango::EllipsizeMode::End)
        .build();
    body.add_css_class("om-summary");
    body.set_margin_top(4);
    header.append(&body);

    let files = gtk::Box::new(gtk::Orientation::Vertical, 0);
    files.set_margin_top(10);
    files.set_margin_bottom(12);
    files.set_margin_start(14);
    files.set_margin_end(14);

    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .child(&files)
        .build();
    scroller.add_css_class("om-grid");

    let totals = gtk::Label::builder()
        .xalign(0.0)
        .ellipsize(pango::EllipsizeMode::End)
        .build();
    totals.add_css_class("om-statusbar");

    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.add_css_class("om-detail");
    root.append(&header);
    root.append(&scroller);
    root.append(&totals);

    let generation = Rc::new(RefCell::new(0u64));
    let running: Rc<RefCell<Option<Cancel>>> = Rc::new(RefCell::new(None));

    let load: Rc<dyn Fn(PathBuf, String)> = {
        let generation = Rc::clone(&generation);
        let running = Rc::clone(&running);
        Rc::new(move |repo: PathBuf, id: String| {
            let mine = {
                let mut counter = generation.borrow_mut();
                *counter += 1;
                *counter
            };
            if let Some(cancel) = running.borrow_mut().take() {
                cancel.stop();
            }
            totals.set_label("Reading…");

            let generation = Rc::clone(&generation);
            let running = Rc::clone(&running);
            let subject = subject.clone();
            let meta = meta.clone();
            let body = body.clone();
            let files = files.clone();
            let totals = totals.clone();

            glib::spawn_future_local(async move {
                glib::timeout_future(std::time::Duration::from_millis(SETTLE_MS)).await;
                if *generation.borrow() != mine {
                    return;
                }

                let (rx, cancel) = commit::spawn(repo, id);
                *running.borrow_mut() = Some(cancel);
                let Ok(result) = rx.recv().await else { return };
                if *generation.borrow() != mine {
                    return;
                }

                while let Some(child) = files.first_child() {
                    files.remove(&child);
                }

                match result {
                    Ok(detail) => {
                        subject.set_label(&detail.summary);
                        subject.set_tooltip_text(Some(&format!(
                            "{}\n{} <{}>\n{}",
                            detail.summary, detail.author, detail.email, detail.id
                        )));
                        meta.set_label(&describe(&detail));
                        body.set_visible(!detail.body.is_empty());
                        body.set_label(&detail.body);
                        for node in commit::tree(&detail.files) {
                            files.append(&row(&node));
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
                            files.append(&more);
                        }
                        totals.set_label(&summary(&detail));
                    }
                    Err(err) => {
                        subject.set_label("Could not read this commit");
                        meta.set_label(&err);
                        body.set_visible(false);
                        totals.set_label("");
                    }
                }
            });
        })
    };

    (root.upcast(), load)
}

fn describe(detail: &Detail) -> String {
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
    if detail.merge {
        // Against the first parent, so this is what the merge brought in
        // rather than everything on both sides.
        parts.push("against the first parent".to_string());
    }
    parts.join("  ·  ")
}

fn row(node: &Node) -> gtk::Widget {
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
            label.upcast()
        }
        Node::File { file, depth } => {
            let line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            line.set_margin_start((*depth as i32) * 16);
            line.add_css_class("om-row");

            let status = gtk::Label::new(Some(file.change.letter()));
            status.add_css_class("om-status");
            status.add_css_class(file.change.css_class());
            line.append(&status);

            let name = gtk::Label::builder()
                .label(&file.path)
                .xalign(0.0)
                .hexpand(true)
                .ellipsize(pango::EllipsizeMode::Middle)
                .build();
            name.add_css_class("om-summary");
            line.append(&name);

            // A rename with no edit is not "nothing changed", and a binary file
            // has no line count to give.
            let churn = gtk::Label::builder()
                .label(if file.binary {
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

            let mut tip = file.path.clone();
            if let Some(from) = &file.from {
                tip = format!("{tip}\nrenamed from {from}");
            }
            line.set_tooltip_text(Some(&tip));
            line.upcast()
        }
    }
}
