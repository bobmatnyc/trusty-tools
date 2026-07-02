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
//! KG-triple counts from whichever palaces are *already* resident in the
//! registry's LRU cache (issue #1924 — never force-opens a palace just to
//! count it), and wraps them in a `ConsoleMetricsReport` that the
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
            (palace_count, cached_palace_count, total_drawers, total_vectors, \
            total_kg_triples) and per-palace detail (first 20). Aggregate counts and \
            per-palace detail only reflect palaces already resident in the open-handle \
            LRU cache — palaces on disk that are not currently open contribute to \
            palace_count but not to the count fields (each entry carries a `cached` \
            bool). Used by the trusty-console dashboard metrics poller.",
        "inputSchema": {
            "type": "object",
            "properties": {},
            "required": []
        }
    })
}

/// Computed per-palace statistics, aggregated from cached handles only.
///
/// Why (issue #1924): the previous implementation opened every palace on
/// disk on every poll, defeating the registry's LRU cache and causing
/// sustained high RSS on machines with many palaces. Aggregating only over
/// handles already resident in the cache is a cheap in-memory operation
/// (`PalaceRegistry::peek`, no FS I/O), so this no longer needs its own
/// blocking-pool closure.
/// What: Holds per-palace JSON entries (limited to MAX_PALACES_IN_REPORT),
/// the true on-disk palace count, how many of those contributed to the
/// aggregate totals (`cached_palace_count`), and the totals themselves.
/// Test: Exercised transitively by `handle_console_metrics_returns_valid_report`
/// and `console_metrics_uses_cache_only_and_does_not_evict`.
struct PalaceStats {
    palace_count: usize,
    cached_palace_count: usize,
    total_drawers: usize,
    total_vectors: usize,
    total_kg_triples: usize,
    palace_entries: Vec<Value>,
}

/// MCP `console_metrics` handler — build and return a `ConsoleMetricsReport`.
///
/// Why: The trusty-console metrics poller calls this tool via a supervised
/// stdio MCP connection every `poll_interval` seconds to refresh the
/// `/api/console/metrics/memory` dashboard panel. Issue #1924: the previous
/// implementation force-opened every palace on disk on every poll (via
/// `PalaceRegistry::open_palace`), defeating the 64-slot LRU open-handle
/// cache and causing sustained multi-GB RSS on machines with dozens of
/// palaces. This version only reads statistics from palaces already resident
/// in the cache.
/// What: Lists all palaces from the shared `AppState` (cheap directory walk),
/// then aggregates drawer / vector / KG counts from whichever of those
/// palaces are already open in the registry's LRU cache via
/// `PalaceRegistry::peek` — a lock-and-clone that never touches disk and
/// never evicts or promotes an entry. Palaces not currently cached still
/// count toward `palace_count` but contribute zero to the totals and are
/// flagged `"cached": false` in their `palaces` entry. Always returns `Ok` so
/// the caller receives valid JSON. Returns a raw `serde_json::Value` (not the
/// MCP content envelope) — the dispatcher in `transport/rpc.rs` wraps it.
/// Test: `handle_console_metrics_returns_valid_report` and
/// `console_metrics_uses_cache_only_and_does_not_evict` in tests below.
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

    // Aggregate over cached handles only (issue #1924). `PalaceRegistry::peek`
    // is a `parking_lot::Mutex` lock + `Arc` clone with no I/O, so this runs
    // directly on the async executor without a `spawn_blocking` hop.
    let stats = collect_palace_stats(&state.registry, &palace_infos);

    let metrics = json!({
        "palace_count": stats.palace_count,
        "cached_palace_count": stats.cached_palace_count,
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
        // Schema bumped 1 -> 2 (issue #1924): added `cached_palace_count` and
        // per-palace `cached` flag; totals now reflect cached palaces only.
        2,
    );

    Ok(serde_json::to_value(&report)?)
}

