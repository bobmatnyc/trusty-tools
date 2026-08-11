//! Background startup tasks for the `trusty-memory` daemon binary.
//!
//! Why: split out of `main.rs` in #4678, which pushed that file to 510 SLOC
//! against the 500 cap. This function was the largest self-contained unit
//! there and depends only on the public `trusty_memory` surface, so it moves
//! without touching the CLI definition or the dispatch table.
//! What: `spawn_startup_tasks` — embedder warm-up, palace hydration, dream
//! scheduler, BM25 sweeps, alias discovery, update check, and the pin scan.
//! Test: `startup_task_tests::spawn_startup_tasks_populates_pin_map`.

use trusty_memory::AppState;

/// Why: startup tasks (palace hydration, alias discovery, pin scan, and the
///      issue-#1529 autonomous dream scheduler) are the same regardless of
///      whether HTTP binds to a fixed or dynamic port; keeping the logic in
///      a single helper means a new startup task only has to be added in one
///      place. Previously, `load_palaces_from_disk` was awaited synchronously
///      before binding the HTTP listener — a single broken `kg.db` (stale WAL
///      sidecar, corrupt file, permissions) could stall hydration for seconds
///      per palace, deferring `/health` becoming reachable until every palace
///      had been visited. The dashboard, MCP clients, and `launchctl`
///      health-probes all interpret that as "the daemon is dead", so the launchd
///      job thrashes and operators see no useful output. Spawning hydration as a
///      background task lets the HTTP server bind immediately; palaces appear in
///      `palace_list` and the dashboard as each one finishes opening.
///      Per-palace failures are already logged and skipped inside
///      `load_palaces_from_disk` so a single bad `kg.db` can never abort the
///      daemon.
/// What: clones `state` (cheap — `AppState` derives `Clone` with `Arc`-wrapped
///       internals) and spawns a background task that:
///       (1) hydrates persisted palaces from disk with timing logs;
///       (2) once palaces are live, kicks off issue-#42 alias auto-discovery
///           against the cwd targeting the default palace (if configured);
///       (3) runs the issue-#470 single-pass pin scan and populates
///           `AppState::pin_project_map` (scan-only — NO palace opens);
///       (4) [issue #1529] spawns per-palace autonomous dream loops via
///           `dream_scheduler::spawn_dream_scheduler` wired to a watch channel
///           that the `spawn_shutdown_bridge` task flips on SIGTERM/SIGINT.
///       Returns immediately — all spawned tasks run concurrently with the HTTP
///       listener bind.
/// Test: `spawn_startup_tasks_populates_pin_map` verifies the scan path runs
///       and populates the map; the log emission is confirmed by the throwaway
///       daemon run documented in the session notes.
pub(crate) fn spawn_startup_tasks(state: &AppState) {
    // #5399: build the WordNet POS table here so the first `memory_remember`
    // does not pay for the parse. Synchronous on purpose — it is 6.6-9.8 ms
    // measured (`examples/wordnet_measure.rs`) and every extraction path needs
    // it, so deferring it to a task only moves the cost onto whichever request
    // loses the race.
    trusty_memory::wordnet_pos::preload();
    // Issue #1529: build watch channel for the autonomous dream scheduler.
    // The sender (`dtx`) is intentionally held here and moved into the
    // hydration task — the shutdown bridge is only wired AFTER
    // `spawn_dream_scheduler` returns. This ordering guarantee prevents a
    // shutdown-race where a SIGTERM that arrives during hydration could flip
    // the watch to `true` before any dream loops are spawned, causing every
    // newly-spawned loop to exit immediately on its first iteration. By
    // spawning the bridge after the loops exist, early-SIGTERM during hydration
    // is still safe: the loops will see the `true` value on their first poll
    // and shut down cleanly, which is the correct behaviour. Normal startup
    // (no early SIGTERM) is unaffected because the bridge task cannot fire
    // until `trusty_common::shutdown_signal()` resolves.
    let (dtx, dream_shutdown_rx) = trusty_memory::dream_scheduler::make_shutdown_watch();
    // Issue #906 / #910 / #911: eager embedder warm-up.
    // Spawn BEFORE the palace hydration task so the CoreML / CUDA cold compile
    // (30-120 s on first run) races ahead concurrently and the warm embedder
    // is likely ready by the time the first `memory_remember` / `memory_recall`
    // arrives.
    //
    // On SUCCESS flip `daemon_readiness` to `Ready` so the recall handlers in
    // `tools/memory_ops.rs` switch from the BM25/L0/L1-only fallback to full
    // vector-backed recall (issue #1970; formerly issue #911's hard-error
    // preflight, which blocked writes/reads outright while Warming — replaced
    // by graceful degradation that mirrors trusty-search's staged pipeline).
    //
    // On FAILURE: log at ERROR, leave state as `Warming`.  The lazy-init path
    // in `shared_embedder()` will retry on the first real request (and the
    // bounded timeout there means it will fail fast, not hang). Writes still
    // succeed while Warming — they defer embedding to a background task
    // (see `PalaceHandle::remember_with_options` / `defer_embedding`).
    let warmup_state = state.clone();
    tokio::spawn(async move {
        let ws = std::time::Instant::now();
        tracing::info!("starting background embedder warm-up (issues #906/#910)");
        match trusty_common::memory_core::retrieval::shared_embedder().await {
            Ok(_) => {
                let elapsed_ms = ws.elapsed().as_millis() as u64;
                tracing::info!(
                    elapsed_ms,
                    "background embedder warm-up complete; daemon is now Ready (issues #910/#911)"
                );
                warmup_state.set_ready();
            }
            Err(e) => tracing::error!(
                elapsed_ms = ws.elapsed().as_millis() as u64,
                "background embedder warm-up failed (daemon stays Warming; \
                 memory ops will return a bounded error on first request): {e:#}"
            ),
        }
    });

    let bg_state = state.clone();
    tokio::spawn(async move {
        let started = std::time::Instant::now();
        tracing::info!("starting background palace hydration");
        match bg_state.load_palaces_from_disk().await {
            Ok(count) => tracing::info!(
                elapsed_ms = started.elapsed().as_millis() as u64,
                "background palace hydration complete: {count} palaces loaded"
            ),
            Err(e) => tracing::error!(
                elapsed_ms = started.elapsed().as_millis() as u64,
                "background palace hydration failed: {e:#}"
            ),
        }

        // Issue #1529: spawn per-palace autonomous dream loops after hydration.
        // ORDERING GUARANTEE: spawn_dream_scheduler is called first, then the
        // shutdown bridge is wired. This ensures the bridge cannot pre-cancel
        // the loops: loops are created with receivers pointing to a watch that
        // is still `false`, so they will do their first sleep before the bridge
        // could ever flip the value.
        // Spawn all background maintenance loops (dream scheduler + idle-to-disk
        // eviction ticker) and the shutdown bridge in one call — see
        // `spawn_background_maintenance` for the #1529 ordering guarantee and the
        // idle-to-disk RAM-reclaim rationale.
        let n = trusty_memory::dream_scheduler::spawn_background_maintenance(
            &bg_state.registry,
            dream_shutdown_rx,
            dtx,
        );
        tracing::info!(loops = n, "dream_scheduler: {n} loop(s) running (#1529)");

        // BM25 backfill sweep. A no-op while the lexical lane is off, which is
        // every deployment until the default is flipped: `spawn_startup_backfill`
        // returns immediately when `bm25_client` is `None`.
        trusty_memory::bm25_backfill::spawn_startup_backfill(&bg_state);

        // BM25 coverage repair sweep. The write path drops on a full queue,
        // and before this the only thing that repaired a drop was the next
        // daemon restart — so the "drops are recoverable" trade was not true
        // of the running process. Also a no-op while the lane is off.
        trusty_memory::bm25_repair::spawn_repair_sweep(&bg_state);

        // Issue #42: once palaces are live, kick off auto-discovery against
        // cwd targeting the default palace (if configured). Without a default
        // palace there's no obvious destination, so skip — explicit MCP
        // `discover_aliases` calls still work.
        if let Some(palace) = bg_state.default_palace.clone() {
            if let Ok(cwd) = std::env::current_dir() {
                bg_state.spawn_alias_discovery(palace, cwd);
            }
        }
        // Issue #537: throttled startup update check. Runs once per 24h (on-disk
        // cache). Result stored in AppState::update_available for /health.
        // Non-blocking: failure degrades to "no update info" — never aborts startup.
        {
            let update_available = bg_state.update_available.clone();
            tokio::spawn(async move {
                let crate_name = env!("CARGO_PKG_NAME");
                let current = env!("CARGO_PKG_VERSION");
                if let Some(info) =
                    trusty_common::update::check_throttled(crate_name, current).await
                {
                    tracing::info!(
                        latest = %info.latest,
                        "update available: {}",
                        trusty_common::update::notice(&info)
                    );
                    eprintln!("{}", trusty_common::update::notice(&info));
                    if let Ok(mut guard) = update_available.lock() {
                        *guard = Some(info.latest);
                    }
                }
            });
        }

        // Issue #470 / #474: single-pass scan-only pin discovery — NO palace
        // opens. Run on the blocking pool because readdir is blocking I/O;
        // the scan is bounded (one level under each search root) so it
        // completes quickly. We populate `pin_project_map` on the shared
        // `AppState` arc so handlers can look up palace_id → project_path
        // cheaply.
        //
        // Fix #474: the completion log is emitted at `info!` level AND via
        // `eprintln!` to stderr directly. The `info!` is visible under
        // `RUST_LOG=info`; the `eprintln!` is visible regardless of the
        // tracing filter and matches the pattern used by `run_http_on` for
        // the bind-address announcement. This dual-emit guarantees the
        // operator can always confirm the scan ran, even when the daemon is
        // started via launchd (stderr → log file) or with the default
        // `RUST_LOG=warn` level.
        let pin_scan_started = std::time::Instant::now();
        let pin_map_ref = bg_state.pin_project_map.clone();
        // Fix #880: when a data-dir override is active the daemon is running
        // in an isolated environment (test rig, CI, parallel run). The
        // default_search_dirs() scan walks the REAL user environment
        // (~/Projects, ~/Developer, …) and would import palaces from the
        // live system into the isolated data root, defeating isolation.
        // Use an empty search list so the scan is a no-op; the pin map stays
        // empty for the lifetime of this isolated instance.
        let override_active = trusty_memory::is_data_dir_override_active();
        let scan_result = tokio::task::spawn_blocking(move || {
            let search_dirs = if override_active {
                Vec::new()
            } else {
                trusty_memory::startup_scan::default_search_dirs()
            };
            trusty_memory::startup_scan::scan_pin_map(&search_dirs)
        })
        .await;
        match scan_result {
            Ok(map) => {
                let count = map.len();
                let elapsed_ms = pin_scan_started.elapsed().as_millis() as u64;
                for (palace_id, project_path) in map {
                    pin_map_ref.insert(palace_id, project_path);
                }
                // Dual-emit: tracing INFO (visible under RUST_LOG=info) +
                // eprintln! to stderr (visible at any filter level, matching
                // the `run_http_on` bind-address announcement pattern).
                // Root cause of #474: when the daemon is started via
                // `trusty-memory start`, the child's stderr is redirected to
                // /dev/null and tracing output is silently lost; when started
                // via launchd the default RUST_LOG=warn suppresses info!.
                // eprintln! bypasses the tracing filter so the completion is
                // always visible in launchd logs and on the operator's
                // terminal when run in the foreground.
                tracing::info!(
                    pins_found = count,
                    elapsed_ms,
                    "startup pin scan complete: {count} pin(s) discovered in {elapsed_ms}ms"
                );
                eprintln!("startup pin scan complete: {count} pin(s) discovered in {elapsed_ms}ms");
            }
            Err(e) => {
                // spawn_blocking join error — should not happen in practice.
                tracing::warn!("startup pin scan task panicked or was cancelled: {e}");
                eprintln!("startup pin scan task panicked or was cancelled: {e}");
            }
        }
    });
}
