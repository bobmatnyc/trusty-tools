//! Path/repo scoping predicate for search results (issue #3401).
//!
//! Why: consumers of the single unified `main` index (~315k chunks) need to
//! scope results to a repo/path subtree without losing recall to `top_k`
//! truncation. This module is the single shared predicate used at both
//! admission points:
//!
//!   1. The vector lane's predicate-pushed HNSW traversal
//!      (`lanes::vector_search_scoped`), which only has the chunk id string
//!      to test cheaply during graph exploration.
//!   2. The authoritative, pre-truncation retain in
//!      `CodeIndexer::apply_score_adjustments`, which has the real
//!      `RawChunk::file`.
//!
//! Every chunk id is built as `"{file}:{start}:{end}"` or
//! `"{file}::{type}::{name}::{start}"` (see `chunker::walk::make_chunk_id`) —
//! i.e. a chunk id always begins with its literal file path. That, plus the
//! path-segment-boundary check in `core::store::path_match`, makes `matches`
//! correct for `path_prefix` whether given a chunk id or a file path
//! directly, and safe — over-inclusive at worst, never under-inclusive —
//! for `repos`. Site (1) only decides which candidates the vector lane
//! *admits*; site (2), which re-checks against the real file path, is what
//! the caller-visible result set is actually filtered by, so any
//! theoretical over-inclusion at (1) can never leak through.
//!
//! **Root-relative normalization (code review HIGH finding, issue #3401):**
//! `RawChunk::file` / chunk ids are stored root-relative, but `CodeChunk::file`
//! in RESULTS is always resolved to an absolute host path (issue #402) — the
//! same absolute form the issue's own reporter observed in `file` fields. A
//! caller who copies a `path_prefix` straight out of a previous result's
//! `file` would otherwise silently get zero rows. `normalized_path_prefix`
//! strips `root_path` from an absolute caller-supplied prefix so both forms
//! work; every production call site MUST go through it rather than reading
//! `query.path_prefix` directly (see `matches`, which takes the already-
//! normalized prefix, not the raw query, to make that unavoidable).
//!
//! What: `is_active` (fast short-circuit for the overwhelmingly common
//! unfiltered case), `normalized_path_prefix` (root-relative normalization),
//! and `matches` (AND-composed `path_prefix` + `repos` check over already-
//! normalized data).
//! Test: `test_path_prefix_matches_and_rejects`,
//! `test_repos_matches_path_segment`, `test_inactive_filter_matches_everything`,
//! `test_path_prefix_and_repos_compose_with_and`,
//! `test_normalizes_absolute_prefix_under_root`,
//! `test_leaves_relative_prefix_untouched`,
//! `test_leaves_foreign_absolute_prefix_untouched` in this module.

use std::path::Path;

use super::super::SearchQuery;

/// `true` when the query carries an active path/repo filter. Callers use
/// this to skip filtering work entirely on the (overwhelmingly common)
/// unfiltered path.
pub(crate) fn is_active(query: &SearchQuery) -> bool {
    query.path_prefix.is_some() || !query.repos.is_empty()
}

/// Normalize `query.path_prefix` against `root_path` (issue #3401 — see
/// module docs). When the prefix is absolute and falls under `root_path`,
/// strips `root_path` (and any leading `/`) so it matches the root-relative
/// form chunks are actually stored/compared in. A prefix that is already
/// relative, or is absolute but outside this index's root entirely, passes
/// through unchanged — the latter simply won't match anything, which is
/// correct (the caller named a path outside this index).
pub(crate) fn normalized_path_prefix(query: &SearchQuery, root_path: &Path) -> Option<String> {
    query.path_prefix.as_deref().map(|prefix| {
        if !prefix.starts_with('/') {
            return prefix.to_string();
        }
        let root = root_path.to_string_lossy();
        match prefix.strip_prefix(root.as_ref()) {
            Some(rest) => rest.trim_start_matches('/').to_string(),
            None => prefix.to_string(),
        }
    })
}

