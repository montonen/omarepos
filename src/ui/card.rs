//! One repository, as a card.
//!
//! A healthy repository's card is grey. Colour is spent on two things only:
//! work that is not finished (yellow for what is uncommitted, the accent for
//! commits that exist nowhere else) and trouble (red). So across forty cards
//! whatever has a colour is something to do, and "clean" and "level" are not
//! said in green on every one of them. The one exception is the dot on a
//! tag, which is the tag's own colour and the same one its chip has in the
//! header.
//!
//! Every fact has a slot of its own, and every slot is built whether or not
//! there is anything to put in it: the cards are all one height, and where a
//! thing is says what it is. Top to bottom, with a narrow gutter on the left
//! that holds marks and no words:
//!
//! 1. the name, and beside it the flag, for the one worst trouble;
//! 2. where it is: the folder it lies in and the remote it belongs to, with
//!    the name left out of both, having just been said;
//! 3. the branch, and across from it what is uncommitted;
//! 4. what exists only here, and across from it what is waiting to come in;
//! 5. the last commit;
//! 6. four weeks of commits a day, which give way to the two commits before
//!    the last on the card that has the keyboard;
//! 7. tags.
//!
//! It used to be a column of pills that wrapped where they ran out of room,
//! which put "ahead" somewhere different on every card, between a path and a
//! remote that each spelled the name out again, over a bar of the languages
//! in use.

use std::rc::Rc;

use gtk::pango;
use gtk::prelude::*;

use crate::config::tildify;
use crate::git::{ACTIVITY_DAYS, Activity, Commit, RepoInfo};
use crate::ui::{Action, relative_time};

/// Everything about a card that does not come from the repository itself.
pub struct Context<'a> {
    pub tags: &'a [String],
    pub selected: bool,
    /// What git said when the last fetch of it failed. Otherwise the failure
    /// only reaches stderr, which under a desktop launcher nobody reads.
    pub fetch_failed: Option<&'a str>,
    /// Found since the user last looked. Nothing unattended fetches it until
    /// it has been opened, or fetched by hand.
    pub unseen: bool,
    /// The first card on the grid, which is the one Enter opens from the
    /// filter. It shows what the card with the keyboard shows.
    pub top_hit: bool,
}

/// Build the card widget. `on_action` is invoked with the repo the user acted
/// on; `on_select` with the new state of the card's selection checkbox.
pub fn build(
    info: &RepoInfo,
    ctx: &Context<'_>,
    on_action: Rc<dyn Fn(Action, &RepoInfo)>,
    on_select: Rc<dyn Fn(&RepoInfo, bool)>,
) -> gtk::Widget {
    let Context { tags, selected, fetch_failed, unseen, top_hit } = *ctx;
    let now = now();
    let card = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .width_request(320)
        .build();
    card.add_css_class("om-card");
    if let Some(edge) = edge(info) {
        card.add_css_class(edge.class());
    }
    if selected {
        card.add_css_class("om-selected");
    }
    if top_hit {
        card.add_css_class("om-top-hit");
    }
    // One width for the checkbox and for every mark under it, so that all
    // the words on a card start on one line down its left.
    let gutter = gtk::SizeGroup::new(gtk::SizeGroupMode::Horizontal);

    // 1. Checkbox, name, flag, menu.
    let title = gtk::Box::new(gtk::Orientation::Horizontal, GAP);
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
    gutter.add_widget(&check);
    title.append(&check);
    let name = gtk::Label::builder()
        .label(&info.name)
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(pango::EllipsizeMode::End)
        .max_width_chars(24)
        .build();
    name.add_css_class("om-name");
    title.append(&name);
    let flagged = flag(info, fetch_failed);
    if let Some(flag) = &flagged {
        let label = gtk::Label::new(Some(&flag.says));
        label.add_css_class("om-flag");
        label.set_valign(gtk::Align::Center);
        label.set_tooltip_text(Some(&flag.why));
        title.append(&label);
    }
    title.append(&action_menu(info, Rc::clone(&on_action)));
    card.append(&title);

    // 2. Where it is.
    let (folder, remote) = address(info);
    let place = row(&gutter, None);
    place.append(&text(&folder, "om-where", pango::EllipsizeMode::Start, 12));
    if let Some(remote) = &remote {
        place.append(&text("·", "om-where", pango::EllipsizeMode::None, 0));
        let remote = text(remote, "om-where", pango::EllipsizeMode::Middle, ROOM - 12 - 2);
        remote.set_hexpand(true);
        place.append(&remote);
    }
    place.set_tooltip_text(Some(&whereabouts(info)));
    card.append(&place);

    // 3. The branch, and what is uncommitted.
    let state = if let Some(err) = &info.error {
        // Nothing else is known of a repository that could not be read, and
        // why is the one thing worth the room.
        let state = row(&gutter, None);
        let why = text(first_line(err), "om-where", pango::EllipsizeMode::End, ROOM);
        why.set_hexpand(true);
        why.set_tooltip_text(Some(err));
        state.append(&why);
        state
    } else {
        branch_row(info, &gutter)
    };
    state.set_margin_top(8);
    card.append(&state);

    // 4. What is only here, and what is waiting.
    card.append(&sync_row(info, fetch_failed, &gutter));

    let rule = gtk::Separator::new(gtk::Orientation::Horizontal);
    rule.add_css_class("om-rule");
    rule.set_margin_top(8);
    rule.set_margin_bottom(7);
    card.append(&rule);

    // 5. The last commit.
    let recent = &info.activity.recent;
    card.append(&match recent.first().or(info.last_commit.as_ref()) {
        Some(commit) => commit_row(commit, now, &gutter),
        None => {
            let none = row(&gutter, None);
            let says = if info.error.is_some() { "could not be read" } else { "no commits yet" };
            none.append(&text(says, "om-where", pango::EllipsizeMode::None, 0));
            none
        }
    });

    // 6. The four weeks before now, or the two commits before that one.
    let lately = gtk::Box::new(gtk::Orientation::Vertical, 0);
    lately.add_css_class("om-less");
    let drawn = row(&gutter, None);
    drawn.set_height_request(PULSE_HEIGHT);
    drawn.append(&pulse(&info.activity));
    lately.append(&drawn);
    let (says, beside) = caption(info, now);
    let under = row(&gutter, None);
    under.set_height_request(CAPTION_HEIGHT);
    let says = text(&says, "om-caption", pango::EllipsizeMode::End, room_beside(&[&beside]));
    says.set_hexpand(true);
    under.append(&says);
    under.append(&text(&beside, "om-caption", pango::EllipsizeMode::None, 0));
    lately.append(&under);

    let before = gtk::Box::new(gtk::Orientation::Vertical, 0);
    before.add_css_class("om-more");
    // Never in the pointer's way: it lies over the drawing when it is not
    // to be seen, and would take the drawing's tooltip.
    before.set_can_target(false);
    // Why, where there is a why: the words that were in a tooltip, on the
    // one card that is being looked at.
    let reason = flagged.as_ref().map(|flag| flag.why.clone());
    before.append(&match recent.get(1) {
        Some(commit) => commit_row(commit, now, &gutter),
        None => row(&gutter, None),
    });
    before.append(&match (&reason, recent.get(2)) {
        (Some(reason), _) => {
            let why = row(&gutter, None);
            let says = text(&one_line(reason), "om-caption", pango::EllipsizeMode::End, ROOM);
            says.set_hexpand(true);
            why.append(&says);
            why
        }
        (None, Some(commit)) => commit_row(commit, now, &gutter),
        (None, None) => row(&gutter, None),
    });

    // Both are there all the time, one over the other, and the stylesheet
    // shows the one or the other: the second on the card the keyboard is on,
    // and on the one under a pointer that has been moved. Nothing is built
    // or measured as either moves, so nothing on the grid moves with them.
    let zone = gtk::Overlay::new();
    zone.set_child(Some(&lately));
    zone.add_overlay(&before);
    zone.set_measure_overlay(&before, true);
    zone.set_tooltip_text(Some(&history(info)));
    card.append(&zone);

    // 7. Tags, and on the card with the keyboard, what its keys are.
    let foot = row(&gutter, None);
    foot.set_margin_top(8);
    foot.set_height_request(FOOT_HEIGHT);
    if unseen {
        let new = gtk::Label::new(Some("new"));
        new.add_css_class("om-tag");
        new.add_css_class("new");
        new.set_valign(gtk::Align::Center);
        new.set_tooltip_text(Some(
            "Found since you last looked. The refresh across the fleet leaves it alone until \
             you have opened it, or pressed f on it: running git in a repository means \
             doing what its own config says, and nobody has looked at this one yet.",
        ));
        foot.append(&new);
    }
    foot.append(&tag_row(tags));
    // Over the end of the row, and asking for no room there: they are on
    // one card at a time, and a column must not be the wider for them.
    let under_keys = gtk::Overlay::new();
    under_keys.set_child(Some(&foot));
    for keys in keys() {
        under_keys.add_overlay(&keys);
    }
    card.append(&under_keys);

    card.upcast()
}


