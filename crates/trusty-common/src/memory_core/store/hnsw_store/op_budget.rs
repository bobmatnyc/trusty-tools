//! #9487 AC4: a time budget and a fail-closed breaker on HNSW operations.
//!
//! Why: a stuck HNSW operation — a graph lock never released, an insert that
//! never returns — blocked its caller forever, and every caller queued behind
//! it. #9487's deadlock is fixed; AC4 asks that the next such defect cost one
//! palace's vector store a typed error instead of wedging the palace for good.
//! What: `upsert` and `search` take the insert gate and the graph lock through
//! `HnswStore::bounded_gate` / `HnswStore::bounded_graph`, each bounded by
//! the store's budget (`TRUSTY_HNSW_OP_BUDGET_SECS`, default twice the redb
//! write-transaction deadline). The vector layer runs its blocking work
//! through [`HnswStore::run_bounded`], whose spawned watcher owns the
//! operation's lifetime: it runs the budget clock even after the caller is
//! dropped, trips the store's [`OpBreaker`], and counts the operation once.
//! From then on every bounded operation returns [`OpBudgetError::Wedged`]
//! without spawning a thread or waiting on a lock. The breaker never resets:
//! it lives on the `HnswStore`, so it clears only when the palace is reopened
//! (a new store) or the process restarts. [`abandoned_ops_in_flight`] counts,
//! process-wide, abandoned operations still running, so health can see them
//! after their palace is evicted.
//! Test: `an_upsert_behind_a_parked_graph_lock_exceeds_its_budget`,
//! `a_tripped_store_refuses_the_next_call_without_spawning`,
//! `a_dropped_caller_still_trips_the_breaker_at_the_budget`.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use anyhow::Context as _;
use hnsw_rs::prelude::{DistCosine, Hnsw};
use parking_lot::{MutexGuard, RwLockReadGuard};
use thiserror::Error;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use super::{HnswStore, HnswStoreError, Result};
use crate::memory_core::timeouts::write_txn_deadline;

/// Environment variable overriding [`default_op_budget`], in whole seconds.
pub const OP_BUDGET_ENV: &str = "TRUSTY_HNSW_OP_BUDGET_SECS";

/// The default budget is this many redb write-transaction deadlines (#9487).
///
/// Why: an upsert's `begin_write` can legitimately wait out one deadlined
/// transaction (an unalias, a compaction) before its own runs; a budget equal
/// to that deadline would trip the sticky breaker on a healthy store.
pub const OP_BUDGET_TXN_DEADLINES: u32 = 2;

/// Why an HNSW operation was refused (#9487).
///
/// Why: a caller has to tell "this palace's vector store is stuck" apart from
/// an ordinary redb or encoding failure, so a remember can fail closed and the
/// health surface can name the palace.
/// What: carried by [`HnswStoreError::OpBudget`]; recover it from an
/// `anyhow::Error` chain with [`op_budget_error`].
/// Test: `an_upsert_behind_a_parked_graph_lock_exceeds_its_budget`,
/// `a_tripped_store_refuses_the_next_call_without_spawning`.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum OpBudgetError {
    /// The operation did not finish inside its budget; the breaker is now tripped.
    #[error(
        "hnsw {op} on palace '{palace}' exceeded its {budget:?} budget \
         (TRUSTY_HNSW_OP_BUDGET_SECS); this vector store now refuses every \
         operation until the palace is reopened or the daemon restarts"
    )]
    BudgetExceeded {
        op: &'static str,
        palace: String,
        budget: Duration,
    },
    /// The breaker was already tripped; the operation was not attempted.
    #[error(
        "hnsw vector store for palace '{palace}' is wedged: an earlier operation \
         exceeded its budget; reopen the palace or restart the daemon"
    )]
    Wedged { palace: String },
}

