//! Dialogs: where repositories are looked for, and how they are tagged.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use gtk::gio;

use crate::config::{Config, RootSpec, tildify};
use crate::meta::{self, Meta};

/// The settings dialog: where repositories are looked for, which tools to
/// launch, how often to refresh, and whether to follow the Omarchy theme.
///
/// The callbacks are separate because the costs are: `on_sources` means the
/// grid's contents changed and a rescan is due, while `on_refresh` only
/// re-arms a timer. Sharing one callback made flipping a refresh switch
/// rescan every repository for nothing.
pub fn settings(
    parent: &impl IsA<gtk::Widget>,
    cfg: Rc<RefCell<Config>>,
    on_sources: Rc<dyn Fn()>,
    on_refresh: Rc<dyn Fn()>,
    on_restyle: Rc<dyn Fn()>,
) {
    let on_change = on_sources;
    let dialog = adw::PreferencesDialog::builder()
        .title("Settings")
        .content_width(540)
        .content_height(680)
        .build();
    let page = adw::PreferencesPage::new();
    dialog.add(&page);

    let roots_group = adw::PreferencesGroup::builder()
        .title("Scan roots")
        .description("Folders searched for repositories, a few levels deep.")
        .build();
    let repos_group = adw::PreferencesGroup::builder()
        .title("Individual repositories")
        .description("Repositories included wherever they live.")
        .build();
    let depth_group = adw::PreferencesGroup::builder().title("Search depth").build();
    let tools_group = adw::PreferencesGroup::builder()
        .title("Tools")
        .description("Used by the open actions on each card.")
        .build();
    let refresh_group = adw::PreferencesGroup::builder()
        .title("Background refresh")
        .description("Keeps the grid current, and optionally the code with it.")
        .build();
    let look_group = adw::PreferencesGroup::builder().title("Appearance").build();
    page.add(&roots_group);
    page.add(&repos_group);
    page.add(&depth_group);
    page.add(&tools_group);
    page.add(&refresh_group);
    page.add(&look_group);

    // Both lists are rebuilt wholesale after each edit; they are short, and it
    // keeps add and remove from needing to agree about row identity.
    let listed: Rc<RefCell<Vec<(adw::PreferencesGroup, gtk::Widget)>>> =
        Rc::new(RefCell::new(Vec::new()));
    // The row builders need to call themselves after an edit.
    let rebuild: Rc<RefCell<Option<Rc<dyn Fn()>>>> = Rc::new(RefCell::new(None));

    let build_rows: Rc<dyn Fn()> = {
        let cfg = Rc::clone(&cfg);
        let roots_group = roots_group.clone();
        let repos_group = repos_group.clone();
        let listed = Rc::clone(&listed);
        let rebuild = Rc::clone(&rebuild);
        let on_change = Rc::clone(&on_change);
        Rc::new(move || {
            for (group, row) in listed.borrow_mut().drain(..) {
                group.remove(&row);
            }

            let (roots, extras) = {
                let cfg = cfg.borrow();
                (cfg.roots.clone(), cfg.extra_repos.clone())
            };
            let mut added = Vec::new();

            if roots.is_empty() {
                let row = adw::ActionRow::builder()
                    .title("No roots")
                    .subtitle("Nothing will be scanned until you add a folder.")
                    .build();
                roots_group.add(&row);
                added.push((roots_group.clone(), row.upcast()));
            }
            for spec in roots {
                let row = root_row(&spec, Rc::clone(&cfg), Rc::clone(&on_change), {
                    let cfg = Rc::clone(&cfg);
                    let rebuild = Rc::clone(&rebuild);
                    let on_change = Rc::clone(&on_change);
                    let path = crate::config::expand(spec.path_str());
                    move || {
                        cfg.borrow_mut().remove_root(&path);
                        persist(&cfg);
                        if let Some(f) = rebuild.borrow().clone() {
                            f();
                        }
                        on_change();
                    }
                });
                roots_group.add(&row);
                added.push((roots_group.clone(), row.upcast()));
            }

            for entry in extras {
                let row = removable_row(&entry, "Remove this repository", {
                    let cfg = Rc::clone(&cfg);
                    let rebuild = Rc::clone(&rebuild);
                    let on_change = Rc::clone(&on_change);
                    let entry = entry.clone();
                    move || {
                        cfg.borrow_mut()
                            .remove_extra_repo(&crate::config::expand(&entry));
                        persist(&cfg);
                        if let Some(f) = rebuild.borrow().clone() {
                            f();
                        }
                        on_change();
                    }
                });
                repos_group.add(&row);
                added.push((repos_group.clone(), row.upcast()));
            }

            *listed.borrow_mut() = added;
        })
    };
    *rebuild.borrow_mut() = Some(Rc::clone(&build_rows));
    build_rows();

    let add_root = add_button("Add folder…");
    roots_group.set_header_suffix(Some(&add_root));
    let add_repo = add_button("Add repository…");
    repos_group.set_header_suffix(Some(&add_repo));

    let depth = adw::SpinRow::with_range(1.0, 8.0, 1.0);
    depth.set_title("Levels below each root");
    depth.set_subtitle("Descent stops at the first repository found");
    depth.set_value(cfg.borrow().max_depth as f64);
    depth_group.add(&depth);
    {
        let cfg = Rc::clone(&cfg);
        let on_change = Rc::clone(&on_change);
        depth.connect_value_notify(move |row| {
            let value = row.value().round().max(1.0) as usize;
            if cfg.borrow().max_depth == value {
                return;
            }
            cfg.borrow_mut().max_depth = value;
            persist(&cfg);
            on_change();
        });
    }

    let terminal = adw::EntryRow::builder()
        .title("Terminal")
        .show_apply_button(true)
        .build();
    terminal.set_text(cfg.borrow().terminal.as_deref().unwrap_or(""));
    terminal.set_tooltip_text(Some(&format!(
        "Blank follows $TERMINAL. Currently: {}",
        cfg.borrow().terminal_command()
    )));
    tools_group.add(&terminal);

    let editor = adw::EntryRow::builder()
        .title("Editor")
        .show_apply_button(true)
        .build();
    editor.set_text(cfg.borrow().editor.as_deref().unwrap_or(""));
    editor.set_tooltip_text(Some(&format!(
        "Blank follows $EDITOR. Currently: {}",
        cfg.borrow().editor_command()
    )));
    tools_group.add(&editor);

    for (row, field) in [(&terminal, Field::Terminal), (&editor, Field::Editor)] {
        let cfg = Rc::clone(&cfg);
        row.connect_apply(move |row| {
            let value = row.text().trim().to_string();
            let value = if value.is_empty() { None } else { Some(value) };
            {
                let mut cfg = cfg.borrow_mut();
                match field {
                    Field::Terminal => cfg.terminal = value,
                    Field::Editor => cfg.editor = value,
                }
            }
            persist(&cfg);
        });
    }

    // --- background refresh ------------------------------------------------
    let every = adw::SpinRow::with_range(0.0, 1440.0, 5.0);
    every.set_title("Refresh every");
    every.set_subtitle("Minutes. Zero turns it off.");
    every.set_value(cfg.borrow().refresh.every_minutes as f64);
    refresh_group.add(&every);

    let fetch = adw::SwitchRow::builder()
        .title("Fetch from the remote")
        .subtitle("Updates what the remote has. Never touches your files.")
        .active(cfg.borrow().refresh.fetch)
        .build();
    refresh_group.add(&fetch);

    let fast_forward = adw::SwitchRow::builder()
        .title("Update the code when it is safe")
        .subtitle("Fast-forward only, and only a clean tree with no commits of its own")
        .active(cfg.borrow().refresh.fast_forward)
        .sensitive(cfg.borrow().refresh.fetch)
        .build();
    refresh_group.add(&fast_forward);

    let scope = adw::EntryRow::builder()
        .title("Only repos tagged")
        .show_apply_button(true)
        .build();
    scope.set_text(&cfg.borrow().refresh.only_tags.join(", "));
    scope.set_tooltip_text(Some("Comma separated. Blank means every repository."));
    refresh_group.add(&scope);

    {
        let cfg = Rc::clone(&cfg);
        let on_change = Rc::clone(&on_refresh);
        every.connect_value_notify(move |row| {
            let value = row.value().round().max(0.0) as u64;
            if cfg.borrow().refresh.every_minutes == value {
                return;
            }
            cfg.borrow_mut().refresh.every_minutes = value;
            persist(&cfg);
            on_change();
        });
    }
    {
        let cfg = Rc::clone(&cfg);
        let on_change = Rc::clone(&on_refresh);
        let fast_forward = fast_forward.clone();
        fetch.connect_active_notify(move |switch| {
            let on = switch.is_active();
            cfg.borrow_mut().refresh.fetch = on;
            // Fast-forwarding without fetching could only ever act on a stale
            // remote ref, so the two travel together.
            if !on {
                cfg.borrow_mut().refresh.fast_forward = false;
                fast_forward.set_active(false);
            }
            fast_forward.set_sensitive(on);
            persist(&cfg);
            on_change();
        });
    }
    {
        let cfg = Rc::clone(&cfg);
        let on_change = Rc::clone(&on_refresh);
        fast_forward.connect_active_notify(move |switch| {
            cfg.borrow_mut().refresh.fast_forward = switch.is_active();
            persist(&cfg);
            on_change();
        });
    }
    {
        let cfg = Rc::clone(&cfg);
        let on_change = Rc::clone(&on_refresh);
        scope.connect_apply(move |entry| {
            let tags = crate::meta::parse_list(&entry.text());
            entry.set_text(&tags.join(", "));
            cfg.borrow_mut().refresh.only_tags = tags;
            persist(&cfg);
            on_change();
        });
    }

    let follow = adw::SwitchRow::builder()
        .title("Follow the Omarchy theme")
        .subtitle("Restyle when the active theme changes")
        .active(cfg.borrow().follow_omarchy_theme)
        .build();
    look_group.add(&follow);
    {
        let cfg = Rc::clone(&cfg);
        let on_restyle = Rc::clone(&on_restyle);
        follow.connect_active_notify(move |row| {
            cfg.borrow_mut().follow_omarchy_theme = row.is_active();
            persist(&cfg);
            on_restyle();
        });
    }

    let config_path = adw::ActionRow::builder()
        .title("Config file")
        .subtitle(tildify(&Config::path()))
        .build();
    config_path.add_css_class("property");
    look_group.add(&config_path);

    let window = parent.as_ref().root().and_downcast::<gtk::Window>();
    {
        let cfg = Rc::clone(&cfg);
        let build_rows = Rc::clone(&build_rows);
        let on_change = Rc::clone(&on_change);
        let window = window.clone();
        add_root.connect_clicked(move |_| {
            let cfg = Rc::clone(&cfg);
            let build_rows = Rc::clone(&build_rows);
            let on_change = Rc::clone(&on_change);
            pick_folder(window.as_ref(), "Choose a folder to scan", move |path| {
                if cfg.borrow_mut().add_root(&path) {
                    persist(&cfg);
                    build_rows();
                    on_change();
                }
            });
        });
    }
    {
        let cfg = Rc::clone(&cfg);
        let build_rows = Rc::clone(&build_rows);
        let on_change = Rc::clone(&on_change);
        let window = window.clone();
        add_repo.connect_clicked(move |button| {
            let cfg = Rc::clone(&cfg);
            let build_rows = Rc::clone(&build_rows);
            let on_change = Rc::clone(&on_change);
            let button = button.clone();
            pick_folder(window.as_ref(), "Choose a repository", move |path| {
                if !path.join(".git").exists() {
                    toast(&button, &format!("{} is not a git repository", tildify(&path)));
                    return;
                }
                if cfg.borrow_mut().add_extra_repo(&path) {
                    persist(&cfg);
                    build_rows();
                    on_change();
                }
            });
        });
    }

    dialog.present(Some(parent.as_ref()));
}

