//! Incremental per-session working-context floor (issue #3948).
//!
//! Why: `session.get_context_budget` is a monitoring path a client polls, and
//! it used to rebuild this session's floor by scanning the multi-session
//! `compression.jsonl` on every call — a read whose cost grows with the
//! machine's whole compression history. Sessions are process-local
//! (`SessionRegistry` holds them in memory and nothing rehydrates them), so
//! an aggregate with the same lifetime as the session it describes is enough.
//! What: every authoritative cadence measurement records one sample; the
//! minimum percentage is retained and the count saturates rather than wraps.
//! Test: `registry_tests::context_floor_tracks_every_measurement`,
//! `registry_tests::context_floor_is_session_scoped`,
//! `registry_tests::context_floor_saturates_sample_count`,
//! `registry_tests::context_floor_after_restart_is_absent_not_wrong`.

/// One session's retained working-context floor and how many measurements
/// produced it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct ContextFloorState {
    pub(super) low_water_mark: Option<u8>,
    pub(super) sample_count: usize,
}

impl ContextFloorState {
    /// Fold one measurement into the floor.
    pub(super) fn record(&mut self, working_context_pct: u8) {
        self.low_water_mark = Some(self.low_water_mark.map_or(working_context_pct, |current| {
            current.min(working_context_pct)
        }));
        self.sample_count = self.sample_count.saturating_add(1);
    }
}
