//! CompactionGuard RAII type for the dream cycle.
//!
//! Why: Extracted from dream.rs to keep each file under the 500-SLOC cap
//! (#607). The guard ensures the `is_compacting` flag is always cleared on
//! exit, even on early errors or panics.
//! What: `CompactionGuard` sets `is_compacting = true` on construction and
//! clears it on drop.
//! Test: `dream::tests::dream_cycle_toggles_is_compacting`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// RAII guard that toggles a palace's `is_compacting` flag for the lifetime
/// of a dream cycle.
///
/// Why: A plain `flag.store(true)` at the top of `dream_cycle` and
/// `flag.store(false)` at the bottom leaks `true` if any pass returns an
/// error or panics, leaving the dashboard stuck on "dreaming". A Drop guard
/// guarantees the flag clears on every exit path.
/// What: Stores `true` in the supplied `AtomicBool` on construction and
/// `false` on drop, both with `Relaxed` ordering (the dashboard read path
/// uses the same ordering — exact happens-before semantics across tasks are
/// not required for a UI indicator).
/// Test: `dream::tests::dream_cycle_toggles_is_compacting`.
pub(crate) struct CompactionGuard {
    pub(crate) flag: Arc<AtomicBool>,
}

impl CompactionGuard {
    /// Why: Centralises the "set flag, then return guard" pattern so callers
    /// can't forget the drop side.
    /// What: Stores `true` and returns the guard.
    /// Test: `dream::tests::dream_cycle_toggles_is_compacting`.
    pub(crate) fn new(flag: Arc<AtomicBool>) -> Self {
        flag.store(true, Ordering::Relaxed);
        Self { flag }
    }
}

impl Drop for CompactionGuard {
    fn drop(&mut self) {
        self.flag.store(false, Ordering::Relaxed);
    }
}
