//! One reindex per index at a time (#8889).
//!
//! Why: on the apex-companion host two reindexes of one index overlapped, both
//! staged into the same `index.redb.tmp`, and each swapped the other's staging
//! file away — the corpus swap failed twice and the index was left with no open
//! corpus. The per-index semaphore only QUEUED the second run: it still answered
//! `queued: true`, replaced the first run's progress entry (orphaning its SSE
//! stream), and parked holding a global reindex permit.
//!
//! What: a per-index claim slot. [`try_claim_reindex`] succeeds only when no run
//! holds the slot; the returned [`ReindexClaim`] is moved into the reindex task
//! and releases the slot in `Drop`, so success, error, panic, and cancellation
//! (the task's future dropped at daemon shutdown) all free it. A second request
//! is REFUSED with the running job's identity rather than coalesced onto it: the
//! running job does not honour the second request's `force`, `root_path` or
//! priority, so reporting it as satisfied would be false. A slot whose mutex is
//! poisoned refuses too — the guard fails closed, never open.
//!
//! Test: `a_second_claim_is_refused_and_names_the_running_job`,
//! `the_claim_is_released_on_panic_and_on_cancellation`,
//! `a_poisoned_slot_refuses_the_claim`, `two_claims_get_distinct_staging_names`
//! (in `claim_tests.rs`), and
//! `a_second_reindex_request_is_refused_while_the_first_runs`,
//! `a_failed_reindex_releases_the_index_for_the_next_request` (`server::tests_8889`).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use dashmap::DashMap;

use crate::core::registry::IndexId;

/// The reindex that currently holds an index's claim.
///
/// Why: a refusal must name the job it lost to, so the caller can follow that
/// job's stream instead of guessing.
/// What: the run id (process-unique), who started it, when, and whether it was
/// a `force` run.
/// Test: `a_second_claim_is_refused_and_names_the_running_job`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RunningReindex {
    /// Process-unique, monotonically increasing run id.
    pub run_id: u64,
    /// Entry point that started the run (`"http"`, `"reconcile"`, `"api"`, ...).
    pub origin: &'static str,
    /// Start time, milliseconds since the Unix epoch.
    pub started_unix_ms: u64,
    /// Whether the run is a `force` rebuild.
    pub force: bool,
}

/// Why a reindex could not claim its index.
///
/// Why: the two cases need different answers — one is "wait for that job", the
/// other is "the guard itself is broken" — and both must stop the reindex.
/// What: `AlreadyRunning` carries the holder; `GuardUnavailable` carries why the
/// slot could not be checked.
/// Test: `a_second_claim_is_refused_and_names_the_running_job`,
/// `a_poisoned_slot_refuses_the_claim`.
#[derive(Debug, thiserror::Error)]
pub enum ReindexClaimError {
    /// Another reindex of this index is in flight.
    #[error("a reindex of index '{index_id}' is already running (run {}, started by {})", running.run_id, running.origin)]
    AlreadyRunning {
        index_id: String,
        running: RunningReindex,
    },
    /// The claim slot could not be read, so the reindex is refused (fail closed).
    #[error(
        "the reindex guard for index '{index_id}' is unavailable ({reason}); refusing the reindex"
    )]
    GuardUnavailable { index_id: String, reason: String },
    /// #9059: an exclude glob does not parse, so the index takes no reindex
    /// until a PATCH fixes it. `message` names the patterns and the fix.
    #[error("{message}")]
    Held {
        index_id: String,
        invalid_exclude_globs: Vec<String>,
        message: String,
    },
}

type Slot = Arc<Mutex<Option<RunningReindex>>>;

/// Per-index claim slots, created on first use.
static SLOTS: OnceLock<DashMap<IndexId, Slot>> = OnceLock::new();

/// Source of process-unique run ids.
static NEXT_RUN_ID: AtomicU64 = AtomicU64::new(1);

