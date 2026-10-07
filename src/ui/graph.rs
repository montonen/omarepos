//! The per-repository commit graph page.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use gtk::pango;

use crate::config::tildify;
use crate::graph::{self, Graph, RefKind, Row};
use crate::ui::{absolute_time, lane_color, relative_time};

/// Rows are a fixed height so the graph cell can draw a row at a time without
/// knowing anything about its neighbours.
const ROW_H: i32 = 28;
const LANE_W: f64 = 14.0;
const DOT_R: f64 = 4.0;
/// Past this many lanes the graph stops being readable; wider rows are clipped.
const MAX_LANES: usize = 16;
const COMMIT_LIMIT: usize = 500;

fn lane_x(lane: usize) -> f64 {
    LANE_W * lane as f64 + LANE_W / 2.0 + 3.0
}

/// Build the page and start loading. `on_contributions` opens the stats page
/// scoped to this repository.
pub fn page(
    path: &Path,
    name: &str,
    on_contributions: Rc<dyn Fn(PathBuf)>,
) -> adw::NavigationPage {
    let title = adw::WindowTitle::new(name, &tildify(path));
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&title));

    let contributions = gtk::Button::builder()
        .icon_name("system-users-symbolic")
        .tooltip_text("Contributions in this repository")
        .has_frame(false)
        .build();
    {
        let path = path.to_path_buf();
        contributions.connect_clicked(move |_| on_contributions(path.clone()));
    }
    header.pack_end(&contributions);

    let spinner = gtk::Spinner::new();
    spinner.start();
    let loading = gtk::Box::new(gtk::Orientation::Vertical, 10);
    loading.set_valign(gtk::Align::Center);
    loading.set_halign(gtk::Align::Center);
    loading.append(&spinner);
    let loading_label = gtk::Label::new(Some("Reading history…"));
    loading_label.add_css_class("om-meta");
    loading.append(&loading_label);

    let rows_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    rows_box.set_margin_top(8);
    rows_box.set_margin_bottom(12);
    rows_box.set_margin_start(10);
    rows_box.set_margin_end(10);

    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .child(&rows_box)
        .build();
    scroller.add_css_class("om-grid");

    let empty = adw::StatusPage::builder()
        .icon_name("document-open-recent-symbolic")
        .title("No commits")
        .description("This repository has no history yet.")
        .build();

    let stack = gtk::Stack::new();
    stack.add_named(&loading, Some("loading"));
    stack.add_named(&scroller, Some("graph"));
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

    let toasts = adw::ToastOverlay::new();
    toasts.set_child(Some(&body));

    let view = adw::ToolbarView::new();
    view.add_top_bar(&header);
    view.set_content(Some(&toasts));

    let page = adw::NavigationPage::builder()
        .title(name)
        .child(&view)
        .build();

    // Loading happens off the main thread; a big repo takes a moment.
    let (tx, rx) = async_channel::bounded(1);
    let load_path = path.to_path_buf();
    std::thread::spawn(move || {
        let _ = tx.send_blocking(graph::build(&load_path, COMMIT_LIMIT));
    });

    let toasts_for_rows = toasts.clone();
    glib::spawn_future_local(async move {
        let Ok(result) = rx.recv().await else { return };
        match result {
            Ok(graph) if graph.rows.is_empty() => {
                stack.set_visible_child_name("empty");
                status.set_label("no commits");
            }
            Ok(graph) => {
                let width = lanes_width(graph.lanes);
                for row in &graph.rows {
                    rows_box.append(&row_widget(row, width, &toasts_for_rows));
                }
                stack.set_visible_child_name("graph");
                status.set_label(&summary(&graph));
            }
            Err(err) => {
                empty.set_title("Could not read this repository");
                empty.set_description(Some(&err));
                stack.set_visible_child_name("empty");
                status.set_label(&err);
            }
        }
    });

    page
}

fn lanes_width(lanes: usize) -> i32 {
    (lane_x(lanes.clamp(1, MAX_LANES).saturating_sub(1)) + LANE_W) as i32
}

fn summary(graph: &Graph) -> String {
    let mut parts = Vec::new();
    parts.push(format!("{} commits", graph.rows.len()));
    if graph.truncated {
        parts.push(format!("newest {COMMIT_LIMIT} shown"));
    }
    parts.push(format!("{} lanes", graph.lanes));
    if graph.lanes > MAX_LANES {
        parts.push(format!("lanes past {MAX_LANES} are clipped"));
    }
    if let Some(head) = &graph.head {
        parts.push(format!("HEAD {head}"));
    }
    parts.join("  ·  ")
}