fn action_menu(info: &RepoInfo, on_action: Rc<dyn Fn(Action, &RepoInfo)>) -> gtk::MenuButton {
    let popover = gtk::Popover::builder().has_arrow(false).build();
    let list = gtk::Box::new(gtk::Orientation::Vertical, 2);

    // Offered only where there is a page to open, and saying where it is:
    // the site by name, the address itself in the tooltip.
    let on_the_web = info
        .web
        .as_deref()
        .map(|page| format!("Open on {}", crate::remote_url::web_site(page)));
    let mut items = vec![
        ("Commit graph", Action::CommitGraph),
        ("Contributions", Action::Contributions),
        ("Tags…", Action::EditTags),
        ("Open in terminal", Action::OpenTerminal),
        ("Open in editor", Action::OpenEditor),
        ("Open folder", Action::OpenFolder),
    ];
    if let Some(label) = &on_the_web {
        items.push((label.as_str(), Action::OpenWeb));
    }
    items.push(("Copy path", Action::CopyPath));
    for (label, action) in items {
        let button = gtk::Button::builder()
            .label(label)
            .has_frame(false)
            .build();
        if let Some(child) = button.child().and_downcast::<gtk::Label>() {
            child.set_xalign(0.0);
        }
        if matches!(action, Action::OpenWeb) {
            button.set_tooltip_text(info.web.as_deref());
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


/// Between the gutter and the words, and between the parts of a row.
const GAP: i32 = 8;
/// A row of words. Every row asks for this much whether or not it has any,
/// which is what keeps one card as tall as the next.
const ROW_HEIGHT: i32 = 18;
/// The drawing and the line under it: together as tall as the two rows that
/// take their place on the card with the keyboard.
const PULSE_HEIGHT: i32 = 21;
const CAPTION_HEIGHT: i32 = 15;
const FOOT_HEIGHT: i32 = 18;
/// The most of a branch name that is asked for, in characters. Past that
/// the middle gives way, and the whole name is in the tooltip.
const BRANCH_CHARS: i32 = 26;
/// The most of an author's name that is asked for.
const WHO_CHARS: i32 = 10;

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_secs() as i64)
        .unwrap_or(0)
}

/// A row of the card: the gutter's cell, with `mark` in it or nothing, and
/// then whatever the caller appends.
fn row(gutter: &gtk::SizeGroup, mark: Option<&gtk::Widget>) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, GAP);
    row.set_height_request(ROW_HEIGHT);
    let cell = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    if let Some(mark) = mark {
        mark.set_hexpand(true);
        mark.set_halign(gtk::Align::Center);
        mark.set_valign(gtk::Align::Center);
        cell.append(mark);
    }
    // Said outright. The mark wants the middle of the cell, and a widget
    // that wants room hands the wish to everything it is in: left to that,
    // the gutter took half the row.
    cell.set_hexpand(false);
    gutter.add_widget(&cell);
    row.append(&cell);
    row
}

/// How many characters a row has room for at the width a card asks for,
/// beside the gutter. A row asks for no more than this in all, however much
/// it has to say. Counted at the widest the card's text comes, eight pixels
/// a character, so that the rows in smaller print ask for a little less
/// than they might and none asks for more.
const ROOM: i32 = 32;

/// What is left of a row for the one part that gives way, when the parts
/// that do not have been given theirs. Never less than a few characters:
/// then the card is simply wider than it asked to be.
fn room_beside(fixed: &[&str]) -> i32 {
    let taken: usize = fixed
        .iter()
        .filter(|part| !part.is_empty())
        .map(|part| part.chars().count() + 1)
        .sum();
    (ROOM - taken as i32).max(8)
}

