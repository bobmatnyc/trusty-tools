//! Shared path/repo predicates for `VectorStore::search_filtered` (issue #3401).
//!
//! Why: [`super::types::VectorStore`]'s default over-fetch fallback,
//! [`super::usearch_impl`]'s predicate-pushed `UsearchStore` override, and
//! `core::indexer::search::path_filter` (which wraps these for its
//! `SearchQuery`-shaped call site) all need "does this candidate satisfy the
//! filter?" checks. Living here (rather than in the indexer module) keeps
//! `core::store` free of any dependency on the indexer — it takes the
//! filter as plain `Option<&str>` / `&[String]` data instead of a
//! `SearchQuery`.
//!
//! **Two functions, not one** (code review finding, issue #3401): a chunk id
//! (`"{file}:{start}:{end}"` or `"{file}::{type}::{name}::{start}"`, see
//! `chunker::walk::make_chunk_id`) and a bare file path (`RawChunk::file` /
//! `CodeChunk::file`) are different candidate SHAPES. `:` is both the
//! chunk-id start/end separator AND a legal POSIX filename/directory
//! character (ISO-8601 timestamp dirs, versioned artifact names) — a single
//! shared boundary rule that accepts `:` unconditionally to satisfy chunk-id
//! matching would then also let `path_prefix: "vendor/foo"` match a real
//! directory named `vendor/foo:evil/`, which is exactly the sibling-prefix
//! leak this module exists to close. So: [`matches_file`] (boundary is only
//! end-of-string or `/` — used wherever the candidate is a bare file path)
//! and [`matches_chunk_id`] (also accepts the literal chunk-id suffix
//! grammar — used wherever the candidate is a chunk id).
//!
//! What: `matches_file` and `matches_chunk_id` each AND-compose `path_prefix`
//! with `repos`; `is_chunk_id_suffix` validates the suffix shape
//! `make_chunk_id` actually emits, rather than accepting any `:`.
//!
//! Test: `test_file_prefix_rejects_sibling_directory`,
//! `test_chunk_id_prefix_rejects_colon_in_real_path_segment`,
//! `test_exact_file_prefix_matches_its_own_chunk_ids`, and others below.

use crate::core::chunk_id::ChunkIdShapes;

/// Test whether `candidate` — a bare file path (`RawChunk::file` /
/// `CodeChunk::file`) — satisfies `path_prefix` (if set) AND every name in
/// `repos` matching as a path segment (if non-empty). An empty filter
/// (`path_prefix: None`, `repos: []`) always matches.
///
/// Use this at any site whose candidate is a real file path, never a chunk
/// id — the authoritative retain in `apply_score_adjustments` is the
/// canonical caller.
pub(crate) fn matches_file(candidate: &str, path_prefix: Option<&str>, repos: &[String]) -> bool {
    matches_with(candidate, path_prefix, repos, file_prefix_boundary_ok)
}

/// Test whether `candidate` — a chunk id — satisfies `path_prefix` (if set)
/// AND `repos` (if non-empty). Same composition as [`matches_file`], but the
/// prefix boundary check also accepts a landing right at the start of the
/// chunk-id suffix `make_chunk_id` appends after the file path.
///
/// Use this at any site whose candidate is a chunk id (BM25/grep/HNSW
/// admission predicates), never a bare file path.
/// `shapes` (#6581) is the per-index chunk-id policy — see
/// [`crate::core::chunk_id::ChunkIdShapes`]. An index that has not run M005
/// still holds pre-#6581 named ids and passes [`ChunkIdShapes::NewAndLegacy`];
/// one that has passes [`ChunkIdShapes::NewOnly`], which stops the wider legacy
/// grammar from admitting a lookalike path for no recall.
pub(crate) fn matches_chunk_id(
    candidate: &str,
    path_prefix: Option<&str>,
    repos: &[String],
    shapes: ChunkIdShapes,
) -> bool {
    matches_with(candidate, path_prefix, repos, |cand, prefix| {
        chunk_id_prefix_boundary_ok(cand, prefix, shapes)
    })
}

