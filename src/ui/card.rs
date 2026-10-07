//! One repository, as a card.

use std::rc::Rc;

use gtk::pango;
use gtk::prelude::*;

use crate::config::tildify;
use crate::git::RepoInfo;
use crate::theme::Palette;
use crate::ui::{Action, relative_time};

/// Build the card widget. `on_action` is invoked with the repo the user acted
/// on; `on_select` with the new state of the card's selection checkbox.
pub fn build(
    info: &RepoInfo,
    tags: &[String],
    selected: bool,
    on_action: Rc<dyn Fn(Action, &RepoInfo)>,
    on_select: Rc<dyn Fn(&RepoInfo, bool)>,
) -> gtk::Widget {
    let card = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(7)
        .width_request(320)
        .build();
    card.add_css_class("om-card");
    if info.error.is_some() {
        card.add_css_class("om-error");
    } else if !info.tree.is_clean() {
        card.add_css_class("om-dirty");
    }
    if selected {
        card.add_css_class("om-selected");
    }

    // Title row: selection checkbox, name, then the action menu.
    let title_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);

    let check = gtk::CheckButton::builder()
        .active(selected)
        .tooltip_text("Include in contributions")
        .valign(gtk::Align::Center)
        .build();
    {
        let info = info.clone();
        let card = card.clone();
        check.connect_toggled(move |check| {
            // Restyle in place: a full rebuild here would fight the click.
            if check.is_active() {
                card.add_css_class("om-selected");
            } else {
                card.remove_css_class("om-selected");
            }
            on_select(&info, check.is_active());
        });
    }
    title_row.append(&check);

    let name = gtk::Label::builder()
        .label(&info.name)
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(pango::EllipsizeMode::End)
        .max_width_chars(24)
        .build();
    name.add_css_class("om-name");
    title_row.append(&name);
    title_row.append(&action_menu(info, Rc::clone(&on_action)));
    card.append(&title_row);

    let path = gtk::Label::builder()
        .label(tildify(&info.path))
        .xalign(0.0)
        .ellipsize(pango::EllipsizeMode::Start)
        .max_width_chars(34)
        .build();
    path.add_css_class("om-path");
    path.set_tooltip_text(Some(&format!(
        "{}\n\nClick the card for its commit graph.",
        info.path.display()
    )));
    card.append(&path);

    card.append(&pills(info));

    if !tags.is_empty() {
        card.append(&tag_row(tags));
    }

    if let Some(remote) = &info.remote {
        let label = gtk::Label::builder()
            .label(remote)
            .xalign(0.0)
            .ellipsize(pango::EllipsizeMode::Middle)
            .max_width_chars(34)
            .build();
        label.add_css_class("om-meta");
        card.append(&label);
    }

    if !info.langs.is_empty() {
        card.append(&lang_bar(info));
        card.append(&lang_legend(info));
    }

    card.append(&footer(info));

    card.upcast()
}

fn action_menu(info: &RepoInfo, on_action: Rc<dyn Fn(Action, &RepoInfo)>) -> gtk::MenuButton {
    let popover = gtk::Popover::builder().has_arrow(false).build();
    let list = gtk::Box::new(gtk::Orientation::Vertical, 2);

    let items = [
        ("Commit graph", Action::CommitGraph),
        ("Contributions", Action::Contributions),
        ("Tags…", Action::EditTags),
        ("Open in terminal", Action::OpenTerminal),
        ("Open in editor", Action::OpenEditor),
        ("Open folder", Action::OpenFolder),
        ("Copy path", Action::CopyPath),
    ];
    for (label, action) in items {
        let button = gtk::Button::builder()
            .label(label)
            .has_frame(false)
            .build();
        if let Some(child) = button.child().and_downcast::<gtk::Label>() {
            child.set_xalign(0.0);
        }
        let info = info.clone();
        let on_action = Rc::clone(&on_action);
        let popover = popover.clone();
        button.connect_clicked(move |_| {
            popover.popdown();
            on_action(action, &info);
        });
        list.append(&button);
    }
    popover.set_child(Some(&list));

    let button = gtk::MenuButton::builder()
        .icon_name("view-more-symbolic")
        .has_frame(false)
        .popover(&popover)
        .tooltip_text("Repository actions")
        .build();
    button.add_css_class("flat");
    button
}

fn pill(text: &str, class: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.add_css_class("om-pill");
    if !class.is_empty() {
        label.add_css_class(class);
    }
    label
}

