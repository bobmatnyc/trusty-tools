//! `SessionRegistry::get_transcript` (#2058), split out of `registry.rs` for
//! the 500-SLOC cap — a child module of `registry` (declared via
//! `#[path = ...] mod transcript_ops;`), so it keeps full access to
//! `SessionRegistry`'s private `lock` helper and `SessionEntry`'s private
//! fields exactly as if the method were still defined there.
//!
//! Why: #2425 added `SessionEntry::memory_durability`/`memory_outcomes` and the
//! reconciler module declaration, which pushed `registry.rs` over its cap. This
//! projection is the natural thing to move: it reads `SessionEntry` and builds
//! a DTO, touching none of the registry's own state machine.
//! What: [`SessionRegistry::get_transcript`].
//! Test: `registry_tests::get_transcript_*`.

use super::*;
use crate::session::transcript::TranscriptRecord;

impl SessionRegistry {
    /// `session.get_transcript`: fetch the stored run record for a session
    /// (#2058).
    ///
    /// Why: [`Self::set_run_outcome`] populates the storage; this is its read
    /// counterpart — the M1 cut-line's "inspect transcript" verb. A never-run
    /// session is a valid, empty transcript, not an error: `SessionEntry`'s
    /// `transcript`/`usage`/`cost_usd` fields already default to
    /// empty/zero/`None` at `create` time, so returning them unconditionally
    /// (once the session itself is confirmed to exist) is correct with no
    /// extra branching.
    /// What: `Err(session_not_found)` if `id` is unknown; otherwise a clone of
    /// whatever is currently stored, wrapped in a self-describing
    /// [`TranscriptRecord`]. Never recomputes cost, usage, or durability —
    /// exposes exactly what [`Self::set_run_outcome`] and (#2425)
    /// [`Self::record_memory_durability`] last stored. `compaction_events`
    /// (#2349) reads `entry.pm_transcript`'s own cumulative counter — `0` when
    /// the session has never run (`pm_transcript` still `None`).
    /// Test: `registry_tests::get_transcript_returns_stored_record`,
    /// `registry_tests::get_transcript_on_never_run_session_is_empty`,
    /// `registry_tests::get_transcript_unknown_session_errors`,
    /// `registry_tests::get_transcript_reports_compaction_events`,
    /// `registry_tests::get_transcript_round_trips_goal_state`,
    /// `registry_tests::memory_durability_retains_counts_resets_streak_and_warns_at_one_and_three`.
    pub fn get_transcript(&self, id: &str) -> Result<TranscriptRecord, RpcError> {
        let sessions = self.lock();
        let entry = sessions
            .get(id)
            .ok_or_else(|| RpcError::session_not_found(id))?;
        Ok(TranscriptRecord {
            session_id: id.to_string(),
            turns: entry.transcript.clone(),
            usage: entry.usage,
            cost_usd: entry.cost_usd,
            mode: entry.session.mode,
            compaction_events: entry
                .pm_transcript
                .as_ref()
                .map(Transcript::compaction_events)
                .unwrap_or(0),
            goals: goal_ops::goal_records(entry),
            memory_durability: entry.memory_durability.clone(),
        })
    }
}
