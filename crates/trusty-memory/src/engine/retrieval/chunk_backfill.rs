//! #9735: the chunk backfill engine — one bounded re-embed slice per call.
//!
//! Why: drawers written before chunking (#9275) hold one truncated vector, so
//! their tail is unfindable, and nothing repaired them without an operator
//! command. A background caller needs a unit of work that never holds
//! `write_mutex` across an embed and never trusts a counter for progress.
//! What: [`PalaceHandle::reembed_slice`] re-embeds missing and short drawers
//! with the embed outside `write_mutex`; [`PalaceHandle::commit_if_current`]
//! takes the mutex only to recheck the drawer text and run one upsert.
//! Test: `chunk_backfill_tests`.

use super::chunk_split::{SplitStep, is_split_part};
use super::deferred_embed::{EmbedLoss, RetryPolicy, with_retry};
use super::embed_repair::EmbedHealth;
use super::embed_store::{chunked_embed_timeout, embed_planned, embed_planned_within, plan_chunks};
use super::handle::PalaceHandle;
use crate::engine::embed::Embedder;
use crate::engine::palace::Drawer;
use crate::engine::store::embed_ledger::{self, EmbedFailure};
use crate::engine::timeouts;
use chrono::{DateTime, Utc};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};
use uuid::Uuid;

/// Limits on one [`PalaceHandle::reembed_slice`] call.
///
/// Why: a background slice must yield to writers and to shutdown (ruling 8).
/// Measured on EVO (x86_64, CPU ONNX, 256-token window): 64 chunks embed in
/// about 1.5 s and a 64-chunk slice ends in about 2.4 s, so the chunk cap,
/// not the 20 s budget, ends a slice; 120 s fits about 5 000 chunks, above
/// the 4096-chunk drawer cap.
/// What: every limit except `embed_timeout` and `lock_wait` is checked
/// between drawers only, so none of them bounds one drawer; `embed_timeout`
/// bounds one drawer's embed and `lock_wait` one commit's lock wait.
/// Test: `slice_stops_starting_drawers_past_the_chunk_cap`,
/// `slice_embed_is_bounded_by_the_slice_timeout`,
/// `slice_defaults_and_recall_contention_measured` (the numbers above).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SliceBounds {
    /// Wall-clock budget, checked between drawers. Env
    /// `TRUSTY_CHUNK_BACKFILL_SLICE_MS`, default 20 000.
    pub(crate) budget: Duration,
    /// Drawers started per slice. Env `TRUSTY_CHUNK_BACKFILL_MAX_DRAWERS`, default 8.
    pub(crate) max_drawers: usize,
    /// Chunks embedded per slice. Env `TRUSTY_CHUNK_BACKFILL_MAX_CHUNKS`, default 64.
    pub(crate) max_chunks: usize,
    /// Ceiling on one drawer's embed. Env `TRUSTY_CHUNK_BACKFILL_EMBED_SECS`, default 120.
    pub(crate) embed_timeout: Duration,
    /// Ceiling on one commit's `write_mutex` wait. Env
    /// `TRUSTY_CHUNK_BACKFILL_LOCK_WAIT_SECS`, default 5.
    pub(crate) lock_wait: Duration,
    /// A missing-vector drawer younger than this is left to the deferred lane.
    /// Env `TRUSTY_CHUNK_BACKFILL_MIN_AGE_SECS`, default 120.
    pub(crate) min_age: Duration,
    /// A drawer with a ledger row younger than this is skipped. Env
    /// `TRUSTY_CHUNK_BACKFILL_BACKOFF_SECS`, default 86 400.
    pub(crate) ledger_backoff: Duration,
    /// Consecutive embed errors that end the slice as host-level; a timeout
    /// never counts. Env `TRUSTY_CHUNK_BACKFILL_HOST_FAILURES`, default 2.
    pub(crate) host_failures: usize,
}

