//! Durable compression-effectiveness telemetry (epic #3866): the
//! `agents-ws-summary` surface (issue #3867, Slice A) and the `rtk` surface
//! (issue #3870, Slice D) share this ONE sink and ONE schema.
//!
//! Why: We ship several independent compression mechanisms that each
//! compute real token/size numbers every time they fire and then throw them
//! away (stderr `tracing` at best, in-memory-only at worst). Owner
//! directive: "measure compression effectiveness, and use logging to track
//! it" (epic #3866). This module is `trusty-agents`'s durable sink, mirroring
//! `usage/mod.rs`'s existing `.trusty-agents/state/usage.jsonl` append-only
//! JSONL convention so operators can grep/aggregate compression savings the
//! same way they already do for per-dispatch cost.
//! What: `CompressionRecord` is the shared JSONL row shape (schema fixed by
//! issue #3867's body, verbatim — `ts`/`session_id`/`surface`/
//! `surface_detail`/`tokens_before`/`tokens_after`/`ratio`/
//! `working_context_pct_after`/`overhead_pct_after`/`compaction_event`/
//! `duration_ms`/`rounds`, plus `compression_path` as issue #3870's
//! additive, RTK-only field). Two pure/testable builders populate it per
//! surface: `rtk_compression_record` (`llm::tool_loop`'s RTK call site) and
//! `ws_summary_compression_record` (`ctrl::pm_task::dispatch::classification`'s
//! `maybe_summarize_workstream`). `append_compression` is the shared
//! best-effort async append, same shape as `usage::append_usage`.
//! `estimate_tokens_from_bytes` documents the byte-based token proxy RTK
//! uses (no tokenizer in this crate's dependency graph); the ws-summary
//! surface instead reuses the crate's existing `compress::estimate_tokens`
//! (word-count heuristic) per issue #3867's "do not introduce a third
//! estimator" instruction.
//! Test: see `tests` module — record shape/schema round-trip, the `ratio`
//! zero-division guard (mirroring `tm compress`'s `log_compression_stats`
//! convention), byte-estimate rounding, and file create + append semantics.

use serde::Serialize;
use std::path::Path;
use tokio::io::AsyncWriteExt;

use trusty_agents_common::compress::CompressionPath;

