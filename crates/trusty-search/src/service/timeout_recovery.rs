//! Recovery for indexes whose eager warm-boot restore timed out (#4250).
//!
//! Why: a timed-out restore is parked in [`ColdIndexStore`] (#4087), which
//! stops it from vanishing for the rest of the boot but does NOT bring it back.
//! Parking makes an index reachable by a query naming its id verbatim and by
//! nothing else: `list_indexes` omits every cold entry, so a client that
//! discovers indexes by listing never learns the id exists; and boot reconcile
//! iterates `registry.list()`, so PR #4717's never-walked guard — the existing
//! retry path — cannot see it either. That guard is right about what it covers
//! and it is not extendable to this: it fires on a REGISTERED handle whose
//! lexical lane is owed, and a timeout-skipped index never becomes a handle at
//! all. What is missing is the step in front of it. This module supplies that
//! step, and once an index is registered again #4717's guard and the staleness
//! paths take over unchanged.
//!
//! The owner's daemon served 25 of 55 registered indexes for hours behind
//! exactly this, and #4250's original report ran 13.5 hours.
//!
//! What: [`recover_timed_out_indexes`] drains the cold store's `TimedOut`
//! cohort — never the `Deferred` cohort, which is `TRUSTY_WARMBOOT_MAX_INDEXES`
//! doing its job. [`spawn_timeout_recovery_ticker`] runs it on a fixed
//! interval, so an index that is still slow on the first attempt gets another,
//! and after [`MAX_RECOVERY_ATTEMPTS`] the entry moves to the store's existing
//! terminal `failed_entries` state where `/health` reports it as
//! `indexes_failed`. Retrying is never unbounded and the terminal state is
//! loud.
//!
//! Nothing here deletes or deregisters anything: a recovery attempt either
//! registers the index or leaves the entry parked.
//!
//! Test: `timeout_recovery_*` unit tests below.
//!
//! [`ColdIndexStore`]: crate::service::lazy_loader::ColdIndexStore
//! [`recover_timed_out_indexes`]: crate::service::timeout_recovery::recover_timed_out_indexes
//! [`spawn_timeout_recovery_ticker`]: crate::service::timeout_recovery::spawn_timeout_recovery_ticker
//! [`MAX_RECOVERY_ATTEMPTS`]: crate::service::timeout_recovery::MAX_RECOVERY_ATTEMPTS

use std::sync::Arc;
use std::time::Duration;

use crate::core::registry::IndexId;
use crate::service::warm_boot::restore_one_index_bounded;
use crate::service::SearchAppState;

/// Environment variable overriding the recovery cadence, in seconds. `0`
/// disables the ticker (the boot pass still runs once).
pub const RECOVERY_INTERVAL_ENV: &str = "TRUSTY_WARMBOOT_RETRY_SECS";

/// Default recovery cadence: 60 seconds.
///
/// Why: the failure being recovered from is a filesystem that was slow for tens
/// of seconds, so a minute is long enough for the transient to clear (and for
/// the abandoned blocking thread from the timed-out attempt to release the redb
/// file — the reason #4087 deliberately declined to retry inline) while still
/// bringing an index back in about a minute rather than at the next restart.
const DEFAULT_RECOVERY_INTERVAL_SECS: u64 = 60;

/// How many recovery attempts an index gets before it is marked failed.
///
/// Why: an unbounded retry would re-enter an expensive restore forever for an
/// index that is genuinely broken rather than slow. Five attempts at the
/// default cadence is five minutes of patience, after which the entry lands in
/// the cold store's existing permanently-failed state and `/health` reports it
/// under `indexes_failed` — a loud terminal state, not a silent one.
pub const MAX_RECOVERY_ATTEMPTS: u32 = 5;

/// Resolve the recovery cadence from the environment.
///
/// Why/What/Test: mirrors `orphan_reaper::reap_interval_secs` — `None` for `0`
/// (ticker disabled), otherwise a positive value or the default.
/// Test: `timeout_recovery_interval_env_branches`.
pub fn recovery_interval_secs() -> Option<u64> {
    match std::env::var(RECOVERY_INTERVAL_ENV) {
        Ok(v) => match v.trim().parse::<u64>() {
            Ok(0) => None,
            Ok(n) => Some(n),
            Err(_) => Some(DEFAULT_RECOVERY_INTERVAL_SECS),
        },
        Err(_) => Some(DEFAULT_RECOVERY_INTERVAL_SECS),
    }
}

/// What to do with a timeout-parked index on this pass (#4250).
///
/// Why: "retry, or give up?" was previously not a decision anything made — the
/// entry simply sat parked forever. Naming the two outcomes makes the give-up
/// path reviewable and lets the attempt arithmetic be unit-tested without a
/// clock, a filesystem, or a daemon.
/// What: a pure decision over the attempt count.
/// Test: `timeout_recovery_retries_until_the_attempt_cap`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryAction {
    /// Attempt the restore again on this pass.
    Retry { attempt: u32 },
    /// The attempt cap is spent — move the entry to the store's permanently-
    /// failed set so `/health` reports it under `indexes_failed` instead of
    /// leaving it parked and quiet. On-disk index data is untouched.
    GiveUp,
}