/// One line of text in one of the card's styles. `chars` caps how wide it
/// asks to be, and zero leaves that to the text.
///
/// The cap is on what it asks for, not on what it gets: the grid makes its
/// columns as wide as the widest card asks to be, so one long subject left
/// uncapped is one card to a row. A label that may grow is given the room
/// that is left over when the card is wider than it asked.
fn text(says: &str, class: &str, ellipsize: pango::EllipsizeMode, chars: i32) -> gtk::Label {
    let label = gtk::Label::builder()
        .label(says)
        .xalign(0.0)
        .single_line_mode(true)
        .ellipsize(ellipsize)
        .build();
    if chars > 0 {
        label.set_max_width_chars(chars);
    }
    label.add_css_class(class);
    label
}

fn arrow(glyph: &str, class: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(glyph));
    label.add_css_class("om-arrow");
    if !class.is_empty() {
        label.add_css_class(class);
    }
    label
}

fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or_default().trim()
}

/// What git said, on one line: its lines run together, since a row holds one.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The folder a repository lies in and the remote it belongs to, each
/// without the repository's own name. The name is on the line above. It
/// used to be on three: `billing-api`, `~/work/acme/billing-api`,
/// `github.com/acme/billing-api`.
///
/// A remote called something else is given whole, since the difference is
/// then the thing to see.
fn address(info: &RepoInfo) -> (String, Option<String>) {
    let folder = match info.path.parent() {
        Some(parent) => {
            let mut folder = tildify(parent);
            if !folder.ends_with('/') {
                folder.push('/');
            }
            folder
        }
        None => tildify(&info.path),
    };
    let remote = info.remote.as_ref().map(|remote| match remote.rsplit_once('/') {
        Some((owner, last)) if last == info.name => format!("{owner}/"),
        _ => remote.clone(),
    });
    (folder, remote)
}

/// The tooltip of the address: everything the line leaves out.
fn whereabouts(info: &RepoInfo) -> String {
    let mut tip = info.path.display().to_string();
    if let Some(remote) = &info.remote {
        tip.push_str(&format!("\n{remote}"));
        if let Some(page) = &info.web {
            tip.push_str(&format!("\no opens {page}"));
        }
    }
    // "Commits as", this used to say, of a reading taken without asking git.
    // It is the configuration, so it is called that; who a commit is recorded
    // as is git's to say, and `git var GIT_AUTHOR_IDENT` in the repository is
    // how to ask it.
    if let Some(identity) = &info.identity {
        tip.push_str(&format!("\n\nConfigured to commit as {identity}"));
        match &info.identity_doubt {
            Some(doubt) => tip.push_str(&format!(
                "\nBut git may record someone else: {doubt}. \
                 `git var GIT_AUTHOR_IDENT` there says who."
            )),
            None => tip.push_str(
                "\n(user.name and user.email, read without running git.)",
            ),
        }
    }
    // The bar of languages went from the card. What it said is still worth
    // a line to whoever asks, and `lang:` in the filter still finds by it.
    if !info.langs.is_empty() {
        let mix: Vec<String> = info
            .langs
            .iter()
            .take(4)
            .map(|lang| format!("{} {:.0}%", lang.name, (lang.share * 100.0).round()))
            .collect();
        tip.push_str(&format!("\n\n{}", mix.join(" · ")));
    }
    tip.push_str("\n\nClick the card for its commit graph.");
    tip
}

/// The branch, and across from it the state of the working tree.
fn branch_row(info: &RepoInfo, gutter: &gtk::SizeGroup) -> gtk::Box {
    let mark = glyph(info.detached);
    let row = row(gutter, Some(mark.upcast_ref()));

    // The branch is the part that gives way, so it is told how much room
    // the rest of the row has already spoken for.
    let strays = match info.stray_branches {
        0 => String::new(),
        1 => "+1 branch".to_string(),
        n => format!("+{n} branches"),
    };
    let tree_says = tree_slot(info);
    let tree_width: Vec<&str> = tree_says.iter().map(|(says, _, _)| says.as_str()).collect();
    let detached = if info.detached { "detached" } else { "" };
    let room = room_beside(&[&strays, &tree_width.join(" "), detached]).min(BRANCH_CHARS);
    let branch = text(&info.head, "om-branch", pango::EllipsizeMode::Middle, room);
    if info.detached {
        branch.add_css_class("detached");
        branch.set_tooltip_text(Some(&format!("{}\nDetached HEAD", info.head)));
    } else if let Some(upstream) = &info.upstream {
        branch.set_tooltip_text(Some(&format!("{}\ntracks {upstream}", info.head)));
    } else {
        branch.set_tooltip_text(Some(&format!("{}\nno upstream", info.head)));
    }
    // Asked for at its cap and given whatever the row has left: on a card
    // wider than it asked to be, more of a long name shows.
    branch.set_hexpand(true);
    row.append(&branch);
    if info.detached {
        row.append(&text("detached", "om-where", pango::EllipsizeMode::None, 0));
    }
    if info.stray_branches > 0 {
        let more = text(&strays, "om-out", pango::EllipsizeMode::None, 0);
        more.set_tooltip_text(Some(&format!(
            "{} other local {} commits that are on no remote.",
            info.stray_branches,
            if info.stray_branches == 1 { "branch holds" } else { "branches hold" }
        )));
        row.append(&more);
    }

    let tree = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    for (says, class, why) in tree_says {
        let label = text(&says, "om-tree", pango::EllipsizeMode::None, 0);
        label.add_css_class(class);
        if let Some(why) = why {
            label.set_tooltip_text(Some(&why));
        }
        tree.append(&label);
    }
    row.append(&tree);
    row
}

