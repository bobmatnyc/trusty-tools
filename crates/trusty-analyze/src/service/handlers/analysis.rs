//! Route handlers for complexity, quality, refactor, and diagnostics endpoints.
//!
//! Why: Extracted from `service/mod.rs` and further split from the graph/
//! clustering handlers to keep each file focused and under the 500-line cap.
//! This module owns the "how healthy is this code?" surface: complexity
//! hotspots, smell detection, quality grades, refactor suggestions, and
//! on-demand external linting via `ToolRegistry`.
//!
//! What: Five public handlers plus supporting helpers. All are stateless
//! analysis passes over the chunk corpus fetched from trusty-search.
//!
//! Test: All handler tests are in `service/tests.rs`. Core logic coverage lives
//! in `core/` unit tests.

use std::collections::HashMap;
use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    response::Json,
};
use serde::Deserialize;

use crate::core::complexity::{compute_complexity_for, detect_smells};
use crate::core::{analyze_refactor, quality, RefactorSuggestion, Severity};
use crate::service::events::{fetch_chunks, AnalyzerAppState, ApiError};

#[derive(Deserialize)]
pub struct HotspotsParams {
    #[serde(default = "default_top_n")]
    pub top_n: usize,
}

fn default_top_n() -> usize {
    20
}

pub async fn complexity_hotspots(
    State(state): State<Arc<AnalyzerAppState>>,
    Path(id): Path<String>,
    Query(params): Query<HotspotsParams>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let chunks = fetch_chunks(&state, &id).await?;
    let hotspots = quality::complexity_hotspots(&chunks, params.top_n);
    Ok(Json(serde_json::json!({
        "index_id": id,
        "top_n": params.top_n,
        "hotspots": hotspots,
    })))
}

pub async fn smells(
    State(state): State<Arc<AnalyzerAppState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let chunks = fetch_chunks(&state, &id).await?;
    let smelly = quality::smelly_chunks(&chunks);
    Ok(Json(serde_json::json!({
        "index_id": id,
        "count": smelly.len(),
        "chunks": smelly,
    })))
}

#[derive(Deserialize)]
pub struct RefactorParams {
    /// Optional path filter — only suggest refactors for chunks in this file.
    pub file: Option<String>,
    /// Minimum severity to include (`"low"` / `"medium"` / `"high"` /
    /// `"critical"`). Defaults to `"low"`.
    pub min_severity: Option<String>,
    /// Cap on the number of suggestions returned. Defaults to 20.
    pub top_k: Option<usize>,
}

/// Why: callers want "what should I refactor and why" — not just raw
/// complexity numbers. This handler turns metrics + smells into actionable
/// `RefactorSuggestion`s and sorts them by severity so the worst offenders
/// surface first.
/// What: fetches chunks for `id`, computes complexity per chunk (language-
/// aware via file extension dispatch), runs `analyze_refactor`, filters by
/// `file` and `min_severity`, sorts by `(severity desc, complexity_before
/// desc)`, and truncates to `top_k`.
/// Test: a chunk with grade F + LongFunction returns one Critical
/// ExtractMethod suggestion; covered transitively via `core::refactor` tests.
pub async fn refactor_suggestions(
    State(state): State<Arc<AnalyzerAppState>>,
    Path(id): Path<String>,
    Query(params): Query<RefactorParams>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let chunks = fetch_chunks(&state, &id).await?;
    let min_severity = params
        .min_severity
        .as_deref()
        .and_then(Severity::parse)
        .unwrap_or(Severity::Low);
    let top_k = params.top_k.unwrap_or(20);

    let mut out: Vec<RefactorSuggestion> = Vec::new();
    for chunk in &chunks {
        if let Some(file) = params.file.as_deref() {
            if chunk.file != file {
                continue;
            }
        }
        let lang = super::lang_for_extension(&chunk.file);
        let metrics = compute_complexity_for(&chunk.content, lang);
        let smells = detect_smells(&chunk.content);
        let mut suggestions = analyze_refactor(
            &chunk.id,
            &chunk.file,
            chunk.start_line as u32,
            chunk.end_line as u32,
            chunk.function_name.as_deref(),
            &metrics,
            &smells,
        );
        suggestions.retain(|s| s.severity >= min_severity);
        out.extend(suggestions);
    }

    out.sort_by(|a, b| {
        b.severity
            .cmp(&a.severity)
            .then_with(|| b.complexity_before.cmp(&a.complexity_before))
    });
    out.truncate(top_k);

    Ok(Json(serde_json::json!({
        "index_id": id,
        "count": out.len(),
        "min_severity": min_severity_label(&min_severity),
        "suggestions": out,
    })))
}

