//! Lossless BM25 backfill for a palace's existing drawers.
//!
//! Why: the BM25 lane indexes a drawer at write time, so every drawer written
//! before the lane was switched on is invisible to it — which is every drawer
//! on this host, since `TRUSTY_BM25_DAEMON=1` has never been set in any shipped
//! path. Turning the lane on without a backfill would produce a palace that
//! answers lexical queries from whatever it happened to see since the last
//! restart, and reports that as a normal empty result.
//!
//! Why NOT the existing write path: `tools::bm25::bm25_index_enqueue` writes
//! into a 256-slot bounded channel with `try_send` and DROPS on full. That is
//! a defensible trade for the write path — a dropped index op costs one stale
//! entry, the drawer itself is durable in redb, and `memory_remember` must not
//! wait on daemon RTT. It is not defensible for a backfill: the largest palace
//! on this host holds 1311 drawers, five times the queue, so a backfill routed
//! through it would silently drop roughly 80% of the corpus and leave the
//! palace answering from a fifth of its content — indistinguishable, from the
//! outside, from working fusion. So the write path is left exactly as it is
//! and backfill gets its own feeder: one document at a time, each awaiting the
//! daemon's ack, so the only backpressure mechanism in play is "wait for the
//! previous write to land". Nothing can be dropped because nothing is ever
//! offered to a full queue.
//!
//! What: [`backfill_palace`] drives the feeder against a socket; [`palace_docs`]
//! extracts the `(drawer_id, text)` pairs; [`backfill_state_palace`] wires both
//! to an [`AppState`] and its spawn supervisor; [`spawn_startup_backfill`]
//! sweeps every palace that has drawers, serially, when the lane is enabled.
//! Idempotent throughout — the daemon's `upsert_document` is keyed by `doc_id`,
//! so a re-run overwrites rather than duplicating.
//!
//! Fail-open: every failure mode degrades to a reported status, never an error
//! that propagates into a caller's request path and never an unbounded wait.
//! Daemon absent, spawn refused, RPC timeout, and per-document errors are all
//! counted and surfaced in the [`BackfillReport`].
//!
//! Test: `bm25_backfill_tests.rs` (unit) and `tests/bm25_backfill_e2e.rs`
//! (`#[ignore]`d, drives a real `trusty-bm25-daemon`).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use trusty_common::bm25_client::Bm25Client;
use trusty_common::memory_core::retrieval::PalaceHandle;

use crate::AppState;

/// Per-document RPC deadline.
///
/// Why: the daemon coalesces writes on a 50 ms window and acks each one, so a
/// healthy round trip is sub-millisecond. Ten seconds is four orders of
/// magnitude of slack — long enough that a loaded host never trips it, short
/// enough that a wedged daemon costs one document's delay rather than stalling
/// the sweep behind it forever. A backfill that can hang is a backfill that can
/// hold a startup task open indefinitely.
/// What: 10 seconds, applied per `index` / `stats` call.
/// Test: `backfill_reports_daemon_unavailable_when_socket_is_dead`.
const OP_TIMEOUT: Duration = Duration::from_secs(10);

/// Whole-palace time budget.
///
/// Why: the per-op timeout bounds one document; this bounds the run. 2300
/// documents across every palace on this host is single-digit MB of text and
/// completes in seconds, so a run still going after two minutes is not slow,
/// it is stuck — and reporting `Partial` with a count beats blocking.
/// What: 120 seconds. On expiry the feeder stops and reports what landed.
/// Test: covered by construction; the counters make a truncated run visible.
const PALACE_BUDGET: Duration = Duration::from_secs(120);

/// Environment opt-out for the startup sweep.
///
/// Why: an operator who wants the lane on but the backfill deferred (a large
/// cold palace on a busy host) needs a way to say so that does not also
/// disable the lane. Without it the only lever is `TRUSTY_BM25_DAEMON=0`,
/// which turns off the thing they were trying to keep.
/// What: `TRUSTY_BM25_NO_BACKFILL=1` skips the sweep. Explicit per-palace
/// calls to [`backfill_state_palace`] still work.
/// Test: `startup_backfill_respects_the_opt_out`.
pub const ENV_NO_BACKFILL: &str = "TRUSTY_BM25_NO_BACKFILL";

