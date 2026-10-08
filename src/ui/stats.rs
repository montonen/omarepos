//! The contributions page: who committed what, over the last N days, across a
//! set of repositories.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use gtk::pango;

use crate::stats::{self, Author, Contributions, RepoCommits};
use crate::ui::{day_window, key_color, relative_time};

const RANGES: [(&str, usize); 3] = [("Last 7 days", 7), ("Last 14 days", 14), ("Last 30 days", 30)];
const BAR_H: i32 = 46;
const SPARK_W: f64 = 11.0;

/// `scope` is shown as the page subtitle, e.g. "4 selected repos".
pub fn page(repos: Vec<PathBuf>, scope: String) -> adw::NavigationPage {
    // The subtitle also says which commits are counted. The totals mean one
    // thing for the branch that is checked out and another for every branch
    // there is, and the page gave no way to tell which it was showing.
    let counted = if repos.len() == 1 { "checked-out branch" } else { "checked-out branches" };
    let title = adw::WindowTitle::new("Contributions", &format!("{scope} · {counted}"));
    title.set_tooltip_text(Some(
        "Counts the branch each repository has checked out and the branch it tracks.\n\
         Other branches are left out, so that work which was squashed, rebased or amended\n\
         is not counted once for every copy of it.",
    ));
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&title));

    let range = gtk::DropDown::from_strings(&RANGES.map(|(label, _)| label));
    range.add_css_class("om-sort");
    range.set_tooltip_text(Some("Time window"));
    header.pack_end(&range);

    let spinner = gtk::Spinner::new();
    spinner.start();
    let progress = gtk::Label::new(Some("Reading history…"));
    progress.add_css_class("om-meta");
    let loading = gtk::Box::new(gtk::Orientation::Vertical, 10);
    loading.set_valign(gtk::Align::Center);
    loading.set_halign(gtk::Align::Center);
    loading.append(&spinner);
    loading.append(&progress);

    let content = gtk::Box::new(gtk::Orientation::Vertical, 14);
    content.set_margin_top(16);
    content.set_margin_bottom(18);
    content.set_margin_start(16);
    content.set_margin_end(16);

    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&content)
        .build();
    scroller.add_css_class("om-grid");

    let empty = adw::StatusPage::builder()
        .icon_name("system-users-symbolic")
        .title("No commits")
        .build();

    let stack = gtk::Stack::new();
    stack.add_named(&loading, Some("loading"));
    stack.add_named(&scroller, Some("content"));
    stack.add_named(&empty, Some("empty"));
    stack.set_vexpand(true);

    let status = gtk::Label::builder()
        .xalign(0.0)
        .ellipsize(pango::EllipsizeMode::End)
        .build();
    status.add_css_class("om-statusbar");

    let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
    body.append(&stack);
    body.append(&status);

    let view = adw::ToolbarView::new();
    view.add_top_bar(&header);
    view.set_content(Some(&body));

    let page = adw::NavigationPage::builder()
        .title("Contributions")
        .child(&view)
        .build();

    // A generation counter so a slow run that the user has already moved on
    // from cannot overwrite the results of the one they are waiting for.
    let generation = Rc::new(RefCell::new(0u64));
    let repos = Rc::new(repos);

    let load: Rc<dyn Fn(usize)> = {
        let generation = Rc::clone(&generation);
        let repos = Rc::clone(&repos);
        let stack = stack.clone();
        let content = content.clone();
        let empty = empty.clone();
        let status = status.clone();
        let progress = progress.clone();
        Rc::new(move |days: usize| {
            let mine = {
                let mut counter = generation.borrow_mut();
                *counter += 1;
                *counter
            };
            stack.set_visible_child_name("loading");
            status.set_label("Reading history…");

            let rx = stats::spawn((*repos).clone(), day_window(days));
            let generation = Rc::clone(&generation);
            let stack = stack.clone();
            let content = content.clone();
            let empty = empty.clone();
            let status = status.clone();
            let progress = progress.clone();
            glib::spawn_future_local(async move {
                while let Ok(event) = rx.recv().await {
                    if *generation.borrow() != mine {
                        break;
                    }
                    match event {
                        stats::Event::Progress(name, done, total) => {
                            progress.set_label(&format!("{name}  ({done}/{total})"));
                        }
                        stats::Event::Done(result) => {
                            while let Some(child) = content.first_child() {
                                content.remove(&child);
                            }
                            if result.total_commits == 0 {
                                empty.set_title("No commits");
                                empty.set_description(Some(&format!(
                                    "Nothing was committed in the last {} days across {} {}.",
                                    result.window.days(),
                                    result.repos_scanned,
                                    plural(result.repos_scanned, "repository", "repositories"),
                                )));
                                stack.set_visible_child_name("empty");
                            } else {
                                content.append(&overview(&result));
                                for author in &result.authors {
                                    content.append(&author_row(author, &result));
                                }
                                stack.set_visible_child_name("content");
                            }
                            status.set_label(&status_line(&result));
                            break;
                        }
                    }
                }
            });
        })
    };

    {
        let load = Rc::clone(&load);
        range.connect_selected_notify(move |dropdown| {
            let days = RANGES
                .get(dropdown.selected() as usize)
                .map(|(_, d)| *d)
                .unwrap_or(7);
            load(days);
        });
    }
    load(RANGES[0].1);

    page
}

