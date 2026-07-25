//! RTK compression-effectiveness recording hook for `tool_loop`'s per-tool
//! result post-processing (issue #3870, epic #3866 Slice D).
//!
//! Why: Split out of `mod.rs` (already over this project's 500-line file
//! cap before this slice) so the new logic doesn't grow an already-oversized
//! file further; also keeps the record-building + append-spawn logic
//! independently unit-testable without driving a full LLM loop, mirroring
//! `classification.rs`'s `should_refresh_summary` testability pattern.
//! What: `compress_success_result`, called from `mod.rs`'s tool-result loop
//! for every `ToolResult::Success`.
//! Test: `tests::compress_success_result_appends_rtk_record_with_correct_fields`.

/// Compress a successful tool result and durably record RTK
/// compression-effectiveness stats (issue #3870, epic #3866 Slice D).
///
/// Why: `compress_tool_output_async_with_path` (#1959) already reports which
/// path (`rtk` binary vs. native fallback) produced the result; the previous
/// call site discarded that signal by using the stats-free
/// `compress_tool_output_async` wrapper.
/// What: Compresses `raw_str` via `compress_tool_output_async_with_path`,
/// builds a `CompressionRecord` via `crate::compression::rtk_compression_record`
/// from the pre/post byte lengths, then spawns a **detached, best-effort**
/// append to `<project_dir>/.trusty-agents/state/compression.jsonl` — this is
/// a per-tool-call hot path, so a slow disk or logging failure must never
/// stall a tool result from reaching the model. Returns the compressed text
/// plus the spawned task's `JoinHandle`; production call sites drop the
/// handle (fire-and-forget), tests await it for deterministic assertions.
/// Test: `compress_success_result_appends_rtk_record_with_correct_fields`.
pub(super) async fn compress_success_result(
    tool_name: &str,
    raw_str: &str,
    project_dir: std::path::PathBuf,
) -> (String, tokio::task::JoinHandle<()>) {
    let started = std::time::Instant::now();
    let (content_str, path) =
        trusty_agents_common::compress::compress_tool_output_async_with_path(tool_name, raw_str)
            .await;
    let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let record = crate::compression::rtk_compression_record(
        tool_name,
        raw_str.len(),
        content_str.len(),
        path,
        duration_ms,
    );
    let handle = tokio::spawn(async move {
        crate::compression::append_compression(&project_dir, &record).await;
    });
    (content_str, handle)
}