#[derive(Clone, Copy)]
enum Field {
    Terminal,
    Editor,
}

fn add_button(label: &str) -> gtk::Button {
    gtk::Button::builder()
        .child(
            &adw::ButtonContent::builder()
                .icon_name("list-add-symbolic")
                .label(label)
                .build(),
        )
        .build()
}

/// One scan root, with its options behind an expander. Options are written
/// straight through on change; only add and remove rebuild the list, so
/// toggling a switch does not collapse the row under the pointer.
fn root_row(
    spec: &RootSpec,
    cfg: Rc<RefCell<Config>>,
    on_change: Rc<dyn Fn()>,
    on_remove: impl Fn() + 'static,
) -> adw::ExpanderRow {
    let resolved = spec.resolve();
    let path = resolved.path.clone();

    let row = adw::ExpanderRow::builder()
        .title(spec.path_str())
        .subtitle(root_summary(&resolved))
        .build();

    let remove = gtk::Button::builder()
        .icon_name("user-trash-symbolic")
        .tooltip_text("Remove this root")
        .valign(gtk::Align::Center)
        .build();
    remove.add_css_class("flat");
    remove.connect_clicked(move |_| on_remove());
    row.add_suffix(&remove);

    let nested = adw::SwitchRow::builder()
        .title("Look inside repositories")
        .subtitle("Follow the clones this folder's own repository ignores")
        .active(resolved.nested)
        .build();
    let tag = adw::EntryRow::builder()
        .title("Tag what is found here")
        .show_apply_button(true)
        .build();
    tag.set_text(resolved.tag.as_deref().unwrap_or(""));
    let include_self = adw::SwitchRow::builder()
        .title("Show the containing repository")
        .active(resolved.include_self)
        .sensitive(resolved.nested)
        .build();

    row.add_row(&nested);
    row.add_row(&tag);
    row.add_row(&include_self);

    {
        let cfg = Rc::clone(&cfg);
        let on_change = Rc::clone(&on_change);
        let path = path.clone();
        let row = row.clone();
        let include_self = include_self.clone();
        nested.connect_active_notify(move |switch| {
            let on = switch.is_active();
            cfg.borrow_mut().update_root(&path, |entry| {
                entry.nested = on;
                // The default flips with `nested`, so an untouched switch
                // should keep following the default rather than freeze.
                if !on {
                    entry.include_self = None;
                }
            });
            include_self.set_sensitive(on);
            if !on {
                include_self.set_active(true);
            }
            persist(&cfg);
            refresh_summary(&cfg, &path, &row);
            on_change();
        });
    }
    {
        let cfg = Rc::clone(&cfg);
        let on_change = Rc::clone(&on_change);
        let path = path.clone();
        let row = row.clone();
        tag.connect_apply(move |entry| {
            let value = entry.text().trim().to_string();
            cfg.borrow_mut().update_root(&path, |root| {
                root.tag = if value.is_empty() { None } else { Some(value.clone()) };
            });
            persist(&cfg);
            refresh_summary(&cfg, &path, &row);
            on_change();
        });
    }
    {
        let cfg = Rc::clone(&cfg);
        let on_change = Rc::clone(&on_change);
        let path = path.clone();
        let row = row.clone();
        include_self.connect_active_notify(move |switch| {
            let on = switch.is_active();
            cfg.borrow_mut().update_root(&path, |entry| {
                entry.include_self = Some(on);
            });
            persist(&cfg);
            refresh_summary(&cfg, &path, &row);
            on_change();
        });
    }

    row
}

