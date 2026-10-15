//! CompactionGuard RAII type for the dream cycle.
//!
//! Why: Extracted from dream.rs to keep each file under the 500-SLOC cap
//! (#607). The guard ensures the `is_compacting` flag is always cleared on
//! exit, even on early errors or panics.
//! What: `CompactionGuard` claims `is_compacting` on construction and clears
//! it on drop. #9172: the claim is exclusive, so one palace handle runs one
//! dream cycle at a time.
//! Test: `dream::tests::dream_cycle_toggles_is_compacting`,
//! `dedup_survivor_tests::a_second_dream_cycle_on_a_dreaming_palace_loses_no_text`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// RAII guard that toggles a palace's `is_compacting` flag for the lifetime
/// of a dream cycle.
///
/// Why: A plain `flag.store(true)` at the top of `dream_cycle` and
/// `flag.store(false)` at the bottom leaks `true` if any pass returns an
/// error or panics, leaving the dashboard stuck on "dreaming". A Drop guard
/// guarantees the flag clears on every exit path.
/// What: Flips the supplied `AtomicBool` from `false` to `true` on
/// construction and back to `false` on drop. The dashboard reads the flag
/// with `Relaxed`; only the claim itself needs to be a single atomic step.
/// Test: `dream::tests::dream_cycle_toggles_is_compacting`.
pub(crate) struct CompactionGuard {
    pub(crate) flag: Arc<AtomicBool>,
}

impl CompactionGuard {
    /// Claim the flag for one dream cycle, or `None` when a cycle holds it.
    ///
    /// Why (#9172): two cycles on one palace interleaved their dedup passes,
    /// and one deleted a drawer the other had just merged text into.
    /// What: one `compare_exchange(false, true)`, so of two racing callers
    /// exactly one gets the guard. The caller skips its cycle on `None`; it
    /// does not wait for the running one.
    /// Test: `dream::tests::dream_cycle_toggles_is_compacting`,
    /// `dedup_survivor_tests::a_second_dream_cycle_on_a_dreaming_palace_loses_no_text`.
    pub(crate) fn try_claim(flag: Arc<AtomicBool>) -> Option<Self> {
        flag.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| Self { flag })
    }
}

impl Drop for CompactionGuard {
    fn drop(&mut self) {
        self.flag.store(false, Ordering::Release);
    }
}
