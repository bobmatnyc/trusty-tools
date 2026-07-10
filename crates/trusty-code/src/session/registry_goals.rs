//! `SessionRegistry`'s #2350 operator-facing goal-slot plumbing:
//! `session.set_goal`/`session.clear_goal`/`session.get_goals`. Split out of
//! `registry.rs` for the same 500-SLOC-cap reason as `registry_events.rs`/
//! `registry_memory_sink.rs` — this is a child module of `registry`
//! (declared via `#[path = ...] mod goal_ops;`), so it shares full access to
//! `SessionRegistry`'s private `lock` helper and `SessionEntry`'s fields
//! exactly as if these methods were still defined in that file.
//!
//! Why: `agent_loop::goals::GoalSlots` already lets the PM's own model write
//! goals via the `set_goal`/`clear_goal` tools (#2347), each tagged
//! `GoalSource::Model`. An operator (human, or a future TUI/CLI) needs the
//! SAME shared slots reachable over `session.*` JSON-RPC, tagged
//! `GoalSource::Operator` — last-write-wins between the two sources is
//! implicit in `GoalSlots::set` simply overwriting whichever slot is named,
//! regardless of who wrote it last.
//! **Design decision (documented per the ticket):** the goal slots live on
//! `Transcript` (`SessionEntry.pm_transcript`), which is only seeded on a
//! session's FIRST `task.run` (`SessionRegistry::begin_pm_transcript`). A
//! session that has never run a task has no `Transcript` and therefore no
//! goal-slot storage to write into yet. Rather than adding a second,
//! parallel "pending goals" slot on `SessionEntry` that would need to be
//! applied at the first `begin_pm_transcript` call (a real design, but more
//! moving parts than this ticket's scope needs), `set_goal`/`clear_goal`
//! return a clear `invalid_argument` error naming the limitation. `get_goals`
//! takes the simpler read-side convention already established by
//! `get_transcript`: a session with no transcript yet is a valid EMPTY
//! result, not an error. Upgrading `set_goal` to a pending-goals path that
//! applies on the first run is documented future work, not needed here.
//! What: `set_goal`/`clear_goal` resolve `id`'s `pm_transcript`'s
//! `goals_handle()`, erroring `session_not_found` if the session itself is
//! unknown or `invalid_argument` if it has no transcript yet or the slot
//! index is out of range; `get_goals` returns `[]` for a never-run session
//! rather than erroring. [`goal_records`] is the shared snapshot helper
//! `registry.rs`'s `get_transcript` also calls, so `TranscriptRecord.goals`
//! and `session.get_goals` can never independently drift on shape.
//! Test: `registry_tests::set_goal_*`, `registry_tests::clear_goal_*`,
//! `registry_tests::get_goals_*`,
//! `registry_tests::get_transcript_round_trips_goal_state`.

use super::*;
use crate::agent_loop::{GoalSlots, GoalSource};
use crate::session::transcript::GoalSlotRecord;

impl SessionRegistry {
    /// `session.set_goal`: write an operator-provided goal into a 1-based
    /// slot (#2350).
    ///
    /// Why: the operator-facing counterpart to the model-facing `set_goal`
    /// tool (#2347) — same shared `GoalSlots`, tagged `GoalSource::Operator`
    /// so a later read can tell the two apart.
    /// What: `Err(session_not_found)` if `id` is unknown. `Err(invalid_argument)`
    /// if the session has no `pm_transcript` yet (see module docs' design
    /// decision) or if `slot` is `0`/greater than `GOAL_SLOT_COUNT`. A
    /// poisoned goals lock is recovered (mirrors `tools::goals`'s
    /// convention) rather than propagating a panic.
    /// Test: `registry_tests::set_goal_writes_operator_source`,
    /// `registry_tests::set_goal_no_transcript_yet_errors`,
    /// `registry_tests::set_goal_out_of_range_slot_errors`,
    /// `registry_tests::set_goal_unknown_session_errors`.
    pub fn set_goal(&self, id: &str, slot: usize, text: &str) -> Result<(), RpcError> {
        let handle = self.goals_handle(id)?;
        let mut guard = handle.lock().unwrap_or_else(|p| p.into_inner());
        guard
            .set(slot, text, GoalSource::Operator)
            .map_err(|e| RpcError::invalid_argument(e.to_string()))
    }

