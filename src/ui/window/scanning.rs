//! Filling the grid: the scan that reads the fleet, and the warm start that
//! puts the last run's readings up before it has (ADR-0009).
//!
//! A launch reads back what the last scan kept (`crate::snapshot`) and shows
//! it at once. Every card from it is marked as a reading from the past, and
//! the scan replaces each one as it reads that repository again, mark and
//! all. What the last run had and this one does not find goes when the scan
//! ends. Until then nothing is fetched: a pass refuses to start while a scan
//! is running, so nothing acts on a reading that is not current.

use std::collections::HashSet;
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use crate::git::RepoInfo;
use crate::scan::{self, Event};
use crate::ui;

use super::{OnDrop, Ui};

impl Ui {
    /// Put the last run's readings on the grid, before the first scan has
    /// read anything. Not for a one-off look, whose roots are not the
    /// fleet's, and not from a file kept for other roots.
    pub(super) fn warm_start(&self) {
        let scope = {
            let cfg = self.cfg.borrow();
            if cfg.one_off() {
                return;
            }
            crate::snapshot::scope(&cfg.roots(), &cfg.extra_repos(), cfg.depth())
        };
        let Some(earlier) = crate::snapshot::load(&scope) else { return };
        if earlier.repos.is_empty() {
            return;
        }
        *self.from_last_run.borrow_mut() = earlier.repos.iter().map(|repo| repo.path.clone()).collect();
        self.last_run_at.set(Some(earlier.taken));
        *self.repos.borrow_mut() = earlier.repos;
    }

    /// What the status bar says of cards that still show the last run's
    /// reading: how many, and how old that reading is.
    pub(super) fn earlier_part(&self) -> Option<String> {
        let waiting = self.from_last_run.borrow().len();
        let at = self.last_run_at.get()?;
        (waiting > 0).then(|| format!("{waiting} as read {}", ui::relative_time(at)))
    }