fn status_line(result: &Contributions) -> String {
    let mut parts = vec![
        format!("{} commits", thousands(result.total_commits)),
        format!(
            "{} {}",
            result.authors.len(),
            plural(result.authors.len(), "person", "people")
        ),
        format!("{}/{} repos active", result.repos_active, result.repos_scanned),
        format!(
            "+{} −{}",
            thousands(result.total_insertions),
            thousands(result.total_deletions)
        ),
    ];
    if !result.errors.is_empty() {
        parts.push(format!("{} unreadable", result.errors.len()));
    }
    parts.join("  ·  ")
}

/// Totals, plus a bar per day across every author.
fn overview(result: &Contributions) -> gtk::Widget {
    let card = gtk::Box::new(gtk::Orientation::Vertical, 12);
    card.add_css_class("om-card");

    let numbers = gtk::Box::new(gtk::Orientation::Horizontal, 26);
    numbers.append(&big_number(
        &thousands(result.total_commits),
        plural(result.total_commits, "commit", "commits"),
    ));
    numbers.append(&big_number(
        &result.authors.len().to_string(),
        plural(result.authors.len(), "person", "people"),
    ));
    numbers.append(&big_number(
        &result.repos_active.to_string(),
        "active repos",
    ));
    numbers.append(&big_number(
        &format!("+{}", thousands(result.total_insertions)),
        "insertions",
    ));
    numbers.append(&big_number(
        &format!("−{}", thousands(result.total_deletions)),
        "deletions",
    ));
    card.append(&numbers);

    let peak = result.per_day_total.iter().copied().max().unwrap_or(0).max(1);
    let days = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    days.set_homogeneous(true);
    for (i, count) in result.per_day_total.iter().enumerate() {
        let column = gtk::Box::new(gtk::Orientation::Vertical, 4);

        let bar = gtk::DrawingArea::builder().content_height(BAR_H).hexpand(true).build();
        let (count, peak) = (*count, peak);
        bar.set_draw_func(move |_, cr, width, height| {
            let w = width as f64;
            let h = height as f64;
            let (r, g, b) = crate::theme::Palette::rgb(crate::ui::ACCENTS[0]);
            // A faint track, so empty days are still legible as days.
            cr.set_source_rgba(r, g, b, 0.12);
            cr.rectangle(0.0, 0.0, w, h);
            let _ = cr.fill();
            if count > 0 {
                let filled = (count as f64 / peak as f64).clamp(0.06, 1.0) * h;
                cr.set_source_rgb(r, g, b);
                cr.rectangle(0.0, h - filled, w, filled);
                let _ = cr.fill();
            }
        });
        column.append(&bar);

        let value = gtk::Label::new(Some(&count.to_string()));
        value.add_css_class("om-summary");
        column.append(&value);

        let label = gtk::Label::new(result.window.day_labels.get(i).map(String::as_str));
        label.add_css_class("om-meta");
        column.append(&label);

        if let Some(title) = result.window.day_titles.get(i) {
            column.set_tooltip_text(Some(&format!(
                "{title} · {count} {}",
                plural(*&count, "commit", "commits")
            )));
        }
        days.append(&column);
    }
    card.append(&days);

    card.upcast()
}

fn big_number(value: &str, caption: &str) -> gtk::Box {
    let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let value = gtk::Label::builder().label(value).xalign(0.0).build();
    value.add_css_class("om-big");
    let caption = gtk::Label::builder().label(caption).xalign(0.0).build();
    caption.add_css_class("om-meta");
    column.append(&value);
    column.append(&caption);
    column
}