/// What the working tree's slot says, part by part: the words, which of the
/// slot's three styles they are in (`work`, `bad`, or `word` for what is
/// merely so), and the tooltip.
fn tree_slot(info: &RepoInfo) -> Vec<(String, &'static str, Option<String>)> {
    let t = &info.tree;
    let mut parts = Vec::new();
    if info.bare {
        parts.push(("bare".to_string(), "word", None));
    } else if let Some(why) = &t.unreadable {
        parts.push((
            "unknown".to_string(),
            "bad",
            Some(format!(
                "What is uncommitted here is not known: the index could not be read.\n{why}\n\
                 git itself may read it without trouble; `git status` in a terminal will say."
            )),
        ));
    } else {
        let counts = counts(info);
        if !counts.is_empty() {
            parts.push((
                counts,
                "work",
                Some(format!(
                    "{} staged · {} modified · {} untracked",
                    t.staged, t.unstaged, t.untracked
                )),
            ));
        }
        if t.conflicted > 0 {
            parts.push((format!("!{}", t.conflicted), "bad", Some(conflicts(t.conflicted).1)));
        }
        // Said, and quietly: it is what most cards have to say, and it is
        // nothing to do. The repository with no commits says so two rows
        // down, and "clean" beside that would be one word too many.
        if t.is_clean() && !info.unborn {
            parts.push(("clean".to_string(), "word", None));
        }
    }
    // Stashes not counted say nothing, as none do: the card has never had
    // a word for them that was not a number.
    if let Some(count) = info.stashes.filter(|count| *count > 0) {
        let lead = if parts.is_empty() { "" } else { "· " };
        let stashes = if count == 1 { "stash" } else { "stashes" };
        parts.push((format!("{lead}{count} {stashes}"), "word", None));
    }
    parts
}

/// What is uncommitted, in git's short notation: staged, modified, untracked.
fn counts(info: &RepoInfo) -> String {
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
    parts.join(" ")
}

/// "1 commit", "3 commits": the tooltips used to say "1 commits".
fn commits(n: usize) -> String {
    format!("{n} commit{}", if n == 1 { "" } else { "s" })
}

fn conflicts(n: usize) -> (String, String) {
    if n == 1 {
        ("1 conflict".into(), "1 file is in conflict: a merge or a rebase is under way.".into())
    } else {
        (
            format!("{n} conflicts"),
            format!("{n} files are in conflict: a merge or a rebase is under way."),
        )
    }
}

/// What exists only here, as the card says it and as its tooltip explains it.
/// "Ahead" is measured against the branch's upstream and says nothing when
/// there is none, so commits that are on no remote at all are counted too.
fn outgoing(info: &RepoInfo) -> Option<(String, String)> {
    let mut says = Vec::new();
    let mut why = Vec::new();
    if info.ahead > 0 {
        says.push(format!("{} ahead", info.ahead));
        why.push(format!(
            "{} here {} not on {}",
            commits(info.ahead),
            if info.ahead == 1 { "is" } else { "are" },
            info.upstream.as_deref().unwrap_or("the upstream")
        ));
    }
    if info.unpushed > info.ahead {
        let count = if info.unpushed >= crate::git::UNPUSHED_CAP {
            format!("{}+", crate::git::UNPUSHED_CAP)
        } else {
            info.unpushed.to_string()
        };
        if info.remote.is_none() {
            says.push("no remote".to_string());
            why.push(
                "This repository has no remote. None of its commits exist anywhere else."
                    .to_string(),
            );
        } else if info.detached {
            says.push(format!("{count} unpublished"));
            why.push(format!("HEAD is detached, and {count} commits behind it are on no remote."));
        } else {
            says.push(format!("{count} unpublished"));
            why.push(format!(
                "{count} commits on {} are on no remote: the branch was never pushed, \
                 or its upstream is gone.",
                info.head
            ));
        }
    }
    (!says.is_empty()).then(|| (says.join(" · "), why.join("\n")))
}

/// What exists only here on the left, what is waiting to come in on the
/// right. Empty when the two are level, which is most of the time.
fn sync_row(info: &RepoInfo, fetch_failed: Option<&str>, gutter: &gtk::SizeGroup) -> gtk::Box {
    let out = if info.error.is_some() { None } else { outgoing(info) };
    let mark = out.as_ref().map(|_| arrow("↑", "out"));
    let row = row(gutter, mark.as_ref().map(|mark| mark.upcast_ref()));
    let (says, why) = out.unwrap_or_default();
    // What is waiting never gives way. What is only here does, to it.
    let behind = if info.behind > 0 { format!("x {} behind", info.behind) } else { String::new() };
    let unread = if info.history_unreadable.is_some() { "history unreadable" } else { "" };
    let room = room_beside(&[&behind, unread]);
    let left = text(&says, "om-out", pango::EllipsizeMode::End, room);
    left.set_hexpand(true);
    if !why.is_empty() {
        left.set_tooltip_text(Some(&why));
    }
    row.append(&left);

    let incoming = gtk::Box::new(gtk::Orientation::Horizontal, 5);
    let dot = || text("·", "om-caption", pango::EllipsizeMode::None, 0);
    if info.behind > 0 {
        // The arrow goes red when the last fetch failed: it is the one mark
        // on this side, and the count beside it is then as old as the last
        // fetch that worked.
        incoming.append(&arrow("↓", if fetch_failed.is_some() { "bad" } else { "" }));
    }
    if info.behind > 0 {
        let behind = text(&format!("{} behind", info.behind), "om-in", pango::EllipsizeMode::None, 0);
        behind.set_tooltip_text(Some(&format!(
            "{} on {} {} not here yet",
            commits(info.behind),
            info.upstream.as_deref().unwrap_or("the upstream"),
            if info.behind == 1 { "is" } else { "are" }
        )));
        incoming.append(&behind);
    }
    if let Some(why) = &info.history_unreadable {
        if incoming.first_child().is_some() {
            incoming.append(&dot());
        }
        let label = text("history unreadable", "om-bad", pango::EllipsizeMode::None, 0);
        label.set_tooltip_text(Some(&format!(
            "How far this is from its remotes is not known: the commits in between could not \
             be read.\n{why}"
        )));
        incoming.append(&label);
    }
    row.append(&incoming);
    row
}

/// A commit on a row: a mark if it is on no remote, how old it is, what it
/// says, and who wrote it when that was somebody else.
fn commit_row(commit: &Commit, now: i64, gutter: &gtk::SizeGroup) -> gtk::Box {
    let mark = commit.unpushed.then(|| arrow("↑", "out"));
    let row = row(gutter, mark.as_ref().map(|mark| mark.upcast_ref()));
    let age = text(&short_age(now, commit.when), "om-age", pango::EllipsizeMode::None, 0);
    age.set_width_chars(3);
    age.set_xalign(1.0);
    row.append(&age);
    let who = if commit.mine { "" } else { commit.author.as_str() };
    let who_room = who.chars().count().min(WHO_CHARS as usize);
    // The age is three characters and a gap, which comes to four.
    let subject_room = ROOM - 4 - if who.is_empty() { 0 } else { who_room as i32 + 1 };
    let subject = text(&commit.summary, "om-subject", pango::EllipsizeMode::End, subject_room);
    subject.set_hexpand(true);
    row.append(&subject);
    // Your own name is not news. Left out, a name on a row means somebody
    // else, and where the repository is set up to commit as someone you are
    // not, your own name turns up here and gives that away.
    if !commit.mine {
        row.append(&text(&commit.author, "om-who", pango::EllipsizeMode::End, WHO_CHARS));
    }
    row.set_tooltip_text(Some(&told(commit)));
    row
}