/// Outcome class of one palace's backfill.
///
/// Why: a caller (and an operator reading logs) needs to tell "nothing to do"
/// from "could not do it" from "did it, partially". Collapsing those into a
/// bool is how a partially-indexed palace comes to look finished.
/// What: five terminal states. Only [`Self::Completed`] means the daemon's
/// corpus is known to cover the palace.
/// Test: `bm25_backfill_tests.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackfillStatus {
    /// The BM25 lane is switched off — nothing was attempted.
    Disabled,
    /// The daemon could not be reached or started. Recall degrades to
    /// vector-only; a later run can repair.
    DaemonUnavailable,
    /// The daemon already holds a document for every drawer.
    AlreadyIndexed,
    /// Every drawer was submitted and acked.
    Completed,
    /// Some drawers failed or the time budget expired. The index is usable but
    /// incomplete; a re-run repairs it.
    Partial,
}

/// What one backfill run did.
///
/// Why: the counters are the evidence that separates "the lane is on" from
/// "the lane has content". `final_doc_count` in particular is read back from
/// the daemon rather than inferred from the submissions, so a run that acked
/// 1311 documents into a daemon that kept 200 reports the discrepancy instead
/// of claiming success.
/// What: plain owned counters; `final_doc_count` is `None` when the post-run
/// `stats` call itself failed.
/// Test: `bm25_backfill_tests.rs`.
#[derive(Debug, Clone)]
pub struct BackfillReport {
    pub palace: String,
    pub status: BackfillStatus,
    /// Drawers the palace holds.
    pub drawers_total: usize,
    /// Drawers with no indexable text, skipped without being submitted.
    pub skipped_empty: usize,
    /// Documents the daemon acked.
    pub indexed: usize,
    /// Documents that errored or timed out.
    pub failed: usize,
    /// `doc_count` the daemon reported after the run.
    pub final_doc_count: Option<usize>,
    pub elapsed_ms: u64,
}

impl BackfillReport {
    /// Terminal report for a run that never started.
    fn short_circuit(palace: &str, status: BackfillStatus, drawers_total: usize) -> Self {
        Self {
            palace: palace.to_string(),
            status,
            drawers_total,
            skipped_empty: 0,
            indexed: 0,
            failed: 0,
            final_doc_count: None,
            elapsed_ms: 0,
        }
    }

    /// True when the daemon is known to hold every drawer this palace has.
    ///
    /// Why: this is the question the whole module exists to answer, and it must
    /// be answered from the daemon's own reported count — not from how many
    /// writes we believe we sent.
    /// What: `true` for `AlreadyIndexed`, or for `Completed` whose read-back
    /// count covers the non-empty drawers. Anything else, including a
    /// `Completed` run whose read-back failed, is `false`.
    /// Test: `fully_indexed_requires_a_read_back_count`.
    pub fn fully_indexed(&self) -> bool {
        match self.status {
            BackfillStatus::AlreadyIndexed => true,
            BackfillStatus::Completed => self
                .final_doc_count
                // `saturating_sub` because the alternative is a debug panic in
                // a coverage predicate — an inconsistent report should read as
                // "not covered", not take the daemon down.
                .is_some_and(|n| n >= self.drawers_total.saturating_sub(self.skipped_empty)),
            _ => false,
        }
    }
}

/// Extract the `(doc_id, text)` pairs a palace should have indexed.
///
/// Why: the drawer table is behind a `parking_lot` lock, which must not be held
/// across an `.await`. Materialising the pairs up front costs one clone of
/// single-digit MB of text — the entire corpus across ~99 palaces is that
/// size — and removes the lock from the async path entirely.
/// What: clones `(id.to_string(), content)` for every drawer whose content has
/// non-whitespace text. Empty drawers are omitted: indexing zero tokens can
/// never produce a hit, so submitting them would inflate the daemon's
/// `doc_count` and break the coverage comparison this module relies on.
/// Test: `palace_docs_skips_blank_drawers`.
pub fn palace_docs(handle: &PalaceHandle) -> Vec<(String, String)> {
    let drawers = handle.drawers.read();
    drawers
        .iter()
        .filter(|d| !d.content.trim().is_empty())
        .map(|d| (d.id.to_string(), d.content.clone()))
        .collect()
}