impl Default for SliceBounds {
    fn default() -> Self {
        Self {
            budget: Duration::from_millis(20_000),
            max_drawers: 8,
            max_chunks: 64,
            embed_timeout: Duration::from_secs(120),
            lock_wait: Duration::from_secs(5),
            min_age: Duration::from_secs(120),
            ledger_backoff: Duration::from_secs(86_400),
            host_failures: 2,
        }
    }
}

impl SliceBounds {
    /// The defaults, each overridable by its env var; a malformed value keeps
    /// the default.
    /// Test: `slice_bounds_read_their_env_overrides`.
    #[cfg_attr(not(any(test, feature = "server")), allow(dead_code))] // #9735: the ticker calls it
    pub(crate) fn from_env() -> Self {
        let d = Self::default();
        let ms = |d: Duration| u64::try_from(d.as_millis()).unwrap_or(u64::MAX);
        Self {
            budget: Duration::from_millis(env_u64("TRUSTY_CHUNK_BACKFILL_SLICE_MS", ms(d.budget))),
            max_drawers: env_usize("TRUSTY_CHUNK_BACKFILL_MAX_DRAWERS", d.max_drawers),
            max_chunks: env_usize("TRUSTY_CHUNK_BACKFILL_MAX_CHUNKS", d.max_chunks),
            embed_timeout: secs("TRUSTY_CHUNK_BACKFILL_EMBED_SECS", d.embed_timeout),
            lock_wait: secs("TRUSTY_CHUNK_BACKFILL_LOCK_WAIT_SECS", d.lock_wait),
            min_age: secs("TRUSTY_CHUNK_BACKFILL_MIN_AGE_SECS", d.min_age),
            ledger_backoff: secs("TRUSTY_CHUNK_BACKFILL_BACKOFF_SECS", d.ledger_backoff),
            host_failures: env_usize("TRUSTY_CHUNK_BACKFILL_HOST_FAILURES", d.host_failures),
        }
    }
}

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(default)
}

fn env_usize(name: &str, default: usize) -> usize {
    usize::try_from(env_u64(name, default as u64)).unwrap_or(default)
}

fn secs(name: &str, default: Duration) -> Duration {
    Duration::from_secs(env_u64(name, default.as_secs()))
}

/// Why a slice ended.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) enum SliceStop {
    /// Every queued drawer was visited.
    #[default]
    QueueEmpty,
    /// The handle cannot write vectors; nothing ran.
    ReadOnly,
    /// The chunk check did not run (no tokenizer, failed scan); nothing ran.
    ChunkCheckUnavailable(String),
    /// The wall-clock budget was spent.
    Budget,
    /// `max_drawers` drawers were started.
    DrawerCap,
    /// `max_chunks` chunks were embedded.
    ChunkCap,
    /// `host_failures` consecutive embeds returned an error.
    HostFailure,
    /// One drawer's embed outlived its bound; that drawer is ledgered. The
    /// embedder may still be running it, so no further drawer is started.
    EmbedTimeout,
    /// `write_mutex` was not granted within `lock_wait`; the palace is busy.
    LockTimeout(String),
    /// The vector store refused an upsert.
    StoreError(String),
}

/// What one slice did. Progress itself is never read from here: it is
/// recomputed from the index by [`EmbedHealth::chunk_progress`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SliceReport {
    /// Drawers embedded or failed (a stale drawer is not counted).
    pub(crate) attempted: usize,
    /// Drawers whose new group committed.
    pub(crate) repaired: usize,
    /// Drawers that could not be split, embedded or stored.
    pub(crate) failed: usize,
    /// Drawers forgotten or rewritten while their embed ran; nothing written.
    pub(crate) stale: usize,
    /// Chunks embedded successfully in this slice.
    pub(crate) chunks_embedded: usize,
    /// `host_failures` or more embed errors and no success: the fault is
    /// the host's, so no ledger row was written for those errors.
    pub(crate) host_failure: bool,
    /// The ledger write or clear that failed at slice end, if any. Every
    /// other row was still attempted, and the clear still ran.
    pub(crate) ledger_error: Option<String>,
    /// Why the slice ended.
    pub(crate) stop: SliceStop,
}

