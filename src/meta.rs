//! Per-repository metadata that git itself does not hold: right now, tags.
//!
//! Kept in `~/.config/omarepos/repos.toml`, keyed by absolute path, so it
//! survives rescans and never touches the repositories themselves.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::home;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RepoMeta {
    pub tags: Vec<String>,
    /// Seeding records from before it was keyed by root. Read so a tag deleted
    /// under the old scheme is not reapplied; written back only while it still
    /// holds something.
    #[serde(rename = "seeded", skip_serializing_if = "Vec::is_empty")]
    pub seeded_legacy: Vec<String>,
    /// Roots, by the same key as `seeded_by`, whose tag was already on the
    /// repository when they asked for it, typed there by hand. Such a root has
    /// a record like any other, so that deleting the tag still sticks, but the
    /// tag is not its own to take away again. Without this a root given the
    /// name of a hand-made tag removed that tag when it was cleared or
    /// renamed. Ahead of `seeded_by` because the file wants plain values
    /// before tables.
    #[serde(skip_serializing_if = "BTreeSet::is_empty")]
    pub adopted: BTreeSet<String>,
    /// The tag each root applied, keyed by the root's absolute path. Keyed
    /// rather than listed so renaming a root's tag can retire the old one, and
    /// kept after the user deletes a tag so it is not applied again.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub seeded_by: BTreeMap<String, String>,
}

impl RepoMeta {
    /// `adopted` only ever names roots that are in `seeded_by`.
    fn is_empty(&self) -> bool {
        self.tags.is_empty() && self.seeded_legacy.is_empty() && self.seeded_by.is_empty()
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct File {
    repos: BTreeMap<String, RepoMeta>,
}

#[derive(Debug, Clone, Default)]
pub struct Meta {
    repos: BTreeMap<String, RepoMeta>,
    /// Why the file on disk could not be used, when it could not. While this
    /// is set the store is empty and `save` refuses. Tags cannot be worked out
    /// again from anything else, and this file used to be overwritten by the
    /// first scan after a launch that failed to read it.
    pub load_error: Option<String>,
    /// The file as it was last read or written, `None` for no file, as
    /// `Config` keeps for its own: `save` looks before it replaces, so tags
    /// typed into the file by hand while the window is open are not written
    /// over by the next scan that seeds one.
    on_disk: std::cell::RefCell<Option<String>>,
}

pub fn path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".config"));
    base.join("omarepos/repos.toml")
}

/// Tags as read from the file, minus any NUL. A tag goes into labels, and a
/// label takes no NUL: written by hand as `"a\u0000b"`, one would be cut
/// short on every card, and stop a debug build.
fn without_nuls(mut repos: BTreeMap<String, RepoMeta>) -> BTreeMap<String, RepoMeta> {
    for meta in repos.values_mut() {
        for tag in &mut meta.tags {
            tag.retain(|c| c != '\0');
        }
    }
    repos
}

/// Trim, drop empties, and fold case-insensitive duplicates into one spelling.
pub fn normalize(tags: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut out: Vec<String> = Vec::new();
    for tag in tags {
        let tag = tag.trim().trim_matches(',').trim().to_string();
        if tag.is_empty() {
            continue;
        }
        if seen.insert(tag.to_lowercase()) {
            out.push(tag);
        }
    }
    out.sort_by_key(|t| t.to_lowercase());
    out
}

/// Split a comma- or space-separated list as typed into a tag editor.
pub fn parse_list(raw: &str) -> Vec<String> {
    normalize(
        raw.split([',', '\n'])
            .flat_map(|part| part.split_whitespace())
            .map(str::to_string),
    )
}

/// Split what a tag editor holds when it is saved. As `parse_list`, except
/// that a tag already in use (`known`) is kept whole when it stands alone
/// between commas.
///
/// The editor shows tags joined by commas, and a space separates as well. A
/// tag with a space in it, which a root's tag field used to accept, was
/// therefore taken apart by opening the editor and pressing Save: `open
/// source` came back as `open` and `source`.
pub fn parse_edited(raw: &str, known: &[String]) -> Vec<String> {
    normalize(raw.split([',', '\n']).flat_map(|part| {
        let part = part.trim();
        match known.iter().find(|tag| tag.eq_ignore_ascii_case(part)) {
            Some(tag) => vec![tag.clone()],
            None => part.split_whitespace().map(str::to_string).collect(),
        }
    }))
}

/// Whether a tag editor can show `tag` and read the same tag back. One with a
/// comma in it cannot be told from two, so the editor leaves it alone.
pub fn editable(tag: &str) -> bool {
    !tag.contains([',', '\n'])
}

/// Whether `tag` is a single tag to everything that asks for tags by name:
/// the editor, `tag:` in the filter and the refresh scope all split on spaces
/// and commas. This is what a root's tag is held to.
pub fn is_one_tag(tag: &str) -> bool {
    parse_list(tag) == [tag]
}