/// Decide whether a timeout-parked index gets another attempt (#4250).
///
/// Why: pure so the bound is testable on its own. The cap is what stops a
/// genuinely broken index from re-entering an expensive restore on every tick.
/// What: `attempts_so_far < max` → `Retry` with the 1-based attempt number;
/// otherwise `GiveUp`. A `max` of `0` gives up immediately.
/// Test: `timeout_recovery_retries_until_the_attempt_cap`.
pub fn classify_recovery(attempts_so_far: u32, max: u32) -> RecoveryAction {
    if attempts_so_far < max {
        RecoveryAction::Retry {
            attempt: attempts_so_far + 1,
        }
    } else {
        RecoveryAction::GiveUp
    }
}

/// Outcome counts for one recovery pass (#4250).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct RecoveryTally {
    /// Indexes that came back and are registered again.
    pub recovered: usize,
    /// Indexes that timed out again and stay parked for the next pass.
    pub still_timing_out: usize,
    /// Indexes that exhausted [`MAX_RECOVERY_ATTEMPTS`] and were marked failed.
    pub gave_up: usize,
}

/// Drive one recovery pass over the cold store's timeout cohort (#4250).
///
/// Why: this is the retry #4250 asks for. It is deliberately scoped to the
/// `TimedOut` cohort — draining `Deferred` entries too would load exactly the
/// indexes `TRUSTY_WARMBOOT_MAX_INDEXES` told the daemon not to load.
/// What: for each timeout-parked entry still absent from the registry, consults
/// [`classify_recovery`] against a per-id attempt count and either re-runs the
/// bounded restore (registering the index on success, via the same
/// `restore_index_on_demand` path a query would take) or marks it failed.
/// Requires a live embedder — the HNSW lane cannot be rebuilt without one — and
/// no-ops until one exists, so a pass during embedder init is a cheap skip
/// rather than a wasted attempt.
///
/// Never deletes or deregisters: an index either returns to the registry or
/// stays where it is.
/// Test: `timed_out_index_is_driven_back_into_the_registry`,
/// `recovery_pass_leaves_deferred_entries_alone`.
pub async fn recover_timed_out_indexes(
    state: &Arc<SearchAppState>,
    attempts: &dashmap::DashMap<IndexId, u32>,
) -> RecoveryTally {
    let mut tally = RecoveryTally::default();
    let cohort = state.cold_store.timed_out_entries();
    if cohort.is_empty() {
        return tally;
    }

    let Some(embedder) = state.current_embedder().await else {
        tracing::debug!(
            "timeout-recovery: {} index(es) awaiting retry but the embedder is not ready \
             yet — deferring to the next pass (issue #4250)",
            cohort.len()
        );
        return tally;
    };

    tracing::info!(
        "timeout-recovery: retrying {} index(es) parked by a warm-boot restore timeout — \
         they are absent from `list_indexes`, so nothing else would ever name them \
         (issue #4250)",
        cohort.len()
    );

    for entry in cohort {
        let id = IndexId::new(entry.id.clone());
        if state.registry.get(&id).is_some() {
            // A query beat us to it; clear the parking and move on.
            state.cold_store.mark_loaded(&id);
            attempts.remove(&id);
            continue;
        }

        let so_far = attempts.get(&id).map(|v| *v).unwrap_or(0);
        match classify_recovery(so_far, MAX_RECOVERY_ATTEMPTS) {
            RecoveryAction::GiveUp => {
                // The store's existing terminal state (#1106): surfaced on
                // `/health` as `indexes_failed`, and the on-disk corpus is left
                // completely alone.
                state.cold_store.mark_failed(&id);
                attempts.remove(&id);
                tally.gave_up += 1;
                tracing::error!(
                    index_id = %id.0,
                    "timeout-recovery: index '{}' failed to restore on {MAX_RECOVERY_ATTEMPTS} \
                     consecutive attempts and is now marked permanently failed for this \
                     daemon's lifetime — it is reported under `indexes_failed` on /health. \
                     Its on-disk index data was NOT touched: re-register with \
                     `trusty-search index <path>` or restart the daemon to retry. \
                     (issue #4250)",
                    id.0,
                );
            }
            RecoveryAction::Retry { attempt } => {
                attempts.insert(id.clone(), attempt);
                let s = Arc::clone(state);
                let e = Arc::clone(&embedder);
                let outcome = restore_one_index_bounded(entry, move |en| async move {
                    crate::service::lazy_restore::restore_index_on_demand(&s, &e, en).await;
                })
                .await;
                if outcome.is_complete() && state.registry.get(&id).is_some() {
                    state.cold_store.mark_loaded(&id);
                    attempts.remove(&id);
                    tally.recovered += 1;
                    tracing::info!(
                        index_id = %id.0,
                        "timeout-recovery: index '{}' restored on attempt {attempt} and is \
                         serving again (issue #4250)",
                        id.0,
                    );
                } else {
                    tally.still_timing_out += 1;
                    tracing::warn!(
                        index_id = %id.0,
                        "timeout-recovery: index '{}' did not restore on attempt \
                         {attempt}/{MAX_RECOVERY_ATTEMPTS} ({outcome:?}) — still parked, \
                         retried on the next pass (issue #4250)",
                        id.0,
                    );
                }
            }
        }
    }

    tally
}

