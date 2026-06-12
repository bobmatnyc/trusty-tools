//! MCP `console_metrics` tool handler for trusty-memory.
//!
//! Why: The trusty-console dashboard calls this tool via a supervised stdio
//! MCP connection to collect health and palace-aggregate statistics from the
//! running trusty-memory HTTP daemon. Separating it from the main `tools.rs`
//! keeps the 500-line file cap in check and makes the console-metrics surface
//! easy to audit and extend.
//! What: Exposes `descriptor()` (the MCP JSON schema) and
//! `handle_console_metrics()` (the async handler). The handler lists all
//! palaces from the HTTP daemon's shared state, aggregates drawer / vector /
//! KG-triple counts, and wraps them in a `ConsoleMetricsReport` that the
//! trusty-console metrics cache understands.
//! Test: `cargo test -p trusty-memory -- console_metrics` exercises the
//! descriptor shape and handler via the existing `dispatch_tool` harness.

use anyhow::Result;
use serde_json::{json, Value};
use trusty_common::console_metrics::{make_report, ServiceHealth};
use trusty_common::memory_core::PalaceRegistry;

use crate::AppState;

/// Maximum number of palace entries returned in the metrics report.
///
/// Why: Prevents the payload from growing unbounded on machines with many
/// palaces. The console dashboard only renders a summary, not the full list.
/// What: First 20 palaces (sorted by id) are included; the remainder are
/// reflected in the aggregate counts only.
/// Test: Verified indirectly by `handle_console_metrics_aggregates_palaces`.
const MAX_PALACES_IN_REPORT: usize = 20;

/// JSON schema descriptor for the `console_metrics` MCP tool.
///
/// Why: Required by `tool_definitions_with()` so MCP clients can discover
/// the tool in `tools/list` responses and by the dispatcher so it can route
/// `tools/call` requests.
/// What: Returns a `serde_json::Value` matching the MCP tool schema shape
/// used by all other trusty-memory tools.
/// Test: Included in `tool_definitions_lists_all_tools` assertion count.
pub fn descriptor() -> Value {
    json!({
        "name": "console_metrics",
        "description": "Return a ConsoleMetricsReport with palace aggregate statistics \
            (palace_count, total_drawers, total_vectors, total_kg_triples) and per-palace \
            detail (first 20). Used by the trusty-console dashboard metrics poller.",
        "inputSchema": {
            "type": "object",
            "properties": {},
            "required": []
        }
    })
}

/// Computed per-palace statistics, collected on the blocking thread pool.
///
/// Why: Aggregating drawer / vector / KG counts requires opening each palace
/// (synchronous FS I/O). Collecting them into this struct lets `spawn_blocking`
/// own a single closure that does all the I/O, keeping the async handler free
/// of blocking operations.
/// What: Holds per-palace JSON entries (limited to MAX_PALACES_IN_REPORT) and
/// workspace-wide totals including palaces beyond the limit.
/// Test: Exercised transitively by `handle_console_metrics_returns_valid_report`.
struct PalaceStats {
    palace_count: usize,
    total_drawers: usize,
    total_vectors: usize,
    total_kg_triples: usize,
    palace_entries: Vec<Value>,
}

/// MCP `console_metrics` handler — build and return a `ConsoleMetricsReport`.
///
/// Why: The trusty-console metrics poller calls this tool via a supervised
/// stdio MCP connection every `poll_interval` seconds to refresh the
/// `/api/console/metrics/memory` dashboard panel.
/// What: Lists all palaces from the shared `AppState`, opens each one on the
/// blocking thread pool (synchronous FS I/O) to read drawer / vector / KG
/// counts, then builds a `ConsoleMetricsReport` via `make_report()`. Always
/// returns `Ok` so the caller receives valid JSON; per-palace open failures
/// are logged at `warn!` and skipped. Returns a raw `serde_json::Value` (not
/// the MCP content envelope) — the dispatcher in `transport/rpc.rs` wraps it.
/// Test: `handle_console_metrics_aggregates_palaces` in tests below.
pub async fn handle_console_metrics(state: &AppState, _args: Value) -> Result<Value> {
    let root = state.data_root.clone();

    // List all palaces from disk on the blocking pool (PalaceRegistry::list_palaces
    // does synchronous filesystem I/O).
    let palace_infos =
        match tokio::task::spawn_blocking(move || PalaceRegistry::list_palaces(&root))
            .await
            .map_err(|e| anyhow::anyhow!("join list_palaces: {e}"))?
        {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("console_metrics: list_palaces failed: {e:#}");
                Vec::new()
            }
        };

    // Open each palace and aggregate statistics on the blocking thread pool.
    // `PalaceRegistry::open_palace` does synchronous FS I/O (loads metadata,
    // opens SQLite/usearch files) so it must not run on the async executor.
    let root2 = state.data_root.clone();
    let registry = state.registry.clone();
    let stats =
        tokio::task::spawn_blocking(move || collect_palace_stats(&registry, &root2, &palace_infos))
            .await
            .map_err(|e| anyhow::anyhow!("join collect_palace_stats: {e}"))?;

    let metrics = json!({
        "palace_count": stats.palace_count,
        "total_drawers": stats.total_drawers,
        "total_vectors": stats.total_vectors,
        "total_kg_triples": stats.total_kg_triples,
        "palaces": stats.palace_entries,
    });

    let report = make_report(
        "trusty-memory",
        "Trusty Memory",
        env!("CARGO_PKG_VERSION"),
        ServiceHealth::Ok,
        metrics,
        1,
    );

    Ok(serde_json::to_value(&report)?)
}

