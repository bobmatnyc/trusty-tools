//! Read the base commit through the Contents API, for the hosted path (#11899).
//!
//! Why: the hosted webhook review has no local checkout (DOC-93 RS-38). It
//! reads `.trusty-review.toml` and each `SKILL.md` at the base SHA through the
//! contents-at-ref reader, which validates the path and needs a full SHA
//! before any request. A failed request must never read as "absent": only a
//! 404 is [`BlobRead::Missing`]; an HTTP error, a timeout or a rate limit is
//! [`BlobRead::Error`] and becomes a named skip.
//! What: [`find_base`] reads the config, then (same-repo PRs only) each
//! requested skill, into a [`PrefetchedTree`]; the synchronous loader then
//! applies step 1's rules to it unchanged. [`read_at_ref`] maps one
//! [`DocFetcher::fetch_entry`] result to a [`BlobRead`].
//! Test: `hosted_tests.rs` in this directory.

use std::collections::HashMap;
use std::sync::Arc;

use super::git_store::{BaseTree, BlobRead};
use super::{Base, CONFIG_CAP_BYTES, MAX_SKILL_ENTRIES, SKILL_CAP_BYTES, SkillsSource};
use crate::config::repo_config::REPO_CONFIG_FILENAME;
use crate::identifier::is_valid_identifier;
use crate::integrations::context::contents_at_ref::{
    DocFetchError, DocFetcher, GithubDocFetcher, is_head_sha,
};
use crate::pipeline::diff::DiffSource;
use crate::pipeline::optional_context::seams::PrHead;

/// The base-commit files the hosted path already read, keyed by path.
///
/// Why: the loader is synchronous and the Contents API is not; reading up
/// front keeps one copy of the rules (`load::load_skills`).
/// What: `read` returns the stored result, re-checked against `cap`; a path
/// never read is an error, never `Missing`.
/// Test: `hosted_skill_loads_at_base_and_reaches_the_prompt`.
#[derive(Debug, Default)]
pub(crate) struct PrefetchedTree {
    blobs: HashMap<String, BlobRead>,
}

impl BaseTree for PrefetchedTree {
    fn read(&self, path: &str, cap: usize) -> BlobRead {
        match self.blobs.get(path) {
            Some(BlobRead::Bytes(b)) if b.len() > cap => BlobRead::TooLarge(b.len()),
            Some(read) => read.clone(),
            None => BlobRead::Error(format!("contents API: {path} was not read")),
        }
    }
}

/// The skill file path for a validated skill name (DOC-93 RS-17).
fn skill_path(name: &str) -> String {
    format!(".claude/skills/{name}/SKILL.md")
}