fn pills(info: &RepoInfo) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 5);

    if let Some(err) = &info.error {
        let p = pill("unreadable", "danger");
        p.set_tooltip_text(Some(err));
        row.append(&p);
        return row;
    }

    let head = if info.detached {
        format!("⌖ {}", info.head)
    } else {
        format!(" {}", info.head)
    };
    let branch = pill(&head, "branch");
    if info.detached {
        branch.set_tooltip_text(Some("Detached HEAD"));
    } else if let Some(upstream) = &info.upstream {
        branch.set_tooltip_text(Some(&format!("tracks {upstream}")));
    } else {
        branch.set_tooltip_text(Some("no upstream"));
    }
    row.append(&branch);

    if info.bare {
        row.append(&pill("bare", ""));
    } else if info.unborn {
        row.append(&pill("no commits", ""));
    } else if info.tree.is_clean() {
        row.append(&pill("clean", "clean"));
    } else {
        let t = &info.tree;
        let mut parts = Vec::new();
        if t.staged > 0 {
            parts.push(format!("+{}", t.staged));
        }
        if t.unstaged > 0 {
            parts.push(format!("~{}", t.unstaged));
        }
        if t.untracked > 0 {
            parts.push(format!("?{}", t.untracked));
        }
        let p = pill(&parts.join(" "), "dirty");
        p.set_tooltip_text(Some(&format!(
            "{} staged · {} modified · {} untracked",
            t.staged, t.unstaged, t.untracked
        )));
        row.append(&p);
        if t.conflicted > 0 {
            row.append(&pill(&format!("!{}", t.conflicted), "danger"));
        }
    }

    if info.ahead > 0 {
        let p = pill(&format!("↑{}", info.ahead), "ahead");
        p.set_tooltip_text(Some("commits not pushed"));
        row.append(&p);
    }
    if info.behind > 0 {
        let p = pill(&format!("↓{}", info.behind), "behind");
        p.set_tooltip_text(Some("commits not pulled"));
        row.append(&p);
    }
    if info.stashes > 0 {
        let p = pill(&format!("⌸{}", info.stashes), "");
        p.set_tooltip_text(Some("stash entries"));
        row.append(&p);
    }

    row
}

/// Proportional language bar, drawn with cairo so the segments can be exact.
fn lang_bar(info: &RepoInfo) -> gtk::DrawingArea {
    let area = gtk::DrawingArea::builder().content_height(5).hexpand(true).build();
    let langs = info.langs.clone();
    area.set_draw_func(move |_, cr, width, height| {
        let width = width as f64;
        let height = height as f64;
        let total: f32 = langs.iter().map(|l| l.share).sum();
        if total <= 0.0 {
            return;
        }
        let mut x = 0.0;
        for (i, l) in langs.iter().enumerate() {
            let mut w = (l.share / total) as f64 * width;
            if i == langs.len() - 1 {
                w = width - x;
            }
            let (r, g, b) = Palette::rgb(&l.color);
            cr.set_source_rgb(r, g, b);
            cr.rectangle(x, 0.0, w.max(1.0), height);
            let _ = cr.fill();
            x += w;
        }
    });
    area
}

fn lang_legend(info: &RepoInfo) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    for l in info.langs.iter().take(3) {
        let item = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        item.append(&crate::ui::color_dot(&l.color));
        let label = gtk::Label::builder()
            .label(format!("{} {:.0}%", l.name, (l.share * 100.0).round()))
            .ellipsize(pango::EllipsizeMode::End)
            .max_width_chars(14)
            .build();
        label.add_css_class("om-meta");
        item.append(&label);
        row.append(&item);
    }
    row
}

/// Tag chips, in a plain box so they hug their text — a flow box would
/// stretch each chip to fill its cell.
fn tag_row(tags: &[String]) -> gtk::Box {
    const SHOWN: usize = 4;

    let row = gtk::Box::new(gtk::Orientation::Horizontal, 5);
    row.set_halign(gtk::Align::Start);
    for tag in tags.iter().take(SHOWN) {
        let chip = gtk::Box::new(gtk::Orientation::Horizontal, 5);
        chip.add_css_class("om-tag");
        chip.append(&crate::ui::color_dot(crate::ui::key_color(&tag.to_lowercase())));
        let label = gtk::Label::builder()
            .label(tag)
            .ellipsize(pango::EllipsizeMode::End)
            .max_width_chars(14)
            .build();
        chip.append(&label);
        row.append(&chip);
    }
    if tags.len() > SHOWN {
        let more = gtk::Label::new(Some(&format!("+{}", tags.len() - SHOWN)));
        more.add_css_class("om-tag");
        more.set_tooltip_text(Some(&tags[SHOWN..].join(", ")));
        row.append(&more);
    }
    row.set_tooltip_text(Some(&tags.join(", ")));
    row
}

fn footer(info: &RepoInfo) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Vertical, 2);
    match &info.last_commit {
        Some(commit) => {
            let meta = gtk::Label::builder()
                .label(format!(
                    "{} · {} · {}",
                    relative_time(commit.when), commit.short_id, commit.author
                ))
                .xalign(0.0)
                .ellipsize(pango::EllipsizeMode::End)
                .max_width_chars(36)
                .build();
            meta.add_css_class("om-meta");
            row.append(&meta);

            let summary = gtk::Label::builder()
                .label(&commit.summary)
                .xalign(0.0)
                .ellipsize(pango::EllipsizeMode::End)
                .max_width_chars(36)
                .build();
            summary.add_css_class("om-summary");
            summary.set_tooltip_text(Some(&commit.summary));
            row.append(&summary);
        }
        None => {
            let label = gtk::Label::builder()
                .label(if info.error.is_some() {
                    "could not be read"
                } else {
                    "no commits yet"
                })
                .xalign(0.0)
                .build();
            label.add_css_class("om-meta");
            row.append(&label);
        }
    }
    row
}