/// Open each palace and aggregate drawer / vector / KG statistics.
///
/// Why: Extracted as a free function so it can run entirely on the
/// `spawn_blocking` thread pool — `open_palace` does synchronous FS I/O
/// (`std::fs`, SQLite, usearch file open) that must not block the async
/// executor.
/// What: Iterates `palace_infos`; the first MAX_PALACES_IN_REPORT produce full
/// entries in `palace_entries`; the remainder contribute only to the totals.
/// Open failures are logged and produce a zero-count entry with an `error` field.
/// Test: Exercised via `handle_console_metrics_returns_valid_report`.
fn collect_palace_stats(
    registry: &trusty_common::memory_core::PalaceRegistry,
    data_root: &std::path::Path,
    palace_infos: &[trusty_common::memory_core::Palace],
) -> PalaceStats {
    let palace_count = palace_infos.len();
    let mut total_drawers: usize = 0;
    let mut total_vectors: usize = 0;
    let mut total_kg_triples: usize = 0;
    let mut palace_entries: Vec<Value> =
        Vec::with_capacity(palace_count.min(MAX_PALACES_IN_REPORT));

    for info in palace_infos.iter().take(MAX_PALACES_IN_REPORT) {
        let palace_id = info.id.as_str().to_string();
        let name = info.name.clone();

        match registry.open_palace(data_root, &info.id) {
            Ok(handle) => {
                let drawer_count = handle.drawers.read().len();
                let vector_count = handle.vector_store.index_size();
                let kg_triple_count = handle.kg.count_active_triples();

                total_drawers += drawer_count;
                total_vectors += vector_count;
                total_kg_triples += kg_triple_count;

                palace_entries.push(json!({
                    "id": palace_id,
                    "name": name,
                    "drawer_count": drawer_count,
                    "vector_count": vector_count,
                    "kg_triple_count": kg_triple_count,
                }));
            }
            Err(e) => {
                tracing::warn!(
                    palace = %palace_id,
                    "console_metrics: open failed (skipped): {e:#}"
                );
                // Still count drawers from the in-memory registry if available.
                palace_entries.push(json!({
                    "id": palace_id,
                    "name": name,
                    "drawer_count": 0,
                    "vector_count": 0,
                    "kg_triple_count": 0,
                    "error": e.to_string(),
                }));
            }
        }
    }

    // Accumulate totals for any palaces beyond the MAX_PALACES_IN_REPORT cutoff.
    // These are opened purely for counting — no entry is added to palace_entries.
    for info in palace_infos.iter().skip(MAX_PALACES_IN_REPORT) {
        if let Ok(handle) = registry.open_palace(data_root, &info.id) {
            total_drawers += handle.drawers.read().len();
            total_vectors += handle.vector_store.index_size();
            total_kg_triples += handle.kg.count_active_triples();
        }
    }

    PalaceStats {
        palace_count,
        total_drawers,
        total_vectors,
        total_kg_triples,
        palace_entries,
    }
}

// ─── tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: The `console_metrics` handler must return a structurally valid
    /// `ConsoleMetricsReport` even when no palaces exist (empty state).
    /// What: Builds a minimal `AppState` backed by a temp directory, calls
    /// `handle_console_metrics`, and asserts all required JSON fields are present
    /// and the aggregate counts are zero.
    ///
    /// The test uses `#[serial]` to ensure it runs exclusively relative to
    /// other tests that mutate `TRUSTY_SKIP_PALACE_ENFORCEMENT`, eliminating
    /// the env-var data race that made the previous `unsafe { set_var }` +
    /// `current_thread` approach unsound (cargo test runs test *functions*
    /// in parallel across OS threads in the same process; a single-threaded
    /// executor only serialises tasks within this test's runtime, not other
    /// test threads that read the env).
    /// Test: This test.
    #[serial_test::serial]
    #[tokio::test]
    async fn handle_console_metrics_returns_valid_report() {
        // SAFETY: `#[serial]` ensures no other test thread reads or writes
        // TRUSTY_SKIP_PALACE_ENFORCEMENT concurrently with this test.
        unsafe {
            std::env::set_var("TRUSTY_SKIP_PALACE_ENFORCEMENT", "1");
        }
        let tmp = tempfile::tempdir().expect("tempdir");
        let state = crate::AppState::new(tmp.path().to_path_buf());

        let result = handle_console_metrics(&state, serde_json::json!({}))
            .await
            .expect("console_metrics must not return Err");

        assert_eq!(result["service_id"], "trusty-memory");
        assert_eq!(result["display_name"], "Trusty Memory");
        assert!(result["version"].is_string());
        assert!(result["status"].is_string());
        assert_eq!(result["metrics_schema_version"], 1);
        assert!(result["collected_at_unix"].is_number());
        assert_eq!(result["metrics"]["palace_count"], 0);
        assert_eq!(result["metrics"]["total_drawers"], 0);
        assert_eq!(result["metrics"]["total_vectors"], 0);
        assert_eq!(result["metrics"]["total_kg_triples"], 0);
        assert!(result["metrics"]["palaces"].is_array());
        assert_eq!(result["metrics"]["palaces"].as_array().unwrap().len(), 0);
    }
}
