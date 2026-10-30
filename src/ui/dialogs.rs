//! Dialogs: where repositories are looked for, and how they are tagged.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use adw::prelude::*;
use gtk::gio;
use gtk::glib;

use crate::config::{Config, Refresh, RootSpec, tildify};
use crate::meta::{self, Meta};

/// The settings dialog: where repositories are looked for, which tools to
/// launch, how often to refresh, and whether to follow the Omarchy theme.
///
/// The callbacks are separate because the costs are: `on_sources` means the
/// grid's contents changed and a rescan is due, while `on_refresh` only
/// re-arms a timer. Sharing one callback made flipping a refresh switch
/// rescan every repository for nothing.
///
/// With the tag store, removing a root, or an individually listed
/// repository, also forgets what the store holds about the repositories
/// nothing else reaches (`Meta::forget_source`). The store is saved here; the
/// rescan that `on_sources` starts redraws the tags. `reports` is what the
/// last scan made of each root, which is what a root's row says of it.
pub fn settings(
    parent: &impl IsA<gtk::Widget>,
    cfg: Rc<RefCell<Config>>,
    meta: Option<Rc<RefCell<Meta>>>,
    reports: Vec<crate::git::RootReport>,
    on_sources: Rc<dyn Fn()>,
    on_refresh: Rc<dyn Fn()>,
    on_restyle: Rc<dyn Fn()>,
) {
    let reports = Rc::new(reports);
    let on_change = on_sources;
    let dialog =
        adw::PreferencesDialog::builder().title("Settings").content_width(540).content_height(680).build();
    // Where a refused save is said. Every switch in here writes the file, and
    // a write that is refused (the file was edited elsewhere, or could not be
    // read) used to go to stderr, with the switch left showing what was not
    // saved.
    COMPLAIN_TO.with(|slot| *slot.borrow_mut() = Some(dialog.downgrade()));
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
        .description(
            "Runs while omarepos is open, and at launch when a pass is overdue. \
             Keeps the grid current, and optionally the code with it.",
        )
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
    let listed: Rc<RefCell<Vec<(adw::PreferencesGroup, gtk::Widget)>>> = Rc::new(RefCell::new(Vec::new()));
    // The row builders need to call themselves after an edit.
    let rebuild: Rc<RefCell<Option<Rc<dyn Fn()>>>> = Rc::new(RefCell::new(None));

    // Taking a source out: of the config, of the tag store, then of the list
    // here and of the grid.
    let remove: Rc<dyn Fn(Source)> = {
        let cfg = Rc::clone(&cfg);
        let rebuild = Rc::clone(&rebuild);
        let on_change = Rc::clone(&on_change);
        let anchor = roots_group.clone();
        Rc::new(move |source: Source| {
            let go = {
                let cfg = Rc::clone(&cfg);
                let meta = meta.clone();
                let rebuild = Rc::clone(&rebuild);
                let on_change = Rc::clone(&on_change);
                let source = source.clone();
                move || {
                    source.remove_from(&mut cfg.borrow_mut());
                    // Only a removal that reached the config file is one to
                    // forget for. A file that could not be read at launch is
                    // never saved over, and what is in memory then is not
                    // what the user configured.
                    if let (true, Some(meta)) = (persist(&cfg), &meta) {
                        let kept = sources(&cfg.borrow());
                        let mut meta = meta.borrow_mut();
                        if meta.forget_source(source.path(), &kept) > 0
                            && let Err(err) = meta.save()
                        {
                            complain(&format!("Its tags are still in the tag file: {err}"));
                        }
                    }
                    if let Some(f) = rebuild.borrow().clone() {
                        f();
                    }
                    on_change();
                }
            };

            // Forgetting takes tags typed by hand with it, and the trash
            // button is one click with no way back. So when there are tags to
            // lose, say so first; a source that tagged nothing goes at once,
            // as it always did.
            let tagged = match &meta {
                Some(meta) if cfg.borrow().load_error.is_none() => {
                    let mut after = cfg.borrow().clone();
                    source.remove_from(&mut after);
                    meta.borrow().tagged_only_under(source.path(), &sources(&after))
                }
                _ => 0,
            };
            if tagged == 0 {
                go();
                return;
            }
            let body = match &source {
                Source::Root(_) => format!(
                    "The tags on {tagged} {} found only through this folder will be forgotten \
                     as well. Adding the folder again brings back a tag it applies itself, not \
                     tags typed by hand.",
                    if tagged == 1 { "repository" } else { "repositories" }
                ),
                Source::Repo(_) => "Its tags will be forgotten as well.".to_string(),
            };
            let question = adw::AlertDialog::builder()
                .heading(format!("Remove {}?", tildify(source.path())))
                .body(body)
                .build();
            question.add_response("cancel", "Cancel");
            question.add_response("remove", "Remove");
            question.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
            question.set_default_response(Some("cancel"));
            question.set_close_response("cancel");
            question.connect_response(None, move |_, response| {
                if response == "remove" {
                    go();
                }
            });
            question.present(Some(&anchor));
        })
    };

    let build_rows: Rc<dyn Fn()> = {
        let cfg = Rc::clone(&cfg);
        let roots_group = roots_group.clone();
        let repos_group = repos_group.clone();
        let listed = Rc::clone(&listed);
        let on_change = Rc::clone(&on_change);
        let reports = Rc::clone(&reports);
        let listed_once = Cell::new(false);
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
                    .use_markup(false)
                    .title("No roots")
                    .subtitle("Nothing will be scanned until you add a folder.")
                    .build();
                roots_group.add(&row);
                added.push((roots_group.clone(), row.upcast()));
            }
            // The reports are of the scan before the dialog opened. Once
            // something in here has changed the roots they are about a list
            // that no longer exists.
            let scanned = !listed_once.replace(true);
            for spec in roots {
                let wanted = crate::config::expand(spec.path_str()).display().to_string();
                let report = reports.iter().find(|report| scanned && report.root == wanted);
                let row = root_row(&spec, report, Rc::clone(&cfg), Rc::clone(&on_change), {
                    let remove = Rc::clone(&remove);
                    let path = crate::config::expand(spec.path_str());
                    move || remove(Source::Root(path.clone()))
                });
                roots_group.add(&row);
                added.push((roots_group.clone(), row.upcast()));
            }

            for entry in extras {
                let row = removable_row(&entry, "Remove this repository", {
                    let remove = Rc::clone(&remove);
                    let path = crate::config::expand(&entry);
                    move || remove(Source::Repo(path.clone()))
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

    let terminal = adw::EntryRow::builder().title("Terminal").show_apply_button(true).build();
    terminal.set_text(cfg.borrow().terminal.as_deref().unwrap_or(""));
    terminal.set_tooltip_text(Some(&format!(
        "Blank follows $TERMINAL. Currently: {}",
        cfg.borrow().terminal_command()
    )));
    tools_group.add(&terminal);

    let runs = adw::EntryRow::builder().title("Run in the terminal").show_apply_button(true).build();
    runs.set_text(cfg.borrow().terminal_runs.as_deref().unwrap_or(""));
    runs.set_tooltip_text(Some(&format!(
        "Started in the repository by “Open in terminal”; the shell is left \
         behind when it exits. Blank follows lazygit when installed, \
         “none” for a plain shell. Currently: {}",
        cfg.borrow().terminal_runs_command().unwrap_or_else(|| "a plain shell".into())
    )));
    tools_group.add(&runs);
    {
        let cfg = Rc::clone(&cfg);
        runs.connect_apply(move |row| {
            let value = row.text().trim().to_string();
            cfg.borrow_mut().terminal_runs = match value.as_str() {
                "" => None,
                "none" => Some(String::new()),
                other => Some(other.to_string()),
            };
            persist(&cfg);
        });
    }

    let editor = adw::EntryRow::builder().title("Editor").show_apply_button(true).build();
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
        .subtitle("Moves origin/* as git fetch does. Never touches your files or branches.")
        .active(cfg.borrow().refresh.fetch)
        .build();
    refresh_group.add(&fetch);

    let scope = adw::EntryRow::builder().title("Only repos tagged").show_apply_button(true).build();
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
        fetch.connect_active_notify(move |switch| {
            cfg.borrow_mut().refresh.fetch = switch.is_active();
            persist(&cfg);
            on_change();
        });
    }
    // The scope was the one refresh setting that did nothing until Enter or
    // the tick was pressed, and it is the last row of the three: the switch
    // above it is live by the time it is typed into. Typed and left, the
    // text stayed in the row and the saved scope stayed empty, which means
    // every repository. So it is also taken when the keyboard leaves the row,
    // and when the dialog closes.
    let commit_scope: Rc<dyn Fn(&adw::EntryRow)> = {
        let cfg = Rc::clone(&cfg);
        let on_change = Rc::clone(&on_refresh);
        Rc::new(move |entry: &adw::EntryRow| {
            let changed = set_scope(&mut cfg.borrow_mut().refresh, &entry.text());
            if changed {
                persist(&cfg);
                on_change();
            }
        })
    };
    {
        let cfg = Rc::clone(&cfg);
        let commit = Rc::clone(&commit_scope);
        scope.connect_apply(move |entry| {
            commit(entry);
            // Show the list as it was understood.
            let understood = cfg.borrow().refresh.only_tags.join(", ");
            if entry.text() != understood {
                entry.set_text(&understood);
            }
        });
    }
    {
        let commit = Rc::clone(&commit_scope);
        let focus = gtk::EventControllerFocus::new();
        // The row is found through the controller: a closure the row owns
        // must not own the row.
        focus.connect_leave(move |controller| {
            if let Some(entry) = controller.widget().and_downcast::<adw::EntryRow>() {
                commit(&entry);
            }
        });
        scope.add_controller(focus);
    }
    {
        let commit = Rc::clone(&commit_scope);
        let scope = scope.clone();
        dialog.connect_closed(move |_| commit(&scope));
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

    let config_path =
        adw::ActionRow::builder().title("Config file").subtitle(tildify(&Config::path())).build();
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

/// Take what the "Only repos tagged" row holds as the refresh scope. Returns
/// whether that changed the scope.
///
/// A row still holding what it was filled in with is left alone, so that a
/// list written into the file by hand is not rewritten by opening Settings
/// and closing it again.
fn set_scope(refresh: &mut Refresh, typed: &str) -> bool {
    if typed == refresh.only_tags.join(", ") {
        return false;
    }
    let tags = meta::parse_list(typed);
    if refresh.only_tags == tags {
        return false;
    }
    refresh.only_tags = tags;
    true
}

fn add_button(label: &str) -> gtk::Button {
    gtk::Button::builder()
        .child(&adw::ButtonContent::builder().icon_name("list-add-symbolic").label(label).build())
        .build()
}

/// One scan root, with its options behind an expander. Options are written
/// straight through on change; only add and remove rebuild the list, so
/// toggling a switch does not collapse the row under the pointer.
fn root_row(
    spec: &RootSpec,
    report: Option<&crate::git::RootReport>,
    cfg: Rc<RefCell<Config>>,
    on_change: Rc<dyn Fn()>,
    on_remove: impl Fn() + 'static,
) -> adw::ExpanderRow {
    let resolved = spec.resolve();
    let path = resolved.path.clone();

    let row = adw::ExpanderRow::builder()
        // A path is not markup, and one with an ampersand in it used to
        // leave its row blank.
        .use_markup(false)
        .title(spec.path_str())
        .subtitle(root_summary(&resolved, report))
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
    let tag = adw::EntryRow::builder().title("Tag what is found here").show_apply_button(true).build();
    tag.set_text(resolved.tag.as_deref().unwrap_or(""));
    let include_self = adw::SwitchRow::builder()
        .title("Show the containing repository")
        .active(resolved.include_self)
        .sensitive(resolved.nested)
        .build();

    row.add_row(&nested);
    row.add_row(&tag);
    row.add_row(&include_self);

    // Set while the first switch is moving the second. Setting a switch runs
    // its handler there and then, as if it had been clicked, and the second
    // one's handler writes what it is given as the user's choice.
    let syncing = Rc::new(Cell::new(false));

    {
        let cfg = Rc::clone(&cfg);
        let on_change = Rc::clone(&on_change);
        let path = path.clone();
        let row = row.clone();
        let include_self = include_self.clone();
        let syncing = Rc::clone(&syncing);
        nested.connect_active_notify(move |switch| {
            let on = switch.is_active();
            let shown = set_nested(&mut cfg.borrow_mut(), &path, on);
            include_self.set_sensitive(on);
            // Show what the config now resolves to, in both directions.
            // Turning this on used to leave the other switch reading on
            // while the container was hidden; turning it off set the other
            // one, whose handler then wrote `include_self = true` over the
            // `None` just written above, saved and rescanned a second time.
            syncing.set(true);
            include_self.set_active(shown);
            syncing.set(false);
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
            // Everything that asks for a tag by name splits on spaces and
            // commas: the tag editor, `tag:` in the filter, the refresh scope.
            // This field took any text, so a root could apply a tag that
            // could be shown and never named, and that the editor took apart.
            if !value.is_empty() && !meta::is_one_tag(&value) {
                entry.add_css_class("error");
                toast(entry, "A tag is one word, with no spaces or commas. Nothing was changed.");
                return;
            }
            entry.remove_css_class("error");
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
            if syncing.get() {
                return;
            }
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

/// Mark the root at `path` as a workspace, or as not one, and say what "Show
/// the containing repository" has to read afterwards: what the root now
/// resolves to.
fn set_nested(cfg: &mut Config, path: &Path, on: bool) -> bool {
    let mut shown = !on;
    cfg.update_root(path, |entry| {
        entry.nested = on;
        // The default flips with `nested`, so an untouched switch should keep
        // following the default rather than freeze.
        if !on {
            entry.include_self = None;
        }
        shown = entry.include_self.unwrap_or(!entry.nested);
    });
    shown
}

/// One line about a root. `report` is what the last scan made of it, when
/// there is one that still applies: how many repositories it gave, or what
/// stood in the way. Without one the line can only say whether the folder is
/// there, which is all it used to say: a root that had quietly stopped giving
/// repositories read exactly like one that gave forty.
fn root_summary(root: &crate::config::Root, report: Option<&crate::git::RootReport>) -> String {
    let mut parts = Vec::new();
    match report {
        Some(report) => match report.says() {
            Some(amiss) => parts.push(amiss),
            None if report.found > 0 => parts.push(format!("{} found", report.found)),
            // Everything it reaches belongs to a narrower root.
            None => parts.push("all under another root".to_string()),
        },
        None if !root.path.exists() => parts.push("missing".to_string()),
        None => {}
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
    if let Some(spec) = cfg.roots.iter().find(|spec| crate::config::expand(spec.path_str()) == path) {
        // The report was for the root as it was scanned, before this change.
        row.set_subtitle(&root_summary(&spec.resolve(), None));
    }
}

fn removable_row(entry: &str, tooltip: &str, on_remove: impl Fn() + 'static) -> adw::ActionRow {
    let expanded = crate::config::expand(entry);
    let row = adw::ActionRow::builder()
        .use_markup(false)
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

fn pick_folder(parent: Option<&gtk::Window>, title: &str, on_pick: impl Fn(PathBuf) + 'static) {
    let dialog = gtk::FileDialog::builder().title(title).modal(true).build();
    dialog.select_folder(parent, gio::Cancellable::NONE, move |result| {
        if let Ok(file) = result
            && let Some(path) = file.path()
        {
            on_pick(path);
        }
    });
}

thread_local! {
    /// The settings dialog that is open, for `persist` to speak through.
    static COMPLAIN_TO: RefCell<Option<glib::WeakRef<adw::PreferencesDialog>>> =
        const { RefCell::new(None) };
}

/// Whether the file was written. When it was not, the dialog says why.
fn persist(cfg: &Rc<RefCell<Config>>) -> bool {
    match cfg.borrow().save() {
        Ok(()) => true,
        Err(err) => {
            complain(&format!("That change was {err}"));
            false
        }
    }
}

/// Say something went wrong, in the settings dialog, until it is dismissed.
fn complain(said: &str) {
    let dialog = COMPLAIN_TO.with(|slot| slot.borrow().as_ref().and_then(|d| d.upgrade()));
    match dialog {
        Some(dialog) => {
            let toast = crate::ui::toast(said);
            toast.set_timeout(0);
            dialog.add_toast(toast);
        }
        None => note!("omarepos: {said}"),
    }
}

/// Something the grid is filled from, as far as taking it out again goes.
#[derive(Clone)]
enum Source {
    Root(PathBuf),
    Repo(PathBuf),
}

impl Source {
    fn path(&self) -> &Path {
        match self {
            Source::Root(path) | Source::Repo(path) => path,
        }
    }

    fn remove_from(&self, cfg: &mut Config) {
        match self {
            Source::Root(path) => cfg.remove_root(path),
            Source::Repo(path) => cfg.remove_extra_repo(path),
        }
    }
}

/// Everywhere the configuration has repositories come from: the roots as
/// configured, not as a command line may have overridden them for this run.
fn sources(cfg: &Config) -> Vec<PathBuf> {
    cfg.roots.iter().map(|spec| crate::config::expand(spec.path_str())).chain(cfg.extra_repos()).collect()
}

fn toast(widget: &impl IsA<gtk::Widget>, text: &str) {
    let mut node = widget.as_ref().parent();
    while let Some(current) = node {
        if let Ok(overlay) = current.clone().downcast::<adw::ToastOverlay>() {
            overlay.add_toast(crate::ui::toast(text));
            return;
        }
        node = current.parent();
    }
    note!("omarepos: {text}");
}

/// Edit the tags on one or more repositories. With several repos the entry
/// starts from the tags they already have in common. `known` is the tags to
/// offer as chips with how many repositories carry each, which is
/// `Meta::tags_among` over the grid.
pub fn edit_tags(
    parent: &impl IsA<gtk::Widget>,
    meta: Rc<RefCell<Meta>>,
    repos: Vec<PathBuf>,
    known: Vec<(String, usize)>,
    on_change: Rc<dyn Fn()>,
    on_refused: Rc<dyn Fn(String)>,
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

    let shared = meta.borrow().shared_tags(&repos);
    // Enter in the field presses Save. An entry row keeps Enter to itself
    // unless told otherwise, so a tag typed and entered went nowhere, under a
    // Save button drawn as the default.
    let entry = adw::EntryRow::builder().title("Tags").activates_default(true).build();
    entry.set_text(&shared.join(", "));

    let group = adw::PreferencesGroup::new();
    group.add(&entry);

    let body = gtk::Box::new(gtk::Orientation::Vertical, 10);
    body.append(&group);

    // Existing tags as one-click chips, so spelling stays consistent. One the
    // entry could not hold is not offered.
    let known: Vec<(String, usize)> = known.into_iter().filter(|(tag, _)| meta::editable(tag)).collect();
    // What the entry may be holding as a single tag: see `parse_edited`.
    let whole: Rc<Vec<String>> =
        Rc::new(shared.iter().cloned().chain(known.iter().map(|(tag, _)| tag.clone())).collect());
    if !known.is_empty() {
        let flow = gtk::FlowBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .max_children_per_line(6)
            .column_spacing(6)
            .row_spacing(6)
            .build();
        for (tag, count) in known {
            let button = gtk::Button::builder().label(format!("{tag} ({count})")).build();
            button.add_css_class("om-tag-chip");
            let entry = entry.clone();
            let whole = Rc::clone(&whole);
            button.connect_clicked(move |_| {
                let mut tags = meta::parse_edited(&entry.text(), &whole);
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
        let wanted = meta::parse_edited(&entry_for_save.text(), &whole);
        {
            let mut meta = meta.borrow_mut();
            meta.replace_shared(&repos, &shared, &wanted);
            if let Err(err) = meta.save() {
                // In the window, not on stderr: the cards now show tags that
                // are not in the file, and the next launch will not have them.
                on_refused(format!("The tags were {err}"));
            }
        }
        on_change();
    });

    // The keyboard starts in the field, after what is already there. Left to
    // itself it can start on Save, where typing a tag does nothing and the
    // first space closes the dialog. And GTK selects the whole of an entry it
    // puts the keyboard in, so that the first key typed would replace the tags
    // the repository has: hence the caret, put at the end once the dialog is
    // up and has done its own focusing.
    dialog.set_focus(Some(&entry));
    {
        let entry = entry.clone();
        dialog.connect_map(move |_| {
            let entry = entry.clone();
            gtk::glib::idle_add_local_once(move || {
                entry.grab_focus_without_selecting();
                entry.set_position(-1);
            });
        });
    }

    dialog.present(Some(parent.as_ref()));
}

/// The halves of the dialog that are about the configuration and not about
/// widgets. Nothing here builds one, so nothing needs a display.
#[cfg(test)]
mod tests {
    use super::*;

    const WORKSPACE: &str = "/work/acme";

    fn with_one_root() -> Config {
        let mut cfg = Config::default();
        cfg.roots = vec![RootSpec::Simple(WORKSPACE.to_string())];
        cfg
    }

    /// "Show the containing repository" was set once, when the row was built.
    /// Turning "Look inside repositories" on changed what the root resolved
    /// to and left that switch where it was, reading on over a subtitle that
    /// said "container hidden".
    #[test]
    fn the_second_workspace_switch_is_told_what_the_root_now_resolves_to() {
        let path = Path::new(WORKSPACE);
        let mut cfg = with_one_root();
        assert!(cfg.roots[0].resolve().include_self, "a plain root shows itself");

        assert!(!set_nested(&mut cfg, path, true), "a workspace hides its container");
        let root = cfg.roots[0].resolve();
        assert!(root.nested && !root.include_self);

        // A choice the user made is kept while the root stays a workspace...
        cfg.update_root(path, |entry| entry.include_self = Some(true));
        assert!(set_nested(&mut cfg, path, true));

        // ...and is not carried out of it: off leaves a bare path, with
        // nothing written that turning it on again would be stuck with.
        assert!(set_nested(&mut cfg, path, false));
        assert!(matches!(cfg.roots[0], RootSpec::Simple(_)), "{:?}", cfg.roots[0]);
        assert!(!set_nested(&mut cfg, path, true));
    }

    /// What leaving the row, or closing the dialog, does with the text in
    /// "Only repos tagged".
    #[test]
    fn the_refresh_scope_is_taken_from_the_row_once_it_has_been_typed_in() {
        let by_hand = ["work", "Side Projects"].map(str::to_string).to_vec();
        let mut refresh = Refresh::default();
        refresh.only_tags = by_hand.clone();

        // Opened and closed: the row holds what it was filled in with, and a
        // list the editor would have split is left as it was written.
        let filled_in = refresh.only_tags.join(", ");
        assert!(!set_scope(&mut refresh, &filled_in));
        assert_eq!(refresh.only_tags, by_hand);

        // Typed and left, without Enter. This used to save nothing, and an
        // empty scope is every repository.
        let mut refresh = Refresh::default();
        assert!(set_scope(&mut refresh, "personal, Tools"));
        assert_eq!(refresh.only_tags, ["personal", "Tools"]);
        // Enter and then closing the dialog is one write, not two.
        assert!(!set_scope(&mut refresh, "personal, Tools"));
        assert!(!set_scope(&mut refresh, " Tools personal,"), "the same list, typed otherwise");

        // Blank still means every repository, when blank is what was typed.
        assert!(set_scope(&mut refresh, ""));
        assert!(refresh.only_tags.is_empty());
    }
}