/// Find the [`OpBudgetError`] inside an `anyhow` chain, if one caused it.
///
/// Why: the vector layer and the remember pipeline wrap store errors in
/// context, so callers need one place that sees through it.
/// What: downcasts to [`HnswStoreError`] (anyhow searches context layers) and
/// returns its `OpBudget` payload.
/// Test: `a_remember_whose_vector_step_exceeds_its_budget_commits_nothing`.
pub fn op_budget_error(err: &anyhow::Error) -> Option<&OpBudgetError> {
    match err.downcast_ref::<HnswStoreError>()? {
        HnswStoreError::OpBudget(e) => Some(e),
        _ => None,
    }
}

/// The default budget for a given redb write-transaction deadline (#9487).
/// Test: `the_default_budget_outlasts_the_write_txn_deadline`.
pub fn op_budget_for_txn_deadline(txn_deadline: Duration) -> Duration {
    txn_deadline.saturating_mul(OP_BUDGET_TXN_DEADLINES)
}

/// Budget for one HNSW operation when [`OP_BUDGET_ENV`] is unset or malformed:
/// [`OP_BUDGET_TXN_DEADLINES`] times the live `write_txn_deadline` (#9487).
pub fn default_op_budget() -> Duration {
    op_budget_for_txn_deadline(write_txn_deadline())
}

/// Parse a raw [`OP_BUDGET_ENV`] value; a malformed or zero value warns and
/// yields `default`.
/// Test: `a_malformed_budget_falls_back_to_the_default`.
pub(super) fn budget_from(raw: Option<&str>, default: Duration) -> Duration {
    let Some(raw) = raw else {
        return default;
    };
    match raw.trim().parse::<u64>() {
        Ok(secs) if secs > 0 => Duration::from_secs(secs),
        _ => {
            tracing::warn!(
                value = raw,
                default_secs = default.as_secs(),
                "#9487: {OP_BUDGET_ENV} is not a positive whole number of seconds; \
                 using the default"
            );
            default
        }
    }
}

/// The process-wide budget, read from the environment once.
fn configured_budget() -> Duration {
    static BUDGET: OnceLock<Duration> = OnceLock::new();
    *BUDGET.get_or_init(|| {
        budget_from(
            std::env::var(OP_BUDGET_ENV).ok().as_deref(),
            default_op_budget(),
        )
    })
}

/// Abandoned HNSW operations whose blocking task has not finished yet.
static ABANDONED_IN_FLIGHT: AtomicU64 = AtomicU64::new(0);

/// Abandoned HNSW operations still running, across every store in the process.
///
/// Why (#9487): a detached operation holds an `Arc<HnswStore>`, not the
/// palace handle, so idle eviction can drop a tripped palace from the cache
/// while its stuck operation still holds a thread and a lock.
/// What: rises when a watcher gives up on an operation at its budget, and
/// falls when that operation's blocking task finally returns.
/// Test: `a_dropped_caller_still_trips_the_breaker_at_the_budget`;
/// trusty-memory's `health_stays_not_ok_after_a_palace_with_an_abandoned_op_is_evicted`.
pub fn abandoned_ops_in_flight() -> u64 {
    ABANDONED_IN_FLIGHT.load(Ordering::Acquire)
}

/// Holds one abandoned operation in [`abandoned_ops_in_flight`] until dropped.
struct InFlight;

