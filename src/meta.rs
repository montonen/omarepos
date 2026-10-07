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
    /// The tag each root applied, keyed by the root's absolute path. Keyed
    /// rather than listed so renaming a root's tag can retire the old one, and
    /// kept after the user deletes a tag so it is not applied again.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub seeded_by: BTreeMap<String, String>,
}

impl RepoMeta {
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
}

pub fn path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".config"));
    base.join("omarepos/repos.toml")
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

impl Meta {
    pub fn load() -> Self {
        let path = path();
        let unusable = |why: String| {
            eprintln!("omarepos: ignoring {}: {why}", path.display());
            Self {
                repos: BTreeMap::new(),
                load_error: Some(crate::config::why_unusable(&path, &why)),
            }
        };
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            // No file yet: nothing has been tagged.
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Self::default(),
            Err(err) => return unusable(err.to_string()),
        };
        match toml::from_str::<File>(&text) {
            Ok(file) => Self { repos: file.repos, load_error: None },
            Err(err) => unusable(err.to_string()),
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

    /// Reconcile one repository with what `root` currently wants tagged on it.
    /// `tag` of `None` means the root no longer tags anything, which retires
    /// whatever it applied before. Returns whether anything changed, so a scan
    /// can save once at the end.
    ///
    /// The root is the unit of bookkeeping: knowing that `~/work/acme` applied
    /// `acme` is what lets a rename to `client-x` remove `acme` instead of leaving
    /// both behind. A tag the user deleted stays deleted, because the record
    /// survives the deletion.
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
            entry.tags.retain(|t| !t.eq_ignore_ascii_case(before));
            if had_old {
                // We just took that tag off ourselves, so any record saying it
                // once went missing is ours too. Leaving it would make setting
                // the same tag again silently do nothing. A record left behind
                // when `had_old` is false is the real thing: the user removed
                // the tag, and that still stands.
                entry.seeded_legacy.retain(|t| !t.eq_ignore_ascii_case(before));
            }
        }
        match tag {
            Some(tag) => {
                entry.seeded_by.insert(root.to_string(), tag.to_string());
                if should_add {
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
    pub fn prune(&mut self) -> usize {
        let before = self.repos.len();
        self.repos.retain(|key, _| {
            let path = Path::new(key);
            if path.exists() {
                return true;
            }
            match path.parent() {
                Some(parent) => !parent.exists(),
                None => true,
            }
        });
        before - self.repos.len()
    }

    /// Every tag in use, sorted, with how many repos carry it.
    pub fn all_tags(&self) -> Vec<(String, usize)> {
        let mut counts: BTreeMap<String, (String, usize)> = BTreeMap::new();
        for meta in self.repos.values() {
            for tag in &meta.tags {
                let entry = counts
                    .entry(tag.to_lowercase())
                    .or_insert_with(|| (tag.clone(), 0));
                entry.1 += 1;
            }
        }
        counts.into_values().collect()
    }

    pub fn save(&self) -> std::io::Result<()> {
        let path = path();
        crate::config::refuse_to_replace(&self.load_error)?;
        let file = File { repos: self.repos.clone() };
        let body = toml::to_string_pretty(&file)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        crate::config::replace_file(
            &path,
            &format!("# omarepos repository metadata — written by the app.\n\n{body}"),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::*;

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

        // What a scan does when the root it found the repository under has a tag.
        meta.apply_root_tag(Path::new("/home/me/code/app"), "/home/me/code", Some("JM"));
        assert!(meta.save().is_err());
        assert_eq!(read(&path()), written);

        let _ = std::fs::remove_file(path());
    }
}
