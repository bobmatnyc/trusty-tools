//! Tests for the three-way embed-line logic in `format_timing_breakdown` (#929).
//!
//! Included via `#[cfg(test)] mod embed_tests;` in `reindex_ui.rs`.

use super::{format_timing_breakdown, ReindexTimings};

fn zero_vector_timings() -> ReindexTimings {
    ReindexTimings {
        walk_ms: 100,
        parse_ms: 500,
        embed_ms: 0,
        bm25_ms: 300,
        vector_upsert_ms: 0,
        kg_ms: 0,
        vector_count: 0,
        symbol_count: 5,
        edge_count: 1,
    }
}

/// Why: `lexical_only=true` means embedding was intentionally disabled; the
///      message must be calm/accurate, not the alarming "unreachable" warning.
/// What: asserts the "lexical-only" label appears and "unreachable" does not.
/// Test: this test.
#[test]
fn embed_line_lexical_only_shows_calm_message() {
    let t = zero_vector_timings();
    let out = format_timing_breakdown(&t, 1_000, 2_000, false, true);
    assert!(
        out.contains("lexical-only"),
        "lexical_only=true must emit 'lexical-only' message; got:\n{out:?}"
    );
    assert!(
        !out.contains("unreachable"),
        "lexical_only=true must not emit embedder-unreachable message; got:\n{out:?}"
    );
}

/// Why: when defer_embed=true embedding runs in the background; printing an
///      "embed skipped" line would contradict the background-note already emitted
///      by `run_reindex_with`.
/// What: asserts the embed row is absent entirely (no "SKIPPED" substring).
/// Test: this test.
#[test]
fn embed_line_defer_embed_suppresses_line() {
    let t = zero_vector_timings();
    let out = format_timing_breakdown(&t, 1_000, 2_000, true, false);
    assert!(
        !out.contains("SKIPPED"),
        "defer_embed=true must suppress the embed line entirely; got:\n{out:?}"
    );
}

/// Why: when the embedder sidecar is simply absent the operator needs a loud,
///      distinctive warning to know they should start the sidecar and reindex.
/// What: asserts "unreachable" and "BM25-only" appear when neither flag is set.
/// Test: this test.
#[test]
fn embed_line_embedder_unavailable_shows_loud_message() {
    let t = zero_vector_timings();
    let out = format_timing_breakdown(&t, 1_000, 2_000, false, false);
    assert!(
        out.contains("unreachable"),
        "embedder-unavailable path must emit 'unreachable' message; got:\n{out:?}"
    );
    assert!(
        out.contains("BM25-only"),
        "embedder-unavailable path must emit 'BM25-only' message; got:\n{out:?}"
    );
}
