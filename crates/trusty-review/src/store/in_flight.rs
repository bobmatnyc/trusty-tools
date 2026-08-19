//! In-process in-flight guard (issue #582 work-item c).
//!
//! Why: the durable redb dedup store is cross-process but commits at review
//! *start*; within a single process, concurrent webhook deliveries for the same
//! PR can race between "check store" and "write claim".  An in-memory guard
//! mirrors code-intelligence's `_PR_IN_FLIGHT`/`_IN_FLIGHT` sets to drop
//! duplicate concurrent runs cheaply before any I/O.
//!
//! What: `InFlightRegistry` holds two concurrent sets — one keyed by
//! `(owner,repo,pr)` (active from request receipt, before the SHA is known) and
//! one keyed by `(owner,repo,pr,sha)`.  `try_acquire_*` insert-if-absent and
//! return an RAII `InFlightGuard` that removes the key on drop, so the slot is
//! always released even if the review task panics.
//!
//! `InFlightCountGuard` applies the same RAII rule to the plain `AtomicU64`
//! counter `GET /status` reports (#5020): the decrement runs on drop, so it
//! survives a client disconnect (which drops the handler future mid-`await`)
//! and a panic unwind alike.
//!
//! Test: `pr_guard_blocks_second`, `pr_guard_released_on_drop`,
//! `sha_guard_independent_of_pr`, `different_pr_not_blocked`,
//! `count_guard_decrements_on_drop`, `count_guard_decrements_on_unwind`.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use dashmap::DashSet;

/// Concurrent in-flight registry shared across handler tasks.
///
/// Why: a single shared registry (behind `Arc`) lets every spawned review task
/// coordinate without a global `Mutex`; `DashSet` gives lock-free insert/remove.
/// What: two sets — PR-level (pre-SHA) and SHA-level — each holding composite
/// string keys.
/// Test: all module tests construct one and exercise the guards.
#[derive(Debug, Default, Clone)]
pub struct InFlightRegistry {
    /// Keys `"{owner}/{repo}/{pr}"` — active from request receipt.
    pr_keys: Arc<DashSet<String>>,
    /// Keys `"{owner}/{repo}/{pr}/{sha}"` — active once the head SHA is known.
    sha_keys: Arc<DashSet<String>>,
}

impl InFlightRegistry {
    /// Create an empty registry.
    ///
    /// Why: the service builds one at startup and clones the `Arc`-backed handle
    /// into `AppState`.
    /// What: returns a registry with two empty concurrent sets.
    /// Test: used by all tests.
    pub fn new() -> Self {
        Self::default()
    }

    /// Try to acquire the PR-level (pre-SHA) in-flight slot.
    ///
    /// Why: a webhook delivery should claim the PR slot the moment it arrives,
    /// before the head SHA is resolved, so two near-simultaneous deliveries for
    /// the same PR do not both proceed.
    /// What: inserts `"{owner}/{repo}/{pr}"`; returns `Some(guard)` if the slot
    /// was free, `None` if a review for this PR is already in flight.
    /// Test: `pr_guard_blocks_second`, `different_pr_not_blocked`.
    pub fn try_acquire_pr(&self, owner: &str, repo: &str, pr: u64) -> Option<InFlightGuard> {
        let key = format!("{owner}/{repo}/{pr}");
        if self.pr_keys.insert(key.clone()) {
            Some(InFlightGuard {
                set: Arc::clone(&self.pr_keys),
                key,
            })
        } else {
            None
        }
    }

    /// Try to acquire the SHA-level in-flight slot.
    ///
    /// Why: once the head SHA is known, a finer-grained guard prevents duplicate
    /// runs for the exact same commit even across different PR-slot lifetimes.
    /// What: inserts `"{owner}/{repo}/{pr}/{sha}"`; returns `Some(guard)` if free,
    /// `None` if already in flight.
    /// Test: `sha_guard_independent_of_pr`.
    pub fn try_acquire_sha(
        &self,
        owner: &str,
        repo: &str,
        pr: u64,
        sha: &str,
    ) -> Option<InFlightGuard> {
        let key = format!("{owner}/{repo}/{pr}/{sha}");
        if self.sha_keys.insert(key.clone()) {
            Some(InFlightGuard {
                set: Arc::clone(&self.sha_keys),
                key,
            })
        } else {
            None
        }
    }
}

/// RAII guard that releases an in-flight slot on drop.
///
/// Why: tying release to `Drop` guarantees the slot is freed on every exit path
/// — normal completion, early return, or panic unwind — so a crashed review
/// never leaves a PR permanently blocked.
/// What: remembers its set handle and key; `Drop` removes the key.
/// Test: `pr_guard_released_on_drop`.
#[derive(Debug)]
pub struct InFlightGuard {
    set: Arc<DashSet<String>>,
    key: String,
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.set.remove(&self.key);
    }
}

/// RAII guard around a plain in-flight *counter* (#5020).
///
/// Why: `POST /review` raised `AppState::in_flight` and lowered it after the
/// `await`, with nothing between. A dropped handler future — which a client
/// disconnect or a consumer timeout produces against a long review — skipped
/// the decrement, so the counter only ever grew and `GET /status` reported a
/// load figure that never came back down. A live daemon read `in_flight: 9`
/// across a minute while reviews were both starting and finishing.
/// What: raises the counter on construction and lowers it in `Drop`, so every
/// exit path — return, cancellation, panic unwind — settles it. The decrement
/// saturates at zero so a stray extra drop cannot wrap the counter to `u64::MAX`
/// and turn an undercount into an absurd overcount.
/// Test: `count_guard_decrements_on_drop`, `count_guard_decrements_on_unwind`,
/// `count_guard_drop_saturates_at_zero`,
/// `review_handler_decrements_in_flight_when_client_disconnects`.
#[derive(Debug)]
pub struct InFlightCountGuard {
    counter: Arc<AtomicU64>,
}