/// Feed a palace's documents to a BM25 daemon, losslessly.
///
/// Why: see the module doc — the live write path drops on a full queue, which
/// is wrong for a corpus five times the queue's depth. This feeder cannot drop
/// because it never offers work to a queue it has not been invited into: each
/// `index` call awaits the daemon's ack, and the daemon's own intake channel
/// applies real backpressure (`send().await`) behind that ack.
/// What: submits `docs` one at a time under [`OP_TIMEOUT`], stopping early if
/// [`PALACE_BUDGET`] expires. Skips the whole run when a pre-flight `stats`
/// shows the daemon already holds at least as many documents as we are about
/// to send, unless `force`. Reads `stats` back afterwards so the report carries
/// the daemon's own count rather than ours.
/// Failure handling is deliberately asymmetric: a failed pre-flight `stats`
/// proceeds with the full run (doing redundant work is safe; skipping work we
/// cannot prove is done is not), while a failed post-run `stats` leaves
/// `final_doc_count` as `None` so `fully_indexed` reports `false`.
/// Test: `tests/bm25_backfill_e2e.rs::backfill_indexes_every_drawer_without_drops`.
pub async fn backfill_palace(
    socket: &Path,
    palace: &str,
    docs: Vec<(String, String)>,
    force: bool,
) -> BackfillReport {
    let started = Instant::now();
    let client = Bm25Client::new(socket.to_path_buf());
    let total = docs.len();

    if total == 0 {
        return BackfillReport::short_circuit(palace, BackfillStatus::AlreadyIndexed, 0);
    }

    // Pre-flight. A failure here is NOT a reason to skip — it is a reason to
    // do the work, because we cannot show the work is already done.
    if !force {
        match tokio::time::timeout(OP_TIMEOUT, client.stats()).await {
            Ok(Ok(stats)) if stats.doc_count >= total => {
                tracing::debug!(
                    palace = %palace,
                    doc_count = stats.doc_count,
                    drawers = total,
                    "bm25 backfill: palace already indexed — skipping"
                );
                let mut report =
                    BackfillReport::short_circuit(palace, BackfillStatus::AlreadyIndexed, total);
                report.final_doc_count = Some(stats.doc_count);
                report.elapsed_ms = started.elapsed().as_millis() as u64;
                return report;
            }
            Ok(Ok(stats)) => tracing::info!(
                palace = %palace,
                indexed = stats.doc_count,
                drawers = total,
                "bm25 backfill: palace under-indexed — running"
            ),
            // The daemon is unreachable. Report it rather than spending the
            // whole budget discovering the same thing 1311 more times.
            Ok(Err(e)) => {
                tracing::warn!(palace = %palace, "bm25 backfill: daemon unreachable: {e:#}");
                return BackfillReport::short_circuit(
                    palace,
                    BackfillStatus::DaemonUnavailable,
                    total,
                );
            }
            Err(_) => {
                tracing::warn!(palace = %palace, "bm25 backfill: stats timed out — daemon wedged");
                return BackfillReport::short_circuit(
                    palace,
                    BackfillStatus::DaemonUnavailable,
                    total,
                );
            }
        }
    }

    let deadline = started + PALACE_BUDGET;
    let mut indexed = 0usize;
    let mut failed = 0usize;
    let mut truncated = false;

    for (doc_id, text) in &docs {
        if Instant::now() >= deadline {
            tracing::warn!(
                palace = %palace,
                indexed,
                remaining = total - indexed - failed,
                "bm25 backfill: time budget expired — reporting partial coverage"
            );
            truncated = true;
            break;
        }
        match tokio::time::timeout(OP_TIMEOUT, client.index(doc_id, text)).await {
            Ok(Ok(())) => indexed += 1,
            Ok(Err(e)) => {
                failed += 1;
                tracing::warn!(palace = %palace, doc_id = %doc_id, "bm25 backfill index failed: {e:#}");
            }
            Err(_) => {
                failed += 1;
                tracing::warn!(palace = %palace, doc_id = %doc_id, "bm25 backfill index timed out");
            }
        }
    }

    // Read the coverage back from the daemon. Our own ack count is what we
    // believe happened; this is what the daemon says happened.
    let final_doc_count = match tokio::time::timeout(OP_TIMEOUT, client.stats()).await {
        Ok(Ok(stats)) => Some(stats.doc_count),
        Ok(Err(e)) => {
            tracing::warn!(palace = %palace, "bm25 backfill: post-run stats failed: {e:#}");
            None
        }
        Err(_) => {
            tracing::warn!(palace = %palace, "bm25 backfill: post-run stats timed out");
            None
        }
    };

    let status = if failed == 0 && !truncated {
        BackfillStatus::Completed
    } else {
        BackfillStatus::Partial
    };
    let report = BackfillReport {
        palace: palace.to_string(),
        status,
        drawers_total: total,
        skipped_empty: 0,
        indexed,
        failed,
        final_doc_count,
        elapsed_ms: started.elapsed().as_millis() as u64,
    };
    tracing::info!(
        palace = %palace,
        ?status,
        indexed,
        failed,
        final_doc_count = ?final_doc_count,
        elapsed_ms = report.elapsed_ms,
        "bm25 backfill finished"
    );
    report
}

