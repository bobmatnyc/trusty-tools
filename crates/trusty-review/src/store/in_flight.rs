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
//! Test: `pr_guard_blocks_second`, `pr_guard_released_on_drop`,
//! `sha_guard_independent_of_pr`, `different_pr_not_blocked`.

use std::sync::Arc;

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