impl InFlightCountGuard {
    /// Raise `counter` and hand back the guard that lowers it again.
    ///
    /// Why: making entry and exit one object is what removes the "did every
    /// path decrement?" question from the call site.
    /// What: `fetch_add(1)` now; the matching `fetch_sub` happens in `Drop`.
    /// Test: `count_guard_decrements_on_drop`.
    pub fn enter(counter: &Arc<AtomicU64>) -> Self {
        counter.fetch_add(1, Ordering::Relaxed);
        Self {
            counter: Arc::clone(counter),
        }
    }
}

impl Drop for InFlightCountGuard {
    fn drop(&mut self) {
        // Saturating: a counter that already read zero must stay zero rather
        // than wrap to u64::MAX.
        let _ = self
            .counter
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
                Some(n.saturating_sub(1))
            });
    }
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pr_guard_blocks_second() {
        let reg = InFlightRegistry::new();
        let g1 = reg.try_acquire_pr("acme", "backend", 42);
        assert!(g1.is_some(), "first PR acquire must succeed");
        let g2 = reg.try_acquire_pr("acme", "backend", 42);
        assert!(g2.is_none(), "second PR acquire must be blocked");
        drop(g1);
    }

    #[test]
    fn pr_guard_released_on_drop() {
        let reg = InFlightRegistry::new();
        {
            let _g = reg.try_acquire_pr("acme", "backend", 42);
        } // guard dropped here
        // Slot must be free again after the guard is dropped.
        assert!(
            reg.try_acquire_pr("acme", "backend", 42).is_some(),
            "slot must be reusable after drop"
        );
    }

    #[test]
    fn different_pr_not_blocked() {
        let reg = InFlightRegistry::new();
        let _g1 = reg.try_acquire_pr("acme", "backend", 42);
        // A different PR is independent.
        assert!(reg.try_acquire_pr("acme", "backend", 43).is_some());
        // A different repo is independent.
        assert!(reg.try_acquire_pr("acme", "frontend", 42).is_some());
    }

    /// The counter returns to its prior value when the guard is dropped.
    ///
    /// Why: this is the whole contract `POST /review` leaned on and did not
    /// have (#5020).
    /// What: enters twice, drops each guard, and reads the counter at each step.
    /// Test: this test.
    #[test]
    fn count_guard_decrements_on_drop() {
        let counter = Arc::new(AtomicU64::new(0));

        let first = InFlightCountGuard::enter(&counter);
        let second = InFlightCountGuard::enter(&counter);
        assert_eq!(counter.load(Ordering::Relaxed), 2);

        drop(second);
        assert_eq!(counter.load(Ordering::Relaxed), 1);
        drop(first);
        assert_eq!(counter.load(Ordering::Relaxed), 0);
    }

    /// REGRESSION (#5020): the decrement must run on a panic unwind, not only
    /// on a normal return.
    ///
    /// Why: the reported leak has two triggers — a dropped future and a panic
    /// inside `run_review`. A fix that only handled the normal path would leave
    /// the second one leaking, and this test is what catches that.
    /// What: panics while holding a guard, catches the unwind, and asserts the
    /// counter came back to zero.
    /// Test: this test.
    #[test]
    fn count_guard_decrements_on_unwind() {
        let counter = Arc::new(AtomicU64::new(0));
        let for_panic = Arc::clone(&counter);

        let caught = std::panic::catch_unwind(move || {
            let _guard = InFlightCountGuard::enter(&for_panic);
            panic!("the review task panicked mid-flight");
        });

        assert!(caught.is_err(), "the panic must have been caught");
        assert_eq!(
            counter.load(Ordering::Relaxed),
            0,
            "a panicking review must not leave the counter raised (#5020)"
        );
    }

    /// A drop against a zero counter must not wrap it to `u64::MAX`.
    ///
    /// Why: an undercount is a small lie; `in_flight: 18446744073709551615` is
    /// an operator-hostile one.
    /// What: forces the counter to zero underneath a live guard, then drops it.
    /// Test: this test.
    #[test]
    fn count_guard_drop_saturates_at_zero() {
        let counter = Arc::new(AtomicU64::new(0));
        let guard = InFlightCountGuard::enter(&counter);
        counter.store(0, Ordering::Relaxed);

        drop(guard);

        assert_eq!(counter.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn sha_guard_independent_of_pr() {
        let reg = InFlightRegistry::new();
        // Holding the PR slot does not block the SHA slot (different sets).
        let _pr = reg.try_acquire_pr("acme", "backend", 42);
        let sha = reg.try_acquire_sha("acme", "backend", 42, "sha-abc");
        assert!(sha.is_some(), "SHA slot is independent of PR slot");
        // Same SHA again is blocked.
        assert!(
            reg.try_acquire_sha("acme", "backend", 42, "sha-abc")
                .is_none()
        );
        // Different SHA is allowed.
        assert!(
            reg.try_acquire_sha("acme", "backend", 42, "sha-def")
                .is_some()
        );
    }
}
