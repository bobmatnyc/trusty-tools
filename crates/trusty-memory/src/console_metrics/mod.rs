//! MCP `console_metrics` tool handler for trusty-memory.
//!
//! Why: The trusty-console dashboard calls this tool via a supervised stdio
//! MCP connection to collect health and palace-aggregate statistics from the
//! running trusty-memory HTTP daemon. Separating it from the main `tools.rs`
//! keeps the 500-line file cap in check and makes the console-metrics surface
//! easy to audit and extend.
//! What: Exposes `descriptor()` (the MCP JSON schema) and
//! `handle_console_metrics()` (the async handler). The handler lists all
//! palaces from the HTTP daemon's shared state and reports drawer / vector /
//! room / KG-triple counts for every one of them: from the registry's LRU
//! cache when a palace is already resident, and otherwise straight off the
//! palace's redb files via [`disk_stats`], which never opens the palace
//! (issue #1924 stands — nothing here enters the cache). #6928: the report
//! also carries this daemon's own RAM footprint and the palace store's disk
//! size, from [`usage`] — the two figures the console's memory tab exists to
//! display.
//! Test: `cargo test -p trusty-memory -- console_metrics` exercises the
//! descriptor shape and handler via the existing `dispatch_tool` harness.

mod disk_stats;
mod usage;

use anyhow::Result;
use serde_json::{json, Value};
use trusty_common::console_metrics::{make_report, ServiceHealth};
use trusty_common::memory_core::store::rooms::list_room_summaries;
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
            (palace_count, counted_palace_count, cached_palace_count, total_drawers, \
            total_vectors, total_rooms, total_kg_triples) and per-palace detail \
            (first 20). Counts come from the open-handle LRU cache for a resident \
            palace and from a read-only pass over the palace's redb files otherwise, \
            so a palace that is merely closed still reports real numbers; no palace is \
            opened to be counted. Each entry carries `cached` (was it resident) and \
            `stats_source` (`cache`, `disk`, or `unavailable`); an unavailable entry \
            carries `stats_error` and null counts. Each entry also carries \
            `last_used_unix` — when a recall, remember or note last touched that \
            palace, or null when none has since the stamp shipped, and `disk_bytes` \
            — that palace directory's allocated size. Schema 5 (#6928) adds this \
            daemon's own usage: `ram_bytes` (physical footprint, null where the OS \
            has no such counter), `disk_bytes` (allocated size of the whole palace \
            store, the figure `du -s` reports), `data_root`, and — present only where \
            the OS supplies them — the `ram_heap_bytes` / `ram_file_backed_bytes` / \
            `ram_compressed_bytes` split. Used by the trusty-console dashboard \
            metrics poller.",
        "inputSchema": {
            "type": "object",
            "properties": {},
            "required": []
        }
    })
}

/// Computed per-palace statistics across every palace on disk.
///
/// Why (#6372): the totals and the per-palace rows used to cover only
/// cache-resident palaces, so a host with 94 palaces and 2 resident reported 2
/// palaces' worth of drawers as if that were the machine's memory. The counts
/// now cover every palace that could be read, cached or not, and
/// `counted_palace_count` says how many that was.
/// What: Holds per-palace JSON entries (limited to MAX_PALACES_IN_REPORT), the
/// true on-disk palace count, how many palaces contributed to the totals
/// (`counted_palace_count`), how many of those were resident
/// (`cached_palace_count`), and the totals themselves.
/// Test: Exercised transitively by `handle_console_metrics_returns_valid_report`
/// and `console_metrics_reports_real_counts_for_an_uncached_palace`.
struct PalaceStats {
    palace_count: usize,
    counted_palace_count: usize,
    cached_palace_count: usize,
    total_drawers: usize,
    total_vectors: usize,
    total_rooms: usize,
    total_kg_triples: usize,
    palace_entries: Vec<Value>,
}

/// One palace's counts plus where they came from.
///
/// Why (#6372): "not in the cache" and "could not be read" are different facts,
/// and the dashboard has to tell them apart — the first now carries real
/// numbers, and only the second earns a `—`. Collapsing both into a `cached`
/// bool is what made 92 populated palaces render as empty.
/// What: `Counted` carries the four counts and whether the cache supplied them;
/// `Unavailable` carries the operator-facing reason.
/// Test: `console_metrics_reports_real_counts_for_an_uncached_palace`.
enum PalaceCounts {
    Counted {
        cached: bool,
        drawers: usize,
        vectors: usize,
        rooms: usize,
        kg_triples: usize,
    },
    Unavailable(String),
}

