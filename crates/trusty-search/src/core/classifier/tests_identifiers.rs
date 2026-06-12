//! Identifier-pattern classification tests for [`QueryClassifier`].
//!
//! Why: split from `tests_intent.rs` to keep each test file under 500 lines;
//! this file covers PascalCase, snake_case, ALL-CAPS, SCREAMING_SNAKE, and
//! the canonical benchmark pinning (issues #91, #119, #142, #197).
//! What: test functions for identifier-driven Definition routing and the
//! canonical benchmark regression suite.
//! Test: run via `cargo test -p trusty-search core::classifier`.

use super::classify::QueryClassifier;
use super::intent::QueryIntent;

// ── PascalCase identifier-dominated tests (issue #91) ──────────────────

#[test]
fn test_pascal_identifier_alone_is_definition() {
    assert_eq!(
        QueryClassifier::classify("QueryClassifier intent classification"),
        QueryIntent::Definition
    );
}

#[test]
fn test_camel_case_with_extra_words_is_definition() {
    assert_eq!(
        QueryClassifier::classify("CodeIndexer pipeline"),
        QueryIntent::Definition
    );
}

#[test]
fn test_pascal_identifier_loses_to_conceptual_verb() {
    // "how does" wins — Conceptual must take precedence over the
    // PascalCase fallback.
    assert_eq!(
        QueryClassifier::classify("how does QueryClassifier work"),
        QueryIntent::Conceptual
    );
}

#[test]
fn test_standalone_enum_is_definition() {
    assert_eq!(
        QueryClassifier::classify("enum for reservation status"),
        QueryIntent::Definition
    );
}

// ── Leading-acronym identifier tests (issue #91) ───────────────────────

#[test]
fn test_leading_acronym_with_digits_is_definition() {
    assert_eq!(
        QueryClassifier::classify("BM25Index lookup"),
        QueryIntent::Definition
    );
}

#[test]
fn test_leading_acronym_io_error_is_definition() {
    assert_eq!(
        QueryClassifier::classify("IOError handling path"),
        QueryIntent::Definition
    );
}

#[test]
fn test_leading_acronym_url_parser_is_definition() {
    assert_eq!(
        QueryClassifier::classify("URLParser implementation"),
        QueryIntent::Definition
    );
}

#[test]
fn test_bm25_alone_is_definition_via_pascal_fallback() {
    assert_eq!(
        QueryClassifier::classify("BM25 ranking"),
        QueryIntent::Definition
    );
}

#[test]
fn test_pure_acronym_now_triggers_definition() {
    // Issue #119: ALL_CAPS acronyms route to Definition.
    assert_eq!(
        QueryClassifier::classify("API endpoints"),
        QueryIntent::Definition
    );
    // TODO is still BugDebt because `bug_re` is checked before the
    // acronym fallback.
    assert_eq!(
        QueryClassifier::classify("TODO items"),
        QueryIntent::BugDebt
    );
}

#[test]
fn test_short_nl_query_not_forced_conceptual() {
    // Only 3 words — should not match the ≥4-word pattern.
    let result = QueryClassifier::classify("reservation booking flow");
    assert_ne!(result, QueryIntent::Conceptual);
}

// ── Single snake_case identifier tests (issue #119) ────────────────────

#[test]
fn test_single_snake_case_is_definition() {
    assert_eq!(
        QueryClassifier::classify("apply_archive_downrank"),
        QueryIntent::Definition
    );
    assert_eq!(
        QueryClassifier::classify("is_default_doc_excluded"),
        QueryIntent::Definition
    );
    assert_eq!(
        QueryClassifier::classify("get_call_chain"),
        QueryIntent::Definition
    );
}

#[test]
fn test_bare_snake_identifier_with_digits_is_definition() {
    assert_eq!(
        QueryClassifier::classify("bm25_search"),
        QueryIntent::Definition
    );
    assert_eq!(
        QueryClassifier::classify("parse_v2_response"),
        QueryIntent::Definition
    );
}

#[test]
fn test_multi_word_with_snake_does_not_match_snake_branch() {
    assert_eq!(
        QueryClassifier::classify("the payment_processor retries failed attempts five times"),
        QueryIntent::Unknown
    );
}

// ── ALL-CAPS acronym tests (issue #119 / #117) ──────────────────────────

#[test]
fn test_acronym_struct_hint_is_definition() {
    // Short acronym queries (≤2 tokens) still route to Definition.
    assert_eq!(
        QueryClassifier::classify("BM25 index"),
        QueryIntent::Definition
    );
    assert_eq!(
        QueryClassifier::classify("RRF fusion"),
        QueryIntent::Definition
    );
    assert_eq!(QueryClassifier::classify("ORT"), QueryIntent::Definition);
    assert_eq!(QueryClassifier::classify("HNSW"), QueryIntent::Definition);
}