/// A directory as written and as it resolves. The store holds resolved paths,
/// because a scan resolves what it finds, and a configured root is held as the
/// user wrote it; the two differ when a root is reached through a symlink.
fn spellings(dir: &Path) -> Vec<PathBuf> {
    let mut both = vec![dir.to_path_buf()];
    if let Ok(real) = dir.canonicalize() {
        if real != dir {
            both.push(real);
        }
    }
    both
}

impl Meta {
    pub fn load() -> Self {
        let path = path();
        let (read, text) = Self::read_file(&path);
        let meta = read.unwrap_or_else(|why| {
            note!("omarepos: ignoring {}: {why}", path.display());
            Self { load_error: Some(crate::config::why_unusable(&path, &why)), ..Self::default() }
        });
        Self { on_disk: std::cell::RefCell::new(text), ..meta }
    }

    /// [`Meta::load`] for a run with no window to show a banner in: the
    /// store, or why the file cannot be used in the parser's own words, and
    /// nothing said on stderr. `--json` is one, and an empty store in place
    /// of a file that did not parse would have it print every repository as
    /// carrying no tags.
    pub fn try_load() -> Result<Self, String> {
        let (read, text) = Self::read_file(&path());
        read.map(|meta| Self { on_disk: std::cell::RefCell::new(text), ..meta })
    }