impl InFlight {
    fn enter() -> Self {
        ABANDONED_IN_FLIGHT.fetch_add(1, Ordering::AcqRel);
        Self
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        ABANDONED_IN_FLIGHT.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Per-store budget, breaker and counters (#9487).
///
/// Why: one wedged store must not hold threads or locks for every later call,
/// and an operator needs to see that it happened and how much work it left
/// half-done.
/// What: `tripped` is sticky; an inner lock bound or the watcher sets it.
/// `abandoned_ops` counts operations given up on at the budget, once each,
/// and only the [`HnswStore::run_bounded`] watcher counts; a vector one of
/// them writes later is an orphan left for `compact_orphans`. `spawned_ops`
/// counts blocking tasks started by `run_bounded`, so a test can prove a
/// refused call spawned none.
/// Test: `a_tripped_store_refuses_the_next_call_without_spawning`,
/// `a_hung_closure_exceeds_the_join_budget_without_any_lock`.
#[derive(Debug)]
pub struct OpBreaker {
    budget_ms: AtomicU64,
    tripped: AtomicBool,
    abandoned: AtomicU64,
    spawned: AtomicU64,
}

impl OpBreaker {
    /// A closed breaker with the process-wide budget.
    pub(super) fn from_env() -> Self {
        let budget_ms = u64::try_from(configured_budget().as_millis()).unwrap_or(u64::MAX);
        Self {
            budget_ms: AtomicU64::new(budget_ms),
            tripped: AtomicBool::new(false),
            abandoned: AtomicU64::new(0),
            spawned: AtomicU64::new(0),
        }
    }

    /// The budget each bounded operation on this store gets.
    pub fn budget(&self) -> Duration {
        Duration::from_millis(self.budget_ms.load(Ordering::Relaxed))
    }

    /// True once any operation on this store exceeded its budget.
    pub fn is_tripped(&self) -> bool {
        self.tripped.load(Ordering::Acquire)
    }

    /// Operations abandoned at the budget since this store opened.
    pub fn abandoned_ops(&self) -> u64 {
        self.abandoned.load(Ordering::Relaxed)
    }

    /// Blocking tasks [`HnswStore::run_bounded`] has spawned on this store.
    pub fn spawned_ops(&self) -> u64 {
        self.spawned.load(Ordering::Relaxed)
    }

    /// Shrink the budget so a test can reach it in milliseconds.
    #[cfg(any(test, feature = "embedder-test-support"))]
    pub fn set_budget_for_test(&self, budget: Duration) {
        let ms = u64::try_from(budget.as_millis()).unwrap_or(u64::MAX);
        self.budget_ms.store(ms, Ordering::Relaxed);
    }

    /// Trip the breaker without a stuck operation, for consumers' tests.
    #[cfg(any(test, feature = "embedder-test-support"))]
    pub fn trip_for_test(&self) {
        self.tripped.store(true, Ordering::Release);
    }

    /// Refuse with `Wedged` when tripped; never waits.
    fn check(&self, palace: &str) -> std::result::Result<(), OpBudgetError> {
        if self.is_tripped() {
            return Err(OpBudgetError::Wedged {
                palace: palace.to_string(),
            });
        }
        Ok(())
    }

    /// Trip the breaker and name the overrun; counts nothing (#9487 F2).
    fn exceeded(&self, op: &'static str, palace: &str) -> OpBudgetError {
        self.tripped.store(true, Ordering::Release);
        OpBudgetError::BudgetExceeded {
            op,
            palace: palace.to_string(),
            budget: self.budget(),
        }
    }

    /// Trip, count the abandoned operation once, and log it. Watcher only.
    fn abandon(&self, op: &'static str, palace: &str) -> OpBudgetError {
        let err = self.exceeded(op, palace);
        let abandoned = self.abandoned.fetch_add(1, Ordering::Relaxed) + 1;
        tracing::error!(
            palace,
            op,
            budget_ms = self.budget().as_millis(),
            abandoned_ops = abandoned,
            "#9487: hnsw operation exceeded its budget; the vector store is now \
             wedged and refuses every operation until the palace is reopened"
        );
        err
    }
}

impl HnswStore {
    /// This store's budget, breaker and counters (#9487).
    pub fn op_breaker(&self) -> &OpBreaker {
        &self.breaker
    }

    /// Take the insert gate within the budget, or trip the breaker (#9487).
    pub(super) fn bounded_gate(&self, op: &'static str) -> Result<MutexGuard<'_, ()>> {
        self.breaker.check(&self.palace)?;
        self.insert_gate
            .try_lock_for(self.breaker.budget())
            .ok_or_else(|| self.breaker.exceeded(op, &self.palace).into())
    }

    /// Take a shared graph guard within the budget, or trip the breaker (#9487).
    pub(super) fn bounded_graph(
        &self,
        op: &'static str,
    ) -> Result<RwLockReadGuard<'_, Hnsw<'static, f32, DistCosine>>> {
        self.breaker.check(&self.palace)?;
        self.index
            .try_read_for(self.breaker.budget())
            .ok_or_else(|| self.breaker.exceeded(op, &self.palace).into())
    }

    /// Run `work` on the blocking pool, bounded by the budget (#9487).
    ///
    /// Why: a `spawn_blocking` task cannot be cancelled, so an unbounded join
    /// hands a stuck operation's wait to every async caller. The budget clock
    /// must outlive the caller: an HTTP client that disconnects, or the
    /// remember pipeline ceiling, drops the caller's future early.
    /// What: a tripped store returns `Wedged` before spawning anything.
    /// Otherwise counts the spawn and hands the task to a spawned watcher that
    /// owns its lifetime (`watch_op`); the caller only awaits the reply.
    /// Test: `an_upsert_behind_a_parked_graph_lock_exceeds_its_budget`,
    /// `a_tripped_store_refuses_the_next_call_without_spawning`,
    /// `a_dropped_caller_still_trips_the_breaker_at_the_budget`,
    /// `a_hung_closure_exceeds_the_join_budget_without_any_lock`.
    pub async fn run_bounded<T, F>(&self, op: &'static str, work: F) -> anyhow::Result<T>
    where
        T: Send + 'static,
        F: FnOnce() -> anyhow::Result<T> + Send + 'static,
    {
        self.breaker
            .check(&self.palace)
            .map_err(HnswStoreError::from)?;
        self.breaker.spawned.fetch_add(1, Ordering::Relaxed);
        let task = tokio::task::spawn_blocking(work);
        let (reply, outcome) = oneshot::channel();
        // #9487 F1: the watcher, not the caller, owns the budget clock.
        tokio::spawn(watch_op(
            task,
            Arc::clone(&self.breaker),
            Arc::clone(&self.palace),
            op,
            reply,
        ));
        outcome
            .await
            .with_context(|| format!("hnsw {op} watcher stopped before it replied"))?
    }
}

/// Own one bounded operation from spawn to finish (#9487 F1, F2, F5).
///
/// Why: a budget clock inside the caller's future stops when that future is
/// dropped, so the stuck task was detached uncounted and health stayed ok.
/// What: joins `task` within the budget. On time, replies with its result,
/// counting it once if an inner lock bound gave up. On timeout, counts and
/// logs the abandonment once, holds [`abandoned_ops_in_flight`] up, replies
/// `BudgetExceeded`, then waits for the task so the gauge falls when it
/// really ends. A dropped caller only makes the reply go nowhere.
/// Test: `a_dropped_caller_still_trips_the_breaker_at_the_budget`.
async fn watch_op<T: Send + 'static>(
    mut task: JoinHandle<anyhow::Result<T>>,
    breaker: Arc<OpBreaker>,
    palace: Arc<str>,
    op: &'static str,
    reply: oneshot::Sender<anyhow::Result<T>>,
) {
    match tokio::time::timeout(breaker.budget(), &mut task).await {
        Ok(joined) => {
            let outcome = joined
                .with_context(|| format!("hnsw {op} task panicked"))
                .and_then(|r| r);
            if let Err(e) = &outcome
                && matches!(
                    op_budget_error(e),
                    Some(OpBudgetError::BudgetExceeded { .. })
                )
            {
                breaker.abandon(op, &palace);
            }
            let _ = reply.send(outcome);
        }
        Err(_) => {
            let _in_flight = InFlight::enter();
            let err = breaker.abandon(op, &palace);
            let _ = reply.send(Err(HnswStoreError::from(err).into()));
            // A late result is discarded; its vector is an orphan for compaction.
            let _ = task.await;
        }
    }
}

/// Test seams that park the locks a bounded operation waits on.
#[cfg(test)]
impl HnswStore {
    /// Hold the graph lock exclusively, so every bounded graph read waits.
    pub(crate) fn park_graph_for_test(
        &self,
    ) -> parking_lot::RwLockWriteGuard<'_, Hnsw<'static, f32, DistCosine>> {
        self.index.write()
    }

    /// Hold the insert gate, so every bounded upsert waits on it.
    pub(crate) fn park_insert_gate_for_test(&self) -> MutexGuard<'_, ()> {
        self.insert_gate.lock()
    }
}

#[cfg(test)]
#[path = "op_budget_tests.rs"]
mod tests;
