//! The filter and the sort: which repositories the grid shows, in what order.
//!
//! `split_query` reads the filter box's little language — free words, `tag:`
//! and `lang:` — and `visible` is the one place the fleet is narrowed and
//! sorted. The grid, Enter on the top hit and the contributions scope all ask
//! it, so they can never disagree about what is showing.

use adw::prelude::*;

use crate::git::RepoInfo;

use super::Ui;

pub(super) const SORT_LABELS: &[&str] = &[
    "Recent activity",
    "Name",
    "Dirty first",
    "Unpushed first",
    "Tracked files",
];

#[derive(Clone, Copy, PartialEq)]
enum Sort {
    Activity,
    Name,
    Dirty,
    Unpushed,
    Files,
}

impl Sort {
    fn from_index(i: u32) -> Self {
        match i {
            1 => Sort::Name,
            2 => Sort::Dirty,
            3 => Sort::Unpushed,
            4 => Sort::Files,
            _ => Sort::Activity,
        }
    }
}

impl Ui {
    /// What the warning toggle keeps. A repository the last fetch failed on
    /// is on this list too: it carries the reddest pill on the grid, and the
    /// toggle used to hide it for being clean.
    fn needs_attention(&self, info: &RepoInfo) -> bool {
        info.error.is_some()
            || !info.tree.is_clean()
            || info.ahead > 0
            || info.unpushed > 0
            || info.behind > 0
            || info.history_unreadable.is_some()
            || self
                .fetch_errors
                .borrow()
                .contains_key(&info.path.display().to_string())
    }

    pub(super) fn visible(&self) -> Vec<RepoInfo> {
        let query = split_query(&self.search.text());
        let dirty_only = self.dirty_only.is_active();
        let tag_filter = self.tag_filter.borrow().clone();
        let untagged_only = self.untagged_only.get();
        let meta = self.meta.borrow();

        let mut repos: Vec<RepoInfo> = self
            .repos
            .borrow()
            .iter()
            .filter(|r| !dirty_only || self.needs_attention(r))
            .filter(|r| {
                let tags: Vec<String> = meta
                    .tags_for(&r.path)
                    .iter()
                    .map(|t| t.to_lowercase())
                    .collect();
                // `tag:` in the search box is an AND; the popover is an OR.
                if !query.tags.iter().all(|wanted| tags.iter().any(|t| t == wanted)) {
                    return false;
                }
                if tag_filter.is_empty() && !untagged_only {
                    return true;
                }
                (untagged_only && tags.is_empty())
                    || tags.iter().any(|t| tag_filter.contains(t))
            })
            .filter(|r| query.langs.iter().all(|lang| r.speaks(lang)))
            .filter(|r| query.words.is_empty() || query.matches(&r.haystack()))
            .cloned()
            .collect();
        drop(meta);

        match Sort::from_index(self.sort.selected()) {
            Sort::Activity => repos.sort_by(|a, b| {
                b.last_active()
                    .cmp(&a.last_active())
                    .then_with(|| a.name.cmp(&b.name))
            }),
            Sort::Name => repos.sort_by(|a, b| {
                a.name
                    .to_lowercase()
                    .cmp(&b.name.to_lowercase())
                    .then_with(|| a.path.cmp(&b.path))
            }),
            Sort::Dirty => repos.sort_by(|a, b| {
                b.tree
                    .total()
                    .cmp(&a.tree.total())
                    .then_with(|| b.last_active().cmp(&a.last_active()))
            }),
            Sort::Unpushed => repos.sort_by(|a, b| {
                // Work that is nowhere else comes first, then what is merely
                // out of step with its upstream.
                let key = |r: &RepoInfo| (r.unpushed.max(r.ahead), r.ahead + r.behind);
                key(b)
                    .cmp(&key(a))
                    .then_with(|| b.last_active().cmp(&a.last_active()))
            }),
            Sort::Files => repos.sort_by(|a, b| {
                b.tracked_files
                    .cmp(&a.tracked_files)
                    .then_with(|| a.name.cmp(&b.name))
            }),
        }
        repos
    }
}

/// A parsed filter: free text, plus the `tag:` and `lang:` terms pulled out of
/// it. Each kind of term is matched against its own field, so a word can only
/// match where the user aimed it.
#[derive(Default, Debug, PartialEq)]
struct Query {
    /// Each has to be found, anywhere and in any order. They used to be
    /// joined back into one phrase, so a second word emptied the grid unless
    /// the two happened to sit side by side in the name or the path.
    words: Vec<String>,
    tags: Vec<String>,
    langs: Vec<String>,
}

impl Query {
    /// Whether every free word is somewhere in `haystack`, which is lower
    /// case already.
    fn matches(&self, haystack: &str) -> bool {
        self.words.iter().all(|word| haystack.contains(word.as_str()))
    }
}

fn split_query(raw: &str) -> Query {
    let mut query = Query::default();
    for token in raw.split_whitespace() {
        // Lowered before the prefixes are looked for, so `Tag:work` is a tag.
        let token = token.to_lowercase();
        if let Some(tag) = token.strip_prefix("tag:") {
            // A bare `tag:` is one that is still being typed. Taken as a
            // word, it emptied the grid between the colon and the first
            // letter after it.
            if !tag.is_empty() {
                query.tags.push(tag.to_string());
            }
        } else if let Some(lang) = token.strip_prefix("lang:") {
            if !lang.is_empty() {
                query.langs.push(lang.to_string());
            }
        } else {
            query.words.push(token);
        }
    }
    query
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_words_both_have_to_be_there_in_either_order() {
        let hay = "billing ~/work/acme/billing main git@example.invalid:acme/billing";
        assert!(split_query("billing").matches(hay));
        assert!(split_query("acme billing").matches(hay));
        assert!(split_query("billing acme").matches(hay));
        assert!(split_query("Billing MAIN").matches(hay));
        assert!(!split_query("billing ledger").matches(hay));
        // No words is no narrowing.
        assert!(split_query("  ").matches(hay));
    }

    #[test]
    fn tag_and_lang_terms_are_pulled_out_whatever_their_case() {
        let query = split_query("Tag:Work lang:Rust api");
        assert_eq!(query.tags, ["work"]);
        assert_eq!(query.langs, ["rust"]);
        assert_eq!(query.words, ["api"]);
    }

    #[test]
    fn a_term_still_being_typed_narrows_nothing() {
        let query = split_query("tag: lang:");
        assert_eq!(query, Query::default());
        assert!(query.matches("anything at all"));
    }
}
