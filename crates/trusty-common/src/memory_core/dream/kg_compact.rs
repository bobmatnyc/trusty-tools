//! The dream cycle's `kg.redb` prune-and-compact phase (#6652).
//!
//! Why: the owner ruling put compaction inside dreaming — "let's include
//! compaction as a process in dreaming" — and the cycle already had the right
//! seam for it. Step 9 of `Dreamer::dream_cycle` called `handle.kg.checkpoint()`,
//! a documented no-op kept for API compatibility since the SQLite days. That is
//! the one place in the cycle whose stated job was "bound the store's on-disk
//! growth" and which did nothing at all.
//!
//! What: [`kg_compact_pass`] measures the file read-only, decides whether the
//! rewrite is worth its cost, and — when it is — runs
//! [`copy_swap::prepare`] with NO lock held, then takes the palace write mutex
//! for exactly [`copy_swap::PreparedCompaction::commit`]. `dry_run` stops after
//! the measurement and writes nothing at all, not even the backup.
//!
//! Test: `kg_compaction_shrinks_the_file_in_a_dream_cycle`,
//! `kg_compaction_is_skipped_below_the_size_gate`,
//! `dry_run_prepares_nothing_and_writes_no_bytes`.

use anyhow::{Context, Result};
use std::path::PathBuf;
use std::sync::Arc;

use super::config::{COMPACT_MIN_RECLAIM_PERCENT, DreamConfig};
use crate::memory_core::retrieval::PalaceHandle;
use crate::memory_core::store::kg_redb::copy_swap::{
    self, CompactFaultHook, CompactPlan, PreparedCompaction,
};
use crate::memory_core::store::kg_redb::stats::{KgRedbStats, history_cutoff_ms};
use crate::memory_core::timeouts;

/// What the phase measured, decided, and did.
///
/// Why: the same record answers three callers — the dream cycle's telemetry,
/// the `palace compact --dry-run` report, and the `palace_dream` MCP response.
/// One shape means the dry run reports exactly the numbers the real run acts
/// on.
/// What: the read-only measurement plus, when the rewrite ran, its before/after
/// byte counts. `skipped` names the gate that stopped it, and is `None` only
/// when the rewrite actually happened.
/// Test: `kg_compaction_is_skipped_below_the_size_gate`.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct KgCompactReport {
    pub stats: KgRedbStats,
    pub dry_run: bool,
    /// Why the rewrite did not run. `None` means it did.
    pub skipped: Option<String>,
    pub bytes_before: u64,
    pub bytes_after: u64,
    pub rows_copied: u64,
    pub history_rows_pruned: u64,
    pub backup: Option<PathBuf>,
}

impl KgCompactReport {
    /// Whether the rewrite ran and swapped the file.
    pub fn ran(&self) -> bool {
        self.skipped.is_none() && !self.dry_run
    }

    /// Bytes returned to the filesystem; `0` when the rewrite did not run.
    pub fn bytes_reclaimed(&self) -> u64 {
        self.bytes_before.saturating_sub(self.bytes_after)
    }

    /// A one-line summary for a CLI or an MCP response.
    pub fn summary(&self) -> String {
        match &self.skipped {
            Some(reason) => format!("skipped: {reason}"),
            None if self.dry_run => format!(
                "dry-run: would prune {} stale history row(s) and reclaim ~{} bytes of {}",
                self.stats.triples_history_stale, self.stats.reclaimable_bytes, self.bytes_before
            ),
            None => format!(
                "compacted {} -> {} bytes ({} reclaimed), pruned {} history row(s)",
                self.bytes_before,
                self.bytes_after,
                self.bytes_reclaimed(),
                self.history_rows_pruned
            ),
        }
    }
}

/// Measure, gate, and (unless `dry_run`) rewrite the palace's `kg.redb`.
///
/// Why: see the module doc. The split between measurement and action is the
/// owner's own "measure before deleting" amendment made mechanical — the
/// numbers that justify the prune are computed by a read-only pass that runs
/// whether or not the rewrite does.
/// What:
///   1. Resolve `<data_dir>/kg.redb`. An in-memory palace has no file and the
///      phase reports that as a skip, not an error.
///   2. [`KgRedbStats::measure`] read-only, at the configured history cutoff.
///   3. Gate: skip unless the file is at least `compact_min_bytes` AND the
///      reclaimable estimate is at least [`COMPACT_MIN_RECLAIM_PERCENT`] of it.
///      A `dry_run` reports the gate's verdict without acting on it.
///   4. `prepare` on a blocking thread, no lock held — this streams the whole
///      file and legitimately outlasts every per-write budget in the system.
///   5. The palace write mutex, held across `commit` alone: one `rename`
///      syscall plus one pointer store.
///
/// A failure at any step returns `Err` with `kg.redb` exactly as it was found;
/// the caller logs it and the next cycle retries.
///
/// Test: `kg_compaction_shrinks_the_file_in_a_dream_cycle`,
/// `dry_run_prepares_nothing_and_writes_no_bytes`.
pub async fn kg_compact_pass(
    handle: &Arc<PalaceHandle>,
    config: &DreamConfig,
    dry_run: bool,
) -> Result<KgCompactReport> {
    kg_compact_pass_with_hook(handle, config, dry_run, None).await
}