/// MCP `console_metrics` handler — build and return a `ConsoleMetricsReport`.
///
/// Why: The trusty-console metrics poller calls this tool via a supervised
/// stdio MCP connection every `poll_interval` seconds to refresh the
/// `/api/console/metrics/memory` dashboard panel. Issue #1924: the previous
/// implementation force-opened every palace on disk on every poll (via
/// `PalaceRegistry::open_palace`), defeating the 64-slot LRU open-handle
/// cache and causing sustained multi-GB RSS on machines with dozens of
/// palaces. #6372: reading only the cache went too far the other way — a host
/// with 94 palaces and 2 resident showed 92 rows of `—`. Both constraints hold
/// at once because the counts are redb B-tree lengths, which
/// [`disk_stats::read`] takes off a closed palace under a shared lock.
/// What: Lists all palaces from the shared `AppState` (cheap directory walk),
/// then for each one reads counts from `PalaceRegistry::peek` when it is
/// resident — a lock-and-clone that never evicts or promotes — and from its
/// redb files when it is not. Nothing here calls `open_palace`, so no palace
/// enters the cache. A palace whose files cannot be read (a writer in another
/// process holds them, or they are missing) reports `stats_source:
/// "unavailable"` with null counts rather than a zero. Always returns `Ok` so
/// the caller receives valid JSON. Returns a raw `serde_json::Value` (not the
/// MCP content envelope) — the dispatcher in `transport/rpc.rs` wraps it.
/// Test: `handle_console_metrics_returns_valid_report`,
/// `console_metrics_reports_real_counts_for_an_uncached_palace`, and
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

    // #6372: the uncached arm reads redb files, so the whole aggregation moves
    // to the blocking pool. The cached arm is still just `peek` — a
    // `parking_lot::Mutex` lock plus an `Arc` clone with no I/O.
    // #6928: the size walks are filesystem I/O too, so they ride the same hop
    // rather than adding a second one.
    let registry = state.registry.clone();
    let data_root = state.data_root.clone();
    let (stats, usage) = tokio::task::spawn_blocking(move || {
        (
            collect_palace_stats(&registry, &palace_infos),
            usage::sample(&data_root),
        )
    })
    .await
    .map_err(|e| anyhow::anyhow!("join collect_palace_stats: {e}"))?;

    let mut metrics = json!({
        "palace_count": stats.palace_count,
        "counted_palace_count": stats.counted_palace_count,
        "cached_palace_count": stats.cached_palace_count,
        "total_drawers": stats.total_drawers,
        "total_vectors": stats.total_vectors,
        "total_rooms": stats.total_rooms,
        "total_kg_triples": stats.total_kg_triples,
        // #6928: the tab's two headline figures. `disk_bytes` is always a
        // number — a walk that reads nothing is a real zero. `ram_bytes` is
        // `null` where the OS has no footprint counter.
        "ram_bytes": usage.ram_bytes,
        "disk_bytes": usage.disk_bytes,
        "data_root": state.data_root.display().to_string(),
        "palaces": stats.palace_entries,
    });
    // #6928: the heap / file-backed / compressed split is OMITTED rather than
    // nulled where the OS does not supply it, so a console can tell "this
    // build reports no breakdown" from "the breakdown is zero".
    insert_ram_breakdown(&mut metrics, &usage);

    let report = make_report(
        "trusty-memory",
        "Trusty Memory",
        env!("CARGO_PKG_VERSION"),
        ServiceHealth::Ok,
        metrics,
        // Schema bumped 2 -> 3 (#6372): added `counted_palace_count`,
        // `total_rooms`, and per-palace `room_count` / `stats_source` /
        // `stats_error`; totals now cover every palace that could be read, not
        // only the cache-resident ones.
        // 3 -> 4 (#6424): added per-palace `last_used_unix`. Additive only —
        // a console built against schema 3 reads unchanged.
        // 4 -> 5 (#6928): added `ram_bytes`, `disk_bytes`, `data_root`, the
        // optional `ram_heap_bytes` / `ram_file_backed_bytes` /
        // `ram_compressed_bytes` split, and per-palace `disk_bytes`. Additive
        // only, for the same reason.
        5,
    );

    Ok(serde_json::to_value(&report)?)
}