/// One row of the compression-effectiveness log — schema shared by every
/// surface (issue #3867's body is the source of truth; `compression_path`
/// is issue #3870's additive RTK-only field on top of it).
///
/// Why: A flat, all-string-or-numeric shape keeps the JSONL trivially
/// grep-able and trivially loadable into pandas / DuckDB / jq, matching
/// `usage::UsageRecord`'s design. Fields with no meaning for a given surface
/// (e.g. `working_context_pct_after` for `agents-ws-summary`, or
/// `compression_path` for anything but `rtk`) serialize to JSON `null`
/// rather than being omitted, so downstream consumers can rely on every key
/// always being present.
/// What: see per-field docs below.
/// Test: `compression_record_serializes_to_valid_jsonl`,
/// `ws_summary_record_serializes_to_valid_jsonl`.
#[derive(Debug, Clone, Serialize)]
pub struct CompressionRecord {
    /// RFC3339 timestamp in UTC of when the compression/summarization
    /// *completed*.
    pub ts: String,
    /// A natural session/turn id when one is cheaply threadable through the
    /// call site; `null` otherwise (issue #3867: "never fabricate one"). Both
    /// surfaces in this crate currently have no such id threaded through, so
    /// this is always `None` here — left as a real field (not dropped) so
    /// the schema stays forward-compatible with a future call site that does
    /// have one.
    pub session_id: Option<String>,
    /// Which compression mechanism produced this record — `"rtk"` or
    /// `"agents-ws-summary"` from this crate; `"tcode-cadence"` /
    /// `"tcode-threshold"` are the sibling `trusty-code` surfaces (Slice A).
    pub surface: String,
    /// Sub-identifier within `surface` — the tool name for `rtk` (e.g.
    /// `"cargo test"`, `"git diff"`), the workstream label for
    /// `agents-ws-summary` (e.g. `"pr-review-tips"`).
    pub surface_detail: String,
    /// Token count (estimator varies by surface — see module docs) of the
    /// pre-compression input.
    pub tokens_before: u32,
    /// Token count of the post-compression output, same estimator as
    /// `tokens_before`.
    pub tokens_after: u32,
    /// `tokens_after / tokens_before`. `0.0` when `tokens_before == 0`
    /// (division-by-zero guard) — mirrors `tm compress`'s
    /// `log_compression_stats` convention
    /// (`crates/trusty-mpm/src/bin/tm/commands/compress.rs`). Unlike that
    /// convention's `pct_reduction`, this is a plain ratio (not a
    /// percentage) per issue #3867's schema — `ratio < 1.0` means the output
    /// shrank, `ratio > 1.0` means it grew.
    pub ratio: f64,
    /// `tcode`-only concept (`CadenceOutcome::working_context_pct`); always
    /// `null` for both `trusty-agents` surfaces.
    pub working_context_pct_after: Option<u8>,
    /// `tcode`-only concept (`CadenceOutcome::overhead_pct`); always `null`
    /// for both `trusty-agents` surfaces.
    pub overhead_pct_after: Option<u8>,
    /// `true` only for `tcode-threshold` fires (Slice B's alarm signal);
    /// always `false` for both `trusty-agents` surfaces.
    pub compaction_event: bool,
    /// Wall-clock milliseconds of the compression/summarization work itself
    /// (the RTK subprocess-or-native-filter call for `rtk`; the LLM
    /// round-trip for `agents-ws-summary`).
    pub duration_ms: u64,
    /// `CadenceOutcome.rounds` for `tcode-cadence`; `1` for every other
    /// surface, including both surfaces in this crate.
    pub rounds: u32,
    /// `Some("rtk_binary" | "native_fallback")` for the `rtk` surface
    /// (issue #3870's additive field, reusing [`CompressionPath::as_str`]
    /// verbatim), `None` for every other surface.
    pub compression_path: Option<String>,
}

/// `tokens_after / tokens_before`, `0.0` when `tokens_before == 0` — the one
/// ratio computation shared by every builder in this module so the
/// zero-division guard can't drift between surfaces.
fn compute_ratio(tokens_before: u32, tokens_after: u32) -> f64 {
    if tokens_before == 0 {
        0.0
    } else {
        f64::from(tokens_after) / f64::from(tokens_before)
    }
}

/// Cheap byte-based token-count proxy, used only by the `rtk` surface.
///
/// Why: RTK compresses raw bytes/chars, not tokens — there is no tokenizer
/// in this crate's dependency graph and adding one just for a telemetry
/// estimate is out of scope. `bytes / 4` is the same cheap heuristic
/// `trusty-code`'s `agent_loop::compaction::estimate_tokens` uses (chars/4
/// for ASCII-dominant tool output); it is a proxy, not a real tokenizer
/// count — do not treat these fields as billing-accurate.
/// What: `(bytes / 4)` saturated into `u32` (tool output is never large
/// enough in practice to overflow, but saturate rather than panic/wrap on
/// the theoretical pathological input).
/// Test: `estimate_tokens_from_bytes_divides_by_four`,
/// `estimate_tokens_from_bytes_saturates_instead_of_overflowing`.
pub fn estimate_tokens_from_bytes(bytes: usize) -> u32 {
    u32::try_from(bytes / 4).unwrap_or(u32::MAX)
}