/// Outcome of [`PalaceHandle::commit_if_current`] that wrote or skipped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Commit {
    /// The drawer still had the embedded text; its group was replaced.
    Committed,
    /// The drawer is gone or its text changed; nothing was written.
    Stale,
}

/// A commit that could not complete.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub(crate) enum ChunkCommitError {
    /// `write_mutex` was not granted in time.
    #[error("palace '{palace}': write lock not granted within {waited:?}")]
    LockWait { palace: String, waited: Duration },
    /// The vector store refused the upsert; redb rolled it back.
    #[error("palace '{palace}': vector upsert failed: {reason}")]
    Upsert { palace: String, reason: String },
}

/// A buffered ledger row and whether it is drawer-specific by construction.
struct PendingRow {
    id: Uuid,
    loss: EmbedLoss,
    drawer_specific: bool,
}

impl PalaceHandle {
    /// Re-embed up to one slice of missing and short drawers.
    ///
    /// Why (#9735): a background caller must repair pre-chunking drawers
    /// without blocking writers, recall or shutdown, and a failure must never
    /// advance progress.
    /// What: recomputes health with `embedder`; returns before any embed on a
    /// read-only handle or an unrun chunk check. Queues missing-vector drawers
    /// older than `min_age`, then short drawers by importance, skipping any
    /// with a ledger row younger than `ledger_backoff`. Per drawer: plan (#9735 PR 1b:
    /// a drawer over the chunk cap is split by `split_in_slice` instead), embed
    /// under `embed_timeout` with no lock, then [`Self::commit_if_current`].
    /// Stops on the bounds, a lock timeout, a store error, an embed timeout
    /// (that drawer is ledgered as drawer-specific), or `host_failures`
    /// consecutive embed errors. Ledger rows are written at the end, except
    /// embed errors in a host-level slice; repaired drawers' rows are cleared.
    /// Test: `chunk_backfill_tests` (e.g. `crash_midway_through_n_drawers_resumes_and_finishes`,
    /// `host_level_slice_failure_writes_no_ledger_rows`,
    /// `a_timed_out_drawer_is_ledgered_and_the_next_slice_moves_past_it`).
    #[cfg_attr(not(any(test, feature = "server")), allow(dead_code))] // #9735: the ticker calls it
    pub(crate) async fn reembed_slice(
        &self,
        embedder: &Arc<dyn Embedder + Send + Sync>,
        bounds: &SliceBounds,
    ) -> SliceReport {
        let mut report = SliceReport::default();
        if self.is_read_only() {
            report.stop = SliceStop::ReadOnly;
            return report;
        }
        let health = self.embed_health_with(Some(embedder.as_ref()));
        if let Some(reason) = health.chunk_check.unavailable_reason() {
            report.stop = SliceStop::ChunkCheckUnavailable(reason.to_string());
            return report;
        }
        let queue = self.backfill_queue(&health, bounds, Utc::now());
        let started = Instant::now();
        let mut rows: Vec<PendingRow> = Vec::new();
        let mut repaired: HashSet<Uuid> = HashSet::new();
        let (mut consecutive, mut embed_failures, mut started_drawers) = (0usize, 0usize, 0usize);
        for id in queue {
            if let Some(stop) = limit_reached(bounds, started, started_drawers, &report) {
                report.stop = stop;
                break;
            }
            let Some(text) = self.drawer_text(id) else {
                report.stale += 1;
                continue;
            };
            started_drawers += 1;
            let chunks: Vec<String> = match plan_chunks(embedder.as_ref(), &text) {
                Ok(c) => c.into_iter().map(str::to_string).collect(),
                Err(e) => {
                    // #9735 PR 1b: an oversized drawer is split, not ledgered.
                    let reason = match self
                        .split_in_slice(
                            embedder.as_ref(),
                            id,
                            &text,
                            &e,
                            bounds.lock_wait,
                            &mut report,
                        )
                        .await
                    {
                        SplitStep::Next => continue,
                        SplitStep::Stop => break,
                        SplitStep::Fail(reason) => reason,
                    };
                    // #9735 gap 12: a split error is the drawer's, never the host's.
                    report.attempted += 1;
                    report.failed += 1;
                    rows.push(PendingRow::new(id, embed_loss(reason), true));
                    continue;
                }
            };
            let vectors = match embed_bounded(embedder, &chunks, bounds.embed_timeout).await {
                Ok(v) => v,
                // #9735: the dropped embed may still hold the model, so the
                // next drawer would time out behind it; stop on this drawer.
                Err(EmbedFail::TimedOut(reason)) => {
                    report.attempted += 1;
                    report.failed += 1;
                    rows.push(PendingRow::new(id, embed_loss(reason), true));
                    report.stop = SliceStop::EmbedTimeout;
                    break;
                }
                Err(EmbedFail::Error(reason)) => {
                    report.attempted += 1;
                    report.failed += 1;
                    (consecutive, embed_failures) = (consecutive + 1, embed_failures + 1);
                    rows.push(PendingRow::new(id, embed_loss(reason), false));
                    if consecutive >= bounds.host_failures.max(1) {
                        report.stop = SliceStop::HostFailure;
                        break;
                    }
                    continue;
                }
            };
            consecutive = 0;
            report.chunks_embedded += chunks.len();
            match self
                .commit_if_current(id, &text, vectors, bounds.lock_wait)
                .await
            {
                Ok(Commit::Committed) => {
                    report.attempted += 1;
                    repaired.insert(id);
                }
                Ok(Commit::Stale) => report.stale += 1,
                Err(e @ ChunkCommitError::LockWait { .. }) => {
                    report.stop = SliceStop::LockTimeout(e.to_string());
                    break;
                }
                Err(e @ ChunkCommitError::Upsert { .. }) => {
                    report.attempted += 1;
                    report.failed += 1;
                    let loss = EmbedLoss::Upsert {
                        reason: e.to_string(),
                        attempts: 1,
                    };
                    rows.push(PendingRow::new(id, loss, true));
                    report.stop = SliceStop::StoreError(e.to_string());
                    break;
                }
            }
            tokio::task::yield_now().await;
        }
        report.repaired = repaired.len();
        report.host_failure = repaired.is_empty() && embed_failures >= bounds.host_failures.max(1);
        report.ledger_error = self
            .settle_slice_ledger(rows, repaired, report.host_failure)
            .await;
        report
    }