    /// `session.clear_goal`: clear an operator-named 1-based slot (#2350).
    ///
    /// Why: the operator-facing counterpart to the model-facing `clear_goal`
    /// tool (#2347).
    /// What: same error shape as [`Self::set_goal`]. Clearing an
    /// already-empty slot is idempotent success (mirrors
    /// `GoalSlots::clear`'s own convention).
    /// Test: `registry_tests::clear_goal_clears_slot`,
    /// `registry_tests::clear_goal_no_transcript_yet_errors`,
    /// `registry_tests::clear_goal_out_of_range_slot_errors`.
    pub fn clear_goal(&self, id: &str, slot: usize) -> Result<(), RpcError> {
        let handle = self.goals_handle(id)?;
        let mut guard = handle.lock().unwrap_or_else(|p| p.into_inner());
        guard
            .clear(slot)
            .map_err(|e| RpcError::invalid_argument(e.to_string()))
    }

    /// `session.get_goals`: snapshot every currently-occupied goal slot
    /// (#2350).
    ///
    /// Why: the operator-facing read path — lets a client inspect the
    /// current goal state (from either source) without going through
    /// `session.get_transcript`'s larger payload.
    /// What: `Err(session_not_found)` if `id` is unknown; otherwise
    /// [`goal_records`] — `[]` for a session with no `pm_transcript` yet,
    /// matching `session.get_transcript`'s never-run convention rather than
    /// `set_goal`/`clear_goal`'s stricter error.
    /// Test: `registry_tests::get_goals_returns_operator_and_model_sources`,
    /// `registry_tests::get_goals_on_never_run_session_is_empty`,
    /// `registry_tests::get_goals_unknown_session_errors`.
    pub fn get_goals(&self, id: &str) -> Result<Vec<GoalSlotRecord>, RpcError> {
        let sessions = self.lock();
        let entry = sessions
            .get(id)
            .ok_or_else(|| RpcError::session_not_found(id))?;
        Ok(goal_records(entry))
    }

    /// Resolve `id`'s live `GoalSlots` handle, shared by `set_goal`/`clear_goal`.
    ///
    /// Why: both mutators need the identical "unknown session" vs.
    /// "no transcript yet" error mapping — centralising it here means they
    /// can never independently drift on the design decision documented in
    /// the module docs.
    /// What: `Err(session_not_found)` if `id` is unknown; `Err(invalid_argument)`
    /// naming the "run a task first" limitation if `pm_transcript` is `None`;
    /// otherwise the cloned `Arc<Mutex<GoalSlots>>` handle.
    fn goals_handle(&self, id: &str) -> Result<Arc<Mutex<GoalSlots>>, RpcError> {
        let sessions = self.lock();
        let entry = sessions
            .get(id)
            .ok_or_else(|| RpcError::session_not_found(id))?;
        entry
            .pm_transcript
            .as_ref()
            .map(Transcript::goals_handle)
            .ok_or_else(|| {
                RpcError::invalid_argument(format!(
                    "session {id} has no transcript yet — run a task first before setting a goal"
                ))
            })
    }
}

/// Snapshot a `SessionEntry`'s current goal slots as wire-shaped
/// [`GoalSlotRecord`]s, or `[]` if it has no `pm_transcript` yet.
///
/// Why: shared by [`SessionRegistry::get_goals`] and `registry.rs`'s
/// `SessionRegistry::get_transcript` (`TranscriptRecord.goals`) so the two
/// read paths can never disagree on shape or on the never-run-session
/// convention.
/// What: `None` `pm_transcript` -> `vec![]`; otherwise locks the transcript's
/// `goals_handle()` (recovering a poisoned lock) and maps every
/// `GoalSlots::occupied` pair through `GoalSlotRecord::from_slot`.
pub(super) fn goal_records(entry: &SessionEntry) -> Vec<GoalSlotRecord> {
    entry
        .pm_transcript
        .as_ref()
        .map(|t| {
            let handle = t.goals_handle();
            let guard = handle.lock().unwrap_or_else(|p| p.into_inner());
            guard
                .occupied()
                .into_iter()
                .map(|(slot, g)| GoalSlotRecord::from_slot(slot, g))
                .collect()
        })
        .unwrap_or_default()
}