/// Spawn the periodic recovery pass (#4250).
///
/// Why: one pass is not enough. The failure recovered from is "the filesystem
/// was slow for longer than the deadline", which does not necessarily clear by
/// the time the first retry runs — and #4087 documents why an immediate inline
/// retry is wrong (the abandoned blocking thread may still hold the redb file).
/// A ticker gives the transient time to pass while keeping the total bounded by
/// [`MAX_RECOVERY_ATTEMPTS`].
/// What: mirrors every other `spawn_*_ticker` — a detached task holding a
/// `Weak<SearchAppState>` so it exits with the daemon, an interval resolved
/// once at spawn (`0` disables and never spawns), and a per-id attempt counter
/// owned by the task.
/// Test: `recover_timed_out_indexes` (the per-pass logic) is covered directly;
/// this is a scheduling wrapper.
pub fn spawn_timeout_recovery_ticker(state: Arc<SearchAppState>) {
    let Some(secs) = recovery_interval_secs() else {
        tracing::info!("timeout-recovery: disabled via {RECOVERY_INTERVAL_ENV}=0");
        return;
    };
    let weak = Arc::downgrade(&state);
    tokio::spawn(async move {
        let attempts: dashmap::DashMap<IndexId, u32> = dashmap::DashMap::new();
        let mut interval = tokio::time::interval(Duration::from_secs(secs));
        // Skip the immediate first tick: warm-boot has only just finished and
        // the abandoned blocking thread from a timed-out restore may still hold
        // the redb file open.
        interval.tick().await;
        loop {
            interval.tick().await;
            let Some(state) = weak.upgrade() else {
                break;
            };
            let tally = recover_timed_out_indexes(&state, &attempts).await;
            if tally != RecoveryTally::default() {
                tracing::info!(
                    "timeout-recovery: pass complete — {} recovered, {} still timing out, \
                     {} gave up (issue #4250)",
                    tally.recovered,
                    tally.still_timing_out,
                    tally.gave_up,
                );
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why (#4250): the retry has to be bounded, or a genuinely broken index
    /// re-enters an expensive restore on every tick forever. This pins both the
    /// bound and the 1-based attempt numbering the log lines report.
    /// What: walks the attempt counter across the cap.
    /// Test: this test.
    #[test]
    fn timeout_recovery_retries_until_the_attempt_cap() {
        assert_eq!(
            classify_recovery(0, 3),
            RecoveryAction::Retry { attempt: 1 },
            "a never-attempted index must be retried"
        );
        assert_eq!(
            classify_recovery(2, 3),
            RecoveryAction::Retry { attempt: 3 }
        );
        assert_eq!(
            classify_recovery(3, 3),
            RecoveryAction::GiveUp,
            "the cap is inclusive — attempt N+1 never runs"
        );
        assert_eq!(classify_recovery(99, 3), RecoveryAction::GiveUp);
        assert_eq!(
            classify_recovery(0, 0),
            RecoveryAction::GiveUp,
            "a zero cap gives up without attempting"
        );
    }

    /// Why: the cadence knob must honour `0` and fall back safely, mirroring
    /// `reap_interval_secs`.
    /// Test: this test.
    #[test]
    #[serial_test::serial]
    fn timeout_recovery_interval_env_branches() {
        unsafe { std::env::set_var(RECOVERY_INTERVAL_ENV, "0") };
        assert_eq!(recovery_interval_secs(), None);
        unsafe { std::env::set_var(RECOVERY_INTERVAL_ENV, "15") };
        assert_eq!(recovery_interval_secs(), Some(15));
        unsafe { std::env::set_var(RECOVERY_INTERVAL_ENV, "junk") };
        assert_eq!(
            recovery_interval_secs(),
            Some(DEFAULT_RECOVERY_INTERVAL_SECS)
        );
        unsafe { std::env::remove_var(RECOVERY_INTERVAL_ENV) };
        assert_eq!(
            recovery_interval_secs(),
            Some(DEFAULT_RECOVERY_INTERVAL_SECS)
        );
    }
}