/// Add the RAM breakdown fields, or leave them out entirely.
///
/// Why a helper rather than three `null`s in the `json!` literal: an omitted
/// key and a `null` key are different answers to the console. `null` would say
/// "this daemon measured the heap and it is unknown", which is not a state
/// that exists — either the OS supplies all three ledgers or it supplies none.
/// What: inserts the three keys when the split is available; otherwise leaves
/// `metrics` untouched.
/// Test: `console_metrics_reports_ram_and_disk_usage`.
fn insert_ram_breakdown(metrics: &mut Value, usage: &usage::DaemonUsage) {
    let Some(map) = metrics.as_object_mut() else {
        return;
    };
    if let Some(heap) = usage.ram_heap_bytes {
        map.insert("ram_heap_bytes".into(), json!(heap));
    }
    if let Some(external) = usage.ram_file_backed_bytes {
        map.insert("ram_file_backed_bytes".into(), json!(external));
    }
    if let Some(compressed) = usage.ram_compressed_bytes {
        map.insert("ram_compressed_bytes".into(), json!(compressed));
    }
}

/// Count one palace, preferring the open handle and falling back to its files.
///
/// Why (#6372): a resident palace already has every number in memory, and
/// re-reading its redb files would both cost more and fail — this process holds
/// those files under an exclusive lock. A non-resident palace has no handle to
/// ask, and #1924 forbids making one, so its numbers come off disk instead.
/// What: `peek` first (no I/O, no eviction, no promotion); on a miss,
/// [`disk_stats::read`]. Room counts follow the same split:
/// `list_room_summaries` over the live store, or the `ROOMS` table length.
/// Test: `console_metrics_reports_real_counts_for_an_uncached_palace`,
/// `console_metrics_uses_cache_only_and_does_not_evict`.
fn count_palace(
    registry: &PalaceRegistry,
    info: &trusty_common::memory_core::Palace,
) -> PalaceCounts {
    match registry.peek(&info.id) {
        Some(handle) => cached_counts(info, &handle),
        None => match disk_stats::read(&info.data_dir) {
            Ok(s) => PalaceCounts::Counted {
                cached: false,
                drawers: s.drawer_count,
                vectors: s.vector_count,
                rooms: s.room_count,
                kg_triples: s.kg_triple_count,
            },
            Err(reason) => {
                tracing::debug!(palace = %info.id, "console_metrics: {reason}");
                PalaceCounts::Unavailable(reason)
            }
        },
    }
}

/// Count a resident palace's stats, without degrading any field's read
/// failure to zero.
///
/// Why (#6376): this arm used to collapse three fallible reads —
/// `list_room_summaries`, `count_active_triples`, and the vector index size
/// — into `0` on error, via `unwrap_or_else`/`unwrap_or(0)` and the
/// deliberately-degrading `kg_triple_count_or_zero` helper (correct for its
/// own callers — see its doc — but wrong for a metrics surface whose whole
/// point is telling a broken read apart from an empty palace, the same
/// failure #6372 already fixed for the disk arm below). `try_index_size`
/// exists specifically to give this caller the raw `Result` `index_size`
/// throws away.
/// What: Reads `drawers` directly (an `RwLock` length — no I/O, no failure
/// mode). Reads `vectors`, `rooms`, and `kg_triples` through [`cached_field`];
/// the first one to fail turns the whole entry `Unavailable`, matching
/// [`disk_stats::read`]'s granularity — a palace that failed halfway
/// through is reported as unknown, never as partially zero.
/// Test: `cached_field_turns_a_read_error_into_unavailable_not_zero` covers
/// the rule this applies; `console_metrics_uses_cache_only_and_does_not_evict`
/// and `console_metrics_reports_room_counts_for_every_palace` cover this
/// function's success path end-to-end.
fn cached_counts(
    info: &trusty_common::memory_core::Palace,
    handle: &trusty_common::memory_core::PalaceHandle,
) -> PalaceCounts {
    let drawers = handle.drawers.read().len();

    let vectors = match cached_field(
        &info.id,
        "vector index",
        handle.vector_store.try_index_size(),
    ) {
        Ok(n) => n,
        Err(reason) => return PalaceCounts::Unavailable(reason),
    };

    // ADR-0027: rooms are registered at open, so a resident palace's room
    // list is authoritative and costs one small redb read.
    let rooms = match cached_field(
        &info.id,
        "room list",
        list_room_summaries(&handle.kg.store()).map(|r| r.len()),
    ) {
        Ok(n) => n,
        Err(reason) => return PalaceCounts::Unavailable(reason),
    };

    let kg_triples = match cached_field(
        &info.id,
        "kg_triple count",
        handle.kg.count_active_triples(),
    ) {
        Ok(n) => n,
        Err(reason) => return PalaceCounts::Unavailable(reason),
    };

    PalaceCounts::Counted {
        cached: true,
        drawers,
        vectors,
        rooms,
        kg_triples,
    }
}