fn author_row(author: &Author, result: &Contributions) -> gtk::Widget {
    let card = gtk::Box::new(gtk::Orientation::Vertical, 0);
    card.add_css_class("om-card");
    card.add_css_class("om-stat-row");

    let head = gtk::Box::new(gtk::Orientation::Horizontal, 14);

    let chevron = gtk::Image::from_icon_name("pan-end-symbolic");
    chevron.set_valign(gtk::Align::Center);
    head.append(&chevron);

    let color = key_color(if author.email.is_empty() { &author.name } else { &author.email });
    head.append(&swatch(color));

    let who = gtk::Box::new(gtk::Orientation::Vertical, 2);
    who.set_hexpand(true);
    let name = gtk::Label::builder()
        .label(&author.name)
        .xalign(0.0)
        .ellipsize(pango::EllipsizeMode::End)
        .build();
    name.add_css_class("om-name");
    who.append(&name);

    let mut repos: Vec<(&String, &RepoCommits)> = author.repos.iter().collect();
    repos.sort_by(|a, b| {
        b.1.total
            .cmp(&a.1.total)
            .then_with(|| a.0.cmp(b.0))
    });
    let repo_line = repos
        .iter()
        .take(4)
        .map(|(name, in_repo)| format!("{name} {}", in_repo.total))
        .collect::<Vec<_>>()
        .join("  ·  ");
    let extra = repos.len().saturating_sub(4);
    let mut line = if extra > 0 {
        format!("{repo_line}  ·  +{extra} more")
    } else {
        repo_line
    };
    // Authors are keyed by e-mail, so the same display name can appear twice.
    // When it does, say which address each row is.
    let ambiguous = result
        .authors
        .iter()
        .filter(|a| a.name.eq_ignore_ascii_case(&author.name))
        .count()
        > 1;
    if ambiguous && !author.email.is_empty() {
        line = format!("{}  ·  {line}", author.email);
    }
    let detail = gtk::Label::builder()
        .label(line)
        .xalign(0.0)
        .ellipsize(pango::EllipsizeMode::End)
        .build();
    detail.add_css_class("om-meta");
    who.append(&detail);
    head.append(&who);

    let counts = gtk::Box::new(gtk::Orientation::Vertical, 2);
    counts.set_halign(gtk::Align::End);
    let commits = gtk::Label::builder()
        .label(format!(
            "{} {}",
            author.commits,
            plural(author.commits, "commit", "commits")
        ))
        .xalign(1.0)
        .build();
    commits.add_css_class("om-summary");
    counts.append(&commits);

    let churn = gtk::Label::builder()
        .label(format!(
            "+{} −{}",
            thousands(author.insertions),
            thousands(author.deletions)
        ))
        .xalign(1.0)
        .build();
    churn.add_css_class("om-meta");
    counts.append(&churn);
    head.append(&counts);

    head.append(&sparkline(author, result, color));

    // A button rather than a click gesture, so the row can be opened from the
    // keyboard like anything else.
    let toggle = gtk::Button::builder().child(&head).build();
    toggle.add_css_class("om-stat-head");
    toggle.set_tooltip_text(Some(&tooltip(author)));
    card.append(&toggle);

    let revealer = gtk::Revealer::builder()
        .transition_type(gtk::RevealerTransitionType::SlideDown)
        .transition_duration(160)
        .child(&commit_list(&repos))
        .build();
    card.append(&revealer);

    toggle.connect_clicked(move |_| {
        let opening = !revealer.reveals_child();
        revealer.set_reveal_child(opening);
        chevron.set_icon_name(Some(if opening {
            "pan-down-symbolic"
        } else {
            "pan-end-symbolic"
        }));
    });

    card.upcast()
}

fn tooltip(author: &Author) -> String {
    let mut tip = format!(
        "{} <{}>\n{} commits",
        author.name,
        if author.email.is_empty() { "no e-mail" } else { &author.email },
        author.commits
    );
    if author.merges > 0 {
        tip.push_str(&format!(
            "\n{} of them merges (not counted in lines)",
            author.merges
        ));
    }
    tip.push_str(&format!(
        "\n{} files touched\nlast commit {}",
        thousands(author.files),
        relative_time(author.last_seen)
    ));
    tip
}