    pub(super) fn start_scan(self: &Rc<Self>) {
        self.reread_config();
        let generation = self.generation.get() + 1;
        self.generation.set(generation);
        self.scanning.set(true);
        self.expected.set(0);
        self.scanned.set(0);
        self.spinner.set_visible(true);
        self.spinner.start();
        self.status.set_label("Scanning…");

        // With nothing on screen there is nothing to keep, and cards arriving
        // one at a time beat an empty window. With a grid on screen the scan
        // collects off to the side and the grid is replaced once, at the end.
        // It used to be emptied first and refilled card by card: every refresh
        // blanked the window to "No repositories found", threw the keyboard
        // back to the first card and the scroll to the top, and built the grid
        // once per repository found.
        //
        // Warm, the grid holds what the last run read, and each reading is
        // put in place of the earlier one as it arrives: a card that reads
        // the same is left standing, and only loses its mark.
        let warm = !self.from_last_run.borrow().is_empty();
        let streaming = !warm && self.repos.borrow().is_empty();
        if streaming || warm {
            self.rebuild();
        }

        let (roots, extras, depth, one_off) = {
            let cfg = self.cfg.borrow();
            (cfg.roots(), cfg.extra_repos(), cfg.depth(), cfg.one_off())
        };
        let scope = crate::snapshot::scope(&roots, &extras, depth);
        let (rx, reports) = scan::spawn(roots, extras, depth);
        let ui = Rc::clone(self);
        glib::spawn_future_local(async move {
            // Whatever becomes of this future, the scan it stands for is over
            // when it is, unless a newer scan has taken over meanwhile.
            let _over = OnDrop({
                let ui = Rc::clone(&ui);
                move || {
                    if ui.generation.get() == generation && ui.scanning.replace(false) {
                        ui.spinner.stop();
                        ui.spinner.set_visible(ui.refreshing.get());
                        ui.update_status();
                    }
                }
            });
            // Roots that seed a tag write to the metadata store as repos come
            // in; saving once at the end keeps it to a single write.
            let mut seeded_any = false;
            let mut fresh: Vec<RepoInfo> = Vec::new();
            while let Ok(event) = rx.recv().await {
                if ui.generation.get() != generation {
                    break;
                }
                match event {
                    Event::Discovered(n) => {
                        ui.expected.set(n);
                        // Sent ahead of this event, so they are there.
                        if let Ok(found) = reports.try_recv() {
                            *ui.root_reports.borrow_mut() = found;
                        }
                        ui.update_status();
                    }
                    Event::Repo { info, seed } => {
                        // A run pointed somewhere else by the command line
                        // has no opinion about tags: its roots are not the
                        // configured ones, and "this root gives no tag" would
                        // be read as "take the tag away".
                        if let Some(seed) = seed.filter(|_| !one_off) {
                            let changed = ui.meta.borrow_mut().apply_root_tag(
                                &info.path,
                                &seed.root,
                                seed.tag.as_deref(),
                            );
                            seeded_any |= changed;
                        }
                        if streaming {
                            ui.repos.borrow_mut().push(info);
                            ui.scanned.set(ui.repos.borrow().len());
                            ui.rebuild();
                        } else if warm {
                            ui.from_last_run.borrow_mut().remove(&info.path);
                            {
                                let mut repos = ui.repos.borrow_mut();
                                match repos.iter_mut().find(|repo| repo.path == info.path) {
                                    Some(slot) => *slot = info,
                                    None => repos.push(info),
                                }
                            }
                            ui.scanned.set(ui.scanned.get() + 1);
                            ui.rebuild();
                        } else {
                            fresh.push(info);
                            ui.scanned.set(fresh.len());
                            ui.update_status();
                        }
                    }
                    Event::Done => {
                        if warm {
                            // What the last run had and this one did not
                            // find: moved, deleted, or under a root that has
                            // gone. Its reading is not to outlive the scan.
                            let gone = std::mem::take(&mut *ui.from_last_run.borrow_mut());
                            ui.repos.borrow_mut().retain(|repo| !gone.contains(&repo.path));
                        } else if !streaming {
                            *ui.repos.borrow_mut() = std::mem::take(&mut fresh);
                        }
                        // A tick on a repository the scan no longer finds is
                        // a tick nobody can see or take off, and it went on
                        // feeding Contributions, Search and the tag editor.
                        {
                            let found: HashSet<PathBuf> =
                                ui.repos.borrow().iter().map(|repo| repo.path.clone()).collect();
                            ui.selected.borrow_mut().retain(|path| found.contains(path));
                        }
                        // Pruning after the scan, so a repository that was just
                        // found is never a candidate for removal.
                        if !one_off {
                            seeded_any |= ui.meta.borrow_mut().prune() > 0;
                            ui.reconcile_known();
                            ui.forget_trouble_for_the_departed();
                            let taken = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map_or(0, |since| since.as_secs() as i64);
                            crate::snapshot::save(scope, taken, ui.repos.borrow().clone());
                        }
                        if seeded_any && let Err(err) = ui.meta.borrow().save() {
                            ui::complain(&ui.toasts, &format!("The tags this scan gave were {err}"));
                        }
                        ui.scanning.set(false);
                        ui.spinner.stop();
                        ui.spinner.set_visible(false);
                        ui.rebuild();
                        ui.read_again(false);
                        // The grid takes a keyboard that is nowhere in
                        // use, so the arrow keys work without hunting for
                        // the cards with Tab. Not at launch any more: the
                        // filter has it then, and this stands back for a
                        // text field. It is for the scans after that one,
                        // which leave the keyboard on whatever button
                        // started them. A beat later, because the grid has
                        // to be real before it can take anything.
                        {
                            let ui = Rc::clone(&ui);
                            glib::timeout_add_local_once(std::time::Duration::from_millis(120), move || {
                                ui.focus_grid(false)
                            });
                        }
                        ui.refresh_if_overdue();
                        break;
                    }
                }
            }
        });
    }
}
