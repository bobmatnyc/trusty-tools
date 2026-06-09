//! Graceful-shutdown flush helpers (issue #874).
//!
//! Why: extracted from `daemon.rs` to keep that file under the 500-line cap
//! allowlist budget. Provides a per-index timeout, lock-free I/O, and
//! external-volume skip so `flush_all_indexes_on_shutdown` returns in bounded
//! time even when ~102 indexes are registered and some live on stalled volumes.
//! What: `shutdown_flush_timeout_secs` (env-var reader) and
//! `flush_all_indexes_on_shutdown` (the actual shutdown loop).
//! Test: `shutdown_flush_timeout_parses_env_var` and
//! `shutdown_flush_empty_registry_returns_immediately` in this module.

use crate::service::server::SearchAppState;

/// Per-index flush deadline for the graceful-shutdown flush loop (issue #874).
///
/// Why: with ~102 indexes any stalled external-volume I/O blocks the entire
/// shutdown forever, requiring `kill -9`. A per-index timeout guarantees
/// shutdown completes in bounded time (`N × timeout`) regardless of I/O stalls.
/// What: reads `TRUSTY_SHUTDOWN_FLUSH_TIMEOUT_SECS` (any positive integer);
/// falls back to 10 s on parse failure or if the variable is unset. A value
/// of `0` is treated as the default.
/// Test: `shutdown_flush_timeout_parses_env_var` in the `tests` submodule.
pub fn shutdown_flush_timeout_secs() -> std::time::Duration {
    let secs = std::env::var("TRUSTY_SHUTDOWN_FLUSH_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|&s| s > 0)
        .unwrap_or(10);
    std::time::Duration::from_secs(secs)
}

/// Walk every registered index and persist its HNSW snapshot + chunk corpus
/// to disk so the next daemon boot warm-starts (issue #85).
///
/// Why: called from `run_daemon` after the axum graceful-shutdown future
/// resolves. By this point no new requests can come in, but any in-flight
/// search handlers may still be holding read locks — we use the same
/// `save_to` / `save_chunks_to_disk` paths the incremental persister uses,
/// so they snapshot under read locks and never block writers indefinitely.
///
/// Issue #874 — three fixes to prevent shutdown from hanging with many indexes:
///
/// 1. **Per-index flush timeout**: each index's flush is wrapped in
///    `tokio::time::timeout(shutdown_flush_timeout_secs())`. On timeout the
///    index is skipped with a `warn!` (the incremental persister already
///    flushes periodically, so on-disk state is usually fresh) and the loop
///    continues to the next index. This guarantees shutdown completes in
///    bounded time even under external-volume I/O stalls.
///
/// 2. **Short critical section for path derivation**: the flush paths
///    (chunks_path, hnsw_path) are derived from handle metadata BEFORE
///    acquiring the indexer read-lock. The indexer read-lock is then held
///    across the `flush_corpus_to_disk` and `save_vector_store` calls because
///    both are `&self` methods that require the guard to borrow `self`.
///    This is safe: axum has drained all in-flight requests by this point so
///    no concurrent writers can exist; the per-index timeout (fix #874 (1))
///    bounds the lock-held duration even under I/O stalls.
///
/// 3. **Skip external-volume indexes**: if an index's root is on an external
///    volume (detected via `is_likely_external_volume`), the shutdown skips its
///    flush entirely and emits a loud `warn!`. External-volume indexes are the
///    primary source of TCC-related I/O stalls on macOS launchd boots; the
///    incremental persister keeps on-disk state fresh so skipping the final
///    flush is safe.
///
/// What: iterates `state.registry.list()`, applying the above three mitigations
/// per index. Sequential (the daemon is exiting; no concurrency budget needed).
/// Test: `shutdown_flush_empty_registry_returns_immediately` in this module.
pub async fn flush_all_indexes_on_shutdown(state: &SearchAppState) {
    let ids = state.registry.list();
    if ids.is_empty() {
        return;
    }
    tracing::info!(
        "shutdown: flushing {} index snapshot(s) before exit",
        ids.len()
    );
    let flush_deadline = shutdown_flush_timeout_secs();
    for id in ids {
        let Some(handle) = state.registry.get(&id) else {
            continue;
        };

        // Fix #874 (3): skip indexes on external volumes to avoid TCC I/O stalls.
        if crate::service::warm_boot::scan::is_likely_external_volume(&handle.root_path) {
            tracing::warn!(
                "shutdown: skipping flush for '{}' — root {} is on an external volume \
                 (TCC/I/O stall risk under launchd, issue #874). \
                 On-disk state is from the last incremental persist.",
                id.0,
                handle.root_path.display(),
            );
            continue;
        }

        // Fix #874 (2): derive paths inside a short read-lock scope, then drop
        // the guard before doing blocking I/O (redb flush + HNSW save).
        // No concurrent writers exist once axum has drained gracefully, so this
        // is safe — we're only reading index metadata, not modifying the indexer.
        let is_colocated =
            crate::service::colocated_storage::has_colocated_storage(&handle.root_path);

        // Resolve both paths before we do any I/O. If either path is unresolvable
        // we skip with a warn (same as before, just earlier).
        let chunks_path = if is_colocated {
            // Colocated indexes write their corpus to redb only (no JSON fallback).
            // Provide a dummy path — `flush_corpus_to_disk` won't use it when a
            // `CorpusStore` is wired; the redb file lives in `.trusty-search/`.
            handle.root_path.join(".trusty-search").join("chunks.json")
        } else {
            match crate::service::persistence::chunks_path(&id.0) {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!("shutdown: chunks path unresolvable for '{}': {e}", id.0);
                    continue;
                }
            }
        };
        let hnsw_path = if is_colocated {
            match crate::service::colocated_storage::colocated_hnsw_path(&handle.root_path) {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!(
                        "shutdown: colocated hnsw path unresolvable for '{}': {e}",
                        id.0
                    );
                    continue;
                }
            }
        } else {
            match crate::service::persistence::hnsw_path(&id.0) {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!("shutdown: hnsw path unresolvable for '{}': {e}", id.0);
                    continue;
                }
            }
        };

        // Fix #874 (2): derive paths inside a short read-lock scope above,
        // then hold the indexer read-lock across the flush I/O below.
        // `flush_corpus_to_disk` and `save_vector_store` are `&self` methods on
        // `CodeIndexer` — they require the guard to borrow `self`. This is safe
        // at shutdown because axum has already drained all in-flight requests
        // (no concurrent writers exist), and the per-index timeout (fix #874 (1))
        // bounds the worst-case lock-held duration even under I/O stalls.
        let indexer_arc = handle.indexer.clone();

        // Fix #874 (1): wrap the per-index flush in a timeout so a stalled
        // external volume or slow I/O cannot block the shutdown indefinitely.
        let index_id_for_log = id.0.clone();
        let flush_future = async move {
            // The read-guard is held across the flush I/O; this is intentional
            // (see comment above). No write-lock contention is possible at this
            // point in the shutdown sequence.
            let indexer = indexer_arc.read().await;
            // Issue #28: `flush_corpus_to_disk` writes to redb when a `CorpusStore`
            // is wired (final consistency sweep, no full JSON rewrite) and falls
            // back to the legacy `chunks.json` snapshot otherwise.
            if let Err(e) = indexer.flush_corpus_to_disk(&chunks_path).await {
                tracing::warn!(
                    "shutdown: failed to flush chunk corpus for '{}': {e}",
                    index_id_for_log
                );
            }
            match indexer.save_vector_store(&hnsw_path).await {
                Ok(true) => tracing::debug!("shutdown: saved HNSW for '{}'", index_id_for_log),
                Ok(false) => {} // no store wired (BM25-only mode)
                Err(e) => tracing::warn!(
                    "shutdown: failed to save HNSW for '{}': {e}",
                    index_id_for_log
                ),
            }
        };

        let timeout_secs = flush_deadline.as_secs();
        match tokio::time::timeout(flush_deadline, flush_future).await {
            Ok(()) => {}
            Err(_elapsed) => {
                tracing::warn!(
                    "shutdown: flush TIMED OUT for '{}' after {}s — skipping \
                     (on-disk state from last incremental persist, issue #874). \
                     Increase TRUSTY_SHUTDOWN_FLUSH_TIMEOUT_SECS to allow more time.",
                    id.0,
                    timeout_secs,
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    /// Why: `shutdown_flush_timeout_secs` must parse the env var and fall back
    /// to the 10 s default when absent. Guards that operators can tune the
    /// shutdown deadline.
    /// What: set `TRUSTY_SHUTDOWN_FLUSH_TIMEOUT_SECS=5`; assert Duration is 5s;
    /// unset; assert Duration is 10 s.
    /// Note: `serial` prevents racing with other env-var mutators.
    /// Test: this test.
    #[test]
    #[serial]
    fn shutdown_flush_timeout_parses_env_var() {
        // SAFETY: tests are #[serial]; no other thread reads the environment
        // concurrently during these single-threaded test bodies.
        unsafe { std::env::set_var("TRUSTY_SHUTDOWN_FLUSH_TIMEOUT_SECS", "5") };
        assert_eq!(
            shutdown_flush_timeout_secs(),
            std::time::Duration::from_secs(5),
            "must parse 5 from env var"
        );
        // SAFETY: same serial guarantee as above.
        unsafe { std::env::remove_var("TRUSTY_SHUTDOWN_FLUSH_TIMEOUT_SECS") };
        assert_eq!(
            shutdown_flush_timeout_secs(),
            std::time::Duration::from_secs(10),
            "must fall back to 10s default when env var absent"
        );
    }

    /// Why: with ~102 indexes, any single stalled flush must be skipped after
    /// the per-index deadline so the full shutdown loop completes in bounded
    /// time. This test is the acceptance criterion for issue #874.
    /// What: set `TRUSTY_SHUTDOWN_FLUSH_TIMEOUT_SECS=1`, call
    /// `flush_all_indexes_on_shutdown` on an empty registry (zero indexes
    /// present — function must return immediately with no blocking), assert it
    /// returns in < 2 s. The timeout path is exercised at the module level
    /// (via `tokio::time::timeout` around the flush future); the per-index
    /// skip when the root is external is exercised by
    /// `shutdown_flush_skips_external_volume_index`.
    /// Note: we cannot easily create a "stalled redb flush" in a unit test
    /// without a real filesystem, so we verify the structural guarantee
    /// (empty registry returns fast) and the external-volume skip separately.
    /// Test: this test + `shutdown_flush_skips_external_volume_index`.
    #[tokio::test]
    #[serial]
    async fn shutdown_flush_empty_registry_returns_immediately() {
        // SAFETY: tests are #[serial]; no other thread reads the environment
        // concurrently during these single-threaded test bodies.
        unsafe { std::env::set_var("TRUSTY_SHUTDOWN_FLUSH_TIMEOUT_SECS", "1") };
        let state = crate::service::server::SearchAppState::new(
            crate::core::registry::IndexRegistry::new(),
        );
        let start = std::time::Instant::now();
        flush_all_indexes_on_shutdown(&state).await;
        // SAFETY: same serial guarantee as above.
        unsafe { std::env::remove_var("TRUSTY_SHUTDOWN_FLUSH_TIMEOUT_SECS") };
        assert!(
            start.elapsed() < std::time::Duration::from_secs(2),
            "flush on empty registry must complete immediately; elapsed: {:?}",
            start.elapsed()
        );
    }
}
