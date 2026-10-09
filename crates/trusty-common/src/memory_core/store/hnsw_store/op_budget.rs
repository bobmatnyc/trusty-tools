//! #9487 AC4: a time budget and a fail-closed breaker on HNSW operations.
//!
//! Why: a stuck HNSW operation — a graph lock never released, an insert that
//! never returns — blocked its caller forever, and every caller queued behind
//! it. #9487's deadlock is fixed; AC4 asks that the next such defect cost one
//! palace's vector store a typed error instead of wedging the palace for good.
//! What: `upsert` and `search` take the insert gate and the graph lock through
//! `HnswStore::bounded_gate` / `HnswStore::bounded_graph`, each bounded by
//! the store's budget (`TRUSTY_HNSW_OP_BUDGET_SECS`, default 30 s). The vector
//! layer runs its blocking work through [`HnswStore::run_bounded`], which joins
//! the `spawn_blocking` task with the same budget. Either timeout returns
//! [`OpBudgetError::BudgetExceeded`] and trips the store's [`OpBreaker`]. From
//! then on every bounded operation returns [`OpBudgetError::Wedged`] without
//! spawning a thread or waiting on a lock. The breaker never resets: it lives
//! on the `HnswStore`, so it clears only when the palace is reopened (a new
//! store) or the process restarts.
//! Test: `an_upsert_behind_a_parked_graph_lock_exceeds_its_budget`,
//! `a_tripped_store_refuses_the_next_call_without_spawning`.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use anyhow::Context as _;
use hnsw_rs::prelude::{DistCosine, Hnsw};
use parking_lot::{MutexGuard, RwLockReadGuard};
use thiserror::Error;

use super::{HnswStore, HnswStoreError, Result};

/// Environment variable overriding [`DEFAULT_OP_BUDGET`], in whole seconds.
pub const OP_BUDGET_ENV: &str = "TRUSTY_HNSW_OP_BUDGET_SECS";

/// Budget for one HNSW operation when [`OP_BUDGET_ENV`] is unset or malformed.
pub const DEFAULT_OP_BUDGET: Duration = Duration::from_secs(30);

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

/// Parse a raw [`OP_BUDGET_ENV`] value; a malformed or zero value warns and
/// yields [`DEFAULT_OP_BUDGET`].
/// Test: `a_malformed_budget_falls_back_to_the_default`.
pub(super) fn budget_from(raw: Option<&str>) -> Duration {
    let Some(raw) = raw else {
        return DEFAULT_OP_BUDGET;
    };
    match raw.trim().parse::<u64>() {
        Ok(secs) if secs > 0 => Duration::from_secs(secs),
        _ => {
            tracing::warn!(
                value = raw,
                default_secs = DEFAULT_OP_BUDGET.as_secs(),
                "#9487: {OP_BUDGET_ENV} is not a positive whole number of seconds; \
                 using the default"
            );
            DEFAULT_OP_BUDGET
        }
    }
}

/// The process-wide budget, read from the environment once.
fn configured_budget() -> Duration {
    static BUDGET: OnceLock<Duration> = OnceLock::new();
    *BUDGET.get_or_init(|| budget_from(std::env::var(OP_BUDGET_ENV).ok().as_deref()))
}

/// Per-store budget, breaker and counters (#9487).
///
/// Why: one wedged store must not hold threads or locks for every later call,
/// and an operator needs to see that it happened and how much work it left
/// half-done.
/// What: `tripped` is sticky. `abandoned_ops` counts operations given up on at
/// the budget; a vector one of them writes later is an orphan left for
/// `compact_orphans`. `spawned_ops` counts blocking tasks started by
/// [`HnswStore::run_bounded`], so a test can prove a refused call spawned none.
/// Test: `a_tripped_store_refuses_the_next_call_without_spawning`.
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

    /// Trip the breaker, count the abandoned operation, and name it.
    fn trip(&self, op: &'static str, palace: &str) -> OpBudgetError {
        self.tripped.store(true, Ordering::Release);
        let abandoned = self.abandoned.fetch_add(1, Ordering::Relaxed) + 1;
        let budget = self.budget();
        tracing::error!(
            palace,
            op,
            budget_ms = budget.as_millis(),
            abandoned_ops = abandoned,
            "#9487: hnsw operation exceeded its budget; the vector store is now \
             wedged and refuses every operation until the palace is reopened"
        );
        OpBudgetError::BudgetExceeded {
            op,
            palace: palace.to_string(),
            budget,
        }
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
            .ok_or_else(|| self.breaker.trip(op, &self.palace).into())
    }

    /// Take a shared graph guard within the budget, or trip the breaker (#9487).
    pub(super) fn bounded_graph(
        &self,
        op: &'static str,
    ) -> Result<RwLockReadGuard<'_, Hnsw<'static, f32, DistCosine>>> {
        self.breaker.check(&self.palace)?;
        self.index
            .try_read_for(self.breaker.budget())
            .ok_or_else(|| self.breaker.trip(op, &self.palace).into())
    }

    /// Run `work` on the blocking pool, joined within the budget (#9487).
    ///
    /// Why: a `spawn_blocking` task cannot be cancelled, so an unbounded join
    /// hands a stuck operation's wait to every async caller.
    /// What: a tripped store returns `Wedged` before spawning anything.
    /// Otherwise counts the spawn and awaits the join under the budget; on
    /// timeout the task is detached (its late vector is an orphan for
    /// `compact_orphans`), the breaker trips, and `BudgetExceeded` returns.
    /// Test: `an_upsert_behind_a_parked_graph_lock_exceeds_its_budget`,
    /// `a_tripped_store_refuses_the_next_call_without_spawning`.
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
        match tokio::time::timeout(self.breaker.budget(), task).await {
            Ok(joined) => joined.with_context(|| format!("hnsw {op} task panicked"))?,
            Err(_) => Err(HnswStoreError::from(self.breaker.trip(op, &self.palace)).into()),
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