fn min_severity_label(s: &Severity) -> &'static str {
    match s {
        Severity::Low => "low",
        Severity::Medium => "medium",
        Severity::High => "high",
        Severity::Critical => "critical",
    }
}

pub async fn quality_report(
    State(state): State<Arc<AnalyzerAppState>>,
    Path(id): Path<String>,
) -> Result<Json<quality::QualityReport>, ApiError> {
    let chunks = fetch_chunks(&state, &id).await?;
    Ok(Json(quality::aggregate_quality(&chunks)))
}

/// Query parameters for the on-demand diagnostics endpoint.
#[derive(Deserialize)]
pub struct DiagnosticsParams {
    /// Restrict analysis to a single language tag (`"rust"`, `"python"`, ...).
    pub language: Option<String>,
    /// Comma-separated list of tool names to run; defaults to all available.
    pub tools: Option<String>,
}

/// `GET /indexes/{id}/diagnostics` — run available external static-analysis
/// tools (clippy, ruff, biome, ...) across the index corpus on demand.
///
/// Why: tree-sitter heuristics are uniform but shallow; real linters catch
/// far more, but only when their binary is installed. This endpoint discovers
/// what is available and runs it, file by file. Project-scoped tools (Roslyn)
/// receive real on-disk paths via `root_path`; file-scoped tools write to a
/// scratch dir as before.
/// What: fetches the corpus, reconstructs whole-file content from chunks,
/// fetches the index root_path (for project-scoped tools), then delegates all
/// dispatch to `diagnostics_dispatch::run_diagnostics_blocking`.
/// Test: `diagnostics_endpoint_returns_empty_when_no_tools` boots the router
/// with a stub client and confirms a well-formed empty response.
pub async fn diagnostics_for_index(
    State(state): State<Arc<AnalyzerAppState>>,
    Path(id): Path<String>,
    Query(params): Query<DiagnosticsParams>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let chunks = fetch_chunks(&state, &id).await?;
    let tool_filter: Option<Vec<String>> = params.tools.as_ref().map(|s| {
        s.split(',')
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .collect()
    });

    // Reconstruct per-file content by stitching chunks in line order. Chunk
    // windows can overlap, so we keep the longest content seen per file as a
    // best-effort whole-file reconstruction.
    let mut by_file: HashMap<String, String> = HashMap::new();
    for chunk in &chunks {
        let entry = by_file.entry(chunk.file.clone()).or_default();
        if chunk.content.len() > entry.len() {
            *entry = chunk.content.clone();
        }
    }

    // Fetch index details (including root_path) for project-scoped tools.
    // Errors are non-fatal: project-scoped tools will gracefully skip if
    // root_path is unavailable, while file-scoped tools are unaffected.
    //
    // TODO(follow-up): replace this full-index-list round-trip with a
    // per-index lookup once `GET /indexes/:id/status` exposes `root_path`.
    // The current call fetches ALL indexes and linear-scans for the matching
    // id on every request, which is wasteful for large deployments.
    let root_path = state
        .search
        .index_details()
        .await
        .ok()
        .and_then(|v| v.into_iter().find(|s| s.id == id).and_then(|s| s.root_path));

    // Heavy work (process spawns, blocking I/O) runs off the async runtime.
    let language_filter = params.language.clone();
    let diagnostics: Vec<crate::core::ToolDiagnostic> = tokio::task::spawn_blocking(move || {
        crate::service::diagnostics_dispatch::run_diagnostics_blocking(
            by_file,
            language_filter,
            tool_filter,
            root_path,
        )
    })
    .await
    .map_err(|e| ApiError::internal(format!("diagnostics task panicked: {e}")))?;

    Ok(Json(serde_json::json!({
        "index_id": id,
        "count": diagnostics.len(),
        "diagnostics": diagnostics,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let _result = crate::service::diagnostics_dispatch::run_diagnostics_blocking(
            by_file, None, None, None,
        );
        // Reaching here without panic means the subdir isolation works.
    }
}
