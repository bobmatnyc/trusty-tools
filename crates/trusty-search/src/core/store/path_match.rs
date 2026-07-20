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
        if !prefix.is_empty() && !prefix_matches_at_boundary(candidate, prefix) {
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

/// `true` when `candidate` starts with `prefix` at a genuine path-segment
/// boundary — code review finding on issue #3401: a bare
/// `candidate.starts_with(prefix)` lets `path_prefix: "/repos/foo"` also
/// match `/repos/foobar/secret.rs`, silently leaking a sibling repo/dir into
/// the "scoped" result set.
///
/// A boundary holds when `prefix` already ends in `/` (the caller opted in
/// explicitly), `candidate` is exactly `prefix` (scoping to one exact file —
/// no trailing content to bound), the byte immediately after `prefix` in
/// `candidate` is `/` (the natural "next path segment" case, e.g.
/// `prefix = "/repos/foo"`, `candidate = "/repos/foo/src/lib.rs"`), or that
/// byte is `:` — `candidate` may be a chunk id rather than a bare file path
/// (`"{file}:{start}:{end}"` / `"{file}::{type}::{name}::{start}"`, see
/// `chunker::walk::make_chunk_id`), and `prefix` naming that file exactly
/// (no trailing separator) must still match its own chunks. Falls through
/// to `false` for anything else, including the `foo`/`foobar` collision and
/// a prefix that lands mid-segment for any other reason (e.g.
/// `candidate = "/repos/foo_backup/x.rs"` against `prefix = "/repos/foo"`).
fn prefix_matches_at_boundary(candidate: &str, prefix: &str) -> bool {
    if !candidate.starts_with(prefix) {
        return false;
    }
    if prefix.ends_with('/') {
        return true;
    }
    matches!(
        candidate.as_bytes().get(prefix.len()),
        None | Some(b'/') | Some(b':')
    )
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

    /// Code review finding (issue #3401): `path_prefix: "/repos/foo"` must
    /// NOT match a sibling directory that merely shares the prefix as a
    /// string, e.g. `/repos/foobar/...`.
    #[test]
    fn test_prefix_does_not_match_sibling_directory() {
        assert!(!matches(
            "/repos/foobar/secret.rs:1:2",
            Some("/repos/foo"),
            &[]
        ));
        // The true subtree entry (boundary at '/') still matches.
        assert!(matches(
            "/repos/foo/src/lib.rs:1:2",
            Some("/repos/foo"),
            &[]
        ));
        // A prefix the caller already terminated with '/' behaves the same
        // (explicit opt-in boundary).
        assert!(matches(
            "/repos/foo/src/lib.rs:1:2",
            Some("/repos/foo/"),
            &[]
        ));
        assert!(!matches(
            "/repos/foobar/secret.rs:1:2",
            Some("/repos/foo/"),
            &[]
        ));
        // Exact-file scoping: candidate equals prefix exactly.
        assert!(matches("/repos/foo", Some("/repos/foo"), &[]));
    }

    /// A `path_prefix` naming one exact file (no trailing separator) must
    /// still match that file's own chunk ids, which carry a `:start:end` (or
    /// `::type::name::start`) suffix rather than ending at the file path —
    /// this is why the boundary check also accepts `:` as a valid next byte.
    #[test]
    fn test_exact_file_prefix_matches_its_own_chunk_ids() {
        assert!(matches("src/auth.rs:10:20", Some("src/auth.rs"), &[]));
        assert!(matches(
            "src/auth.rs::function::login::10",
            Some("src/auth.rs"),
            &[]
        ));
        // Still rejects a same-string-prefixed sibling file.
        assert!(!matches("src/auth.rs.bak:1:2", Some("src/auth.rs"), &[]));
    }
}
