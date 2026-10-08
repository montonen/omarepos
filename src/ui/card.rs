//! One repository, as a card.

use std::rc::Rc;

use gtk::pango;
use gtk::prelude::*;

use crate::config::{Trouble, tildify};
use crate::git::RepoInfo;
use crate::theme::Palette;
use crate::ui::{Action, relative_time};

/// Everything about a card that does not come from the repository itself.
pub struct Context<'a> {
    pub tags: &'a [String],
    pub selected: bool,
    /// Whether the background refresh is set to fast-forward. Only then is it
    /// true to say a repository is being held back rather than merely behind.
    pub auto_update: bool,
    /// What the last refresh pass said, when it had something to complain
    /// about. Otherwise the failure only reaches stderr, which under a desktop
    /// launcher nobody reads.
    pub trouble: Option<&'a Trouble>,
    /// Found since the user last looked. Nothing unattended fetches it until
    /// it has been opened, or updated by hand.
    pub unseen: bool,
}

/// Build the card widget. `on_action` is invoked with the repo the user acted
/// on; `on_select` with the new state of the card's selection checkbox.
pub fn build(
    info: &RepoInfo,
    ctx: &Context<'_>,
    on_action: Rc<dyn Fn(Action, &RepoInfo)>,
    on_select: Rc<dyn Fn(&RepoInfo, bool)>,
) -> gtk::Widget {
    let Context { tags, selected, auto_update, trouble, unseen } = *ctx;
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
        // Weakly. The checkbox lives inside the card, and a closure stored on
        // it that kept the card alive kept the pair alive for good: every
        // card ever built, on every rescan and every keystroke in the filter,
        // stayed in memory until the app quit.
        let card = card.downgrade();
        check.connect_toggled(move |check| {
            // Restyle in place: a full rebuild here would fight the click.
            if let Some(card) = card.upgrade() {
                if check.is_active() {
                    card.add_css_class("om-selected");
                } else {
                    card.remove_css_class("om-selected");
                }
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
    let mut where_tip = info.path.display().to_string();
    // "Commits as", this used to say, of a reading taken without asking git.
    // It is the configuration, so it is called that; who a commit is recorded
    // as is git's to say, and the commit box is where git is asked.
    if let Some(identity) = &info.identity {
        where_tip.push_str(&format!("\n\nConfigured to commit as {identity}"));
        match &info.identity_doubt {
            Some(doubt) => where_tip.push_str(&format!(
                "\nBut git may record someone else: {doubt}. \
                 The commit box, under Local changes, asks git."
            )),
            None => where_tip.push_str(
                "\n(user.name and user.email, read without running git. \
                 The commit box, under Local changes, asks git.)",
            ),
        }
    }
    where_tip.push_str("\n\nClick the card for its commit graph.");
    path.set_tooltip_text(Some(&where_tip));
    card.append(&path);

    card.append(&pills(info, auto_update, trouble, unseen));

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
        // Weakly, for the same reason: the button is inside the popover.
        let popover = popover.downgrade();
        button.connect_clicked(move |_| {
            if let Some(popover) = popover.upgrade() {
                popover.popdown();
            }
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

fn pills(
    info: &RepoInfo,
    auto_update: bool,
    trouble: Option<&Trouble>,
    unseen: bool,
) -> gtk::Box {
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
    } else {
        // "no commits" used to end the matter, ahead of the counts. But a
        // repository with no commits yet is the one where everything in it is
        // uncommitted, and the first commit is when that is worth seeing: the
        // card said nothing about the files staged and waiting for it.
        if info.unborn {
            row.append(&pill("no commits", ""));
        }
        let t = &info.tree;
        if let Some(why) = &t.unreadable {
            // Its own pill. "clean" would be false, and the pill below, with
            // every count at zero, would be drawn with nothing in it.
            let p = pill("status unreadable", "danger");
            p.set_tooltip_text(Some(&format!(
                "What is uncommitted here is not known: the index could not be read.\n{why}\n\
                 git itself may read it without trouble; `git status` in a terminal will say."
            )));
            row.append(&p);
        } else if t.is_clean() {
            // Beside "no commits" it would only say that an empty directory
            // is empty.
            if !info.unborn {
                row.append(&pill("clean", "clean"));
            }
        } else {
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
            // Nothing but conflicts leaves all three at zero, and a pill was
            // drawn all the same, with no text in it.
            if !parts.is_empty() {
                let p = pill(&parts.join(" "), "dirty");
                p.set_tooltip_text(Some(&format!(
                    "{} staged · {} modified · {} untracked",
                    t.staged, t.unstaged, t.untracked
                )));
                row.append(&p);
            }
            if t.conflicted > 0 {
                row.append(&pill(&format!("!{}", t.conflicted), "danger"));
            }
        }
    }

    // Spelled out rather than an arrow and a number: this is the thing people
    // come to the grid to find out.
    if info.ahead > 0 {
        let p = pill(&format!("{} ahead", info.ahead), "ahead");
        p.set_tooltip_text(Some(&format!(
            "{} commits here are not on {}",
            info.ahead,
            info.upstream.as_deref().unwrap_or("the upstream")
        )));
        row.append(&p);
    }
    // `ahead` is measured against the upstream, and says nothing when there is
    // none. This is the rest of it: work on HEAD that is on no remote at all.
    if info.unpushed > info.ahead {
        let count = if info.unpushed >= crate::git::UNPUSHED_CAP {
            format!("{}+", crate::git::UNPUSHED_CAP)
        } else {
            info.unpushed.to_string()
        };
        let (label, why) = if info.remote.is_none() {
            (
                "no remote".to_string(),
                "This repository has no remote. None of its commits exist anywhere else."
                    .to_string(),
            )
        } else if info.detached {
            (
                format!("{count} unpublished"),
                format!("HEAD is detached, and {count} commits behind it are on no remote."),
            )
        } else {
            (
                format!("{count} unpublished"),
                format!(
                    "{count} commits on {} are on no remote: the branch was never pushed, \
                     or its upstream is gone.",
                    info.head
                ),
            )
        };
        let p = pill(&label, "ahead");
        p.set_tooltip_text(Some(&why));
        row.append(&p);
    }
    if info.stray_branches > 0 {
        let p = pill(
            &format!(
                "+{} {}",
                info.stray_branches,
                if info.stray_branches == 1 { "branch" } else { "branches" }
            ),
            "",
        );
        p.set_tooltip_text(Some(&format!(
            "{} other local {} commits that are on no remote.",
            info.stray_branches,
            if info.stray_branches == 1 { "branch holds" } else { "branches hold" }
        )));
        row.append(&p);
    }
    // Without this, counts that could not be taken draw no pill at all, which
    // is exactly how "nothing to push" is drawn.
    if let Some(why) = &info.history_unreadable {
        let p = pill("history unreadable", "danger");
        p.set_tooltip_text(Some(&format!(
            "How far this is from its remotes is not known: the commits in between could not \
             be read.\n{why}"
        )));
        row.append(&p);
    }
    if info.behind > 0 {
        let p = pill(&format!("{} behind", info.behind), "behind");
        p.set_tooltip_text(Some(&format!(
            "{} commits on {} are not here yet",
            info.behind,
            info.upstream.as_deref().unwrap_or("the upstream")
        )));
        row.append(&p);

        // Behind and unable to catch up are very different situations, and
        // the grid showed them identically.
        let blocked = if auto_update { info.update_blocked_by() } else { None };
        if let Some(reason) = blocked {
            let held = pill("held back", "danger");
            let mut tip = format!(
                "These {} commits will not arrive on their own: {reason}.\n\
                 The background refresh skips this repository until that is resolved.",
                info.behind
            );
            if reason == "uncommitted changes" {
                tip.push_str(
                    "\nPress p to update it yourself. git keeps your changes where the update \
                     does not touch the same files, and refuses, changing nothing, where it does.",
                );
            }
            held.set_tooltip_text(Some(&tip));
            row.append(&held);
        } else if let Some(Trouble::Update { refused }) = trouble {
            // Nothing the card can see is in the way, and git still said no:
            // an untracked or ignored file where an incoming one wants to go,
            // most often. Only git knows, so quote it.
            let held = pill("held back", "danger");
            held.set_tooltip_text(Some(&format!(
                "The last refresh fetched this repository, but git would not \
                 fast-forward it:\n{refused}\nNothing was changed."
            )));
            row.append(&held);
        }
    }
    // A fetch that failed leaves a repository looking merely out of date, and
    // the reason only in a log nobody opens.
    if let Some(Trouble::Fetch(error)) = trouble {
        let p = pill("fetch failed", "danger");
        p.set_tooltip_text(Some(&format!(
            "The last refresh could not fetch this repository:\n{error}"
        )));
        row.append(&p);
    }

    if unseen {
        let p = pill("new", "");
        p.set_tooltip_text(Some(
            "Found since you last looked. The refresh across the fleet leaves it alone until \
             you have opened it, or pressed p or f on it: running git in a repository means \
             doing what its own config says, and nobody has looked at this one yet.",
        ));
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
///
/// The six largest, scaled to fill the bar between them. The cut used to be
/// made where the list was built, which is also the list the `lang:` filter
/// reads; it belongs here, where the reason for it is.
fn lang_bar(info: &RepoInfo) -> gtk::DrawingArea {
    let area = gtk::DrawingArea::builder().content_height(5).hexpand(true).build();
    let langs: Vec<_> = info.langs.iter().take(6).cloned().collect();
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
            // When the branch is behind, the age of the local commit on its own
            // reads as "nothing has happened here lately", which can be the
            // opposite of the truth.
            let age = match (info.behind > 0, info.upstream_when) {
                (true, Some(upstream)) => format!(
                    "{} · remote {}",
                    relative_time(commit.when),
                    relative_time(upstream)
                ),
                _ => relative_time(commit.when),
            };
            let meta = gtk::Label::builder()
                .label(format!("{} · {} · {}", age, commit.short_id, commit.author))
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