fn root_summary(root: &crate::config::Root) -> String {
    let mut parts = Vec::new();
    if !root.path.exists() {
        parts.push("missing".to_string());
    }
    if root.nested {
        parts.push("workspace".to_string());
        if !root.include_self {
            parts.push("container hidden".to_string());
        }
    }
    if let Some(tag) = &root.tag {
        parts.push(format!("tagged {tag}"));
    }
    parts.join(" · ")
}

fn refresh_summary(cfg: &Rc<RefCell<Config>>, path: &std::path::Path, row: &adw::ExpanderRow) {
    let cfg = cfg.borrow();
    if let Some(spec) = cfg
        .roots
        .iter()
        .find(|spec| crate::config::expand(spec.path_str()) == path)
    {
        row.set_subtitle(&root_summary(&spec.resolve()));
    }
}

fn removable_row(
    entry: &str,
    tooltip: &str,
    on_remove: impl Fn() + 'static,
) -> adw::ActionRow {
    let expanded = crate::config::expand(entry);
    let row = adw::ActionRow::builder()
        .title(entry)
        .subtitle(if expanded.exists() { "" } else { "missing" })
        .build();
    let remove = gtk::Button::builder()
        .icon_name("user-trash-symbolic")
        .tooltip_text(tooltip)
        .valign(gtk::Align::Center)
        .build();
    remove.add_css_class("flat");
    remove.connect_clicked(move |_| on_remove());
    row.add_suffix(&remove);
    row
}