/// Build the `CompressionRecord` for an RTK (tool-output) compression event.
///
/// Why: Extracted as a pure function (no I/O, no async) so `llm::tool_loop`'s
/// call site is unit-testable without a live LLM loop — mirrors
/// `classification.rs`'s `should_refresh_summary` testability pattern named
/// in issue #3870.
/// What: `surface = "rtk"`, `surface_detail = tool_name`, token counts via
/// [`estimate_tokens_from_bytes`], `compression_path` set from
/// [`CompressionPath::as_str`] verbatim (never restrung). `session_id`,
/// `working_context_pct_after`, `overhead_pct_after` are always `None`;
/// `compaction_event` is always `false`; `rounds` is always `1`.
/// Test: `rtk_compression_record_reports_rtk_binary_path`,
/// `rtk_compression_record_reports_native_fallback_path`.
pub fn rtk_compression_record(
    tool_name: &str,
    bytes_before: usize,
    bytes_after: usize,
    path: CompressionPath,
    duration_ms: u64,
) -> CompressionRecord {
    let tokens_before = estimate_tokens_from_bytes(bytes_before);
    let tokens_after = estimate_tokens_from_bytes(bytes_after);
    CompressionRecord {
        ts: chrono::Utc::now().to_rfc3339(),
        session_id: None,
        surface: "rtk".to_string(),
        surface_detail: tool_name.to_string(),
        tokens_before,
        tokens_after,
        ratio: compute_ratio(tokens_before, tokens_after),
        working_context_pct_after: None,
        overhead_pct_after: None,
        compaction_event: false,
        duration_ms,
        rounds: 1,
        compression_path: Some(path.as_str().to_string()),
    }
}

/// Build the `CompressionRecord` for an `agents-ws-summary` (per-workstream
/// summary refresh) event (issue #3867, instrumentation point 3).
///
/// Why: Extracted as a pure function so the record shape is unit-testable
/// without a mock LLM daemon — the caller
/// (`classification::maybe_summarize_workstream`) is responsible for
/// computing `tokens_before`/`tokens_after` (via `compress::estimate_tokens`,
/// reusing the crate's existing word-count heuristic per issue #3867's
/// "do not introduce a third estimator") and timing the LLM round-trip.
/// What: `surface = "agents-ws-summary"`, `surface_detail = label` (the
/// workstream label). `session_id` is `None` — issue #3867 accepts `null`
/// here unless threading a real session/turn id through is cheap; it is not
/// at this call site without a disproportionate signature change (see
/// `maybe_summarize_workstream`'s call site comment). `working_context_pct_after`,
/// `overhead_pct_after`, `compression_path` are `tcode`/`rtk`-only concepts
/// and always `None` here; `compaction_event` is always `false`; `rounds`
/// is always `1`.
/// Test: `ws_summary_compression_record_reports_shrinkage`.
pub fn ws_summary_compression_record(
    label: &str,
    tokens_before: u32,
    tokens_after: u32,
    duration_ms: u64,
) -> CompressionRecord {
    CompressionRecord {
        ts: chrono::Utc::now().to_rfc3339(),
        session_id: None,
        surface: "agents-ws-summary".to_string(),
        surface_detail: label.to_string(),
        tokens_before,
        tokens_after,
        ratio: compute_ratio(tokens_before, tokens_after),
        working_context_pct_after: None,
        overhead_pct_after: None,
        compaction_event: false,
        duration_ms,
        rounds: 1,
        compression_path: None,
    }
}

