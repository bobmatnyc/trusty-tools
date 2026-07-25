//! `agents-ws-summary` compression-telemetry helper for
//! `classification::maybe_summarize_workstream` (issue #3867, epic #3866
//! Slice A).
//!
//! Why: Split out of `classification.rs` (already over this project's
//! 500-line file cap before this slice) so the new logic doesn't grow an
//! already-oversized file further — mirrors `llm::tool_loop`'s
//! `compression_hook` extraction for the sibling `rtk` surface (issue
//! #3870).
//! What: `record`, called from `maybe_summarize_workstream` right after its
//! existing `tracing::info!("workstream summary refreshed")` call.
//! Test: `classification_tests.rs`'s
//! `maybe_summarize_workstream_emits_exactly_one_compression_event` covers
//! this at the real call site; the token-count/record-shape logic itself is
//! covered by `compression.rs`'s `ws_summary_compression_record` tests.

use std::path::Path;

/// Build and durably append the `agents-ws-summary` `CompressionRecord` for
/// one workstream-summary refresh.
///
/// Why: `tokens_before`/`tokens_after` reuse the crate's existing
/// `compress::estimate_tokens` word-count heuristic (no new estimator, per
/// issue #3867's "do not introduce a third estimator" instruction).
/// `session_id` is left `None` inside `ws_summary_compression_record` — no
/// session/turn id is threadable through from this call site without a
/// signature change disproportionate to this slice (issue #3867's explicit
/// fallback for that case). Awaited inline rather than spawned like RTK's
/// hot-path append — this only fires every `summarize_every` turns, so
/// synchronous best-effort is fine and keeps the call site simple.
/// What: Estimates tokens for `joined` (the pre-summary turn window) and
/// `summary_text`, builds the record via
/// `crate::compression::ws_summary_compression_record`, and appends it to
/// `<project_path>/.trusty-agents/state/compression.jsonl` via
/// `crate::compression::append_compression` — the SAME sink RTK's
/// `compress_success_result` writes to (one writer, two surfaces).
/// Test: see module doc.
pub(super) async fn record(
    project_path: &Path,
    label: &str,
    joined: &str,
    summary_text: &str,
    duration_ms: u64,
) {
    let tokens_before = u32::try_from(crate::compress::estimate_tokens(joined)).unwrap_or(u32::MAX);
    let tokens_after =
        u32::try_from(crate::compress::estimate_tokens(summary_text)).unwrap_or(u32::MAX);
    let record = crate::compression::ws_summary_compression_record(
        label,
        tokens_before,
        tokens_after,
        duration_ms,
    );
    crate::compression::append_compression(project_path, &record).await;
}