/// The author's commits, grouped by repository, busiest first.
fn commit_list(repos: &[(&String, &RepoCommits)]) -> gtk::Box {
    let list = gtk::Box::new(gtk::Orientation::Vertical, 10);
    list.set_margin_top(12);
    list.set_margin_start(26);

    for (name, in_repo) in repos {
        let group = gtk::Box::new(gtk::Orientation::Vertical, 2);

        let heading = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        heading.append(&crate::ui::color_dot(key_color(name)));
        let repo_name = gtk::Label::builder().label(*name).xalign(0.0).build();
        repo_name.add_css_class("om-summary");
        heading.append(&repo_name);
        let count = gtk::Label::new(Some(&format!(
            "{} {}",
            in_repo.total,
            plural(in_repo.total, "commit", "commits")
        )));
        count.add_css_class("om-meta");
        heading.append(&count);
        group.append(&heading);

        let mut commits = in_repo.commits.clone();
        commits.sort_by(|a, b| b.when.cmp(&a.when));
        for commit in &commits {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            row.add_css_class("om-row");

            let sha = gtk::Label::builder()
                .label(&commit.short_id)
                .xalign(0.0)
                .width_chars(7)
                .build();
            sha.add_css_class("om-sha");
            row.append(&sha);

            let summary = gtk::Label::builder()
                .label(&commit.summary)
                .xalign(0.0)
                .hexpand(true)
                .ellipsize(pango::EllipsizeMode::End)
                .build();
            summary.add_css_class("om-summary");
            row.append(&summary);

            let churn = gtk::Label::builder()
                .label(if commit.merge {
                    "merge".to_string()
                } else {
                    format!("+{} −{}", thousands(commit.insertions), thousands(commit.deletions))
                })
                .xalign(1.0)
                .width_chars(14)
                .build();
            churn.add_css_class("om-meta");
            row.append(&churn);

            let when = gtk::Label::builder()
                .label(relative_time(commit.when))
                .xalign(1.0)
                .width_chars(13)
                .build();
            when.add_css_class("om-meta");
            row.append(&when);

            row.set_tooltip_text(Some(&format!(
                "{}\n{}\n{}",
                commit.short_id,
                commit.summary,
                crate::ui::absolute_time(commit.when)
            )));
            group.append(&row);
        }

        if in_repo.total > commits.len() {
            let more = gtk::Label::builder()
                .label(format!("+{} older", in_repo.total - commits.len()))
                .xalign(0.0)
                .build();
            more.add_css_class("om-meta");
            more.set_margin_start(8);
            group.append(&more);
        }

        list.append(&group);
    }

    list
}

fn swatch(color: &'static str) -> gtk::DrawingArea {
    let area = gtk::DrawingArea::builder()
        .content_width(10)
        .content_height(10)
        .valign(gtk::Align::Center)
        .build();
    area.set_draw_func(move |_, cr, width, height| {
        let (r, g, b) = crate::theme::Palette::rgb(color);
        cr.set_source_rgb(r, g, b);
        cr.arc(
            width as f64 / 2.0,
            height as f64 / 2.0,
            width.min(height) as f64 / 2.0,
            0.0,
            std::f64::consts::TAU,
        );
        let _ = cr.fill();
    });
    area
}

/// Per-day commits for one author, scaled against the busiest author-day so the
/// strips are comparable between rows.
fn sparkline(author: &Author, result: &Contributions, color: &'static str) -> gtk::DrawingArea {
    let days = result.window.days();
    let peak = result
        .authors
        .iter()
        .flat_map(|a| a.per_day.iter().copied())
        .max()
        .unwrap_or(0)
        .max(1);

    let area = gtk::DrawingArea::builder()
        .content_width((days as f64 * SPARK_W) as i32)
        .content_height(30)
        .valign(gtk::Align::Center)
        .build();

    let per_day = author.per_day.clone();
    area.set_draw_func(move |_, cr, width, height| {
        let h = height as f64;
        let step = width as f64 / per_day.len().max(1) as f64;
        let (r, g, b) = crate::theme::Palette::rgb(color);
        for (i, count) in per_day.iter().enumerate() {
            let x = i as f64 * step;
            cr.set_source_rgba(r, g, b, 0.10);
            cr.rectangle(x + 1.0, 0.0, step - 2.0, h);
            let _ = cr.fill();
            if *count > 0 {
                let filled = (*count as f64 / peak as f64).clamp(0.1, 1.0) * h;
                cr.set_source_rgb(r, g, b);
                cr.rectangle(x + 1.0, h - filled, step - 2.0, filled);
                let _ = cr.fill();
            }
        }
    });

    area.set_tooltip_text(Some(
        &result
            .window
            .day_titles
            .iter()
            .zip(author.per_day.iter())
            .map(|(title, n)| format!("{title}: {n}"))
            .collect::<Vec<_>>()
            .join("\n"),
    ));
    area
}

fn plural(n: usize, one: &'static str, many: &'static str) -> &'static str {
    if n == 1 { one } else { many }
}

fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push('\u{202f}');
        }
        out.push(c);
    }
    out
}
