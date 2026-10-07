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

    let files = gtk::ListBox::new();
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
        .start_child(&scroller)
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

    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.add_css_class("om-detail");
    root.append(&header);
    root.append(&inner);
    root.append(&totals);

    let generation = Rc::new(RefCell::new(0u64));
    let running: Rc<RefCell<Option<Cancel>>> = Rc::new(RefCell::new(None));
    // Which commit the file list belongs to, so selecting a file knows what to
    // diff, and a patch load of its own that can be abandoned the same way.
    let showing: Rc<RefCell<Option<(PathBuf, String)>>> = Rc::new(RefCell::new(None));
    let patching: Rc<RefCell<Option<Cancel>>> = Rc::new(RefCell::new(None));
    let patch_generation = Rc::new(RefCell::new(0u64));

    {
        let showing = Rc::clone(&showing);
        let patching = Rc::clone(&patching);
        let patch_generation = Rc::clone(&patch_generation);
        let buffer = buffer.clone();
        files.connect_row_selected(move |_, row| {
            let Some(row) = row else { return };
            let Some(path) = row.widget_name().strip_prefix("file:").map(str::to_string) else {
                return;
            };
            let path_for_render = path.clone();
            let Some((repo, id)) = showing.borrow().clone() else { return };

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
                let (rx, cancel) = commit::spawn_patch(repo, id, path.clone());
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
            let showing = Rc::clone(&showing);
            let buffer = buffer.clone();

            glib::spawn_future_local(async move {
                glib::timeout_future(std::time::Duration::from_millis(SETTLE_MS)).await;
                if *generation.borrow() != mine {
                    return;
                }

                *showing.borrow_mut() = Some((repo.clone(), id.clone()));
                let (rx, cancel) = commit::spawn(repo, id);
                *running.borrow_mut() = Some(cancel);
                let Ok(result) = rx.recv().await else { return };
                if *generation.borrow() != mine {
                    return;
                }

                files.remove_all();
                buffer.set_text("");

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
fn render_diff(buffer: &gtk::TextBuffer, diff: &commit::FileDiff, path: &str, dark: bool) {
    buffer.set_text("");
    if diff.binary {
        buffer.set_text("Binary file.");
        return;
    }
    if diff.lines.is_empty() {
        buffer.set_text("No textual changes.");
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

        let owned = format!("{}\n", line.text);
        let spans = highlighter
            .as_mut()
            .and_then(|h| h.highlight_line(&owned, syntaxes).ok());
        match spans {
            Some(spans) => {
                for (style, piece) in spans {
                    let piece = piece.trim_end_matches('\n');
                    if piece.is_empty() {
                        continue;
                    }
                    let at = buffer.end_iter().offset();
                    let mut end = buffer.end_iter();
                    buffer.insert(&mut end, piece);
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

fn row(node: &Node) -> gtk::ListBoxRow {
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

            let row = gtk::ListBoxRow::builder().child(&line).build();
            // The full path travels on the row, since the label only carries
            // the part worth showing.
            row.set_widget_name(&format!("file:{}", file.path));
            row
        }
    }
}