    /// Recheck the drawer under `write_mutex`, then upsert its group once.
    ///
    /// Why (#9735 Q5): an embed outside the lock can finish after a `forget`
    /// or a rewrite; committing then leaves an orphan or a stale group.
    /// What: waits at most `lock_wait` for `write_mutex`, the same mutex
    /// `forget` and `remember` hold; `Stale` (nothing written) when the drawer
    /// is gone or its text differs from `text`; otherwise one `upsert_chunks`
    /// under the guard, never retried inside it.
    /// Test: `no_orphan_rows_after_a_forget_races_the_slice`,
    /// `no_stale_vector_after_a_merge_races_the_slice`,
    /// `a_busy_palace_makes_the_slice_stop_not_queue`.
    pub(crate) async fn commit_if_current(
        &self,
        id: Uuid,
        text: &str,
        vectors: Vec<Vec<f32>>,
        lock_wait: Duration,
    ) -> Result<Commit, ChunkCommitError> {
        let palace = || self.id.as_str().to_string();
        let _guard = timeouts::lock_with_timeout(&self.write_mutex, lock_wait, self.id.as_str())
            .await
            .map_err(|_| ChunkCommitError::LockWait {
                palace: palace(),
                waited: lock_wait,
            })?;
        if self.drawer_text(id).as_deref() != Some(text) {
            return Ok(Commit::Stale);
        }
        self.vector_store
            .upsert_chunks(id, vectors)
            .await
            .map(|()| Commit::Committed)
            .map_err(|e| ChunkCommitError::Upsert {
                palace: palace(),
                reason: format!("{e:#}"),
            })
    }