fn matches_with(
    candidate: &str,
    path_prefix: Option<&str>,
    repos: &[String],
    prefix_boundary_ok: impl Fn(&str, &str) -> bool,
) -> bool {
    if let Some(prefix) = path_prefix {
        if !prefix.is_empty() && !prefix_boundary_ok(candidate, prefix) {
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

/// Boundary rule for a bare file path: `prefix` must be the whole of
/// `candidate`, or `candidate` must continue with `/` right after it (or
/// `prefix` was already `/`-terminated by the caller). No other byte is
/// ever a valid boundary — in particular NOT `:`, which is a legal POSIX
/// path character, so accepting it here would let `path_prefix: "vendor/foo"`
/// match a real directory `vendor/foo:evil/...`.
fn file_prefix_boundary_ok(candidate: &str, prefix: &str) -> bool {
    if !candidate.starts_with(prefix) {
        return false;
    }
    if prefix.ends_with('/') {
        return true;
    }
    matches!(candidate.as_bytes().get(prefix.len()), None | Some(b'/'))
}

/// Boundary rule for a chunk id: everything [`file_prefix_boundary_ok`]
/// accepts, PLUS landing exactly at the start of a genuine chunk-id suffix
/// (validated against the shape `chunker::walk::make_chunk_id` actually
/// emits — not "any `:`", which is what let a real `vendor/foo:evil/...`
/// directory leak through in the prior version of this check).
fn chunk_id_prefix_boundary_ok(candidate: &str, prefix: &str, shapes: ChunkIdShapes) -> bool {
    if file_prefix_boundary_ok(candidate, prefix) {
        return true;
    }
    if !candidate.starts_with(prefix) {
        return false;
    }
    is_chunk_id_suffix(&candidate[prefix.len()..], shapes)
}

/// `true` when `suffix` (everything in a chunk id after a matched file-path
/// prefix) is EXACTLY one of the shapes `make_chunk_id` emits — the WHOLE
/// remainder is validated, not just its first byte or two (code review finding,
/// issue #3401: peeking only at the leading bytes let `src/foo::bar.rs:10:20`
/// and `src/foo:9lives.rs:15:25` — neither of which is a real chunk-id suffix —
/// pass as if they were, letting a coincidentally-named sibling file consume a
/// slot in a lane's `want` candidate pool ahead of a genuine in-scope match — a
/// recall bug, not a disclosure one, but the exact property #3401 exists to
/// guarantee).
///
/// Why it delegates (#6581): this re-implemented the grammar with a hard-coded
/// three-segment named shape, so it went stale the moment the named shape grew
/// an `end_line`, and it rejected every method chunk outright because it
/// required `name` to be colon-free (a qualified method name is `Type::method`).
/// What: [`crate::core::chunk_id::is_valid_suffix`] is the single owner of the
/// grammar; `shapes` decides whether the pre-#6581 named shape still counts for
/// THIS index.
/// Test: `chunk_id_suffix_shape_is_gated_per_index` in this module's `tests`,
/// and `core::chunk_id::tests::suffix_policy_gates_the_legacy_shape`.
fn is_chunk_id_suffix(suffix: &str, shapes: ChunkIdShapes) -> bool {
    crate::core::chunk_id::is_valid_suffix(suffix, shapes)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Keeps every pre-#6581 assertion below at its original three-argument
    /// shape while the production signature carries the per-index policy. The
    /// permissive policy is what those assertions were written against.
    fn matches_chunk_id(candidate: &str, path_prefix: Option<&str>, repos: &[String]) -> bool {
        super::matches_chunk_id(candidate, path_prefix, repos, ChunkIdShapes::NewAndLegacy)
    }

    /// The owner ruling of 2026-09-05, per index: before an index's M005 marker
    /// is set both named shapes match; after it, only the new one. An
    /// implementation that ignores `shapes` fails the second assertion, and one
    /// that forgets `end_line` fails the first.
    #[test]
    fn chunk_id_suffix_shape_is_gated_per_index() {
        let new_id = "src/auth.rs::Function::login::10::40";
        let legacy_id = "src/auth.rs::Function::login::10";

        // Index A — has not run M005: both shapes are in scope.
        for id in [new_id, legacy_id] {
            assert!(
                super::matches_chunk_id(id, Some("src/auth.rs"), &[], ChunkIdShapes::NewAndLegacy),
                "pre-M005 index must match {id}"
            );
        }

        // Index B — M005 has run: the legacy shape is no longer a chunk-id
        // boundary, so the prefix lands mid-path and the candidate is rejected.
        assert!(super::matches_chunk_id(
            new_id,
            Some("src/auth.rs"),
            &[],
            ChunkIdShapes::NewOnly
        ));
        assert!(!super::matches_chunk_id(
            legacy_id,
            Some("src/auth.rs"),
            &[],
            ChunkIdShapes::NewOnly
        ));

        // The positional shape is unaffected by the policy either way.
        for shapes in [ChunkIdShapes::NewAndLegacy, ChunkIdShapes::NewOnly] {
            assert!(super::matches_chunk_id(
                "src/auth.rs:10:20",
                Some("src/auth.rs"),
                &[],
                shapes
            ));
        }
    }

    #[test]
    fn test_empty_filter_matches_everything() {
        assert!(matches_file("/any/path.rs", None, &[]));
        assert!(matches_chunk_id("/any/path.rs:1:2", None, &[]));
    }

    #[test]
    fn test_prefix_and_repos_compose_with_and() {
        let repos = vec!["foo".to_string()];
        assert!(matches_chunk_id(
            "/repos/foo/src/lib.rs:1:2",
            Some("/repos/foo"),
            &repos
        ));
        assert!(!matches_chunk_id(
            "/repos/bar/src/lib.rs:1:2",
            Some("/repos/foo"),
            &repos
        ));
        assert!(!matches_chunk_id(
            "/repos/foo/tests/lib.rs:1:2",
            Some("/repos/foo/src"),
            &repos
        ));
    }

    /// Code review finding (issue #3401): `path_prefix: "/repos/foo"` must
    /// NOT match a sibling directory that merely shares the prefix as a
    /// string, e.g. `/repos/foobar/...`. Pinned against BOTH functions.
    #[test]
    fn test_file_prefix_rejects_sibling_directory() {
        assert!(!matches_file(
            "/repos/foobar/secret.rs",
            Some("/repos/foo"),
            &[]
        ));
        assert!(matches_file(
            "/repos/foo/src/lib.rs",
            Some("/repos/foo"),
            &[]
        ));
        assert!(matches_file(
            "/repos/foo/src/lib.rs",
            Some("/repos/foo/"),
            &[]
        ));
        assert!(!matches_file(
            "/repos/foobar/secret.rs",
            Some("/repos/foo/"),
            &[]
        ));
        assert!(matches_file("/repos/foo", Some("/repos/foo"), &[]));
    }

    #[test]
    fn test_chunk_id_prefix_rejects_sibling_directory() {
        assert!(!matches_chunk_id(
            "/repos/foobar/secret.rs:1:2",
            Some("/repos/foo"),
            &[]
        ));
        assert!(matches_chunk_id(
            "/repos/foo/src/lib.rs:1:2",
            Some("/repos/foo"),
            &[]
        ));
    }

    /// The exact regression the second code-review pass caught: the OLD
    /// single-function boundary check accepted any `:` as a valid boundary
    /// byte, so `path_prefix: "vendor/foo"` matched the real directory
    /// `vendor/foo:evil/secret.rs` — a colon is a legal POSIX path
    /// character, not just the chunk-id separator. `matches_file` must
    /// reject this (candidate here is a bare file path, so the `:` boundary
    /// allowance never applies at all), and `matches_chunk_id` must ALSO
    /// reject it even though `:` is allowed there in principle, because the
    /// byte after `:` is `e` (not a digit, not a second `:`) — it doesn't
    /// match the real chunk-id suffix grammar.
    #[test]
    fn test_chunk_id_prefix_rejects_colon_in_real_path_segment() {
        assert!(!matches_file(
            "vendor/foo:evil/secret.rs",
            Some("vendor/foo"),
            &[]
        ));
        assert!(!matches_chunk_id(
            "vendor/foo:evil/secret.rs:1:2",
            Some("vendor/foo"),
            &[]
        ));
        // Same shape, single-segment version (no trailing chunk suffix at
        // all) — still must not match.
        assert!(!matches_chunk_id(
            "vendor/foo:evil/secret.rs",
            Some("vendor/foo"),
            &[]
        ));
        // And the extracted repro from the code-review comment.
        assert!(!matches_chunk_id("src/foo:bar.rs", Some("src/foo"), &[]));
    }

    /// Third code-review pass (issue #3401): the boundary check must fully
    /// parse the chunk-id suffix, not merely peek at its first byte or two.
    /// Peeking accepted `src/foo::bar.rs:10:20` (the `::` branch matched
    /// unconditionally past byte 2) and `src/foo:9lives.rs:15:25` (the
    /// single-`:` branch accepted on one leading digit, never requiring the
    /// rest of the segment to actually BE digits). Neither is a real
    /// `make_chunk_id` suffix — both are real file names that happen to
    /// start with a shape the old peek-based check mistook for one. Full
    /// validation of the entire remainder rejects both.
    #[test]
    fn test_chunk_id_prefix_rejects_lookalike_full_suffix() {
        assert!(!matches_chunk_id(
            "src/foo::bar.rs:10:20",
            Some("src/foo"),
            &[]
        ));
        assert!(!matches_chunk_id(
            "src/foo:9lives.rs:15:25",
            Some("src/foo"),
            &[]
        ));
    }

    /// Canary: a `path_prefix` naming one exact file (no trailing
    /// separator) must still match that file's own chunk ids, which carry a
    /// `:start:end` (or `::type::name::start`) suffix rather than ending at
    /// the file path — this is why `matches_chunk_id` accepts a validated
    /// colon-led suffix as a boundary at all. Must NOT regress when the
    /// boundary check is tightened against the colon-leak above.
    #[test]
    fn test_exact_file_prefix_matches_its_own_chunk_ids() {
        assert!(matches_chunk_id(
            "src/auth.rs:10:20",
            Some("src/auth.rs"),
            &[]
        ));
        assert!(matches_chunk_id(
            "src/auth.rs::function::login::10",
            Some("src/auth.rs"),
            &[]
        ));
        // Still rejects a same-string-prefixed sibling file (non-digit,
        // non-double-colon after the single ':').
        assert!(!matches_chunk_id(
            "src/auth.rs.bak:1:2",
            Some("src/auth.rs"),
            &[]
        ));
        // A bare file path (no chunk suffix at all) for the SAME exact-file
        // prefix must also match via matches_file (end-of-string boundary).
        assert!(matches_file("src/auth.rs", Some("src/auth.rs"), &[]));
    }
}