/// A commit in full, for a tooltip.
fn told(commit: &Commit) -> String {
    format!(
        "{} · {} · {}{}\n{}",
        commit.short_id,
        commit.author,
        relative_time(commit.when),
        if commit.unpushed { " · on no remote" } else { "" },
        commit.summary
    )
}

/// How long ago, in a column three characters wide: `11m`, `2h`, `1d`,
/// `3w`, `5mo`, `2y`.
fn short_age(now: i64, when: i64) -> String {
    const MINUTE: i64 = 60;
    const HOUR: i64 = 60 * MINUTE;
    const DAY: i64 = 24 * HOUR;
    // Saturating: a commit can carry any date at all.
    match now.saturating_sub(when) {
        d if d < MINUTE => "now".to_string(),
        d if d < HOUR => format!("{}m", d / MINUTE),
        d if d < DAY => format!("{}h", d / HOUR),
        d if d < 14 * DAY => format!("{}d", d / DAY),
        d if d < 60 * DAY => format!("{}w", d / (7 * DAY)),
        d if d < 365 * DAY => format!("{}mo", d / (30 * DAY)),
        d => format!("{}y", d / (365 * DAY)),
    }
}

/// How long, in words, for "quiet for 3 weeks".
fn spell(seconds: i64) -> String {
    const DAY: i64 = 24 * 60 * 60;
    let plural = |n: i64, unit: &str| format!("{n} {unit}{}", if n == 1 { "" } else { "s" });
    match seconds {
        d if d < 14 * DAY => plural((d / DAY).max(1), "day"),
        d if d < 60 * DAY => plural(d / (7 * DAY), "week"),
        d if d < 730 * DAY => plural(d / (30 * DAY), "month"),
        d => plural(d / (365 * DAY), "year"),
    }
}

/// The line under the drawing: how busy the last week was, or for how long
/// nothing has happened; and beside it, when the repository is behind, how
/// fresh the remote's newest commit is.
fn caption(info: &RepoInfo, now: i64) -> (String, String) {
    let activity = &info.activity;
    let newest = info
        .last_commit
        .as_ref()
        .map(|commit| commit.when)
        .into_iter()
        .chain(info.upstream_when)
        .max();
    let says = if activity.week > 0 {
        match activity.people {
            0 | 1 => format!("{} this week", activity.week),
            people => format!("{} this week · {people} people", activity.week),
        }
    } else {
        match newest {
            Some(when) if !activity.days.is_empty() => {
                format!("quiet for {}", spell(now.saturating_sub(when)))
            }
            _ => String::new(),
        }
    };
    let beside = match (info.behind > 0, info.upstream_when) {
        (true, Some(tip)) => match short_age(now, tip).as_str() {
            "now" => "remote tip just now".to_string(),
            age => format!("remote tip {age} ago"),
        },
        _ => String::new(),
    };
    (says, beside)
}

/// The tooltip of the drawing: the same days as numbers, and the commits the
/// card with the keyboard lists.
fn history(info: &RepoInfo) -> String {
    let activity = &info.activity;
    let mut tip = String::new();
    for commit in &activity.recent {
        tip.push_str(&told(commit));
        tip.push_str("\n\n");
    }
    if activity.days.is_empty() {
        tip.push_str("What has been committed lately is not known.");
        return tip;
    }
    let column = |index: usize| {
        activity.days.iter().map(|day| day[index].to_string()).collect::<Vec<_>>().join(" ")
    };
    tip.push_str(&format!(
        "Commits a day over the last {ACTIVITY_DAYS} days, today last.\n"
    ));
    if activity.split {
        tip.push_str(&format!("yours   {}\nothers  {}", column(0), column(1)));
    } else {
        tip.push_str(&column(1));
        tip.push_str("\nWhose they are is not told apart: who this repository commits as is in doubt.");
    }
    if activity.reached < activity.days.len() {
        tip.push_str(&format!(
            "\nOnly the last {} days were read: the rest is not known, which is not quiet.",
            activity.reached
        ));
    }
    tip
}

/// As many commits a day as fill a column. One scale for every card, so
/// that a tall column means the same on each of them: scaled to the card,
/// one commit in a quiet repository stood as tall as thirty in a busy one.
const FULL_DAY: u32 = 6;

/// Four weeks of commits a day, today at the right: yours from the baseline
/// up, other people's on top of those and fainter.
///
/// It is drawn in the widget's own colour, which the stylesheet gives it, and
/// asked for when it draws. A change of theme reloads the stylesheet, and the
/// drawing is then in the new theme's colours with nothing here to tell.
fn pulse(activity: &Activity) -> gtk::DrawingArea {
    let area = gtk::DrawingArea::builder()
        .content_height(PULSE_HEIGHT)
        .hexpand(true)
        .build();
    area.add_css_class("om-pulse");
    let days = activity.days.clone();
    let reached = activity.reached;
    let split = activity.split;
    area.set_draw_func(move |area, cr, width, height| {
        if days.is_empty() {
            return;
        }
        let ink = area.color();
        let paint = |alpha: f64| {
            cr.set_source_rgba(
                ink.red() as f64,
                ink.green() as f64,
                ink.blue() as f64,
                ink.alpha() as f64 * alpha,
            );
        };
        // Whole pixels throughout: a column a fraction of a pixel wide is a
        // blur, and there are twenty-eight of them.
        let (gap, week_gap) = (2.0, 4.0);
        let weeks = (days.len() as f64 / 7.0).ceil();
        let gaps = (days.len() as f64 - weeks) * gap + (weeks - 1.0) * week_gap;
        let column = ((width as f64 - gaps) / days.len() as f64).floor().max(1.0);
        let floor = height as f64 - 1.0;
        let unit = (floor - 1.0) / FULL_DAY as f64;
        let unread = days.len().saturating_sub(reached);
        let mut x = 0.0;
        for (index, day) in days.iter().enumerate() {
            if index > 0 {
                x += if index % 7 == 0 { week_gap } else { gap };
            }
            // No baseline under a day the walk never reached: that would
            // draw "not known" as "nothing happened". What it did find there
            // is drawn all the same, since a commit written long before it
            // was committed is counted on the day it was written.
            if index >= unread {
                paint(0.25);
                let reach = if index % 7 == 6 || index == days.len() - 1 { column } else { column + gap };
                cr.rectangle(x, floor, reach, 1.0);
                let _ = cr.fill();
            }
            let [mine, others] = if split { *day } else { [day[0] + day[1], 0] };
            let tall = |n: u32| (n.min(FULL_DAY) as f64 * unit).round();
            let (low, all) = (tall(mine), tall(mine.saturating_add(others)));
            if low > 0.0 {
                paint(1.0);
                cr.rectangle(x, floor - low, column, low);
                let _ = cr.fill();
            }
            if all > low {
                paint(0.45);
                cr.rectangle(x, floor - all, column, all - low);
                let _ = cr.fill();
            }
            x += column;
        }
    });
    area
}