    /// The drawer's current text, or `None` once it is gone.
    pub(super) fn drawer_text(&self, id: Uuid) -> Option<String> {
        self.drawers
            .read()
            .iter()
            .find(|d| d.id == id)
            .map(|d| d.content().to_string())
    }

    /// Missing-vector drawers first, then short drawers by importance.
    ///
    /// Why (rulings 7, 9): a drawer with no vector is invisible to recall, and
    /// a young one may still have a deferred embed in flight.
    /// What: drops drawers with a ledger row younger than `ledger_backoff`
    /// and missing-vector drawers younger than `min_age`; short drawers sort
    /// by descending importance. The age rule never applies to short drawers
    /// or to split parts (#9735).
    /// Test: `queue_orders_missing_first_then_short_by_importance`,
    /// `missing_vector_drawer_younger_than_two_minutes_is_skipped`,
    /// `split_part_younger_than_two_minutes_still_embeds`.
    pub(crate) fn backfill_queue(
        &self,
        health: &EmbedHealth,
        bounds: &SliceBounds,
        now: DateTime<Utc>,
    ) -> Vec<Uuid> {
        let younger = |at: DateTime<Utc>, limit: Duration| {
            now.signed_duration_since(at)
                .to_std()
                .map_or(true, |age| age < limit)
        };
        let backed_off: HashSet<Uuid> = health
            .recorded_failures
            .iter()
            .filter(|f| younger(f.failed_at, bounds.ledger_backoff))
            .map(|f| f.drawer_id)
            .collect();
        let drawers = self.drawers.read();
        let by_id: HashMap<Uuid, &Drawer> = drawers.iter().map(|d| (d.id, d)).collect();
        let mut queue: Vec<Uuid> = health
            .missing_vector_ids
            .iter()
            .filter(|id| !backed_off.contains(id))
            .filter(|id| {
                by_id
                    .get(id)
                    // #9735 PR 1b: no deferred embed is in flight for a split part.
                    .is_some_and(|d| is_split_part(d) || !younger(d.created_at, bounds.min_age))
            })
            .copied()
            .collect();
        let mut short: Vec<(f32, Uuid)> = health
            .short_chunk_ids
            .iter()
            .filter(|id| !backed_off.contains(id))
            .map(|id| (by_id.get(id).map_or(0.0, |d| d.importance), *id))
            .collect();
        short.sort_by(|a, b| b.0.total_cmp(&a.0));
        queue.extend(short.into_iter().map(|(_, id)| id));
        queue
    }

    /// Write the slice's buffered ledger rows and clear repaired drawers' rows.
    ///
    /// Why (#9735 gap 12): an embedder that is down fails every drawer, and a
    /// row per drawer would back healthy drawers off for a day.
    /// What: in a host-level slice only drawer-specific rows (split, upsert,
    /// timeout) are written; otherwise every row is. One `spawn_blocking`,
    /// because `json_rmw` blocks on an flock. Each row and the clear run
    /// through [`write_ledger`]; a failure is logged and returned.
    /// Test: `host_level_slice_failure_writes_no_ledger_rows`,
    /// `one_bad_drawer_among_good_ones_is_ledgered_and_the_rest_repair`,
    /// `a_malformed_ledger_is_reported_on_the_slice`.
    async fn settle_slice_ledger(
        &self,
        rows: Vec<PendingRow>,
        repaired: HashSet<Uuid>,
        host_failure: bool,
    ) -> Option<String> {
        let dir = self.data_dir.clone()?;
        let now = Utc::now();
        let entries: Vec<EmbedFailure> = rows
            .into_iter()
            .filter(|r| r.drawer_specific || !host_failure)
            .map(|r| EmbedFailure {
                drawer_id: r.id,
                failed_at: now,
                attempts: r.loss.attempts(),
                reason: r.loss.reason(),
            })
            .collect();
        if entries.is_empty() && repaired.is_empty() {
            return None;
        }
        let written = tokio::task::spawn_blocking(move || {
            write_ledger(
                entries,
                &repaired,
                |entry| embed_ledger::record(&dir, entry),
                |ids| embed_ledger::clear(&dir, ids),
            )
        })
        .await
        .unwrap_or_else(|e| Some(format!("slice ledger task failed: {e}")));
        if let Some(e) = &written {
            tracing::warn!(palace = %self.id, "#9735: slice ledger write failed: {e}");
        }
        written
    }
}