/// Apply the cached arm's "unavailable, never zero" rule to one field's read.
///
/// Why (#6376): the three cached-arm fields need identical log-then-format
/// handling, and taking the already-performed read as a parameter — rather
/// than performing the I/O in here — is the same technique
/// `service::helpers::triple_count_or_zero` uses to put a real
/// trusty-common read failure under an in-crate test: inducing one for real
/// needs a live write transaction against the *same already-open* `Database`
/// (see `count_active_triples_surfaces_read_failure` in trusty-common),
/// which needs `KgStoreRedb::db()` / an equivalent on the vector store —
/// both stay `pub(super)`, unreachable from this crate. An OS-level write to
/// the backing file while the store stays open does not reach this either:
/// redb serves reads from its own already-validated in-memory state rather
/// than by re-scanning the file, so overwriting the file's bytes on disk out
/// from under an already-open `Database` was tried and left every read
/// succeeding — not the deterministic failure a regression test needs.
/// What: `Ok(n)` passes through unchanged. `Err` is logged at warn against
/// the palace id and becomes `Err("<field> unavailable: <error>")` — a
/// distinguishable failure, never a bare `0`.
/// Test: `cached_field_turns_a_read_error_into_unavailable_not_zero`,
/// `cached_field_passes_a_successful_read_through`.
fn cached_field(
    palace: &trusty_common::memory_core::PalaceId,
    field: &str,
    read: anyhow::Result<usize>,
) -> Result<usize, String> {
    read.map_err(|e| {
        let reason = format!("{field} unavailable: {e:#}");
        tracing::warn!(palace = %palace, "{reason}");
        reason
    })
}

/// Aggregate drawer / vector / room / KG statistics over every palace on disk.
///
/// Why (issue #1924, amended by #6372): the loop that used to run past
/// `MAX_PALACES_IN_REPORT` called `registry.open_palace()` purely to count a
/// palace, thrashing the LRU cache every poll. That is still forbidden and
/// still absent — [`count_palace`] reads a closed palace's files directly
/// rather than opening it, so the totals are complete without any palace
/// entering the cache.
/// What: Iterates `palace_infos`; every palace contributes to the totals, and
/// the first MAX_PALACES_IN_REPORT also produce a JSON entry in
/// `palace_entries`. An entry that could not be read carries null counts and
/// `stats_error`, so the dashboard can render "unknown" without mistaking it
/// for "empty".
/// Test: `handle_console_metrics_returns_valid_report` (empty case),
/// `console_metrics_reports_real_counts_for_an_uncached_palace`, and
/// `console_metrics_uses_cache_only_and_does_not_evict`.
fn collect_palace_stats(
    registry: &PalaceRegistry,
    palace_infos: &[trusty_common::memory_core::Palace],
) -> PalaceStats {
    let palace_count = palace_infos.len();
    let mut stats = PalaceStats {
        palace_count,
        counted_palace_count: 0,
        cached_palace_count: 0,
        total_drawers: 0,
        total_vectors: 0,
        total_rooms: 0,
        total_kg_triples: 0,
        palace_entries: Vec::with_capacity(palace_count.min(MAX_PALACES_IN_REPORT)),
    };

    for (rank, info) in palace_infos.iter().enumerate() {
        let counts = count_palace(registry, info);
        if let PalaceCounts::Counted {
            cached,
            drawers,
            vectors,
            rooms,
            kg_triples,
        } = &counts
        {
            stats.counted_palace_count += 1;
            stats.cached_palace_count += usize::from(*cached);
            stats.total_drawers += drawers;
            stats.total_vectors += vectors;
            stats.total_rooms += rooms;
            stats.total_kg_triples += kg_triples;
        }
        if rank < MAX_PALACES_IN_REPORT {
            stats.palace_entries.push(palace_entry(info, &counts));
        }
    }

    stats
}