/// Append a `CompressionRecord` as a single JSONL line to
/// `<project_dir>/.trusty-agents/state/compression.jsonl`.
///
/// Why: Same append-only JSONL rationale as `usage::append_usage` — each
/// line is atomic on POSIX for a single short write, survives concurrent
/// writers from multi-agent runs, and needs no schema migration tooling.
/// `rtk`'s call site is a per-tool-call hot path; any failure here must
/// never propagate and stall a tool result from reaching the model, so
/// every I/O error is logged at debug level and swallowed — the same
/// best-effort posture applies to the much-less-frequent `agents-ws-summary`
/// call site for consistency.
/// What: Best-effort `mkdir -p` of `.trusty-agents/state`, then opens the
/// file in `create + append` mode, writes one `serde_json::to_string(record)`
/// line followed by `\n`, then flushes so the write is visible to
/// subsequent readers within the same process.
/// Test: `append_compression_creates_file`, `append_compression_appends`.
pub async fn append_compression(project_dir: &Path, record: &CompressionRecord) {
    let state_dir = project_dir.join(".trusty-agents").join("state");
    let path = state_dir.join("compression.jsonl");
    let _ = tokio::fs::create_dir_all(&state_dir).await;
    let line = match serde_json::to_string(record) {
        Ok(s) => format!("{s}\n"),
        Err(e) => {
            tracing::debug!(error = %e, "compression: serialize failed");
            return;
        }
    };
    match tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .await
    {
        Ok(mut f) => {
            if let Err(e) = f.write_all(line.as_bytes()).await {
                tracing::debug!(error = %e, path = %path.display(), "compression: write failed");
                return;
            }
            if let Err(e) = f.flush().await {
                tracing::debug!(error = %e, path = %path.display(), "compression: flush failed");
            }
        }
        Err(e) => {
            tracing::debug!(error = %e, path = %path.display(), "compression: open failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compression_record_serializes_to_valid_jsonl() {
        let r = CompressionRecord {
            ts: "2026-07-24T19:00:00Z".to_string(),
            session_id: None,
            surface: "rtk".to_string(),
            surface_detail: "cargo test".to_string(),
            tokens_before: 400,
            tokens_after: 100,
            ratio: 0.25,
            working_context_pct_after: None,
            overhead_pct_after: None,
            compaction_event: false,
            duration_ms: 12,
            rounds: 1,
            compression_path: Some("rtk_binary".to_string()),
        };
        let json = serde_json::to_string(&r).expect("serialize");
        assert!(!json.contains('\n'), "JSONL invariant: single line");
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["surface"], "rtk");
        assert_eq!(parsed["surface_detail"], "cargo test");
        assert_eq!(parsed["tokens_before"], 400);
        assert_eq!(parsed["tokens_after"], 100);
        assert_eq!(parsed["ratio"], 0.25);
        assert_eq!(parsed["duration_ms"], 12);
        assert_eq!(parsed["rounds"], 1);
        assert_eq!(parsed["compaction_event"], false);
        assert_eq!(parsed["compression_path"], "rtk_binary");
        assert!(parsed["session_id"].is_null());
        assert!(parsed["working_context_pct_after"].is_null());
        assert!(parsed["overhead_pct_after"].is_null());
    }

    #[test]
    fn ws_summary_record_serializes_to_valid_jsonl() {
        let r = ws_summary_compression_record("pr-review-tips", 1000, 200, 850);
        let json = serde_json::to_string(&r).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["surface"], "agents-ws-summary");
        assert_eq!(parsed["surface_detail"], "pr-review-tips");
        assert_eq!(parsed["tokens_before"], 1000);
        assert_eq!(parsed["tokens_after"], 200);
        assert_eq!(parsed["ratio"], 0.2);
        assert_eq!(parsed["duration_ms"], 850);
        assert!(parsed["compression_path"].is_null());
        assert!(parsed["session_id"].is_null());
    }

    #[test]
    fn compression_path_is_null_for_non_rtk_surfaces() {
        // Slice A's non-RTK surfaces (tcode-cadence, agents-ws-summary, ...)
        // have no rtk-vs-native distinction — must serialize to JSON `null`,
        // not be omitted, so downstream consumers can rely on the key always
        // being present.
        let r = ws_summary_compression_record("session-1", 1000, 600, 5);
        let json = serde_json::to_string(&r).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(parsed["compression_path"].is_null());
    }

    #[test]
    fn ratio_is_zero_for_empty_input() {
        assert_eq!(compute_ratio(0, 0), 0.0);
        let r = rtk_compression_record("bash", 0, 0, CompressionPath::NativeFallback, 0);
        assert_eq!(r.ratio, 0.0);
    }

    #[test]
    fn ratio_can_exceed_one_when_output_expands() {
        // If a compression path ever expands the input, the record must
        // still emit — a ratio > 1.0 is the honest signal that compression
        // made things worse, not a bug in this function to clamp away
        // (mirrors `tm compress`'s unclamped `pct_reduction`, PR #1968).
        let r = rtk_compression_record("bash", 40, 80, CompressionPath::NativeFallback, 0);
        assert!(
            r.ratio > 1.0,
            "expansion must report a ratio > 1.0, not clamp to 1.0"
        );
    }

    #[test]
    fn estimate_tokens_from_bytes_divides_by_four() {
        assert_eq!(estimate_tokens_from_bytes(400), 100);
        assert_eq!(estimate_tokens_from_bytes(0), 0);
        assert_eq!(estimate_tokens_from_bytes(3), 0);
    }

    #[test]
    fn estimate_tokens_from_bytes_saturates_instead_of_overflowing() {
        // usize on 64-bit hosts can exceed u32::MAX * 4; must saturate, not
        // panic or silently wrap.
        let huge = (u32::MAX as usize) * 8;
        assert_eq!(estimate_tokens_from_bytes(huge), u32::MAX);
    }

    #[test]
    fn rtk_compression_record_reports_rtk_binary_path() {
        let r = rtk_compression_record("cargo test", 400, 100, CompressionPath::RtkBinary, 7);
        assert_eq!(r.surface, "rtk");
        assert_eq!(r.surface_detail, "cargo test");
        assert_eq!(r.tokens_before, 100);
        assert_eq!(r.tokens_after, 25);
        assert_eq!(r.duration_ms, 7);
        assert_eq!(r.compression_path.as_deref(), Some("rtk_binary"));
    }

    #[test]
    fn rtk_compression_record_reports_native_fallback_path() {
        let r = rtk_compression_record("git diff", 40, 40, CompressionPath::NativeFallback, 3);
        assert_eq!(r.compression_path.as_deref(), Some("native_fallback"));
        assert_eq!(r.ratio, 1.0);
    }

    #[test]
    fn ws_summary_compression_record_reports_shrinkage() {
        let r = ws_summary_compression_record("feat-x", 800, 150, 620);
        assert_eq!(r.surface, "agents-ws-summary");
        assert_eq!(r.surface_detail, "feat-x");
        assert!(r.tokens_before > r.tokens_after);
        assert_eq!(r.rounds, 1);
        assert!(!r.compaction_event);
    }

    #[tokio::test]
    async fn append_compression_creates_file() {
        let dir = tempfile::tempdir().unwrap();
        let r = rtk_compression_record("bash", 400, 100, CompressionPath::NativeFallback, 1);
        append_compression(dir.path(), &r).await;
        let path = dir.path().join(".trusty-agents/state/compression.jsonl");
        assert!(path.exists(), "compression.jsonl should be created");
        let contents = tokio::fs::read_to_string(&path).await.unwrap();
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 1, "exactly one line after one append");
        let parsed: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(parsed["surface"], "rtk");
        assert_eq!(parsed["surface_detail"], "bash");
    }

    #[tokio::test]
    async fn append_compression_appends() {
        let dir = tempfile::tempdir().unwrap();
        let r1 = rtk_compression_record("cargo test", 400, 100, CompressionPath::RtkBinary, 1);
        let r2 = ws_summary_compression_record("git diff", 200, 150, 2);
        append_compression(dir.path(), &r1).await;
        append_compression(dir.path(), &r2).await;
        let path = dir.path().join(".trusty-agents/state/compression.jsonl");
        let contents = tokio::fs::read_to_string(&path).await.unwrap();
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 2, "second append should not overwrite");
        let p1: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        let p2: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(p1["surface"], "rtk");
        assert_eq!(p1["surface_detail"], "cargo test");
        assert_eq!(p2["surface"], "agents-ws-summary");
        assert_eq!(p2["surface_detail"], "git diff");
    }
}