/// Record each row on its own, then clear `repaired` whatever happened.
///
/// Why (#9735): stopping at the first failed `record` dropped the other rows
/// and skipped the clear, so every failing drawer lost its backoff.
/// What: calls `record` once per entry and `clear` once; returns the joined
/// errors, or `None` when every call succeeded.
/// Test: `a_ledger_fault_on_one_row_still_records_the_rest_and_clears`,
/// `a_failed_ledger_clear_is_reported_on_the_slice`.
pub(super) fn write_ledger(
    entries: Vec<EmbedFailure>,
    repaired: &HashSet<Uuid>,
    mut record: impl FnMut(EmbedFailure) -> anyhow::Result<()>,
    clear: impl FnOnce(&HashSet<Uuid>) -> anyhow::Result<()>,
) -> Option<String> {
    let mut errors: Vec<String> = entries
        .into_iter()
        .filter_map(|entry| record(entry).err().map(|e| format!("{e:#}")))
        .collect();
    if let Err(e) = clear(repaired) {
        errors.push(format!("{e:#}"));
    }
    (!errors.is_empty()).then(|| errors.join("; "))
}

impl PendingRow {
    fn new(id: Uuid, loss: EmbedLoss, drawer_specific: bool) -> Self {
        Self {
            id,
            loss,
            drawer_specific,
        }
    }
}

fn embed_loss(reason: String) -> EmbedLoss {
    EmbedLoss::Embed {
        reason,
        attempts: 1,
    }
}

/// The bound a slice hit before starting another drawer, if any.
fn limit_reached(
    bounds: &SliceBounds,
    started: Instant,
    started_drawers: usize,
    report: &SliceReport,
) -> Option<SliceStop> {
    if started.elapsed() >= bounds.budget {
        Some(SliceStop::Budget)
    } else if started_drawers >= bounds.max_drawers {
        Some(SliceStop::DrawerCap)
    } else if report.chunks_embedded >= bounds.max_chunks {
        Some(SliceStop::ChunkCap)
    } else {
        None
    }
}

/// Why one drawer's slice embed failed.
enum EmbedFail {
    /// The embedder returned an error.
    Error(String),
    /// The bound elapsed. Only the future was dropped: the embedder's
    /// blocking work, and its model lock, may still be running.
    TimedOut(String),
}

/// Embed `chunks` under the scaled per-chunk ceiling and the slice's own.
///
/// Why (#9735 gap 11): the scaled ceiling can be minutes; a slice must give
/// up on one drawer sooner, and must tell a timeout from an error.
/// What: one timeout at the lower of `slice_timeout` and the scaled bound,
/// so either expiry is [`EmbedFail::TimedOut`].
/// Test: `slice_embed_is_bounded_by_the_slice_timeout`,
/// `a_timed_out_drawer_is_ledgered_and_the_next_slice_moves_past_it`.
async fn embed_bounded(
    embedder: &Arc<dyn Embedder + Send + Sync>,
    chunks: &[String],
    slice_timeout: Duration,
) -> Result<Vec<Vec<f32>>, EmbedFail> {
    let scaled = chunked_embed_timeout(timeouts::embed_batch_timeout(), chunks.len());
    let bound = slice_timeout.min(scaled);
    match tokio::time::timeout(bound, embed_planned(embedder.as_ref(), chunks)).await {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) => Err(EmbedFail::Error(format!("{e:#}"))),
        Err(_) => Err(EmbedFail::TimedOut(format!(
            "slice embed of {} chunks timed out after {bound:?}",
            chunks.len()
        ))),
    }
}