/// Read the hosted review's base commit into a [`Base`] (#11899).
///
/// Why: DOC-93 RS-38, RS-21, RS-45 on the hosted path.
/// What: needs a GitHub diff source and a full lowercase hex base SHA, else
/// the store is `None` (same outcome as step 1's unreadable base). It reads
/// the config at the base SHA, fork or not — the base repository's own file —
/// so a fork whose base lists skills records them as `fork` skips. Only a
/// same-repo PR reads skill files (RS-39: a fork makes no skill request): one
/// per distinct valid name among the first [`MAX_SKILL_ENTRIES`] entries.
/// Test: `hosted_skill_loads_at_base_and_reaches_the_prompt`,
/// `hosted_fork_with_configured_skills_requests_no_skill`,
/// `hosted_without_valid_base_sha_makes_no_request`.
pub(super) async fn find_base(
    source: &DiffSource,
    head: &PrHead,
    seam: Option<&Arc<dyn DocFetcher>>,
    requests: &SkillsSource,
) -> Base {
    let (sha, fork) = (head.base_sha.as_str(), head.fork);
    let none = |why: &str, sha: &str| Base {
        store: None,
        sha: sha.to_string(),
        fork,
        why: why.to_string(),
    };
    let DiffSource::Github {
        owner, repo, token, ..
    } = source
    else {
        return none("only a GitHub pull request has a hosted base commit", "");
    };
    if !is_head_sha(sha) {
        return none("the pull request base commit is unknown", "");
    }
    let fetcher: Arc<dyn DocFetcher> = match seam {
        Some(seam) => seam.clone(),
        None => match GithubDocFetcher::new(owner, repo, token) {
            Ok(f) => Arc::new(f),
            Err(e) => return none(&format!("contents API client: {e}"), sha),
        },
    };
    let mut tree = PrefetchedTree::default();
    let config = read_at_ref(
        fetcher.as_ref(),
        REPO_CONFIG_FILENAME,
        sha,
        CONFIG_CAP_BYTES,
    )
    .await;
    tree.blobs.insert(REPO_CONFIG_FILENAME.to_string(), config);
    if !fork {
        let names: Vec<String> = match requests {
            SkillsSource::Requests(reqs) => reqs.iter().map(|r| r.name.clone()).collect(),
            SkillsSource::RepoConfig => super::config_entries(&tree)
                .into_iter()
                .filter_map(|e| e.ok().map(|r| r.name))
                .collect(),
        };
        for name in names.into_iter().take(MAX_SKILL_ENTRIES) {
            let path = skill_path(&name);
            if !is_valid_identifier(&name) || tree.blobs.contains_key(&path) {
                continue; // the loader names it invalid or duplicate; no request
            }
            let read = read_at_ref(fetcher.as_ref(), &path, sha, SKILL_CAP_BYTES).await;
            tree.blobs.insert(path, read);
        }
    }
    Base {
        store: Some(Box::new(tree)),
        sha: sha.to_string(),
        fork,
        why: String::new(),
    }
}

/// Read `path` at `sha` through `fetcher` as a [`BlobRead`] (#11899).
///
/// Why: DOC-93 RS-18, RS-23 and the fail-open rule — a symlink never loads,
/// the cap is checked against the API's `size` before the bytes are used,
/// and only a 404 is `Missing`.
/// What: 404 → `Missing`; a directory, a non-`file` type, or an entry the
/// API answered for another path (a followed symlink) → `NotRegular`; `size`
/// over `cap` → `TooLarge`; a file with no inline content → `Error`; every
/// other failure (HTTP status, timeout, rate limit, missing base commit) →
/// `Error` naming it.
/// Test: `hosted_read_maps_each_contents_result`,
/// `hosted_config_errors_are_config_unreadable_never_silent`.
pub(crate) async fn read_at_ref(
    fetcher: &dyn DocFetcher,
    path: &str,
    sha: &str,
    cap: usize,
) -> BlobRead {
    let entry = match fetcher.fetch_entry(path, sha).await {
        Ok(Some(entry)) => entry,
        Ok(None) => return BlobRead::Missing,
        Err(DocFetchError::Directory) => return BlobRead::NotRegular("dir".to_string()),
        Err(e) => return BlobRead::Error(error_reason(&e)),
    };
    if entry.kind != "file" {
        return BlobRead::NotRegular(entry.kind);
    }
    if entry.path != path {
        // #11899 (RS-18): the API follows an in-repo symlink to its target.
        return BlobRead::NotRegular("symlink".to_string());
    }
    if entry.size > cap {
        return BlobRead::TooLarge(entry.size);
    }
    match entry.content {
        Some(bytes) if bytes.len() > cap => BlobRead::TooLarge(bytes.len()),
        Some(bytes) => BlobRead::Bytes(bytes),
        None => BlobRead::Error("contents API: no inline content".to_string()),
    }
}

/// One-line skip reason for a Contents API failure; a rate limit says so.
fn error_reason(e: &DocFetchError) -> String {
    let limited = matches!(e, DocFetchError::Api { status: 429, .. })
        || matches!(e, DocFetchError::Api { status: 403, body }
            if body.to_ascii_lowercase().contains("rate limit"));
    if limited {
        format!("contents API rate limited: {e}")
    } else {
        format!("contents API: {e}")
    }
}