/// Render one palace's row for the `palaces` array.
///
/// Why (#6372): the row has to say where its numbers came from. `cached` is
/// kept because a console built before this change reads it, and
/// `stats_source` is what a console built after it renders — an `unavailable`
/// row shows `—`, a `disk` row shows real numbers with no badge claiming they
/// are missing.
/// What: null counts plus `stats_error` for an unreadable palace; real counts
/// otherwise.
/// Test: `console_metrics_reports_real_counts_for_an_uncached_palace`,
/// `console_metrics_marks_an_unreadable_palace_unavailable`.
fn palace_entry(info: &trusty_common::memory_core::Palace, counts: &PalaceCounts) -> Value {
    let id = info.id.as_str().to_string();
    // #6424: one small file read per palace, on the same blocking pool as the
    // counts. Null for a palace never used since the stamp shipped — the tab
    // renders that as "never" and sorts it last.
    let last_used_unix = crate::palace_last_used::read(&info.data_dir);
    // #6928: the per-palace half of the tab's disk figure. Measured off the
    // directory, so a palace whose redb counts are unreadable still reports a
    // size — the two failures are unrelated.
    let disk_bytes = usage::palace_disk_bytes(&info.data_dir);
    match counts {
        PalaceCounts::Counted {
            cached,
            drawers,
            vectors,
            rooms,
            kg_triples,
        } => json!({
            "id": id,
            "name": info.name,
            "drawer_count": drawers,
            "vector_count": vectors,
            "room_count": rooms,
            "kg_triple_count": kg_triples,
            "cached": cached,
            "stats_source": if *cached { "cache" } else { "disk" },
            "last_used_unix": last_used_unix,
            "disk_bytes": disk_bytes,
        }),
        PalaceCounts::Unavailable(reason) => json!({
            "id": id,
            "name": info.name,
            "drawer_count": Value::Null,
            "vector_count": Value::Null,
            "room_count": Value::Null,
            "kg_triple_count": Value::Null,
            "cached": false,
            "stats_source": "unavailable",
            "stats_error": reason,
            // #6424: the stamp is a separate file from the redb corpus, so a
            // palace whose counts are unreadable can still report when it was
            // last used. #6928: the directory's size is readable for the same
            // reason.
            "last_used_unix": last_used_unix,
            "disk_bytes": disk_bytes,
        }),
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
        // #6424: 3 -> 4 for the additive per-palace `last_used_unix`.
        // #6928: 4 -> 5 for the additive RAM and disk usage fields.
        assert_eq!(result["metrics_schema_version"], 5);
        assert!(result["collected_at_unix"].is_number());
        assert_eq!(result["metrics"]["palace_count"], 0);
        assert_eq!(result["metrics"]["counted_palace_count"], 0);
        assert_eq!(result["metrics"]["cached_palace_count"], 0);
        assert_eq!(result["metrics"]["total_drawers"], 0);
        assert_eq!(result["metrics"]["total_vectors"], 0);
        assert_eq!(result["metrics"]["total_rooms"], 0);
        assert_eq!(result["metrics"]["total_kg_triples"], 0);
        assert!(result["metrics"]["palaces"].is_array());
        assert_eq!(result["metrics"]["palaces"].as_array().unwrap().len(), 0);
    }

    /// Why (#6928): the console memory tab renders disk and RAM usage and
    /// nothing else, so a payload missing either field leaves the tab with
    /// nothing to show. The breakdown is asserted as present-or-absent rather
    /// than present, because it is an OS capability — but on macOS, where the
    /// tab actually runs, it must be there.
    /// Test: this is the test.
    #[serial_test::serial]
    #[tokio::test]
    async fn console_metrics_reports_ram_and_disk_usage() {
        // SAFETY: `#[serial]` ensures no other test thread reads or writes
        // TRUSTY_SKIP_PALACE_ENFORCEMENT concurrently with this test.
        unsafe {
            std::env::set_var("TRUSTY_SKIP_PALACE_ENFORCEMENT", "1");
        }
        let tmp = tempfile::tempdir().expect("tempdir");
        // A file in the data root so `disk_bytes` has something real to count.
        std::fs::write(tmp.path().join("ballast.bin"), vec![0u8; 64_000]).expect("write");
        let state = crate::AppState::new(tmp.path().to_path_buf());

        let result = handle_console_metrics(&state, serde_json::json!({}))
            .await
            .expect("console_metrics must not return Err");
        let metrics = &result["metrics"];

        let disk = metrics["disk_bytes"]
            .as_u64()
            .expect("disk_bytes is always a number");
        assert!(
            disk >= 64_000,
            "disk_bytes must cover the data root's contents, got {disk}"
        );
        assert_eq!(
            metrics["data_root"].as_str(),
            Some(tmp.path().display().to_string().as_str()),
            "the payload names the directory disk_bytes measured"
        );

        #[cfg(target_os = "macos")]
        {
            let ram = metrics["ram_bytes"]
                .as_u64()
                .expect("macOS reports a physical footprint");
            assert!(ram > 1024 * 1024, "a live daemon holds more than 1 MB");
            for field in [
                "ram_heap_bytes",
                "ram_file_backed_bytes",
                "ram_compressed_bytes",
            ] {
                assert!(
                    metrics[field].is_u64(),
                    "macOS supplies {field}; a single aggregate figure is not enough (#7084)"
                );
            }
        }
    }

    /// Why (#6928): an omitted breakdown key and a `null` one mean different
    /// things to the console — "this OS has no such counter" versus "measured,
    /// and unknown". Only the first is a state that exists.
    /// Test: this is the test.
    #[test]
    fn an_unavailable_ram_breakdown_omits_its_keys_rather_than_nulling_them() {
        let mut metrics = json!({"ram_bytes": 10_u64});
        insert_ram_breakdown(&mut metrics, &usage::DaemonUsage::default());
        assert_eq!(
            metrics.as_object().expect("object").len(),
            1,
            "no breakdown key may appear when the OS supplies none: {metrics}"
        );

        let mut metrics = json!({"ram_bytes": 10_u64});
        insert_ram_breakdown(
            &mut metrics,
            &usage::DaemonUsage {
                ram_heap_bytes: Some(1),
                ram_file_backed_bytes: Some(2),
                ram_compressed_bytes: Some(3),
                ..usage::DaemonUsage::default()
            },
        );
        assert_eq!(metrics["ram_heap_bytes"], json!(1));
        assert_eq!(metrics["ram_file_backed_bytes"], json!(2));
        assert_eq!(metrics["ram_compressed_bytes"], json!(3));
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
    /// `"cached": true` rather than silently opening or omitting them; and
    /// (5, #6372) the evicted entry still carries real counts, read off its
    /// files with `stats_source: "disk"`.
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

        // #6372: not cached is not the same as not known.
        assert_eq!(
            entry("a")["stats_source"],
            "disk",
            "an evicted palace's counts come off its files: {:?}",
            entry("a")
        );
        assert_eq!(entry("b")["stats_source"], "cache");
    }

    /// Why (#6372 regression guard): this is the bug the owner reported. On a
    /// host with 94 palaces and 2 resident, 92 rows rendered `—` because the
    /// handler reported zero for anything the LRU cache did not hold, so a
    /// populated palace was indistinguishable from an empty one. Without the
    /// disk-read arm this test sees `drawer_count: 0`.
    /// What: writes two drawers into a palace, drops every handle so nothing
    /// is cache-resident, and asserts the row reports both drawers and names
    /// `disk` as where the numbers came from — while `cached_palace_count`
    /// still honestly reports zero residents.
    /// Test: This test.
    #[tokio::test]
    async fn console_metrics_reports_real_counts_for_an_uncached_palace() {
        use trusty_common::memory_core::{Drawer, Palace, PalaceId};

        let tmp = tempfile::tempdir().expect("tempdir");
        let data_root = tmp.path().to_path_buf();

        {
            let registry = PalaceRegistry::with_max_open(4);
            let handle = registry
                .create_palace(
                    &data_root,
                    Palace {
                        id: PalaceId::new("cold"),
                        name: "cold".to_string(),
                        description: None,
                        created_at: chrono::Utc::now(),
                        data_dir: data_root.join("cold"),
                    },
                )
                .expect("create_palace");
            for i in 0..2 {
                let drawer = Drawer::new(uuid::Uuid::new_v4(), format!("drawer {i}"));
                handle.kg.store().upsert_drawer(&drawer).expect("upsert");
            }
            drop(handle);
            registry.remove(&PalaceId::new("cold"));
        }

        let state = crate::AppState::new(data_root);
        assert_eq!(state.registry.len(), 0, "nothing may be cache-resident");

        let result = handle_console_metrics(&state, serde_json::json!({}))
            .await
            .expect("console_metrics must not return Err");

        let entry = &result["metrics"]["palaces"][0];
        assert_eq!(entry["id"], "cold");
        assert_eq!(
            entry["drawer_count"], 2,
            "an uncached palace must report its real drawers, not 0: {entry:?}"
        );
        assert_eq!(entry["stats_source"], "disk");
        assert_eq!(entry["cached"], false);
        assert_eq!(
            result["metrics"]["total_drawers"], 2,
            "the totals must include palaces read off disk"
        );
        assert_eq!(result["metrics"]["counted_palace_count"], 1);
        assert_eq!(
            result["metrics"]["cached_palace_count"], 0,
            "residency is still reported honestly"
        );
        // #6928: the per-palace disk figure the tab drills into.
        let palace_disk = entry["disk_bytes"]
            .as_u64()
            .expect("every palace row carries disk_bytes");
        assert!(
            palace_disk > 0,
            "a palace with redb files on disk cannot measure zero bytes"
        );
        assert!(
            palace_disk
                <= result["metrics"]["disk_bytes"]
                    .as_u64()
                    .expect("aggregate disk_bytes"),
            "one palace cannot exceed the whole store it sits in"
        );
    }

    /// Why (#6372): rooms were absent from the payload entirely, so the
    /// dashboard had nothing to render however the counts were sourced. This
    /// pins `room_count` into every per-palace entry and into the totals, from
    /// both the cached and the disk arm — the two must agree on the number.
    /// What: creates a room in one palace, keeps it resident, evicts a second
    /// empty palace, and asserts both rows carry a `room_count` and the
    /// resident one counts the room that was created.
    /// Test: This test.
    #[tokio::test]
    async fn console_metrics_reports_room_counts_for_every_palace() {
        use trusty_common::memory_core::store::rooms::create_room;
        use trusty_common::memory_core::{Palace, PalaceId, RoomType};

        let tmp = tempfile::tempdir().expect("tempdir");
        let data_root = tmp.path().to_path_buf();
        let registry = PalaceRegistry::with_max_open(4);

        let mut resident = None;
        for name in ["hot", "cold"] {
            let handle = registry
                .create_palace(
                    &data_root,
                    Palace {
                        id: PalaceId::new(name),
                        name: name.to_string(),
                        description: None,
                        created_at: chrono::Utc::now(),
                        data_dir: data_root.join(name),
                    },
                )
                .unwrap_or_else(|e| panic!("create_palace({name}): {e:#}"));
            if name == "hot" {
                create_room(
                    &handle.kg.store(),
                    &RoomType::Custom("decisions".to_string()),
                    None,
                )
                .expect("create_room");
                resident = Some(handle);
            } else {
                drop(handle);
                registry.remove(&PalaceId::new(name));
            }
        }
        let _resident = resident.expect("hot palace handle kept");

        let mut state = crate::AppState::new(data_root);
        state.registry = std::sync::Arc::new(registry);

        let result = handle_console_metrics(&state, serde_json::json!({}))
            .await
            .expect("console_metrics must not return Err");

        let entries = result["metrics"]["palaces"]
            .as_array()
            .expect("palaces array");
        for e in entries {
            assert!(
                e["room_count"].is_number(),
                "every palace row must carry a room_count: {e:?}"
            );
        }
        let hot = entries
            .iter()
            .find(|e| e["id"] == "hot")
            .expect("hot entry present");
        assert_eq!(hot["stats_source"], "cache");
        assert_eq!(
            hot["room_count"], 1,
            "the room that was created must be counted: {hot:?}"
        );
        assert_eq!(
            result["metrics"]["total_rooms"], 1,
            "total_rooms sums the per-palace counts"
        );
    }

    /// Why (#6372): a count that could not be read must not render as zero —
    /// that is the failure mode the whole ticket is about, one layer down. A
    /// palace whose directory has no redb file yet reports null counts and a
    /// reason, so the dashboard shows "unknown" rather than "empty".
    /// What: registers a palace, then removes its files before polling.
    /// Test: This test.
    #[tokio::test]
    async fn console_metrics_marks_an_unreadable_palace_unavailable() {
        use trusty_common::memory_core::{Palace, PalaceId};

        let tmp = tempfile::tempdir().expect("tempdir");
        let data_root = tmp.path().to_path_buf();
        {
            let registry = PalaceRegistry::with_max_open(4);
            let handle = registry
                .create_palace(
                    &data_root,
                    Palace {
                        id: PalaceId::new("gone"),
                        name: "gone".to_string(),
                        description: None,
                        created_at: chrono::Utc::now(),
                        data_dir: data_root.join("gone"),
                    },
                )
                .expect("create_palace");
            drop(handle);
            registry.remove(&PalaceId::new("gone"));
        }
        // `palace.json` stays, so the palace is still listed; its store does not.
        std::fs::remove_file(data_root.join("gone").join("kg.redb")).expect("remove kg store");

        let state = crate::AppState::new(data_root);
        let result = handle_console_metrics(&state, serde_json::json!({}))
            .await
            .expect("console_metrics must not return Err");

        let entry = &result["metrics"]["palaces"][0];
        assert_eq!(entry["stats_source"], "unavailable");
        assert!(
            entry["drawer_count"].is_null(),
            "an unreadable count must be null, never 0: {entry:?}"
        );
        assert!(
            entry["stats_error"].is_string(),
            "an unavailable row must say why: {entry:?}"
        );
        assert_eq!(
            result["metrics"]["counted_palace_count"], 0,
            "a palace that could not be read did not contribute to the totals"
        );
    }

    /// Why (#6376 regression guard): before this fix, `count_palace`'s cached
    /// arm read `room_count`/`kg_triples`/`vectors` through
    /// `unwrap_or_else(|_| 0)`, `kg_triple_count_or_zero`, and
    /// `index_size()`'s `unwrap_or(0)` — every one of those degrades an
    /// `Err` into a bare `0`, indistinguishable from a genuinely empty
    /// count. This pins the rule [`cached_field`] now applies instead: an
    /// `Err` must become a named `Err(reason)`, never `Ok(0)`. Inducing a
    /// genuine redb read failure on an already-open, resident store needs a
    /// live write transaction against that same `Database` (see
    /// `count_active_triples_surfaces_read_failure` in trusty-common), which
    /// needs `db()` accessors trusty-common keeps `pub(super)` — unreachable
    /// from here, matching the boundary that test's own doc already notes.
    /// Taking the read as a parameter, exactly as
    /// `service::helpers::triple_count_or_zero` does for the same reason,
    /// puts the branch this ticket cares about under a fast, deterministic
    /// in-crate test instead.
    /// What: feeds a synthetic `Err` in for each of the three field names
    /// `cached_counts` uses, and asserts none of them collapse to a count.
    /// Test: This test.
    #[test]
    fn cached_field_turns_a_read_error_into_unavailable_not_zero() {
        use trusty_common::memory_core::PalaceId;

        let palace = PalaceId::new("hot");
        for field in ["vector index", "room list", "kg_triple count"] {
            let result = cached_field(&palace, field, Err(anyhow::anyhow!("redb read failed")));
            let reason = match result {
                Err(reason) => reason,
                Ok(n) => panic!("field {field:?} must surface its read error, not {n}"),
            };
            assert!(
                reason.contains(field),
                "the reason must name the field that failed: {reason}"
            );
            assert!(
                reason.contains("redb read failed"),
                "the reason must carry the underlying error: {reason}"
            );
        }
    }

    /// Why (#6376): the error path above must not come at the cost of the
    /// success path — a real count has to pass through unchanged, and
    /// `cached_counts`' end-to-end success coverage
    /// (`console_metrics_uses_cache_only_and_does_not_evict`,
    /// `console_metrics_reports_room_counts_for_every_palace`) exercises
    /// this indirectly, through real reads. This pins the same property
    /// directly against the shared helper.
    /// What: feeds `Ok(7)` in and asserts it comes back unchanged.
    /// Test: This test.
    #[test]
    fn cached_field_passes_a_successful_read_through() {
        use trusty_common::memory_core::PalaceId;

        let palace = PalaceId::new("hot");
        assert_eq!(cached_field(&palace, "vector index", Ok(7)), Ok(7));
    }
}