#[test]
fn test_multi_word_acronym_with_nl_words_is_conceptual() {
    // Regression for issue #197: multi-word queries that combine an
    // ALL_CAPS acronym with natural-language tokens read as concept
    // questions, not symbol lookups.
    assert_eq!(
        QueryClassifier::classify("HNSW vector similarity search"),
        QueryIntent::Conceptual
    );
    assert_eq!(
        QueryClassifier::classify("RRF fusion algorithm explanation"),
        QueryIntent::Conceptual
    );
}

// ── Multi-noun conceptual tests (issue #119) ────────────────────────────

#[test]
fn test_four_word_lowercase_is_conceptual() {
    assert_eq!(
        QueryClassifier::classify("axum middleware concurrency limiter"),
        QueryIntent::Conceptual
    );
    assert_eq!(
        QueryClassifier::classify("redb persistence write transaction"),
        QueryIntent::Conceptual
    );
    assert_eq!(
        QueryClassifier::classify("embed batch async worker pool"),
        QueryIntent::Conceptual
    );
    assert_eq!(
        QueryClassifier::classify("Louvain community detection modularity"),
        QueryIntent::Conceptual
    );
}

// ── SCREAMING_SNAKE_CASE identifier tests (issue #142) ─────────────────

#[test]
fn test_screaming_snake_brusilov_epoch_is_definition() {
    assert_eq!(
        QueryClassifier::classify("BRUSILOV_EPOCH"),
        QueryIntent::Definition
    );
}

#[test]
fn test_screaming_snake_max_batch_size_is_definition() {
    assert_eq!(
        QueryClassifier::classify("MAX_BATCH_SIZE"),
        QueryIntent::Definition
    );
}

#[test]
fn test_screaming_snake_foo_bar_baz_is_definition() {
    assert_eq!(
        QueryClassifier::classify("FOO_BAR_BAZ"),
        QueryIntent::Definition
    );
}

#[test]
fn test_screaming_snake_is_default_doc_excluded_is_definition() {
    assert_eq!(
        QueryClassifier::classify("IS_DEFAULT_DOC_EXCLUDED"),
        QueryIntent::Definition
    );
}

#[test]
fn test_screaming_snake_does_not_change_multiword_query() {
    // "HNSW vector similarity" — 3 tokens with NL words → falls through
    // to Unknown (the 4-word variant classifies as Conceptual).
    assert_eq!(
        QueryClassifier::classify("HNSW vector similarity"),
        QueryIntent::Unknown
    );
}

#[test]
fn test_regular_snake_case_unaffected_by_scream_rule() {
    assert_eq!(
        QueryClassifier::classify("authenticate_user"),
        QueryIntent::Definition
    );
}

#[test]
fn test_fn_authenticate_unaffected_by_scream_rule() {
    assert_eq!(
        QueryClassifier::classify("fn authenticate"),
        QueryIntent::Definition
    );
}

#[test]
fn test_lowercase_mixed_words_unaffected_by_scream_rule() {
    assert_eq!(
        QueryClassifier::classify("reservation booking flow"),
        QueryIntent::Unknown
    );
}

// ── Canonical benchmark pinning (issue #119) ────────────────────────────

/// Pin the canonical 14-query benchmark from the v0.8.1 grep-equivalency
/// report. Of these, ≥12 must produce a non-`Unknown` intent so the
/// downstream intent-aware ranking, lane selection, and mode override all
/// engage on real queries.
///
/// Why: intent classification is a hot path; regressions that flip a
/// non-Unknown intent to Unknown silently degrade search quality without
/// compile errors. This test acts as a canary.
/// What: asserts that at least 12 of 14 canonical benchmark queries do not
/// classify as Unknown.
/// Test: this function is the test; run with `cargo test`.
#[test]
fn test_canonical_benchmark_at_least_12_of_14_classified() {
    let queries: &[&str] = &[
        "SearchMode",
        "WalkOptions",
        "apply_archive_downrank",
        "is_default_doc_excluded",
        "get_call_chain",
        "symbol graph BFS expansion",
        "Louvain community detection modularity",
        "axum middleware concurrency limiter",
        "redb persistence write transaction",
        "embed batch async worker pool",
        "chunker AST tree-sitter code split",
        "HNSW vector similarity search",
        "install via cargo",
        "what is BM25",
    ];
    let non_unknown = queries
        .iter()
        .filter(|q| QueryClassifier::classify(q) != QueryIntent::Unknown)
        .count();
    assert!(
        non_unknown >= 12,
        "expected ≥12/14 queries to classify as non-Unknown; got {non_unknown}/14. \
         Per-query intents: {:?}",
        queries
            .iter()
            .map(|q| (*q, QueryClassifier::classify(q)))
            .collect::<Vec<_>>()
    );
}