/// The mark in front of the branch: a branch, or a ring for a HEAD that is
/// on no branch. Drawn, and in the widget's own colour like the pulse: the
/// glyph this used to be is in some fonts and not in others.
fn glyph(detached: bool) -> gtk::DrawingArea {
    let area = gtk::DrawingArea::builder().content_width(12).content_height(12).build();
    area.add_css_class("om-glyph");
    area.set_draw_func(move |area, cr, _, _| {
        let ink = area.color();
        cr.set_source_rgba(ink.red() as f64, ink.green() as f64, ink.blue() as f64, ink.alpha() as f64);
        cr.set_line_width(1.2);
        let turn = std::f64::consts::TAU;
        if detached {
            cr.arc(6.0, 6.0, 3.2, 0.0, turn);
            let _ = cr.stroke();
            return;
        }
        for (x, y) in [(3.0, 2.5), (3.0, 9.5), (9.0, 4.0)] {
            cr.new_sub_path();
            cr.arc(x, y, 1.5, 0.0, turn);
        }
        let _ = cr.stroke();
        cr.move_to(3.0, 4.0);
        cr.line_to(3.0, 8.0);
        cr.move_to(9.0, 5.5);
        cr.curve_to(9.0, 8.0, 3.0, 6.5, 3.0, 8.0);
        let _ = cr.stroke();
    });
    area
}

fn tag_row(tags: &[String]) -> gtk::Box {
    const SHOWN: usize = 4;

    let row = gtk::Box::new(gtk::Orientation::Horizontal, 5);
    row.set_halign(gtk::Align::Start);
    for tag in tags.iter().take(SHOWN) {
        // The tag's own colour, as a dot, the same as on its chip in the
        // header: a tag is one colour wherever it is shown, and the chip to
        // press up there is found by the colour seen down here. It is the
        // one colour on a card that is not about the state of anything.
        let chip = gtk::Box::new(gtk::Orientation::Horizontal, 5);
        chip.add_css_class("om-tag");
        chip.set_valign(gtk::Align::Center);
        chip.append(&crate::ui::color_dot(crate::ui::key_color(&tag.to_lowercase())));
        chip.append(
            &gtk::Label::builder()
                .label(tag)
                .ellipsize(pango::EllipsizeMode::End)
                .max_width_chars(10)
                .build(),
        );
        row.append(&chip);
    }
    if tags.len() > SHOWN {
        let more = gtk::Label::new(Some(&format!("+{}", tags.len() - SHOWN)));
        more.add_css_class("om-tag");
        more.set_valign(gtk::Align::Center);
        more.set_tooltip_text(Some(&tags[SHOWN..].join(", ")));
        row.append(&more);
    }
    if !tags.is_empty() {
        row.set_tooltip_text(Some(&tags.join(", ")));
    }
    row
}

/// What the keyboard does to this card, as keycaps: the letters that act on
/// a card when it has the keyboard, and Enter on the one the filter would
/// open. Neither is to be seen until the stylesheet says so; see where the
/// card's sixth row is put together.
fn keys() -> [gtk::Box; 2] {
    let keycap = |says: &str| {
        let key = gtk::Label::new(Some(says));
        key.add_css_class("om-key");
        key
    };
    let group = |class: &str| {
        let group = gtk::Box::new(gtk::Orientation::Horizontal, 3);
        group.add_css_class(class);
        group.set_halign(gtk::Align::End);
        group.set_valign(gtk::Align::End);
        group.set_can_target(false);
        group
    };
    let letters = group("om-keys");
    for letter in ["l", "e", "o", "p", "f"] {
        letters.append(&keycap(letter));
    }
    let enter = group("om-enter");
    enter.append(&keycap("Enter"));
    [letters, enter]
}

/// The loudest thing a card says: the one solid patch of colour on it,
/// beside the name.
struct Flag {
    says: String,
    why: String,
}

/// It goes to trouble and to nothing else, and to one trouble at a time:
/// the first of a repository that cannot be read, files in conflict, a
/// working tree whose state is not known, and a fetch that failed. Work that
/// is merely unfinished is not trouble, and has the card's left edge.
fn flag(info: &RepoInfo, fetch_failed: Option<&str>) -> Option<Flag> {
    if let Some(err) = &info.error {
        return Some(Flag { says: "unreadable".into(), why: err.clone() });
    }
    if info.tree.conflicted > 0 {
        let (says, why) = conflicts(info.tree.conflicted);
        return Some(Flag { says, why });
    }
    if let Some(why) = &info.tree.unreadable {
        return Some(Flag {
            says: "status unreadable".into(),
            why: format!("What is uncommitted here is not known: the index could not be read.\n{why}"),
        });
    }
    if let Some(err) = fetch_failed {
        return Some(Flag {
            says: "fetch failed".into(),
            why: format!("The last refresh could not fetch this repository:\n{err}"),
        });
    }
    None
}

/// The colour down a card's left edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Edge {
    /// Something is wrong with the repository itself.
    Trouble,
    /// There is uncommitted work in it.
    Work,
    /// It has commits that exist nowhere else.
    Out,
}

