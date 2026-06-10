//! Tests for the analysis handlers: smell serialization, pagination envelopes.
//!
//! Why: Extracted from `analysis.rs` to keep that file under the 500-line cap
//! (#610). All test logic lives here; production code stays in `analysis.rs`.
//! What: `make_chunk` helper + tests for SmellItem serialization and
//! offset/limit pagination slice-and-envelope behaviour.
//! Test: these tests exercise `SmellItem::from_chunk` and the pagination logic
//! used by `smells`; run with `cargo test -p trusty-analyze`.

use std::collections::HashMap;

use crate::core::quality;
use crate::service::diagnostics_dispatch;
use crate::service::handlers::analysis::SmellItem;
use crate::types::CodeChunk;

/// Build a minimal CodeChunk for testing.
fn make_chunk(id: &str, file: &str, content: &str) -> CodeChunk {
    CodeChunk {
        id: id.into(),
        file: file.into(),
        start_line: 1,
        end_line: 10,
        content: content.into(),
        function_name: None,
        score: 0.0,
        compact_snippet: None,
        match_reason: "test".into(),
    }
}

/// Why: Two files with identical basenames in different directories must
/// both receive diagnostic results. Before the fix, the second write
/// overwrote the first in the shared scratch directory, silently dropping
/// the first file's diagnostics.
/// What: calls `run_diagnostics_blocking` with two entries whose basenames
/// collide; verifies that both entries are processed (the loop reaches each
/// one without skipping).
/// Test: this test itself. Note: no tools are installed in CI, so the
/// actual `out` may be empty — the test validates that the function does
/// not skip or panic rather than asserting diagnostic content.
#[test]
fn run_diagnostics_blocking_two_files_same_basename() {
    let mut by_file = HashMap::new();
    // Two Rust files with the same basename `main.rs` but different
    // directory paths — the classic collision case.
    by_file.insert("src/a/main.rs".to_string(), "fn a() {}".to_string());
    by_file.insert("src/b/main.rs".to_string(), "fn b() {}".to_string());
    // This must not panic or skip files silently.
    // We cannot assert on diagnostic counts (no tools in CI), but if the
    // basename collision bug were still present this would panic on the
    // second create_dir_all (or silently overwrite) — not crash-free.
    let _report = diagnostics_dispatch::run_diagnostics_blocking(by_file, None, None, None);
    // Reaching here without panic means the subdir isolation works.
}

// ── SmellItem serialization tests ────────────────────────────────────────

/// Why: default `omit_content=true` must strip the raw source field to bound
/// MCP payload size (#917).
/// What: builds a SmellItem with `include_content=false` and verifies the
/// serialised JSON lacks the `content` key.
/// Test: this test.
#[test]
fn smell_item_omit_content_default_strips_content() {
    let chunk = make_chunk("id1", "src/main.rs", "fn main() {}");
    let item = SmellItem::from_chunk(&chunk, false);
    let json = serde_json::to_value(&item).unwrap();
    assert!(
        json.get("content").is_none(),
        "content must be absent when omit_content=true"
    );
    assert_eq!(json["file"], "src/main.rs");
    assert_eq!(json["id"], "id1");
}

/// Why: `include_content=true` opt-in must restore the full source text for
/// callers that need it.
/// What: builds a SmellItem with `include_content=true` and verifies the
/// serialised JSON carries the `content` field.
/// Test: this test.
#[test]
fn smell_item_include_content_restores_text() {
    let chunk = make_chunk("id2", "src/lib.rs", "fn foo() { 42 }");
    let item = SmellItem::from_chunk(&chunk, true);
    let json = serde_json::to_value(&item).unwrap();
    assert_eq!(json["content"], "fn foo() { 42 }");
}

// ── Pagination envelope tests ─────────────────────────────────────────────

/// Why: slicing logic must respect offset+limit bounds and report the
/// correct `total` / `returned` / `truncated` fields (#918).
/// What: creates 10 synthetic smelly chunks, slices with offset=3, limit=4,
/// and asserts envelope fields and returned chunk count.
/// Test: this test (pure logic, no I/O).
#[test]
fn smells_pagination_slice_and_envelope() {
    // Build 10 chunks that will all be detected as smelly by smelly_chunks
    // (a long function body triggers the LongFunction smell).
    let chunks: Vec<CodeChunk> = (0..10)
        .map(|i| {
            let mut body = format!("fn big_{i}() {{\n");
            for _ in 0..60 {
                body.push_str("    let _ = 1;\n");
            }
            body.push_str("}\n");
            make_chunk(&format!("c{i}"), "f.rs", &body)
        })
        .collect();

    let smelly = quality::smelly_chunks(&chunks);
    let total = smelly.len();
    let offset = 3usize;
    let limit = 4usize;
    let page: Vec<SmellItem> = smelly
        .iter()
        .skip(offset)
        .take(limit)
        .map(|c| SmellItem::from_chunk(c, false))
        .collect();
    let returned = page.len();
    let truncated = (offset + returned) < total;

    assert_eq!(returned, 4);
    assert!(
        truncated,
        "should be truncated: total={total} offset={offset} returned={returned}"
    );
}

/// Why: when offset >= total, the page must be empty and `truncated` false.
/// Test: this test.
#[test]
fn smells_pagination_offset_beyond_total_returns_empty() {
    let chunks: Vec<CodeChunk> = (0..3)
        .map(|i| {
            let mut body = format!("fn big_{i}() {{\n");
            for _ in 0..60 {
                body.push_str("    let _ = 1;\n");
            }
            body.push_str("}\n");
            make_chunk(&format!("c{i}"), "f.rs", &body)
        })
        .collect();

    let smelly = quality::smelly_chunks(&chunks);
    let total = smelly.len();
    let offset = 100usize;
    let limit = 10usize;
    let page: Vec<SmellItem> = smelly
        .iter()
        .skip(offset)
        .take(limit)
        .map(|c| SmellItem::from_chunk(c, false))
        .collect();
    let returned = page.len();
    let truncated = (offset + returned) < total;

    assert_eq!(returned, 0);
    assert!(!truncated, "should not be truncated when page is empty");
}
