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
    /// Tags a root applied automatically. Kept after the user deletes one, so
    /// the next scan does not put it back.
    pub seeded: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct File {
    repos: BTreeMap<String, RepoMeta>,
}

#[derive(Debug, Clone, Default)]
pub struct Meta {
    repos: BTreeMap<String, RepoMeta>,
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
        let Ok(text) = std::fs::read_to_string(path()) else {
            return Self::default();
        };
        match toml::from_str::<File>(&text) {
            Ok(file) => Self { repos: file.repos },
            Err(err) => {
                eprintln!("omarepos: ignoring {}: {err}", path().display());
                Self::default()
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
        match self.repos.get_mut(&key) {
            Some(meta) if !meta.seeded.is_empty() => meta.tags.clear(),
            _ => {
                self.repos.remove(&key);
            }
        }
    }

    /// Apply a root's tag the first time this repository is seen. Returns
    /// whether anything changed, so a scan can save once at the end.
    pub fn seed(&mut self, repo: &Path, tag: &str) -> bool {
        let tag = tag.trim();
        if tag.is_empty() {
            return false;
        }
        let key = Self::key(repo);
        if let Some(meta) = self.repos.get(&key) {
            if meta.seeded.iter().any(|t| t.eq_ignore_ascii_case(tag)) {
                return false;
            }
        }
        let meta = self.repos.entry(key).or_default();
        meta.seeded.push(tag.to_string());
        let mut tags = meta.tags.clone();
        tags.push(tag.to_string());
        meta.tags = normalize(tags);
        true
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
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = File { repos: self.repos.clone() };
        let body = toml::to_string_pretty(&file)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        std::fs::write(
            &path,
            format!("# omarepos repository metadata — written by the app.\n\n{body}"),
        )
    }
}