fn row_widget(row: &Row, graph_width: i32, toasts: &adw::ToastOverlay) -> gtk::Widget {
    let line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    line.set_height_request(ROW_H);
    line.add_css_class("om-row");

    line.append(&graph_cell(row, graph_width));

    let sha = gtk::Label::builder()
        .label(&row.short_id)
        .xalign(0.0)
        .width_chars(7)
        .build();
    sha.add_css_class("om-sha");
    line.append(&sha);

    for label in &row.refs {
        let pill = gtk::Label::new(Some(&label.name));
        pill.add_css_class("om-ref");
        pill.add_css_class(match label.kind {
            RefKind::Head => "head",
            RefKind::LocalBranch => "local",
            RefKind::RemoteBranch => "remote",
            RefKind::Tag => "tag",
        });
        if label.current {
            pill.add_css_class("current");
        }
        line.append(&pill);
    }

    let summary = gtk::Label::builder()
        .label(&row.summary)
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(pango::EllipsizeMode::End)
        .build();
    summary.add_css_class("om-summary");
    line.append(&summary);

    let author = gtk::Label::builder()
        .label(&row.author)
        .xalign(1.0)
        .width_chars(16)
        .max_width_chars(16)
        .ellipsize(pango::EllipsizeMode::End)
        .build();
    author.add_css_class("om-meta");
    line.append(&author);

    let when = gtk::Label::builder()
        .label(relative_time(row.when))
        .xalign(1.0)
        .width_chars(13)
        .build();
    when.add_css_class("om-meta");
    line.append(&when);

    line.set_tooltip_text(Some(&format!(
        "{}\n\n{}\n{} <{}>\n{}",
        row.short_id,
        if row.body.is_empty() { &row.summary } else { &row.body },
        row.author,
        row.email,
        absolute_time(row.when),
    )));

    // Clicking a row puts its sha on the clipboard — the thing you nearly
    // always want a commit graph for.
    let click = gtk::GestureClick::new();
    let id = row.id.to_string();
    let toasts = toasts.clone();
    click.connect_released(move |gesture, _, _, _| {
        gesture.set_state(gtk::EventSequenceState::Claimed);
        if let Some(widget) = gesture.widget() {
            widget.clipboard().set_text(&id);
        }
        toasts.add_toast(adw::Toast::new(&format!("Copied {}", &id[..7.min(id.len())])));
    });
    line.add_controller(click);

    line.upcast()
}

/// One row of the DAG. The top half draws what arrives from the row above, the
/// bottom half what leaves towards the row below, so neighbouring rows meet at
/// the boundary without either knowing about the other.
fn graph_cell(row: &Row, width: i32) -> gtk::DrawingArea {
    let area = gtk::DrawingArea::builder()
        .content_width(width)
        .content_height(ROW_H)
        .build();

    let row = row.clone();
    area.set_draw_func(move |_, cr, _, height| {
        let h = height as f64;
        let mid = h / 2.0;
        let dot_x = lane_x(row.lane);
        cr.set_line_width(1.6);

        // --- arriving from above -----------------------------------------
        for (i, slot) in row.incoming.iter().enumerate() {
            if slot.is_none() || i >= MAX_LANES {
                continue;
            }
            set_color(cr, lane_color(i));
            if *slot == Some(row.id) && i != row.lane {
                // A second child's lane bending into this commit.
                curve(cr, lane_x(i), 0.0, dot_x, mid);
            } else if *slot == Some(row.id) {
                cr.move_to(dot_x, 0.0);
                cr.line_to(dot_x, mid);
            } else {
                cr.move_to(lane_x(i), 0.0);
                cr.line_to(lane_x(i), mid);
            }
            let _ = cr.stroke();
        }

        // --- leaving towards the row below --------------------------------
        for (j, slot) in row.outgoing.iter().enumerate() {
            if slot.is_none() || j >= MAX_LANES {
                continue;
            }
            set_color(cr, lane_color(j));
            let unchanged = row.incoming.get(j).copied().flatten() == *slot;
            if unchanged {
                cr.move_to(lane_x(j), mid);
                cr.line_to(lane_x(j), h);
                let _ = cr.stroke();
            }
            if row.down.contains(&j) {
                if j == row.lane {
                    cr.move_to(dot_x, mid);
                    cr.line_to(dot_x, h);
                } else {
                    curve(cr, dot_x, mid, lane_x(j), h);
                }
                let _ = cr.stroke();
            }
        }

        // --- the commit itself ---------------------------------------------
        if row.lane < MAX_LANES {
            set_color(cr, lane_color(row.lane));
            if row.is_merge {
                // A ring, so merges read differently at a glance.
                cr.set_line_width(2.0);
                cr.arc(dot_x, mid, DOT_R, 0.0, std::f64::consts::TAU);
                let _ = cr.stroke();
            } else {
                cr.arc(dot_x, mid, DOT_R, 0.0, std::f64::consts::TAU);
                let _ = cr.fill();
            }
            if !row.refs.is_empty() {
                cr.set_line_width(1.0);
                cr.arc(dot_x, mid, DOT_R + 2.5, 0.0, std::f64::consts::TAU);
                let _ = cr.stroke();
            }
        }
    });

    area
}

fn curve(cr: &gtk::cairo::Context, x0: f64, y0: f64, x1: f64, y1: f64) {
    let dy = y1 - y0;
    cr.move_to(x0, y0);
    cr.curve_to(x0, y0 + dy * 0.55, x1, y1 - dy * 0.55, x1, y1);
}

fn set_color(cr: &gtk::cairo::Context, hex: &str) {
    let (r, g, b) = crate::theme::Palette::rgb(hex);
    cr.set_source_rgb(r, g, b);
}