fn pick_folder(
    parent: Option<&gtk::Window>,
    title: &str,
    on_pick: impl Fn(PathBuf) + 'static,
) {
    let dialog = gtk::FileDialog::builder().title(title).modal(true).build();
    dialog.select_folder(parent, gio::Cancellable::NONE, move |result| {
        if let Ok(file) = result {
            if let Some(path) = file.path() {
                on_pick(path);
            }
        }
    });
}

fn persist(cfg: &Rc<RefCell<Config>>) {
    if let Err(err) = cfg.borrow().save() {
        eprintln!("omarepos: could not write the config file: {err}");
    }
}

fn toast(widget: &impl IsA<gtk::Widget>, text: &str) {
    let mut node = widget.as_ref().parent();
    while let Some(current) = node {
        if let Ok(overlay) = current.clone().downcast::<adw::ToastOverlay>() {
            overlay.add_toast(adw::Toast::new(text));
            return;
        }
        node = current.parent();
    }
    eprintln!("omarepos: {text}");
}

/// Edit the tags on one or more repositories. With several repos the entry
/// starts from the tags they already have in common.
pub fn edit_tags(
    parent: &impl IsA<gtk::Widget>,
    meta: Rc<RefCell<Meta>>,
    repos: Vec<PathBuf>,
    on_change: Rc<dyn Fn()>,
) {
    if repos.is_empty() {
        return;
    }
    let heading = if repos.len() == 1 {
        format!(
            "Tags for {}",
            repos[0]
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| tildify(&repos[0]))
        )
    } else {
        format!("Tags for {} repositories", repos.len())
    };

    let shared = common_tags(&meta.borrow(), &repos);
    let entry = adw::EntryRow::builder().title("Tags").build();
    entry.set_text(&shared.join(", "));

    let group = adw::PreferencesGroup::new();
    group.add(&entry);

    let body = gtk::Box::new(gtk::Orientation::Vertical, 10);
    body.append(&group);

    // Existing tags as one-click chips, so spelling stays consistent.
    let known = meta.borrow().all_tags();
    if !known.is_empty() {
        let flow = gtk::FlowBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .max_children_per_line(6)
            .column_spacing(6)
            .row_spacing(6)
            .build();
        for (tag, count) in known {
            let button = gtk::Button::builder()
                .label(format!("{tag} ({count})"))
                .build();
            button.add_css_class("om-tag-chip");
            let entry = entry.clone();
            button.connect_clicked(move |_| {
                let mut tags = meta::parse_list(&entry.text());
                if tags.iter().any(|t| t.eq_ignore_ascii_case(&tag)) {
                    tags.retain(|t| !t.eq_ignore_ascii_case(&tag));
                } else {
                    tags.push(tag.clone());
                }
                entry.set_text(&meta::normalize(tags).join(", "));
            });
            flow.append(&button);
        }
        body.append(&flow);
    }

    let dialog = adw::AlertDialog::builder()
        .heading(heading)
        .body(if repos.len() == 1 {
            "Separate tags with commas."
        } else {
            "These tags replace the ones these repositories share. Tags unique to one of them are kept."
        })
        .extra_child(&body)
        .build();
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("save", "Save");
    dialog.set_response_appearance("save", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("save"));
    dialog.set_close_response("cancel");

    let entry_for_save = entry.clone();
    dialog.connect_response(None, move |_, response| {
        if response != "save" {
            return;
        }
        let wanted = meta::parse_list(&entry_for_save.text());
        {
            let mut meta = meta.borrow_mut();
            for repo in &repos {
                // Keep anything that was not part of the shared set, so editing
                // several repos at once cannot silently drop their own tags.
                let mut tags: Vec<String> = meta
                    .tags_for(repo)
                    .iter()
                    .filter(|t| !shared.iter().any(|s| s.eq_ignore_ascii_case(t)))
                    .cloned()
                    .collect();
                tags.extend(wanted.iter().cloned());
                meta.set_tags(repo, tags);
            }
            if let Err(err) = meta.save() {
                eprintln!("omarepos: could not write the tag file: {err}");
            }
        }
        on_change();
    });

    dialog.present(Some(parent.as_ref()));
}

fn common_tags(meta: &Meta, repos: &[PathBuf]) -> Vec<String> {
    let mut iter = repos.iter();
    let Some(first) = iter.next() else { return Vec::new() };
    let mut shared: Vec<String> = meta.tags_for(first).to_vec();
    for repo in iter {
        let theirs = meta.tags_for(repo);
        shared.retain(|tag| theirs.iter().any(|t| t.eq_ignore_ascii_case(tag)));
    }
    shared
}
