//! In-flight registry of `HnswStore::upsert` / `search` calls (#9487).
//!
//! Why: during the #9487 wedge a `spawn_blocking` thread sat deadlocked inside
//! `hnsw_rs` while every async-side gauge read healthy. The #8314 pipeline
//! timeout drops the awaiting future, which releases `write_mutex` and the
//! worker-liveness guard, but cannot stop the blocking thread. Health then
//! reported `in_flight = 0` and no lock held. Only state owned by the blocking
//! call itself outlives the dropped future.
//! What: [`OpWatch`] records each op's kind and start time under a short
//! `parking_lot` mutex (never held across store work, and never poisoned). The
//! RAII [`OpGuard`] is created on the blocking thread inside the store call and
//! removes its entry when the call returns or unwinds. [`HnswStore::oldest_op`]
//! reads the longest-running entry. Ids are issued and stamped under the same
//! lock, so the smallest live id is the oldest op. The registry is an `Arc` so
//! a `PalaceRegistry` can keep a `Weak` to it past the palace's eviction: the
//! blocked thread holds the store, so the `Weak` upgrades until the call returns.
//! Test: `op_watch_tests.rs`.

use std::collections::BTreeMap;
use std::sync::{Arc, Weak};
use std::time::Instant;

use super::HnswStore;

/// Which store call an in-flight entry belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HnswOpKind {
    /// `HnswStore::upsert`.
    Upsert,
    /// `HnswStore::search`.
    Search,
}

/// The longest-running in-flight store call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HnswOp {
    /// Which call is running.
    pub kind: HnswOpKind,
    /// When the call registered, on its own blocking thread.
    pub since: Instant,
}

#[derive(Debug, Default)]
struct Live {
    next: u64,
    ops: BTreeMap<u64, HnswOp>,
}

/// The per-store registry; see the module docs.
#[derive(Debug, Default)]
pub(crate) struct OpWatch {
    live: parking_lot::Mutex<Live>,
    #[cfg(any(test, feature = "embedder-test-support"))]
    park: parking_lot::Mutex<Option<std::sync::Arc<OpPark>>>,
}

/// Removes its op from the registry on drop, including during unwind.
#[must_use = "the op is unregistered as soon as the guard drops"]
pub(super) struct OpGuard<'a> {
    watch: &'a OpWatch,
    id: u64,
}

impl Drop for OpGuard<'_> {
    fn drop(&mut self) {
        self.watch.live.lock().ops.remove(&self.id);
    }
}

impl OpWatch {
    /// Register an op of `kind` starting now; the guard unregisters it.
    ///
    /// What: under the test-support feature, an installed [`OpPark`] holds the
    /// calling thread here, after registration, so a test can observe a
    /// blocked op without a real deadlock.
    /// Test: `a_parked_search_is_reported_until_it_returns`.
    pub(super) fn begin(&self, kind: HnswOpKind) -> OpGuard<'_> {
        let id = {
            let mut live = self.live.lock();
            let id = live.next;
            live.next = live.next.wrapping_add(1);
            live.ops.insert(
                id,
                HnswOp {
                    kind,
                    since: Instant::now(),
                },
            );
            id
        };
        let guard = OpGuard { watch: self, id };
        #[cfg(any(test, feature = "embedder-test-support"))]
        {
            let park = self.park.lock().clone();
            if let Some(park) = park {
                park.hold();
            }
        }
        guard
    }

    /// The longest-running registered op, if any.
    pub(crate) fn oldest(&self) -> Option<HnswOp> {
        self.live.lock().ops.values().next().copied()
    }
}

impl HnswStore {
    /// The longest-running in-flight `upsert` or `search` on this store (#9487).
    ///
    /// Why: a call blocked inside `hnsw_rs` outlives the future that awaited
    /// it, so only the store can report it; see the module docs.
    /// What: `None` when no call is running. The read takes one short mutex
    /// that no store call holds while working, so it cannot block behind a
    /// wedged op, and it cannot fail.
    /// Test: `a_parked_search_is_reported_until_it_returns`,
    /// `a_dropped_future_leaves_its_blocking_op_registered`.
    pub fn oldest_op(&self) -> Option<HnswOp> {
        self.ops.oldest()
    }

    /// A `Weak` to this store's in-flight registry (#9487); it upgrades while
    /// the store lives, including after its palace handle is evicted.
    pub(crate) fn op_watch(&self) -> Weak<OpWatch> {
        Arc::downgrade(&self.ops)
    }

    /// Install (or clear, with `None`) a test hook that holds every later
    /// `upsert` / `search` after it registers, until released.
    #[cfg(any(test, feature = "embedder-test-support"))]
    pub fn set_op_park(&self, park: Option<std::sync::Arc<OpPark>>) {
        *self.ops.park.lock() = park;
    }
}

/// Test hook that holds a store call inside its registered section (#9487).
///
/// Why: a test must observe a blocked op deterministically, without
/// reproducing the `hnsw_rs` deadlock or sleeping.
/// What: [`Self::wait_entered`] returns once a call is held, bounded by a
/// timeout; [`Self::release`] lets every held and later call through.
/// Test: `a_parked_search_is_reported_until_it_returns`.
#[cfg(any(test, feature = "embedder-test-support"))]
#[derive(Debug, Default)]
pub struct OpPark {
    state: std::sync::Mutex<ParkState>,
    cv: std::sync::Condvar,
}

#[cfg(any(test, feature = "embedder-test-support"))]
#[derive(Debug, Default)]
struct ParkState {
    entered: usize,
    released: bool,
}

#[cfg(any(test, feature = "embedder-test-support"))]
impl OpPark {
    /// A new, unreleased park.
    pub fn new() -> std::sync::Arc<Self> {
        std::sync::Arc::default()
    }

    fn state(&self) -> std::sync::MutexGuard<'_, ParkState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn hold(&self) {
        let mut state = self.state();
        state.entered += 1;
        self.cv.notify_all();
        while !state.released {
            state = self
                .cv
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    /// Block until a call is held, or `timeout` passes; true when one is held.
    pub fn wait_entered(&self, timeout: std::time::Duration) -> bool {
        let state = self.state();
        let (state, _) = self
            .cv
            .wait_timeout_while(state, timeout, |s| s.entered == 0)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.entered > 0
    }

    /// Let every held and later call through.
    pub fn release(&self) {
        self.state().released = true;
        self.cv.notify_all();
    }
}

#[cfg(test)]
#[path = "op_watch_tests.rs"]
mod tests;