/// [`kg_compact_pass`] with the fault-injection seam exposed.
///
/// Why: every failure branch in the swap has to be provable, and the only
/// reliable way to fail an `fsync` or a `rename` on demand is an explicit hook.
/// Test: `a_crash_before_the_rename_leaves_the_original_untouched`,
/// `a_crash_between_rename_and_install_recovers_on_reopen`.
pub async fn kg_compact_pass_with_hook(
    handle: &Arc<PalaceHandle>,
    config: &DreamConfig,
    dry_run: bool,
    hook: Option<CompactFaultHook>,
) -> Result<KgCompactReport> {
    let Some(path) = kg_redb_path(handle) else {
        return Ok(skipped(
            empty_stats(),
            dry_run,
            "palace has no on-disk data directory",
        ));
    };
    let days = config.effective_prune_history_days();
    let measure_path = path.clone();
    let stats = tokio::task::spawn_blocking(move || KgRedbStats::measure(&measure_path, days))
        .await
        .context("join kg.redb measurement")??;

    if let Some(reason) = gate(&stats, config) {
        return Ok(skipped(stats, dry_run, &reason));
    }
    if dry_run {
        let bytes_before = stats.file_bytes;
        return Ok(KgCompactReport {
            stats,
            dry_run: true,
            skipped: None,
            bytes_before,
            bytes_after: bytes_before,
            rows_copied: 0,
            history_rows_pruned: 0,
            backup: None,
        });
    }

    let plan = CompactPlan {
        history_cutoff_ms: Some(history_cutoff_ms(
            chrono::Utc::now().timestamp_millis(),
            days,
        )),
        keep_backup: config.compact_keep_backup,
    };
    let store = handle.kg.redb_store().clone();
    let prepare_hook = hook.clone();
    let prepared: PreparedCompaction = tokio::task::spawn_blocking(move || {
        copy_swap::prepare(&store, plan, prepare_hook.as_ref())
    })
    .await
    .context("join kg.redb rewrite")??;

    // The ONLY exclusive section: re-check, rename, install. Everything above
    // ran with writers free to proceed; anything they wrote is caught by the
    // fingerprint re-check inside `commit`, which aborts rather than swap.
    //
    // Two locks, always in this order. The palace write mutex keeps
    // `remember`/`forget` out, and the store's own `swap_lock` — taken inside
    // `commit` — keeps out every writer that never touches the palace mutex,
    // `KgWriter`'s actor first among them (#6652, code-critic BLOCK). The whole
    // thing runs on a blocking thread because `swap_lock` is a std lock that
    // waits for in-flight redb transactions to drain; taking it on an executor
    // thread would block that worker.
    let outcome = {
        let _write_guard = timeouts::lock_with_timeout(
            &handle.write_mutex,
            timeouts::write_lock_timeout(),
            handle.id.as_str(),
        )
        .await?;
        let store = handle.kg.redb_store().clone();
        let commit_hook = hook.clone();
        tokio::task::spawn_blocking(move || prepared.commit(&store, commit_hook.as_ref()))
            .await
            .context("join kg.redb swap")??
    };

    Ok(KgCompactReport {
        stats,
        dry_run: false,
        skipped: None,
        bytes_before: outcome.bytes_before,
        bytes_after: outcome.bytes_after,
        rows_copied: outcome.rows_copied,
        history_rows_pruned: outcome.history_rows_pruned,
        backup: outcome.backup,
    })
}

/// The reason to skip the rewrite, or `None` to run it.
///
/// Why: two gates, both about cost. A small file has nothing worth reclaiming;
/// a file whose slack is a rounding error is not worth a full read+write pass
/// plus a same-size backup.
/// What: `compact` off, or file under `compact_min_bytes`, or reclaimable under
/// [`COMPACT_MIN_RECLAIM_PERCENT`] of the file.
/// Test: `kg_compaction_is_skipped_below_the_size_gate`.
fn gate(stats: &KgRedbStats, config: &DreamConfig) -> Option<String> {
    if !config.compact {
        return Some("dream.compact is disabled".to_string());
    }
    if stats.file_bytes < config.compact_min_bytes {
        return Some(format!(
            "kg.redb is {} bytes, under the {}-byte dream.compact_min_bytes floor",
            stats.file_bytes, config.compact_min_bytes
        ));
    }
    let threshold = stats.file_bytes.saturating_mul(COMPACT_MIN_RECLAIM_PERCENT) / 100;
    if stats.reclaimable_bytes < threshold {
        return Some(format!(
            "only ~{} of {} bytes are reclaimable, under the {COMPACT_MIN_RECLAIM_PERCENT}% \
             floor",
            stats.reclaimable_bytes, stats.file_bytes
        ));
    }
    None
}

/// `<data_dir>/kg.redb`, or `None` for an in-memory palace.
fn kg_redb_path(handle: &Arc<PalaceHandle>) -> Option<PathBuf> {
    handle.data_dir.as_ref().map(|d| d.join("kg.redb"))
}

/// A report for a run that did not happen.
fn skipped(stats: KgRedbStats, dry_run: bool, reason: &str) -> KgCompactReport {
    let bytes = stats.file_bytes;
    KgCompactReport {
        stats,
        dry_run,
        skipped: Some(reason.to_string()),
        bytes_before: bytes,
        bytes_after: bytes,
        rows_copied: 0,
        history_rows_pruned: 0,
        backup: None,
    }
}

/// A zeroed measurement, for a palace with no file to measure.
fn empty_stats() -> KgRedbStats {
    KgRedbStats {
        path: PathBuf::new(),
        file_bytes: 0,
        from_snapshot: false,
        tables: Vec::new(),
        triples_active: 0,
        triples_closed_in_place: 0,
        triples_history: 0,
        triples_history_stale: 0,
        triples_history_stale_bytes: 0,
        history_cutoff_days: 0,
        superseded_drawers: 0,
        dead_predicate_index: None,
        reclaimable_bytes: 0,
    }
}