/// Resolve a palace's daemon socket, starting the daemon if needed.
///
/// Why: [`backfill_palace`] takes a socket rather than an `AppState` so it can
/// be tested against a bare daemon. This is the adapter that turns "a palace
/// id" into "a socket that is being served", and it is also the point at which
/// the supervisor's daemon cap applies — a sweep across many palaces spawns
/// them one at a time and the cap reaps the ones that fall out of the window.
/// What: `None` when the lane is off or the supervisor could not start a
/// daemon. Uses the per-palace socket the supervisor returns, NOT
/// `state.bm25_client`, which is bound to the default palace's socket only.
/// Test: `backfill_state_palace_is_disabled_without_a_client`.
async fn socket_for_palace(state: &AppState, palace: &str) -> Option<PathBuf> {
    state.bm25_client.as_ref()?;
    let supervisor = state.bm25_supervisor.as_ref()?;
    let data_dir = state.data_root.join(palace).join("bm25");
    match supervisor.ensure_running(palace, &data_dir).await {
        Ok(socket) => Some(socket),
        Err(e) => {
            tracing::warn!(palace = %palace, "bm25 backfill: could not start daemon: {e:#}");
            None
        }
    }
}

/// Backfill one palace through the daemon's spawn supervisor.
///
/// Why: the entry point callers actually use. Keeping the lane check here
/// means every caller degrades identically when the lane is off, instead of
/// each remembering to test `bm25_client.is_some()` first.
/// What: returns [`BackfillStatus::Disabled`] when the lane is off and
/// [`BackfillStatus::DaemonUnavailable`] when the daemon will not start —
/// neither is an error, because neither should fail a caller's request.
/// Test: `backfill_state_palace_is_disabled_without_a_client`.
pub async fn backfill_state_palace(
    state: &AppState,
    handle: &PalaceHandle,
    palace: &str,
    force: bool,
) -> BackfillReport {
    if state.bm25_client.is_none() {
        return BackfillReport::short_circuit(palace, BackfillStatus::Disabled, 0);
    }
    let docs = palace_docs(handle);
    let Some(socket) = socket_for_palace(state, palace).await else {
        return BackfillReport::short_circuit(
            palace,
            BackfillStatus::DaemonUnavailable,
            docs.len(),
        );
    };
    backfill_palace(&socket, palace, docs, force).await
}

/// Sweep every registered palace that has drawers, serially.
///
/// Why: a daemon restart is the only moment at which the whole corpus is known
/// to be reachable and nothing is waiting on it. Serial is deliberate: the
/// supervisor caps concurrently-live daemons at three, so a parallel sweep
/// would spend its time reaping daemons it had just spawned.
/// What: returns immediately, doing the work on a spawned task. No-op when the
/// lane is off or [`ENV_NO_BACKFILL`] is set — which, until the lane's default
/// is flipped, is every deployment. Palaces with zero drawers are skipped
/// without starting a daemon, which is roughly 80 of the ~99 on this host.
/// Test: `startup_backfill_respects_the_opt_out`.
pub fn spawn_startup_backfill(state: &AppState) {
    if state.bm25_client.is_none() {
        tracing::debug!("bm25 backfill: lane disabled — skipping startup sweep");
        return;
    }
    if std::env::var(ENV_NO_BACKFILL).as_deref() == Ok("1") {
        tracing::info!("bm25 backfill: {ENV_NO_BACKFILL}=1 — skipping startup sweep");
        return;
    }
    let state = state.clone();
    tokio::spawn(async move {
        let started = Instant::now();
        let mut swept = 0usize;
        let mut incomplete = 0usize;
        for id in state.registry.list() {
            // `get` rather than `peek`: the sweep genuinely uses the handle, so
            // it should refresh the registry's own idle-eviction recency.
            let Some(handle) = state.registry.get(&id) else {
                continue;
            };
            let palace = id.as_str().to_string();
            // Skipping empty palaces here rather than inside `backfill_palace`
            // is what keeps the sweep from starting ~80 daemons to tell each
            // one it has nothing to do.
            if handle.drawers.read().is_empty() {
                continue;
            }
            let report = backfill_state_palace(&state, &handle, &palace, false).await;
            swept += 1;
            if !report.fully_indexed() {
                incomplete += 1;
            }
        }
        tracing::info!(
            swept,
            incomplete,
            elapsed_ms = started.elapsed().as_millis() as u64,
            "bm25 backfill: startup sweep complete"
        );
    });
}

#[cfg(test)]
#[path = "bm25_backfill_tests.rs"]
mod tests;