/// Test whether `candidate` (a chunk id OR a file path — see module docs)
/// satisfies the filter described by `path_prefix` (already normalized via
/// [`normalized_path_prefix`] — this function does NOT do that itself) and
/// `repos`.
///
/// `path_prefix` and `repos` compose with AND: when both are set, a
/// candidate must satisfy both to match. An inactive filter (`path_prefix:
/// None`, `repos: []`) always matches — this function is still safe to call
/// unconditionally. Delegates to `core::store::path_match` — the single
/// source of truth shared with `VectorStore::search_filtered`'s predicate.
pub(crate) fn matches(candidate: &str, path_prefix: Option<&str>, repos: &[String]) -> bool {
    crate::core::store::path_match::matches(candidate, path_prefix, repos)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query_with(path_prefix: Option<&str>, repos: &[&str]) -> SearchQuery {
        SearchQuery {
            path_prefix: path_prefix.map(str::to_string),
            repos: repos.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn test_inactive_filter_matches_everything() {
        let q = query_with(None, &[]);
        assert!(!is_active(&q));
        assert!(matches(
            "/any/path/at/all.rs",
            q.path_prefix.as_deref(),
            &q.repos
        ));
    }

    #[test]
    fn test_path_prefix_matches_and_rejects() {
        let q = query_with(Some("/repos/foo/src"), &[]);
        assert!(is_active(&q));
        assert!(matches(
            "/repos/foo/src/lib.rs:1:5",
            q.path_prefix.as_deref(),
            &q.repos
        ));
        assert!(!matches(
            "/repos/bar/src/lib.rs:1:5",
            q.path_prefix.as_deref(),
            &q.repos
        ));
    }

    #[test]
    fn test_repos_matches_path_segment() {
        let q = query_with(None, &["foo"]);
        assert!(matches(
            "/home/user/repos/foo/src/lib.rs:1:5",
            q.path_prefix.as_deref(),
            &q.repos
        ));
        assert!(matches(
            "foo/src/lib.rs:1:5",
            q.path_prefix.as_deref(),
            &q.repos
        ));
        assert!(!matches(
            "/home/user/repos/foobar/src/lib.rs:1:5",
            q.path_prefix.as_deref(),
            &q.repos
        ));
    }

    #[test]
    fn test_path_prefix_and_repos_compose_with_and() {
        let q = query_with(Some("/repos/foo/src"), &["foo"]);
        assert!(matches(
            "/repos/foo/src/lib.rs:1:5",
            q.path_prefix.as_deref(),
            &q.repos
        ));
        // Matches `repos` but not `path_prefix`.
        assert!(!matches(
            "/repos/foo/tests/lib.rs:1:5",
            q.path_prefix.as_deref(),
            &q.repos
        ));
    }

    #[test]
    fn test_normalizes_absolute_prefix_under_root() {
        let root = Path::new("/tmp/test-root");
        let q = query_with(Some("/tmp/test-root/vendor/acme"), &[]);
        let normalized = normalized_path_prefix(&q, root);
        assert_eq!(normalized.as_deref(), Some("vendor/acme"));
        // And the normalized form actually matches the root-relative
        // `RawChunk::file` / chunk id stored form.
        assert!(matches(
            "vendor/acme/src/lib.rs:1:5",
            normalized.as_deref(),
            &[]
        ));
    }

    #[test]
    fn test_leaves_relative_prefix_untouched() {
        let root = Path::new("/tmp/test-root");
        let q = query_with(Some("vendor/acme"), &[]);
        assert_eq!(
            normalized_path_prefix(&q, root).as_deref(),
            Some("vendor/acme")
        );
    }

    #[test]
    fn test_leaves_foreign_absolute_prefix_untouched() {
        // An absolute path that isn't under this index's root: left as-is,
        // which simply won't match anything under `root_path` — correct,
        // since the caller named a path outside this index.
        let root = Path::new("/tmp/test-root");
        let q = query_with(Some("/somewhere/else/vendor"), &[]);
        assert_eq!(
            normalized_path_prefix(&q, root).as_deref(),
            Some("/somewhere/else/vendor")
        );
    }
}