/// The claim slot for `id`, created on first use.
///
/// Why: every entry point must reach the same slot for one index.
/// What: `DashMap::entry` + `or_insert_with`; the slot is never evicted, so a
/// run that outlives its index's deletion still releases the slot it claimed.
/// Test: `a_poisoned_slot_refuses_the_claim` poisons a slot through this.
pub(crate) fn claim_slot(id: &IndexId) -> Slot {
    SLOTS
        .get_or_init(DashMap::new)
        .entry(id.clone())
        .or_insert_with(|| Arc::new(Mutex::new(None)))
        .clone()
}

/// Proof that the holder is the only reindex of its index; releases on drop.
///
/// Why: tying the release to `Drop` covers every exit path of the task that
/// owns it — return, `?`, panic unwind, and the future being dropped when the
/// runtime shuts down — without any caller having to remember to release.
/// What: holds the slot and the run it wrote there. `Drop` clears the slot only
/// if it still holds THIS run, and recovers a poisoned mutex to do so, so a
/// release is never skipped.
/// Test: `the_claim_is_released_on_panic_and_on_cancellation`.
#[derive(Debug)]
pub struct ReindexClaim {
    index_id: IndexId,
    slot: Slot,
    run: RunningReindex,
}

impl ReindexClaim {
    /// The run this claim represents.
    pub fn run(&self) -> &RunningReindex {
        &self.run
    }

    /// The index this claim holds.
    pub fn index_id(&self) -> &IndexId {
        &self.index_id
    }

    /// File name of this run's own staging corpus (#8889).
    ///
    /// Why: a fixed `index.redb.tmp` let two runs open, unlink and rename one
    /// file. A per-run name makes each staging file belong to exactly one run.
    /// What: `index.redb.run-<pid>-<run id>-<start ms>.tmp`. The pid and start
    /// time keep it unique across daemon restarts, not just within one process.
    /// Test: `two_claims_get_distinct_staging_names`.
    pub(crate) fn staging_file_name(&self) -> String {
        format!(
            "{}{}-{}-{}{}",
            super::staging_leftovers::RUN_STAGING_PREFIX,
            std::process::id(),
            self.run.run_id,
            self.run.started_unix_ms,
            super::staging_leftovers::RUN_STAGING_SUFFIX,
        )
    }
}

impl Drop for ReindexClaim {
    fn drop(&mut self) {
        let mut slot = self
            .slot
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if slot.as_ref().map(|r| r.run_id) == Some(self.run.run_id) {
            *slot = None;
        }
    }
}

/// Claim `id` for one reindex, or refuse.
///
/// Why: the single gate every reindex entry point passes — HTTP/MCP/CLI through
/// `reindex_report`, boot reconcile and library callers through
/// `spawn_reindex_with_cleanup` — so no path can start a second concurrent run.
/// What: locks the index's slot; a poisoned lock → `GuardUnavailable`; an
/// occupied slot → `AlreadyRunning` with the holder; otherwise records a new
/// run and returns its claim. The check and the write happen under one lock, so
/// two racing callers cannot both succeed.
/// Test: `a_second_claim_is_refused_and_names_the_running_job`,
/// `a_poisoned_slot_refuses_the_claim`,
/// `server::tests_8889::a_second_reindex_request_is_refused_while_the_first_runs`.
pub fn try_claim_reindex(
    id: &IndexId,
    origin: &'static str,
    force: bool,
) -> Result<ReindexClaim, ReindexClaimError> {
    let slot = claim_slot(id);
    let mut held = slot.lock().map_err(|e| {
        tracing::error!(
            "reindex[{}]: claim slot is poisoned ({e}) — refusing (#8889)",
            id.0
        );
        ReindexClaimError::GuardUnavailable {
            index_id: id.0.clone(),
            reason: "claim slot poisoned by a panic".to_string(),
        }
    })?;
    if let Some(running) = held.as_ref() {
        return Err(ReindexClaimError::AlreadyRunning {
            index_id: id.0.clone(),
            running: running.clone(),
        });
    }
    let run = RunningReindex {
        run_id: NEXT_RUN_ID.fetch_add(1, Ordering::Relaxed),
        origin,
        started_unix_ms: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0),
        force,
    };
    *held = Some(run.clone());
    drop(held);
    Ok(ReindexClaim {
        index_id: id.clone(),
        slot,
        run,
    })
}

#[cfg(test)]
#[path = "claim_tests.rs"]
mod tests;
