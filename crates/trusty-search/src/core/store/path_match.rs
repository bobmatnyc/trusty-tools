//! Shared path/repo predicate for `VectorStore::search_filtered` (issue #3401).
//!
//! Why: both [`super::types::VectorStore`]'s default over-fetch fallback and
//! [`super::usearch_impl`]'s predicate-pushed `UsearchStore` override need
//! the exact same "does this chunk id/file path satisfy the filter?" check.
//! Living here (rather than in `core::indexer::search::path_filter`, which
//! wraps this for its `SearchQuery`-shaped call site) keeps `core::store`
//! free of any dependency on the indexer module — it takes the filter as
//! plain `Option<&str>` / `&[String]` data instead of a `SearchQuery`.
//! What: `matches` — AND-composed `path_prefix` + `repos` check, identical
//! semantics to `core::indexer::search::path_filter::matches`.
//! Test: `test_prefix_and_repos_compose_with_and`,
//! `test_empty_filter_matches_everything` below;
//! `core::indexer::search::path_filter`'s tests cover the `SearchQuery`-facing
//! wrapper.

/// Test whether `candidate` (a chunk id or file path — both begin with the
/// literal file path, see `chunker::walk::make_chunk_id`) satisfies
/// `path_prefix` (if set) AND every name in `repos` matching as a path
/// segment (if non-empty). An empty filter (`path_prefix: None`,
/// `repos: []`) always matches.
pub(crate) fn matches(candidate: &str, path_prefix: Option<&str>, repos: &[String]) -> bool {
    if let Some(prefix) = path_prefix {
        if !prefix.is_empty() && !candidate.starts_with(prefix) {
            return false;
        }
    }
    if !repos.is_empty() {
        let in_any_repo = repos.iter().any(|repo| {
            if repo.is_empty() {
                return false;
            }
            candidate.starts_with(&format!("{repo}/")) || candidate.contains(&format!("/{repo}/"))
        });
        if !in_any_repo {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_filter_matches_everything() {
        assert!(matches("/any/path.rs:1:2", None, &[]));
    }

    #[test]
    fn test_prefix_and_repos_compose_with_and() {
        let repos = vec!["foo".to_string()];
        assert!(matches(
            "/repos/foo/src/lib.rs:1:2",
            Some("/repos/foo"),
            &repos
        ));
        assert!(!matches(
            "/repos/bar/src/lib.rs:1:2",
            Some("/repos/foo"),
            &repos
        ));
        assert!(!matches(
            "/repos/foo/tests/lib.rs:1:2",
            Some("/repos/foo/src"),
            &repos
        ));
    }
}