/// Outcome of [`PalaceHandle::repair_one`] on the operator path.
#[derive(Debug)]
pub(crate) enum Repair {
    /// The new group committed after this many attempts.
    Repaired(u32),
    /// The drawer was forgotten or rewritten during the embed; nothing written.
    Stale,
    /// The split, embed or upsert failed after its retries.
    Failed(EmbedLoss),
    /// `write_mutex` was not granted in time; the run must stop.
    Locked(ChunkCommitError),
}

impl PalaceHandle {
    /// Re-embed one drawer for `backfill_with`.
    ///
    /// Why (#9735 Q5): `embed_and_store` upserts with no recheck, so a forget
    /// during its embed left an orphan row; wrapping it in the lock would hold
    /// writers across the embed and its retry sleeps.
    /// What: [`embed_outside`] with retries and no lock, then
    /// [`Self::commit_if_current`] per upsert attempt, sleeping between
    /// attempts with the guard already dropped. A stale drawer is not retried.
    /// Test: `backfill_forget_between_read_and_upsert_leaves_no_orphan_row`,
    /// `backfill_upsert_failure_is_retried_without_holding_the_lock_between_attempts`.
    pub(crate) async fn repair_one(
        &self,
        embedder: &Arc<dyn Embedder + Send + Sync>,
        id: Uuid,
        text: &str,
        policy: &RetryPolicy,
        lock_wait: Duration,
    ) -> Repair {
        let (vectors, embed_attempts) = match embed_outside(embedder, text, policy).await {
            Ok(v) => v,
            Err(loss) => return Repair::Failed(loss),
        };
        let total = policy.attempts.max(1);
        let mut attempt = 1;
        loop {
            match self
                .commit_if_current(id, text, vectors.clone(), lock_wait)
                .await
            {
                Ok(Commit::Committed) => return Repair::Repaired(embed_attempts.max(attempt)),
                Ok(Commit::Stale) => return Repair::Stale,
                Err(e @ ChunkCommitError::LockWait { .. }) => return Repair::Locked(e),
                Err(ChunkCommitError::Upsert { reason, .. }) if attempt >= total => {
                    return Repair::Failed(EmbedLoss::Upsert {
                        reason,
                        attempts: total,
                    });
                }
                // #9735 Q5: the guard dropped with `commit_if_current`.
                Err(ChunkCommitError::Upsert { .. }) => {
                    tokio::time::sleep(policy.delay_for(attempt)).await;
                    attempt += 1;
                }
            }
        }
    }
}

/// Split `text` once and embed it with retries, holding no lock.
///
/// Why (#9735 Q5): the embed and its retry sleeps must stay outside
/// `write_mutex`.
/// What: a split error is an `Embed` loss after one attempt (as in
/// `embed_and_store`); each embed attempt runs under the scaled, clamped bound.
/// Test: `backfill_embed_and_retry_sleeps_run_outside_write_mutex`.
pub(crate) async fn embed_outside(
    embedder: &Arc<dyn Embedder + Send + Sync>,
    text: &str,
    policy: &RetryPolicy,
) -> Result<(Vec<Vec<f32>>, u32), EmbedLoss> {
    let chunks: Vec<String> = plan_chunks(embedder.as_ref(), text)
        .map_err(|e| embed_loss(format!("{e:#}")))?
        .into_iter()
        .map(str::to_string)
        .collect();
    let per_chunk = timeouts::embed_batch_timeout();
    with_retry(policy, |_| {
        let chunks = &chunks;
        async move {
            embed_planned_within(embedder.as_ref(), chunks, per_chunk)
                .await
                .map_err(|e| format!("{e:#}"))
        }
    })
    .await
    .map_err(|(reason, attempts)| EmbedLoss::Embed { reason, attempts })
}
