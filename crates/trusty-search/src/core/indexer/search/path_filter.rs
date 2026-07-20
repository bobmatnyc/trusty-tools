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
//! i.e. a chunk id always begins with its literal file path. That makes
//! `matches` exact for `path_prefix` whether given a chunk id or a file path
//! directly (`id.starts_with(prefix) == file.starts_with(prefix)` whenever
//! `prefix` is no longer than `file`), and safe — over-inclusive at worst,
//! never under-inclusive — for `repos`. Site (1) only decides which
//! candidates the vector lane *admits*; site (2), which re-checks against
//! the real file path, is what the caller-visible result set is actually
//! filtered by, so any theoretical over-inclusion at (1) can never leak
//! through.
//!
//! What: `is_active` (fast short-circuit for the overwhelmingly common
//! unfiltered case) and `matches` (AND-composed `path_prefix` + `repos`
//! check).
//! Test: `test_path_prefix_matches_and_rejects`,
//! `test_repos_matches_path_segment`, `test_inactive_filter_matches_everything`,
//! `test_path_prefix_and_repos_compose_with_and` in `indexer::tests`.

use super::super::SearchQuery;

/// `true` when the query carries an active path/repo filter. Callers use
/// this to skip filtering work entirely on the (overwhelmingly common)
/// unfiltered path.
pub(crate) fn is_active(query: &SearchQuery) -> bool {
    query.path_prefix.is_some() || !query.repos.is_empty()
}

/// Test whether `candidate` (a chunk id OR a file path — see module docs)
/// satisfies the query's path/repo filter.
///
/// `path_prefix` and `repos` compose with AND: when both are set, a
/// candidate must satisfy both to match. An inactive filter (`is_active`
/// false) always matches — this function is still safe to call
/// unconditionally. Delegates to `core::store::path_match` — the single
/// source of truth shared with `VectorStore::search_filtered`'s predicate,
/// which takes the same `path_prefix` / `repos` shape as plain data (see
/// that module's docs for why it can't take a `SearchQuery` directly).
pub(crate) fn matches(candidate: &str, query: &SearchQuery) -> bool {
    crate::core::store::path_match::matches(candidate, query.path_prefix.as_deref(), &query.repos)
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
        assert!(matches("/any/path/at/all.rs", &q));
    }

    #[test]
    fn test_path_prefix_matches_and_rejects() {
        let q = query_with(Some("/repos/foo/src"), &[]);
        assert!(is_active(&q));
        assert!(matches("/repos/foo/src/lib.rs:1:5", &q));
        assert!(!matches("/repos/bar/src/lib.rs:1:5", &q));
    }

    #[test]
    fn test_repos_matches_path_segment() {
        let q = query_with(None, &["foo"]);
        assert!(matches("/home/user/repos/foo/src/lib.rs:1:5", &q));
        assert!(matches("foo/src/lib.rs:1:5", &q));
        assert!(!matches("/home/user/repos/foobar/src/lib.rs:1:5", &q));
    }

    #[test]
    fn test_path_prefix_and_repos_compose_with_and() {
        let q = query_with(Some("/repos/foo/src"), &["foo"]);
        assert!(matches("/repos/foo/src/lib.rs:1:5", &q));
        // Matches `repos` but not `path_prefix`.
        assert!(!matches("/repos/foo/tests/lib.rs:1:5", &q));
    }
}
