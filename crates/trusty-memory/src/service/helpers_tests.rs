//! The two display helpers every recall response goes through.
//!
//! Why this file exists (#6286): both tests lived in `web::tests::recall_tests`
//! beside router-driven ones, and neither ever touched a router — they call
//! `drawer_content_preview` and `recall_entry_json` directly. The router's
//! removal moved them rather than costing them.
//!
//! Test: run with `cargo test -p trusty-memory service::helpers_tests`.

use trusty_common::memory_core::retrieval::RecallResult;
use uuid::Uuid;

use crate::service::{drawer_content_preview, recall_entry_json, DRAWER_PREVIEW_MAX_CHARS};

/// Why: `drawer_content_preview` is what every recall listing renders, so its
/// contract — collapse whitespace, truncate long content, leave short content
/// alone — is worth pinning where a regression shows up immediately.
/// Test: itself.
#[test]
fn drawer_preview_collapses_whitespace_and_truncates() {
    // Short single-line content is returned verbatim.
    assert_eq!(drawer_content_preview("hello world"), "hello world");

    // Multiline / tab-laden content collapses to single-spaced text.
    assert_eq!(
        drawer_content_preview("first line\n\nsecond\tline   third"),
        "first line second line third"
    );

    // Leading / trailing whitespace is stripped.
    assert_eq!(drawer_content_preview("   padded   "), "padded");

    // Empty content yields an empty preview (a fallback signal for clients).
    assert_eq!(drawer_content_preview(""), "");

    // Long content is truncated to DRAWER_PREVIEW_MAX_CHARS with an ellipsis.
    let long = "x".repeat(DRAWER_PREVIEW_MAX_CHARS + 50);
    let preview = drawer_content_preview(&long);
    assert_eq!(preview.chars().count(), DRAWER_PREVIEW_MAX_CHARS);
    assert!(preview.ends_with('…'));

    // Content right at the limit is not truncated.
    let exact = "y".repeat(DRAWER_PREVIEW_MAX_CHARS);
    assert_eq!(drawer_content_preview(&exact), exact);
}

/// Issue #69 — `recall_entry_json` hoists the drawer's fields to the top level
/// so `content` is directly reachable.
///
/// Why: recall used to wrap the drawer under a `"drawer"` key, so a client
/// scanning the top level for `content`/`tags` found nothing and every recall
/// looked empty. This locks the flattened shape so the regression cannot
/// silently return.
/// Test: itself.
#[test]
fn recall_entry_json_hoists_drawer_fields() {
    use trusty_common::memory_core::Drawer;

    let room = Uuid::new_v4();
    let mut drawer = Drawer::new(room, "the answer is 42");
    drawer.tags = vec!["source:kuzu".to_string()];
    drawer.importance = 0.7;

    let entry = recall_entry_json(RecallResult {
        drawer,
        score: 0.699,
        layer: 1,
    });

    // Content must be reachable WITHOUT a `drawer` wrapper (#69).
    assert_eq!(
        entry.get("content").and_then(|v| v.as_str()),
        Some("the answer is 42"),
        "content must be at the top level, got {entry:?}"
    );
    assert!(
        entry.get("drawer").is_none(),
        "the legacy `drawer` wrapper must not be present, got {entry:?}"
    );
    // Other drawer fields are hoisted too.
    assert_eq!(
        entry["importance"].as_f64().map(|f| (f * 10.0).round()),
        Some(7.0)
    );
    assert_eq!(
        entry["tags"][0].as_str(),
        Some("source:kuzu"),
        "tags must be hoisted, got {entry:?}"
    );
    // Ranking metadata sits alongside the hoisted fields.
    assert_eq!(entry["layer"].as_u64(), Some(1));
    assert!(
        entry["score"]
            .as_f64()
            .is_some_and(|s| (s - 0.699).abs() < 1e-6),
        "score must be preserved, got {entry:?}"
    );
}
