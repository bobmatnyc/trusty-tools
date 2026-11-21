//! Measured open cost per palace, and the idle-evict exemption it drives (#11613).
//!
//! Why: after an idle eviction the next call re-opens its palace inside the
//! request. A large palace (8.1k vectors in the report) re-opens slower than
//! the client's 5 s UDS exchange, so the client gave up with a broken pipe
//! while the daemon finished the call. The client has no "opening" signal it
//! honours, so the daemon must answer inside the budget; the only way to do
//! that for a palace whose open alone exceeds it is not to evict it.
//! What: [`ReopenCosts`] keeps the last measured open duration of each palace
//! the registry opened. On a registry armed with
//! `PalaceRegistry::with_reopen_pin` (the daemon's), `evict_idle` skips a
//! palace whose last open took at least [`SLOW_REOPEN`]. An unarmed registry
//! (CLI, stdio, tests) and a palace never measured stay evictable, as before.
//! Test: `registry_tests::first_call_after_idle_sweep_does_not_wait_on_a_slow_reopen`,
//! `registry_tests::a_fast_open_palace_is_still_evicted_and_a_stalled_reopen_errors`.

use std::time::Duration;

use dashmap::DashMap;

use crate::engine::palace::PalaceId;

/// The open duration at which a palace stops being idle-evictable (#11613).
///
/// Why: the client exchange budget is 5 s and a measured open is taken under
/// whatever load the host had then. The reporter's host ran at load averages
/// of 8 to 20, which stretches an open several-fold, so the margin is wide.
/// What: 1 s. Compared with `>=` against the last measured open.
pub(crate) const SLOW_REOPEN: Duration = Duration::from_secs(1);

/// The last measured open duration of each palace, by id, and the pin
/// threshold; `None` never pins (#11613).
#[derive(Debug, Default)]
pub(crate) struct ReopenCosts(DashMap<PalaceId, Duration>, Option<Duration>);

impl ReopenCosts {
    /// A table that pins a palace whose last open took at least `pin`.
    pub(crate) fn pinning_at(pin: Duration) -> Self {
        Self(DashMap::new(), Some(pin))
    }

    /// Record how long the latest open of `id` took; replaces the prior value.
    pub(crate) fn record(&self, id: &PalaceId, took: Duration) {
        self.0.insert(id.clone(), took);
    }

    /// Whether this table pins, and `id`'s last open took at least the pin.
    pub(crate) fn too_slow_to_evict(&self, id: &PalaceId) -> bool {
        self.1
            .is_some_and(|pin| self.0.get(id).is_some_and(|took| *took >= pin))
    }

    /// Drop the measurement of a removed palace.
    pub(crate) fn forget(&self, id: &PalaceId) {
        self.0.remove(id);
    }
}