impl Edge {
    fn class(self) -> &'static str {
        match self {
            Edge::Trouble => "om-edge-trouble",
            Edge::Work => "om-edge-work",
            Edge::Out => "om-edge-out",
        }
    }
}

/// The edge says where your work is: lost track of, uncommitted, or
/// committed and nowhere but here. A fetch that failed is not among them.
/// It has the flag, and it is about the remote: a day without a network
/// must not paint over the edge of every card and hide which ones have
/// something in them.
fn edge(info: &RepoInfo) -> Option<Edge> {
    if info.error.is_some() || info.tree.conflicted > 0 || info.tree.unreadable.is_some() {
        Some(Edge::Trouble)
    } else if !info.tree.is_clean() {
        Some(Edge::Work)
    } else if info.ahead > 0 || info.unpushed > 0 || info.stray_branches > 0 {
        Some(Edge::Out)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::*;
    use std::path::PathBuf;

    const HOUR: i64 = 60 * 60;
    const DAY: i64 = 24 * HOUR;

    /// A repository as a scan reads it, to be bent into the state under test:
    /// one commit on `main`, nothing uncommitted, no remote.
    fn repo() -> RepoInfo {
        let dir = scratch("card-repo");
        git(&dir, &["init", "-q", "-b", "main"]);
        commit_file(&dir, "a.txt", "a\n", "one");
        let mut info = crate::git::inspect(&dir);
        assert_eq!(info.error, None);
        // No remote means every commit is "only here". Most tests are about
        // something else, and start from a repository that is level.
        info.remote = Some("git.example.org/acme/billing-api".into());
        info.unpushed = 0;
        info.name = "billing-api".into();
        info.path = home().join("work/acme/billing-api");
        info
    }

    fn home() -> PathBuf {
        PathBuf::from(std::env::var_os("HOME").expect("the test kit sets a home"))
    }

    fn commit(when: i64) -> Commit {
        Commit {
            summary: "Round totals per line".into(),
            author: "Mira Holm".into(),
            when,
            short_id: "c20ddc9".into(),
            mine: true,
            unpushed: false,
        }
    }

    #[test]
    fn the_name_is_said_once_and_not_again_in_the_address() {
        let _guard = isolate();
        let mut info = repo();
        assert_eq!(
            address(&info),
            ("~/work/acme/".to_string(), Some("git.example.org/acme/".to_string()))
        );

        // A remote called something else is the thing to see, so it is whole.
        info.remote = Some("git.example.org/acme/invoicing".into());
        assert_eq!(address(&info).1.as_deref(), Some("git.example.org/acme/invoicing"));

        // A name that only ends the same way is not the same name.
        info.remote = Some("git.example.org/acme/old-billing-api".into());
        assert_eq!(address(&info).1.as_deref(), Some("git.example.org/acme/old-billing-api"));

        info.remote = None;
        assert_eq!(address(&info), ("~/work/acme/".to_string(), None));

        // Whatever the line leaves out is in its tooltip, the name included.
        info.remote = Some("git.example.org/acme/billing-api".into());
        info.web = Some("https://git.example.org/acme/billing-api".into());
        let tip = whereabouts(&info);
        assert!(tip.starts_with(&info.path.display().to_string()), "{tip}");
        assert!(tip.contains("git.example.org/acme/billing-api\no opens https://"), "{tip}");
    }

    #[test]
    fn an_age_fits_a_column_three_characters_wide() {
        let now = 2_000_000_000;
        for (ago, says) in [
            (0, "now"),
            (59, "now"),
            (60, "1m"),
            (11 * 60, "11m"),
            (HOUR, "1h"),
            (23 * HOUR + 59 * 60, "23h"),
            (DAY, "1d"),
            (13 * DAY, "13d"),
            (14 * DAY, "2w"),
            (59 * DAY, "8w"),
            (60 * DAY, "2mo"),
            (364 * DAY, "12mo"),
            (365 * DAY, "1y"),
            (3000 * DAY, "8y"),
        ] {
            assert_eq!(short_age(now, now - ago), says, "{ago}s ago");
        }
        // A commit dated ahead of the clock, or at the far end of time.
        assert_eq!(short_age(now, now + DAY), "now");
        assert_eq!(short_age(now, i64::MIN), "292471208677y");
        assert_eq!(short_age(i64::MIN, i64::MAX), "now");
    }

    #[test]
    fn the_line_under_the_drawing_says_how_busy_or_how_long_quiet() {
        let _guard = isolate();
        let now = 2_000_000_000;
        let mut info = repo();
        info.last_commit = Some(commit(now - 11 * 60));
        info.activity.days = vec![[0, 0]; ACTIVITY_DAYS];
        info.activity.reached = ACTIVITY_DAYS;

        info.activity.week = 14;
        info.activity.people = 2;
        assert_eq!(caption(&info, now), ("14 this week · 2 people".to_string(), String::new()));
        // One person is not a crowd worth counting.
        info.activity.people = 1;
        assert_eq!(caption(&info, now).0, "14 this week");

        info.activity.week = 0;
        info.last_commit = Some(commit(now - 22 * DAY));
        assert_eq!(caption(&info, now).0, "quiet for 3 weeks");
        info.last_commit = Some(commit(now - 9 * DAY));
        assert_eq!(caption(&info, now).0, "quiet for 9 days");
        info.last_commit = Some(commit(now - 3 * HOUR));
        assert_eq!(caption(&info, now).0, "quiet for 1 day");
        info.last_commit = Some(commit(now - 400 * DAY));
        assert_eq!(caption(&info, now).0, "quiet for 13 months");

        // Behind: the remote has moved since, and that is the news. Quiet is
        // measured from whichever is newer.
        info.behind = 12;
        info.upstream_when = Some(now - 3 * HOUR);
        assert_eq!(
            caption(&info, now),
            ("quiet for 1 day".to_string(), "remote tip 3h ago".to_string())
        );

        // History that was not read is not said to be quiet.
        info.activity.days.clear();
        assert_eq!(caption(&info, now).0, "");
    }

    #[test]
    fn the_flag_goes_to_the_worst_trouble_and_to_one() {
        let _guard = isolate();
        let fetch = Some("could not resolve host");
        let says = |info: &RepoInfo, failed: Option<&str>| flag(info, failed).map(|flag| flag.says);

        let mut info = repo();
        assert_eq!(says(&info, None), None);
        // Unfinished work is not trouble, and neither is being behind.
        info.tree.unstaged = 3;
        info.ahead = 2;
        info.behind = 5;
        assert_eq!(says(&info, None), None);

        assert_eq!(says(&info, fetch).as_deref(), Some("fetch failed"));
        info.tree.unreadable = Some("bad index".into());
        assert_eq!(says(&info, fetch).as_deref(), Some("status unreadable"));
        info.tree.conflicted = 2;
        assert_eq!(says(&info, fetch).as_deref(), Some("2 conflicts"));
        info.tree.conflicted = 1;
        assert_eq!(says(&info, fetch).as_deref(), Some("1 conflict"));
        info.error = Some("not a repository".into());
        assert_eq!(says(&info, fetch).as_deref(), Some("unreadable"));

        // The flag's tooltip carries what was said, word for word.
        let mut info = repo();
        info.error = None;
        assert!(flag(&info, fetch).unwrap().why.ends_with("could not resolve host"));
    }

    #[test]
    fn the_edge_says_where_the_work_is_and_a_failed_fetch_does_not_take_it() {
        let _guard = isolate();
        let mut info = repo();
        assert_eq!(edge(&info), None);

        info.stray_branches = 1;
        assert_eq!(edge(&info), Some(Edge::Out));
        info.stray_branches = 0;
        info.unpushed = 3;
        assert_eq!(edge(&info), Some(Edge::Out));
        info.unpushed = 0;
        info.ahead = 1;
        assert_eq!(edge(&info), Some(Edge::Out));

        // Uncommitted work outranks commits that are only here.
        info.tree.untracked = 4;
        assert_eq!(edge(&info), Some(Edge::Work));
        info.tree.conflicted = 1;
        assert_eq!(edge(&info), Some(Edge::Trouble));
        info.tree.conflicted = 0;
        info.tree.unreadable = Some("bad index".into());
        assert_eq!(edge(&info), Some(Edge::Trouble));

        // Being behind is the remote's news, not yours.
        let mut info = repo();
        info.behind = 12;
        assert_eq!(edge(&info), None);
        assert_eq!(
            [Edge::Trouble, Edge::Work, Edge::Out].map(Edge::class),
            ["om-edge-trouble", "om-edge-work", "om-edge-out"]
        );
    }

    #[test]
    fn what_is_only_here_is_counted_against_the_upstream_and_against_every_remote() {
        let _guard = isolate();
        let says = |info: &RepoInfo| outgoing(info).map(|(says, _)| says);
        let mut info = repo();
        assert_eq!(says(&info), None);

        info.ahead = 2;
        info.unpushed = 2;
        assert_eq!(says(&info).as_deref(), Some("2 ahead"));
        // More on no remote than ahead of the upstream: other work besides.
        info.unpushed = 5;
        assert_eq!(says(&info).as_deref(), Some("2 ahead · 5 unpublished"));
        info.ahead = 0;
        assert_eq!(says(&info).as_deref(), Some("5 unpublished"));
        info.unpushed = crate::git::UNPUSHED_CAP;
        assert_eq!(says(&info).as_deref(), Some("1000+ unpublished"));
        info.remote = None;
        assert_eq!(says(&info).as_deref(), Some("no remote"));
    }

    #[test]
    fn the_working_trees_slot_says_one_thing_at_a_time_and_clean_quietly() {
        let _guard = isolate();
        let says = |info: &RepoInfo| -> Vec<(String, &'static str)> {
            tree_slot(info).into_iter().map(|(says, class, _)| (says, class)).collect()
        };
        let mut info = repo();
        assert_eq!(says(&info), [("clean".to_string(), "word")]);

        info.stashes = Some(2);
        assert_eq!(says(&info), [("clean".to_string(), "word"), ("· 2 stashes".to_string(), "word")]);
        info.stashes = None;
        assert_eq!(says(&info), [("clean".to_string(), "word")]);
        info.stashes = Some(0);

        info.tree.staged = 1;
        info.tree.unstaged = 3;
        info.tree.untracked = 4;
        assert_eq!(says(&info), [("+1 ~3 ?4".to_string(), "work")]);
        info.tree.conflicted = 2;
        assert_eq!(says(&info), [("+1 ~3 ?4".to_string(), "work"), ("!2".to_string(), "bad")]);

        // What cannot be read is not counted, and is not called clean.
        info.tree.unreadable = Some("bad index".into());
        assert_eq!(says(&info), [("unknown".to_string(), "bad")]);

        let mut info = repo();
        info.bare = true;
        assert_eq!(says(&info), [("bare".to_string(), "word")]);

        // No commits yet is said where the commit would be, not here as well.
        let mut info = repo();
        info.unborn = true;
        assert_eq!(says(&info), []);
        info.stashes = Some(1);
        assert_eq!(says(&info), [("1 stash".to_string(), "word")]);
    }

    #[test]
    fn a_row_asks_for_no_more_than_the_card_has_room_for() {
        assert_eq!(room_beside(&[]), ROOM);
        assert_eq!(room_beside(&["", ""]), ROOM);
        // Each part that does not give way takes its length and a space.
        assert_eq!(room_beside(&["+1 ~3 ?4"]), ROOM - 9);
        assert_eq!(room_beside(&["+2 branches", "clean", ""]), ROOM - 12 - 6);
        // Never nothing: a branch with eight characters to its name is still a name.
        assert_eq!(room_beside(&["a part as long as the row itself, and then some"]), 8);
    }

    #[test]
    fn the_drawings_tooltip_gives_the_days_as_numbers_and_says_what_is_not_known() {
        let _guard = isolate();
        let mut info = repo();
        info.activity = Activity::default();
        assert!(history(&info).contains("is not known"), "{}", history(&info));

        info.activity.days = vec![[0, 0]; ACTIVITY_DAYS];
        info.activity.days[ACTIVITY_DAYS - 1] = [2, 1];
        info.activity.reached = ACTIVITY_DAYS;
        info.activity.split = true;
        info.activity.recent = vec![commit(0)];
        let tip = history(&info);
        assert!(tip.starts_with("c20ddc9 · Mira Holm · "), "{tip}");
        assert!(tip.contains("yours   0 0") && tip.ends_with("0 1"), "{tip}");

        info.activity.split = false;
        info.activity.reached = 9;
        let tip = history(&info);
        assert!(tip.contains("not told apart") && tip.contains("Only the last 9 days"), "{tip}");
    }
}