/// Aggregate drawer / vector / KG statistics from already-cached palace handles.
///
/// Why (issue #1924): the tail loop that used to run past
/// `MAX_PALACES_IN_REPORT` called `registry.open_palace()` purely to count a
/// palace, discarding the handle immediately — on a machine with dozens of
/// palaces this thrashed the entire LRU cache every poll cycle. Using
/// `PalaceRegistry::peek` instead means a poll never loads a palace that
/// isn't already resident, so it can never be the cause of cache churn.
/// What: Iterates `palace_infos`; the first MAX_PALACES_IN_REPORT produce a
/// JSON entry in `palace_entries` (with a `cached` bool indicating whether
/// real counts were available); the remainder contribute only to the totals
/// when cached, and are skipped entirely (no error, no I/O) otherwise.
/// Test: Exercised via `handle_console_metrics_returns_valid_report` (empty
/// case) and `console_metrics_uses_cache_only_and_does_not_evict` (mixed
/// cached/evicted case).
fn collect_palace_stats(
    registry: &trusty_common::memory_core::PalaceRegistry,
    palace_infos: &[trusty_common::memory_core::Palace],
) -> PalaceStats {
    let palace_count = palace_infos.len();
    let mut total_drawers: usize = 0;
    let mut total_vectors: usize = 0;
    let mut total_kg_triples: usize = 0;
    let mut cached_palace_count: usize = 0;
    let mut palace_entries: Vec<Value> =
        Vec::with_capacity(palace_count.min(MAX_PALACES_IN_REPORT));

    for info in palace_infos.iter().take(MAX_PALACES_IN_REPORT) {
        let palace_id = info.id.as_str().to_string();
        let name = info.name.clone();

        match registry.peek(&info.id) {
            Some(handle) => {
                let drawer_count = handle.drawers.read().len();
                let vector_count = handle.vector_store.index_size();
                let kg_triple_count = handle.kg.count_active_triples();

                total_drawers += drawer_count;
                total_vectors += vector_count;
                total_kg_triples += kg_triple_count;
                cached_palace_count += 1;

                palace_entries.push(json!({
                    "id": palace_id,
                    "name": name,
                    "drawer_count": drawer_count,
                    "vector_count": vector_count,
                    "kg_triple_count": kg_triple_count,
                    "cached": true,
                }));
            }
            None => {
                // Not currently open — report zero counts rather than
                // force-opening it (issue #1924).
                palace_entries.push(json!({
                    "id": palace_id,
                    "name": name,
                    "drawer_count": 0,
                    "vector_count": 0,
                    "kg_triple_count": 0,
                    "cached": false,
                }));
            }
        }
    }

    // Accumulate totals for any palaces beyond the MAX_PALACES_IN_REPORT
    // cutoff — cache hits only, never opened, no entry added either way.
    for info in palace_infos.iter().skip(MAX_PALACES_IN_REPORT) {
        if let Some(handle) = registry.peek(&info.id) {
            total_drawers += handle.drawers.read().len();
            total_vectors += handle.vector_store.index_size();
            total_kg_triples += handle.kg.count_active_triples();
            cached_palace_count += 1;
        }
    }

    PalaceStats {
        palace_count,
        cached_palace_count,
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
        assert_eq!(result["metrics_schema_version"], 2);
        assert!(result["collected_at_unix"].is_number());
        assert_eq!(result["metrics"]["palace_count"], 0);
        assert_eq!(result["metrics"]["cached_palace_count"], 0);
        assert_eq!(result["metrics"]["total_drawers"], 0);
        assert_eq!(result["metrics"]["total_vectors"], 0);
        assert_eq!(result["metrics"]["total_kg_triples"], 0);
        assert!(result["metrics"]["palaces"].is_array());
        assert_eq!(result["metrics"]["palaces"].as_array().unwrap().len(), 0);
    }

    /// Why (issue #1924 regression guard): `console_metrics` must never
    /// force-open a palace that isn't already in the registry's LRU cache —
    /// doing so on every poll cycle was the root cause of runaway RSS on
    /// machines with many palaces, because it thrashed the whole 64-slot
    /// cache every few seconds.
    /// What: Builds a capacity-2 registry, creates three palaces (which
    /// evicts the first, "a", by the time the third is created), points a
    /// fresh `AppState` at that registry, then calls `handle_console_metrics`
    /// and asserts: (1) `palace_count` reports the true on-disk total (3);
    /// (2) `cached_palace_count` reports only the two still-resident handles;
    /// (3) the registry's cache membership and size are byte-for-byte
    /// unchanged after the call — "a" is still evicted, "b" and "c" are still
    /// present, and `len()` is still 2; (4) the per-palace `palaces` array
    /// flags the evicted entry `"cached": false` and the resident ones
    /// `"cached": true` rather than silently opening or omitting them.
    /// Test: This test.
    #[tokio::test]
    async fn console_metrics_uses_cache_only_and_does_not_evict() {
        use trusty_common::memory_core::{Palace, PalaceId};

        let tmp = tempfile::tempdir().expect("tempdir");
        let data_root = tmp.path().to_path_buf();

        // Capacity 2 so the third `create_palace` call is guaranteed to
        // evict the first, giving us a deterministic cached/evicted split.
        let registry = PalaceRegistry::with_max_open(2);
        for name in ["a", "b", "c"] {
            let palace = Palace {
                id: PalaceId::new(name),
                name: name.to_string(),
                description: None,
                created_at: chrono::Utc::now(),
                data_dir: data_root.join(name),
            };
            registry
                .create_palace(&data_root, palace)
                .unwrap_or_else(|e| panic!("create_palace({name}) failed: {e:#}"));
        }
        assert_eq!(
            registry.len(),
            2,
            "capacity-2 registry must hold only 2 handles after 3 creates"
        );
        assert!(
            registry.peek(&PalaceId::new("a")).is_none(),
            "'a' must already be evicted before console_metrics runs"
        );

        let mut state = crate::AppState::new(data_root);
        state.registry = std::sync::Arc::new(registry);

        let result = handle_console_metrics(&state, serde_json::json!({}))
            .await
            .expect("console_metrics must not return Err");

        assert_eq!(
            result["metrics"]["palace_count"], 3,
            "palace_count reflects all 3 on-disk palaces"
        );
        assert_eq!(
            result["metrics"]["cached_palace_count"], 2,
            "cached_palace_count reflects only the 2 still-resident handles"
        );

        // The metrics call must not have touched the cache at all.
        assert_eq!(
            state.registry.len(),
            2,
            "console_metrics must not grow the LRU cache"
        );
        assert!(
            state.registry.peek(&PalaceId::new("a")).is_none(),
            "console_metrics must not reopen the evicted palace 'a'"
        );
        assert!(state.registry.peek(&PalaceId::new("b")).is_some());
        assert!(state.registry.peek(&PalaceId::new("c")).is_some());

        let entries = result["metrics"]["palaces"]
            .as_array()
            .expect("palaces array present");
        assert_eq!(entries.len(), 3);
        let entry = |id: &str| {
            entries
                .iter()
                .find(|e| e["id"] == id)
                .unwrap_or_else(|| panic!("entry for '{id}' present"))
        };
        assert_eq!(entry("a")["cached"], false, "'a' is not cached");
        assert_eq!(entry("b")["cached"], true, "'b' is cached");
        assert_eq!(entry("c")["cached"], true, "'c' is cached");
    }
}