    /// The file at `path`: the store it holds, or why it cannot be used; and
    /// its text, when there was any to read.
    fn read_file(path: &Path) -> (Result<Self, String>, Option<String>) {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            // No file yet: nothing has been tagged.
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return (Ok(Self::default()), None);
            }
            Err(err) => return (Err(err.to_string()), None),
        };
        let read = toml::from_str::<File>(&text)
            .map(|file| Self { repos: without_nuls(file.repos), ..Self::default() })
            .map_err(|err| err.to_string());
        (read, Some(text))
    }

    /// Read the file again if it is not what was last read or written.
    pub fn reread(&mut self) -> crate::config::Reread {
        use crate::config::Reread;
        let path = path();
        let disk = std::fs::read_to_string(&path).ok();
        if disk == *self.on_disk.borrow() {
            return Reread::Unchanged;
        }
        let parsed = match &disk {
            None => Ok(File::default()),
            Some(text) => toml::from_str::<File>(text).map_err(|err| err.to_string()),
        };
        match parsed {
            Ok(file) => {
                *self = Self { repos: without_nuls(file.repos), ..Self::default() };
                *self.on_disk.borrow_mut() = disk;
                Reread::Loaded
            }
            Err(why) => {
                // The tags in memory stay; the file is not saved over.
                let why = crate::config::why_unusable(&path, &why);
                self.load_error = Some(why.clone());
                *self.on_disk.borrow_mut() = disk;
                Reread::Broken(why)
            }
        }
    }

    fn key(repo: &Path) -> String {
        repo.display().to_string()
    }

    pub fn tags_for(&self, repo: &Path) -> &[String] {
        self.repos
            .get(&Self::key(repo))
            .map(|m| m.tags.as_slice())
            .unwrap_or(&[])
    }

    pub fn set_tags(&mut self, repo: &Path, tags: Vec<String>) {
        let tags = normalize(tags);
        let key = Self::key(repo);
        if !tags.is_empty() {
            self.repos.entry(key).or_default().tags = tags;
            return;
        }
        // An entry with nothing but a seeding record still has to stay, or the
        // tag the user just removed would reappear on the next scan.
        if let Some(meta) = self.repos.get_mut(&key) {
            meta.tags.clear();
            if meta.is_empty() {
                self.repos.remove(&key);
            }
        }
    }

    /// The tags every one of `repos` carries, which is what a tag editor
    /// opened on them starts from. A tag the editor could not read back is
    /// left out, and so is not among the ones a save replaces.
    pub fn shared_tags(&self, repos: &[PathBuf]) -> Vec<String> {
        let mut iter = repos.iter();
        let Some(first) = iter.next() else { return Vec::new() };
        let mut shared: Vec<String> = self.tags_for(first).to_vec();
        for repo in iter {
            let theirs = self.tags_for(repo);
            shared.retain(|tag| theirs.iter().any(|t| t.eq_ignore_ascii_case(tag)));
        }
        shared.retain(|tag| editable(tag));
        shared
    }

    /// Save a tag editor: on each of `repos`, `wanted` takes the place of the
    /// tags the editor opened with (`shared`). Anything else a repository
    /// carries is kept, so editing several at once cannot silently drop their
    /// own tags.
    pub fn replace_shared(&mut self, repos: &[PathBuf], shared: &[String], wanted: &[String]) {
        for repo in repos {
            let mut tags: Vec<String> = self
                .tags_for(repo)
                .iter()
                .filter(|t| !shared.iter().any(|s| s.eq_ignore_ascii_case(t)))
                .cloned()
                .collect();
            tags.extend(wanted.iter().cloned());
            self.set_tags(repo, tags);
        }
    }

    /// Reconcile one repository with what `root` currently wants tagged on it.
    /// `tag` of `None` means the root no longer tags anything, which retires
    /// whatever it applied before. Returns whether anything changed, so a scan
    /// can save once at the end.
    ///
    /// The root is the unit of bookkeeping: knowing that `~/work/acme` applied
    /// `acme` is what lets a rename to `client-x` remove `acme` instead of leaving
    /// both behind. A tag the user deleted stays deleted, because the record
    /// survives the deletion.
    ///
    /// A root only ever takes away a tag it put there. One that was on the
    /// repository already — typed by hand — is recorded as adopted and left
    /// where it is, and one that another root still records stays for that
    /// root. Retiring went by name alone before, so clearing a root's tag also
    /// removed the same tag wherever the user had applied it themselves.
    pub fn apply_root_tag(&mut self, repo: &Path, root: &str, tag: Option<&str>) -> bool {
        let tag = tag.map(str::trim).filter(|t| !t.is_empty());
        let key = Self::key(repo);
        let previous = self
            .repos
            .get(&key)
            .and_then(|meta| meta.seeded_by.get(root))
            .cloned();

        match (&previous, tag) {
            // Already carries this root's current tag.
            (Some(before), Some(now)) if before.eq_ignore_ascii_case(now) => return false,
            // Nothing applied before, nothing to apply now.
            (None, None) => return false,
            _ => {}
        }

        // Under the old flat scheme we cannot tell which root applied a tag,
        // only that it was applied once. Honour that: record the root so future
        // renames work, but do not re-add a tag the user has since deleted.
        let offered_before = previous.is_none()
            && tag.is_some_and(|t| {
                self.repos.get(&key).is_some_and(|meta| {
                    meta.seeded_legacy.iter().any(|seen| seen.eq_ignore_ascii_case(t))
                })
            });

        // Renaming a root's tag renames it in place. If the old tag is already
        // gone the user deleted it, and there is nothing to rename — handing
        // them the new name would resurrect a tag they rejected.
        let had_old = previous.as_ref().is_some_and(|before| {
            self.repos.get(&key).is_some_and(|meta| {
                meta.tags.iter().any(|t| t.eq_ignore_ascii_case(before))
            })
        });
        let should_add = !offered_before && (previous.is_none() || had_old);

        let entry = self.repos.entry(key.clone()).or_default();
        if let Some(before) = &previous {
            let adopted = entry.adopted.remove(root);
            let also_recorded_by: Vec<String> = entry
                .seeded_by
                .iter()
                .filter(|(other, theirs)| {
                    other.as_str() != root && theirs.eq_ignore_ascii_case(before)
                })
                .map(|(other, _)| other.clone())
                .collect();
            if adopted {
                // Never this root's: it was there before the root asked for it.
            } else if !also_recorded_by.is_empty() {
                // This root's, and now theirs. They hold it as their own from
                // here on, so the last of them to let go still removes it.
                for other in &also_recorded_by {
                    entry.adopted.remove(other);
                }
            } else {
                entry.tags.retain(|t| !t.eq_ignore_ascii_case(before));
                if had_old {
                    // We just took that tag off ourselves, so any record saying
                    // it once went missing is ours too. Leaving it would make
                    // setting the same tag again silently do nothing. A record
                    // left behind when `had_old` is false is the real thing:
                    // the user removed the tag, and that still stands.
                    entry.seeded_legacy.retain(|t| !t.eq_ignore_ascii_case(before));
                }
            }
        }
        match tag {
            Some(tag) => {
                let present = entry.tags.iter().any(|t| t.eq_ignore_ascii_case(tag));
                // Put there by a root: the flat record says one seeded it, or
                // another root holds it as its own.
                let from_a_root = entry
                    .seeded_legacy
                    .iter()
                    .any(|seen| seen.eq_ignore_ascii_case(tag))
                    || entry.seeded_by.iter().any(|(other, theirs)| {
                        other.as_str() != root
                            && theirs.eq_ignore_ascii_case(tag)
                            && !entry.adopted.contains(other)
                    });
                entry.seeded_by.insert(root.to_string(), tag.to_string());
                if present {
                    if !from_a_root {
                        entry.adopted.insert(root.to_string());
                    }
                } else if should_add {
                    let mut tags = entry.tags.clone();
                    tags.push(tag.to_string());
                    entry.tags = normalize(tags);
                }
            }
            None => {
                entry.seeded_by.remove(root);
            }
        }
        if entry.is_empty() {
            self.repos.remove(&key);
        }
        true
    }

    /// Forget repositories that are gone. Only entries whose *parent* directory
    /// still exists are dropped: a path that has vanished along with everything
    /// around it is more likely an unmounted disk than a deleted repository, and
    /// losing someone's tags to a disk that is merely offline is not worth the
    /// tidiness. Returns how many entries were dropped.
    ///
    /// The parent has to have something in it as well. A mount point from
    /// fstab stays behind as an empty directory when its disk is away, so "the
    /// parent exists" alone dropped every repository that sat directly under
    /// one. The price is an entry that lingers when the last repository in a
    /// folder is deleted and the folder is kept.
    pub fn prune(&mut self) -> usize {
        let before = self.repos.len();
        self.repos.retain(|key, _| {
            let path = Path::new(key);
            if path.exists() {
                return true;
            }
            match path.parent() {
                // A directory that cannot be listed proves nothing either way.
                Some(parent) => std::fs::read_dir(parent)
                    .map(|mut entries| entries.next().is_none())
                    .unwrap_or(true),
                None => true,
            }
        });
        before - self.repos.len()
    }

    /// The tags carried by `repos`, sorted, with how many of them carry each.
    ///
    /// This is the one to build chips and counts from, handed the repositories
    /// on the grid. The store is wider than the grid: it keeps what it knows
    /// about a repository no root reaches any more, and about one whose disk
    /// is away. Counted over the store, a tag none of whose repositories was
    /// still scanned stayed in the header with its old count, counted towards
    /// the three-tag limit, and filtered to an empty grid.
    pub fn tags_among<'a>(
        &self,
        repos: impl IntoIterator<Item = &'a Path>,
    ) -> Vec<(String, usize)> {
        let mut counts: BTreeMap<String, (String, usize)> = BTreeMap::new();
        for repo in repos {
            for tag in self.tags_for(repo) {
                let entry = counts
                    .entry(tag.to_lowercase())
                    .or_insert_with(|| (tag.clone(), 0));
                entry.1 += 1;
            }
        }
        counts.into_values().collect()
    }

    /// The entries a removed source leaves behind: those nothing in `kept`
    /// reaches any more, and those something still does.
    ///
    /// An entry is the source's when its path lies under it, or when the
    /// source is on record as having tagged it. The record is what still
    /// answers for a root on a disk that has since gone, whose path no longer
    /// resolves to where its repositories were found.
    fn left_behind(&self, gone: &Path, kept: &[PathBuf]) -> (Vec<String>, Vec<String>) {
        let root = gone.display().to_string();
        let gone = spellings(gone);
        let kept: Vec<PathBuf> = kept.iter().flat_map(|dir| spellings(dir)).collect();
        let mut orphaned = Vec::new();
        let mut reached = Vec::new();
        for (key, meta) in &self.repos {
            let path = Path::new(key);
            let theirs = gone.iter().any(|dir| path.starts_with(dir))
                || meta.seeded_by.contains_key(&root);
            if !theirs {
                continue;
            }
            if kept.iter().any(|dir| path.starts_with(dir)) {
                reached.push(key.clone());
            } else {
                orphaned.push(key.clone());
            }
        }
        (orphaned, reached)
    }

    /// How many tagged repositories `forget_source` would forget outright,
    /// for asking before it does.
    pub fn tagged_only_under(&self, gone: &Path, kept: &[PathBuf]) -> usize {
        let (orphaned, _) = self.left_behind(gone, kept);
        orphaned
            .iter()
            .filter(|key| self.repos.get(*key).is_some_and(|meta| !meta.tags.is_empty()))
            .count()
    }

    /// Forget what a scan root, or an individually listed repository, brought
    /// in, once the user has removed it in Settings. `kept` is every source
    /// that is still configured. Returns how many entries changed.
    ///
    /// Removing a root used to leave the path and tags of every repository
    /// under it in the file for good: no scan visits them again, so nothing
    /// retires a tag, and `prune` keeps whatever is still on disk or went
    /// along with its whole directory. A repository only the removed source
    /// reached is forgotten whole, hand-made tags included, which is what
    /// "stop looking here" has to mean for the file to stop naming it. One
    /// that something else still reaches keeps its entry and loses what the
    /// removed root applied, as when the root's tag is cleared.
    ///
    /// Deliberately not done from a scan, by dropping whatever no configured
    /// root contains: `--root` and a config that failed to parse both scan
    /// with roots that are not the user's, and would take every tag with them.
    pub fn forget_source(&mut self, gone: &Path, kept: &[PathBuf]) -> usize {
        let (orphaned, reached) = self.left_behind(gone, kept);
        let root = gone.display().to_string();
        let mut changed = orphaned.len();
        for key in &orphaned {
            self.repos.remove(key);
        }
        for key in &reached {
            changed += usize::from(self.apply_root_tag(Path::new(key), &root, None));
        }
        changed
    }

    pub fn save(&self) -> std::io::Result<()> {
        let path = path();
        crate::config::refuse_to_replace(&self.load_error)?;
        if std::fs::read_to_string(&path).ok() != *self.on_disk.borrow() {
            return Err(std::io::Error::other(format!(
                "not saved: {} has been changed outside omarepos since it was read. \
                 Rescan (Ctrl+R) to read it again, then make the change once more.",
                crate::config::tildify(&path)
            )));
        }
        let file = File { repos: self.repos.clone() };
        let body = toml::to_string_pretty(&file)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        let text = format!("# omarepos repository metadata — written by the app.\n\n{body}");
        crate::config::replace_file(&path, &text)?;
        *self.on_disk.borrow_mut() = Some(text);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::*;

    #[test]
    fn tags_typed_into_the_file_are_read_again_and_never_saved_over() {
        let _guard = isolate();
        let file = path();
        let _ = std::fs::remove_file(&file);
        let repo = Path::new("/srv/fixtures/billing");

        let mut meta = Meta::load();
        meta.set_tags(repo, vec!["work".to_string()]);
        meta.save().unwrap();
        assert_eq!(meta.reread(), crate::config::Reread::Unchanged, "its own save is not an edit");

        // By hand, with the window open.
        let edited = read(&file).replace("work", "client");
        write(&file, &edited);

        meta.set_tags(repo, vec!["urgent".to_string()]);
        let refused = meta.save().expect_err("the file is not what was read");
        assert!(refused.to_string().contains("changed outside"), "{refused}");
        assert_eq!(read(&file), edited);

        assert_eq!(meta.reread(), crate::config::Reread::Loaded);
        assert_eq!(meta.tags_for(repo), ["client"]);

        // Half-edited: what is in memory stays, and is not written.
        write(&file, "[repos.\"/srv/fixtures/billing\"]\ntags = [");
        assert!(matches!(meta.reread(), crate::config::Reread::Broken(_)));
        assert_eq!(meta.tags_for(repo), ["client"]);
        assert!(meta.save().is_err());

        write(&file, &edited);
        assert_eq!(meta.reread(), crate::config::Reread::Loaded);
        assert_eq!(meta.load_error, None);
        meta.save().unwrap();

        let _ = std::fs::remove_file(&file);
    }

    #[test]
    fn tags_survive_being_saved_and_loaded() {
        let _guard = isolate();
        let _ = std::fs::remove_file(path());
        let repo = Path::new("/home/me/code/app");

        let mut meta = Meta::load();
        assert_eq!(meta.load_error, None, "no file yet means nothing tagged, not a failure");
        meta.set_tags(repo, vec!["work".to_string(), "rust".to_string()]);
        meta.save().unwrap();

        assert_eq!(Meta::load().tags_for(repo), ["rust".to_string(), "work".to_string()]);
        let _ = std::fs::remove_file(path());
    }

    /// Tags cannot be worked out again from anything else. A launch that could
    /// not read the file used to start from an empty store, and the first scan
    /// under a tagged root then wrote that empty store back.
    #[test]
    fn a_tag_file_that_cannot_be_read_is_not_replaced_by_the_next_scan() {
        let _guard = isolate();
        let written = "[repos.\"/home/me/code/app\"]\ntags = [\"work\"\n";
        write(&path(), written);

        let mut meta = Meta::load();
        let why = meta.load_error.clone().expect("the failure is remembered");
        assert!(why.contains("repos.toml"), "{why}");
        // Where there is no banner, the failure, and no empty store.
        let refused = Meta::try_load().expect_err("not an empty store");
        assert!(refused.contains("TOML parse error"), "{refused}");

        // What a scan does when the root it found the repository under has a tag.
        meta.apply_root_tag(Path::new("/home/me/code/app"), "/home/me/code", Some("JM"));
        assert!(meta.save().is_err());
        assert_eq!(read(&path()), written);

        let _ = std::fs::remove_file(path());
    }

    const ROOT: &str = "/home/me/work/acme";

    fn repo(name: &str) -> PathBuf {
        Path::new(ROOT).join(name)
    }

    /// The table in the README, row by row, for a root that put its own tag
    /// on what it found.
    #[test]
    fn a_root_tag_is_reconciled_the_way_the_readme_says() {
        let (api, web) = (repo("api"), repo("web"));
        let mut meta = Meta::default();

        // Clone a new repo into the workspace: tagged on the next scan, and
        // the scan after that has nothing to do.
        assert!(meta.apply_root_tag(&api, ROOT, Some("acme")));
        assert!(meta.apply_root_tag(&web, ROOT, Some("acme")));
        assert_eq!(meta.tags_for(&api), ["acme"]);
        assert!(!meta.apply_root_tag(&api, ROOT, Some("acme")));

        // Delete the tag from one repo: it stays deleted.
        meta.set_tags(&web, Vec::new());
        assert!(!meta.apply_root_tag(&web, ROOT, Some("acme")));
        assert!(meta.tags_for(&web).is_empty());

        // Rename the root's tag: renamed in place, and not resurrected where
        // it had been deleted.
        meta.apply_root_tag(&api, ROOT, Some("client-x"));
        meta.apply_root_tag(&web, ROOT, Some("client-x"));
        assert_eq!(meta.tags_for(&api), ["client-x"]);
        assert!(meta.tags_for(&web).is_empty());

        // Clear the root's tag: retired from every repo under it, and nothing
        // is kept about a repository there is nothing left to say about.
        meta.apply_root_tag(&api, ROOT, None);
        meta.apply_root_tag(&web, ROOT, None);
        assert!(meta.tags_for(&api).is_empty());
        assert!(meta.repos.is_empty(), "{:?}", meta.repos);
    }

    /// A tag applied under the flat scheme, before the root was recorded, is
    /// still the root's: the record of it having been seeded says so.
    #[test]
    fn a_tag_seeded_before_roots_were_recorded_is_still_the_roots_to_retire() {
        let api = repo("api");
        let mut meta = Meta::default();
        meta.repos.insert(
            Meta::key(&api),
            RepoMeta {
                tags: vec!["acme".to_string()],
                seeded_legacy: vec!["acme".to_string()],
                ..RepoMeta::default()
            },
        );

        meta.apply_root_tag(&api, ROOT, Some("acme"));
        assert_eq!(meta.tags_for(&api), ["acme"]);
        meta.apply_root_tag(&api, ROOT, None);
        assert!(meta.tags_for(&api).is_empty());
    }

    /// The last row of the README's table: "Tag a repo yourself: never touched
    /// by any of the above". A root given the same tag afterwards used to
    /// record the tag as its own, and take it away again when it was cleared.
    #[test]
    fn a_hand_made_tag_survives_the_root_clearing_the_same_tag() {
        let api = repo("api");
        let mut meta = Meta::default();
        meta.set_tags(&api, vec!["acme".to_string(), "mine".to_string()]);

        meta.apply_root_tag(&api, ROOT, Some("acme"));
        assert_eq!(meta.tags_for(&api), ["acme", "mine"]);
        meta.apply_root_tag(&api, ROOT, None);
        assert_eq!(meta.tags_for(&api), ["acme", "mine"]);
    }

    /// Whose tag it is has to outlive the session that noticed.
    #[test]
    fn a_hand_made_tag_is_still_the_users_after_a_restart() {
        let _guard = isolate();
        let _ = std::fs::remove_file(path());
        let api = repo("api");
        let mut meta = Meta::default();
        meta.set_tags(&api, vec!["acme".to_string()]);
        meta.apply_root_tag(&api, ROOT, Some("acme"));
        meta.save().unwrap();

        let mut meta = Meta::load();
        assert_eq!(meta.load_error, None, "{}", read(&path()));
        meta.apply_root_tag(&api, ROOT, None);
        assert_eq!(meta.tags_for(&api), ["acme"], "{}", read(&path()));

        let _ = std::fs::remove_file(path());
    }

    /// The same, for a rename: it turned the hand-made tag into the new name.
    #[test]
    fn a_hand_made_tag_survives_the_root_renaming_the_same_tag() {
        let api = repo("api");
        let mut meta = Meta::default();
        meta.set_tags(&api, vec!["acme".to_string()]);

        meta.apply_root_tag(&api, ROOT, Some("acme"));
        meta.apply_root_tag(&api, ROOT, Some("client-x"));
        assert_eq!(meta.tags_for(&api), ["acme", "client-x"]);

        // The new name is the root's own, and goes when the root says so.
        meta.apply_root_tag(&api, ROOT, None);
        assert_eq!(meta.tags_for(&api), ["acme"]);
    }

    /// A root that renames onto a tag typed by hand has not applied it either.
    #[test]
    fn a_root_renamed_onto_a_hand_made_tag_does_not_take_it_along_later() {
        let api = repo("api");
        let mut meta = Meta::default();
        meta.apply_root_tag(&api, ROOT, Some("acme"));
        meta.set_tags(&api, vec!["acme".to_string(), "client-x".to_string()]);

        meta.apply_root_tag(&api, ROOT, Some("client-x"));
        assert_eq!(meta.tags_for(&api), ["client-x"]);
        meta.apply_root_tag(&api, ROOT, None);
        assert_eq!(meta.tags_for(&api), ["client-x"]);
    }

    /// Two roots can both have recorded a repository: one contains the other,
    /// and which of them a scan reports it under has changed over time.
    #[test]
    fn two_roots_seeding_the_same_tag_do_not_take_it_from_each_other() {
        let api = repo("api");
        let wider = "/home/me/work";
        let mut meta = Meta::default();
        meta.apply_root_tag(&api, ROOT, Some("acme"));
        meta.apply_root_tag(&api, wider, Some("acme"));

        meta.apply_root_tag(&api, ROOT, None);
        assert_eq!(meta.tags_for(&api), ["acme"], "the wider root still wants it");

        // ...and it is not left behind for good once the last of them lets go.
        meta.apply_root_tag(&api, wider, None);
        assert!(meta.tags_for(&api).is_empty());
        assert!(meta.repos.is_empty(), "{:?}", meta.repos);
    }

    /// What the tag editor does between opening and Save, without the dialog.
    fn edit(meta: &mut Meta, repos: &[PathBuf], typing: impl FnOnce(String) -> String) {
        let shared = meta.shared_tags(repos);
        let typed = typing(shared.join(", "));
        let wanted = parse_edited(&typed, &shared);
        meta.replace_shared(repos, &shared, &wanted);
    }

    /// A root's tag field took any text, and the editor shows tags joined by
    /// commas and reads them back split on spaces as well. So a root tagged
    /// `open source` gave every repository one tag, and the first visit to the
    /// editor, with nothing changed, left it with `open` and `source`. The
    /// root's record still said `open source`, so no scan put it right.
    #[test]
    fn opening_the_tag_editor_and_pressing_save_changes_nothing() {
        let api = repo("api");
        let mut meta = Meta::default();
        meta.apply_root_tag(&api, ROOT, Some("open source"));
        // The last one is what a file edited by hand can hold.
        let by_hand = ["open source", "rust", "a,b"].map(str::to_string).to_vec();
        meta.set_tags(&api, by_hand);
        let before = meta.tags_for(&api).to_vec();
        assert_eq!(before, ["a,b", "open source", "rust"]);

        edit(&mut meta, &[api.clone()], |shown| shown);
        assert_eq!(meta.tags_for(&api), before);

        // An edit made beside them leaves them whole as well...
        edit(&mut meta, &[api.clone()], |shown| format!("{shown}, cli"));
        assert_eq!(meta.tags_for(&api), ["a,b", "cli", "open source", "rust"]);
        assert!(!meta.apply_root_tag(&api, ROOT, Some("open source")));

        // ...and a tag of two words comes off the way any other does.
        edit(&mut meta, &[api.clone()], |shown| shown.replace("open source", ""));
        assert_eq!(meta.tags_for(&api), ["a,b", "cli", "rust"]);
    }

    #[test]
    fn a_space_still_separates_tags_nobody_has_used_as_one() {
        let known = ["open source".to_string()];
        assert_eq!(
            parse_edited("side projects, Open Source,rust", &known),
            ["open source", "projects", "rust", "side"]
        );
        assert_eq!(parse_edited("side projects", &[]), parse_list("side projects"));
    }

    /// Editing several repositories at once only replaces what they share.
    #[test]
    fn editing_several_repositories_keeps_what_only_one_of_them_has() {
        let (api, web) = (repo("api"), repo("web"));
        let mut meta = Meta::default();
        meta.set_tags(&api, vec!["acme".to_string(), "rust".to_string()]);
        meta.set_tags(&web, vec!["Acme".to_string(), "ruby".to_string()]);

        let both = [api.clone(), web.clone()];
        assert_eq!(meta.shared_tags(&both), ["acme"]);
        edit(&mut meta, &both, |_| "client-x".to_string());
        assert_eq!(meta.tags_for(&api), ["client-x", "rust"]);
        assert_eq!(meta.tags_for(&web), ["client-x", "ruby"]);
    }

    /// What the root's tag field in Settings holds a value to. Everything
    /// that names a tag (the editor, `tag:` in the filter, the refresh scope)
    /// splits on spaces and commas, so a tag with either in it can be shown
    /// and never asked for.
    #[test]
    fn a_root_tag_is_one_word() {
        for tag in ["acme", "client-x", "r&d", "Größe", "a.b_c"] {
            assert!(is_one_tag(tag), "{tag:?}");
            // ...which is what makes it survive being shown and read back.
            let pair = [tag, "other"];
            assert_eq!(parse_list(&pair.join(", ")), normalize(pair.map(str::to_string)));
        }
        for tag in ["open source", "a,b", "a, b", " acme", "acme,", "tab\tbed", "", ","] {
            assert!(!is_one_tag(tag), "{tag:?}");
        }
    }

    /// The store is wider than the grid: it keeps what it knows about a
    /// repository whose root was removed, or whose disk is away. The header
    /// chips were counted over the store, so a tag with nothing left on the
    /// grid stayed as a chip, with its old count, and filtered to nothing.
    #[test]
    fn tags_are_counted_over_the_repositories_asked_about_not_the_whole_store() {
        let (api, web) = (repo("api"), repo("web"));
        let left = PathBuf::from("/home/me/work/former/app");
        let mut meta = Meta::default();
        meta.set_tags(&api, vec!["acme".to_string(), "rust".to_string()]);
        meta.set_tags(&web, vec!["Acme".to_string()]);
        meta.set_tags(&left, vec!["acme".to_string(), "former".to_string()]);
        let counted = |tags: &[(&str, usize)]| -> Vec<(String, usize)> {
            tags.iter().map(|(tag, count)| (tag.to_string(), *count)).collect()
        };

        let grid = [api.as_path(), web.as_path(), Path::new("/home/me/code/untagged")];
        assert_eq!(meta.tags_among(grid), counted(&[("acme", 2), ("rust", 1)]));
        assert_eq!(meta.tags_among([left.as_path()]), counted(&[("acme", 1), ("former", 1)]));
        assert!(meta.tags_among(std::iter::empty()).is_empty());
    }

    fn stored(meta: &Meta) -> Vec<&str> {
        meta.repos.keys().map(String::as_str).collect()
    }

    /// "Remove this root" took the root out of the config and left in
    /// repos.toml the path and the tags of every repository it had brought
    /// in, for good: nothing scans them again, and `prune` keeps anything
    /// that is still on disk, or that went along with its whole directory.
    #[test]
    fn removing_a_root_forgets_what_only_it_reached() {
        let mut meta = Meta::default();
        meta.apply_root_tag(&repo("api"), ROOT, Some("acme"));
        meta.apply_root_tag(&repo("web"), ROOT, Some("acme"));
        // Tagged by hand, and nothing from the root.
        meta.set_tags(&repo("notes"), vec!["mine".to_string()]);
        // Only a record: the root's tag was deleted here.
        meta.apply_root_tag(&repo("infra"), ROOT, Some("acme"));
        meta.set_tags(&repo("infra"), Vec::new());
        // Under another root, and under a folder whose name merely starts
        // the same way.
        meta.set_tags(Path::new("/home/me/code/app"), vec!["acme".to_string()]);
        meta.set_tags(Path::new("/home/me/work/acme-old/app"), vec!["acme".to_string()]);

        let kept = [PathBuf::from("/home/me/code")];
        assert_eq!(meta.tagged_only_under(Path::new(ROOT), &kept), 3);
        assert_eq!(meta.forget_source(Path::new(ROOT), &kept), 4);
        assert_eq!(stored(&meta), ["/home/me/code/app", "/home/me/work/acme-old/app"]);
        assert_eq!(meta.forget_source(Path::new(ROOT), &kept), 0, "and once is enough");
    }

    /// A repository something else still reaches keeps its entry and its
    /// hand-made tags. What the removed root applied goes with the root, as
    /// it does when the root's tag is cleared: nothing would retire it later.
    #[test]
    fn removing_a_root_inside_another_takes_only_its_own_tag_along() {
        let api = repo("api");
        let mut meta = Meta::default();
        meta.apply_root_tag(&api, ROOT, Some("acme"));
        meta.set_tags(&api, vec!["acme".to_string(), "mine".to_string()]);

        let kept = [PathBuf::from("/home/me/work")];
        assert_eq!(meta.tagged_only_under(Path::new(ROOT), &kept), 0);
        assert_eq!(meta.forget_source(Path::new(ROOT), &kept), 1);
        assert_eq!(meta.tags_for(&api), ["mine"]);
        assert!(meta.repos[&Meta::key(&api)].seeded_by.is_empty());

        // An individually listed repository is a source too: the one under
        // the removed root that is also listed by itself stays.
        let mut meta = Meta::default();
        meta.set_tags(&api, vec!["mine".to_string()]);
        meta.set_tags(&repo("web"), vec!["mine".to_string()]);
        assert_eq!(meta.forget_source(Path::new(ROOT), std::slice::from_ref(&api)), 1);
        assert_eq!(stored(&meta), ["/home/me/work/acme/api"]);
        // ...until it is removed in its turn.
        assert_eq!(meta.forget_source(&api, &[]), 1);
        assert!(meta.repos.is_empty());
    }

    /// The store holds resolved paths, because a scan resolves what it finds;
    /// a root is held as the user wrote it. Comparing the two as text would
    /// forget nothing under a root reached through a symlink.
    #[test]
    fn a_root_reached_through_a_symlink_is_forgotten_all_the_same() {
        let _guard = isolate();
        let base = scratch("meta-forget-symlink");
        std::fs::create_dir_all(base.join("disk/acme/api")).unwrap();
        std::os::unix::fs::symlink(base.join("disk/acme"), base.join("acme")).unwrap();
        let written = base.join("acme");
        let found = written.join("api").canonicalize().unwrap();
        assert!(!found.starts_with(&written), "the fixture is not a symlink after all");

        let mut meta = Meta::default();
        meta.set_tags(&found, vec!["mine".to_string()]);
        // Seeded by the root, and its disk has gone since: the path proves
        // nothing any more, the record still does.
        let offline = Path::new("/mnt/unplugged/acme/web");
        meta.apply_root_tag(offline, &written.display().to_string(), Some("acme"));

        assert_eq!(meta.tagged_only_under(&written, &[]), 2);
        assert_eq!(meta.forget_source(&written, &[]), 2);
        assert!(meta.repos.is_empty(), "{:?}", meta.repos);
    }

    /// The unmounted disk the doc comment is about. A mount point from fstab
    /// stays behind as an empty directory, so "the parent is still there" was
    /// true of every repository directly under it, and their tags were dropped
    /// at the end of the first scan without the disk.
    #[test]
    fn prune_keeps_tags_for_repositories_directly_under_an_empty_mount_point() {
        let _guard = isolate();
        let base = scratch("meta-prune");
        let mount = base.join("mnt/disk");
        std::fs::create_dir_all(&mount).unwrap();
        std::fs::create_dir_all(base.join("code/still-here/.git")).unwrap();

        let mut meta = Meta::default();
        let tag = || vec!["work".to_string()];
        meta.set_tags(&mount.join("app"), tag());
        meta.set_tags(&base.join("code/still-here"), tag());
        meta.set_tags(&base.join("code/deleted"), tag());
        meta.set_tags(&base.join("unplugged/group/app"), tag());

        assert_eq!(meta.prune(), 1);
        assert_eq!(meta.tags_for(&mount.join("app")), ["work"], "an empty mount point");
        assert_eq!(meta.tags_for(&base.join("code/still-here")), ["work"]);
        assert!(meta.tags_for(&base.join("code/deleted")).is_empty(), "deleted beside others");
        assert_eq!(meta.tags_for(&base.join("unplugged/group/app")), ["work"], "no parent");
    }
}
