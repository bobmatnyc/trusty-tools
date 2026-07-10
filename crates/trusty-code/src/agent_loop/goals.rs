//! Goal slots: 5 fixed, privileged standing-goal slots pinned near the top
//! of the prompt (#2347, epic #2343 pillar 3).
//!
//! Why: Infinite Sessions (#2343) must keep a long-running PM anchored on
//! its objective even after many rounds of compaction/summarisation have
//! discarded the turns that originally stated it. A free-form "remember
//! your goal" instruction buried in the transcript is exactly the kind of
//! content non-destructive compaction (#2070) is entitled to fold into a
//! summary once it ages out of the kept window — so goals cannot live as an
//! ordinary transcript entry. Fixed, numbered slots (rather than an
//! unbounded list) keep the injected block small and bounded — the vision
//! spec's ~2K-token goals budget out of the 200K-window overhead accounting
//! (#2343) assumes a small constant number of slots, not an
//! attacker/model-controlled list length.
//! What: [`GoalSlots`] holds exactly [`GOAL_SLOT_COUNT`] `Option<GoalSlot>`
//! entries. [`GoalSlots::set`]/[`GoalSlots::clear`] mutate a 1-based slot
//! number (matching the `set_goal`/`clear_goal` tool schemas in
//! `tools::goals`), returning [`GoalSlotError::OutOfRange`] rather than
//! panicking on an out-of-range index. [`GoalSlots::render`] produces the
//! synthetic prompt block `agent_loop::transcript::Transcript::to_messages`
//! injects at position `[1]` — `None` when every slot is empty, so an
//! all-empty `GoalSlots` never adds a message at all.
//! Test: `goals_tests::*` — set/clear round-trips, out-of-range rejection,
//! render formatting (numbering skips empty slots, `None` when all empty),
//! and that `set` overwrites in place (updating `updated_at`) rather than
//! appending.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// The number of fixed goal slots (#2343 design decision: exactly 5).
///
/// Why: A fixed, small constant — rather than a caller-supplied capacity —
/// is what makes the rendered block's size bounded and predictable for the
/// epic's overhead-budget accounting; growing it is a deliberate design
/// change, not a runtime configuration knob.
pub const GOAL_SLOT_COUNT: usize = 5;

/// Who last wrote a given [`GoalSlot`].
///
/// Why: `set_goal` (the `tools::goals::SetGoalTool` the PM's own model
/// calls) and the future operator-facing `session.set_goal` RPC (#2350)
/// both mutate the same [`GoalSlots`]; recording the source lets a future
/// UI/audit view distinguish "the model decided this" from "an operator
/// pinned this" without a second parallel data structure.
/// Test: `goals_tests::set_records_model_source`,
/// `goals_tests::goal_source_serialises_snake_case`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalSource {
    /// Written by the PM's own model via the `set_goal` tool call.
    Model,
    /// Written by a human operator (#2350's `session.set_goal` RPC).
    Operator,
}

/// One occupied goal slot: its text, who wrote it, and when.
///
/// Why: A bare `String` would lose the provenance/recency information a
/// future operator-facing view (#2350) needs; bundling all three in one
/// struct keeps `GoalSlots`'s storage a simple fixed-size array of
/// `Option<GoalSlot>` rather than three parallel arrays.
/// What: `text` is the free-form goal description; `source` is
/// [`GoalSource`]; `updated_at` is the UTC timestamp of the last `set` call
/// that wrote this slot (matching the `chrono::Utc` convention already used
/// by `session::registry`/`session::model` for session timestamps).
/// Test: `goals_tests::set_then_get_round_trips_all_fields`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GoalSlot {
    /// Free-form goal description.
    pub text: String,
    /// Who last wrote this slot.
    pub source: GoalSource,
    /// UTC timestamp of the last write.
    pub updated_at: DateTime<Utc>,
}

/// Slot-index validation failure.
///
/// Why: `set`/`clear` accept a caller- (ultimately model-) supplied 1-based
/// slot number; an out-of-range value (0, or greater than
/// [`GOAL_SLOT_COUNT`]) must be a recoverable error the LLM tool layer can
/// report back to the model, never a panic/index-out-of-bounds.
/// What: The single variant names the offending `slot` value so the
/// rendered error message is actionable.
/// Test: `goals_tests::set_out_of_range_slot_errors`,
/// `goals_tests::clear_out_of_range_slot_errors`.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum GoalSlotError {
    /// `slot` was `0` or greater than [`GOAL_SLOT_COUNT`].
    #[error("goal slot {slot} out of range: must be between 1 and {GOAL_SLOT_COUNT}")]
    OutOfRange {
        /// The invalid slot number as supplied by the caller.
        slot: usize,
    },
}

/// The fixed preamble prefixed to every non-empty [`GoalSlots::render`]
/// output.
///
/// Why: Without an explicit framing line the model has no way to tell this
/// injected block apart from an ordinary system note, and might treat it as
/// a todo list to be checked off and discarded rather than a standing
/// objective to keep re-reading. Fixed rather than configurable per #2347's
/// scope: the schema descriptions on the `set_goal`/`clear_goal` tools (not
/// this preamble) carry the "not todos" usage guidance per the issue's
/// explicit "do NOT modify prompt/assembler.rs or preamble.rs" constraint.
const GOALS_PREAMBLE: &str = "## Active Goals\n\
These are your privileged, standing goals — fixed slots that persist across \
turns and survive context compaction. They are not a todo list; use \
set_goal/clear_goal only when the underlying objective itself changes.";

/// Exactly [`GOAL_SLOT_COUNT`] fixed, privileged goal slots (#2347).
///
/// Why: See module docs — this is the in-memory state
/// `agent_loop::transcript::Transcript` renders fresh on every
/// `to_messages()` call and `tools::goals::{SetGoalTool, ClearGoalTool}`
/// mutate on the model's behalf.
/// What: A fixed-size array of `Option<GoalSlot>`; `Default` is all-empty.
/// Test: `goals_tests::*`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GoalSlots {
    slots: [Option<GoalSlot>; GOAL_SLOT_COUNT],
}

impl GoalSlots {
    /// Construct an all-empty `GoalSlots`.
    ///
    /// Why: Explicit constructor mirrors this crate's `Tool::new()`
    /// convention even though `Default` would do the same.
    /// Test: `goals_tests::new_is_all_empty`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Write (or overwrite) a 1-based `slot` with `text` from `source`.
    ///
    /// Why: The model-facing `set_goal` tool and the future operator RPC
    /// (#2350) both need one shared, validated write path rather than
    /// reaching into the array directly.
    /// What: Validates `slot` via [`Self::validate`], then replaces (does
    /// NOT append to) that slot with a fresh `GoalSlot { text, source,
    /// updated_at: Utc::now() }`. Overwriting an occupied slot discards its
    /// previous content — slots hold the CURRENT goal only, not a history.
    /// Test: `goals_tests::set_then_get_round_trips_all_fields`,
    /// `goals_tests::set_overwrites_existing_slot`,
    /// `goals_tests::set_out_of_range_slot_errors`.
    pub fn set(
        &mut self,
        slot: usize,
        text: impl Into<String>,
        source: GoalSource,
    ) -> Result<(), GoalSlotError> {
        let idx = Self::validate(slot)?;
        self.slots[idx] = Some(GoalSlot {
            text: text.into(),
            source,
            updated_at: Utc::now(),
        });
        Ok(())
    }

    /// Clear a 1-based `slot`, leaving it empty.
    ///
    /// Why: The model-facing `clear_goal` tool needs a validated way to
    /// retire a completed/obsolete goal without disturbing the other four
    /// slots.
    /// What: Validates `slot` via [`Self::validate`], then sets it to
    /// `None`. Clearing an already-empty slot is a no-op success, not an
    /// error — idempotent clearing is simpler for the model to reason about
    /// than a "nothing to clear" failure.
    /// Test: `goals_tests::clear_empties_slot`,
    /// `goals_tests::clear_already_empty_slot_is_ok`,
    /// `goals_tests::clear_out_of_range_slot_errors`.
    pub fn clear(&mut self, slot: usize) -> Result<(), GoalSlotError> {
        let idx = Self::validate(slot)?;
        self.slots[idx] = None;
        Ok(())
    }

    /// Read the current content of a 1-based `slot`, if occupied.
    ///
    /// Why: Test/observability accessor; also lets a future operator view
    /// (#2350) inspect one slot without re-deriving the whole rendered
    /// block.
    /// What: Returns `None` for an out-of-range slot OR an empty one — the
    /// two cases are intentionally not distinguished here (callers that
    /// need to distinguish "invalid" from "empty" use `set`/`clear`'s
    /// `Result` instead).
    /// Test: `goals_tests::set_then_get_round_trips_all_fields`.
    pub fn get(&self, slot: usize) -> Option<&GoalSlot> {
        let idx = slot.checked_sub(1)?;
        self.slots.get(idx)?.as_ref()
    }

    /// Snapshot every currently-occupied slot as `(1-based slot, GoalSlot)`
    /// pairs, in slot order (#2350).
    ///
    /// Why: `session.get_goals` and `TranscriptRecord.goals`
    /// (`session::transcript`) both need a serialisable view of every
    /// occupied slot together with its 1-based index — cloning here (rather
    /// than returning references) lets the caller build its own DTO without
    /// holding this `GoalSlots`'s lock any longer than the snapshot itself.
    /// What: Filters out empty slots, cloning each occupied `GoalSlot` paired
    /// with its 1-based slot number.
    /// Test: `goals_tests::occupied_skips_empty_slots_and_preserves_index`,
    /// `goals_tests::occupied_empty_when_all_slots_empty`.
    pub fn occupied(&self) -> Vec<(usize, GoalSlot)> {
        self.slots
            .iter()
            .enumerate()
            .filter_map(|(i, slot)| slot.clone().map(|g| (i + 1, g)))
            .collect()
    }

    /// Render the privileged-goals prompt block, or `None` if every slot is
    /// empty.
    ///
    /// Why: This is what `Transcript::to_messages` injects at position
    /// `[1]` on every call — computed fresh from live state rather than
    /// stored, so it can never drift from what `set`/`clear` last wrote.
    /// Returning `None` when nothing is set means an all-empty `GoalSlots`
    /// injects NOTHING, matching the acceptance criterion that the goals
    /// message is absent (not an empty stub) when no goal has ever been set.
    /// What: Numbers each occupied slot by its 1-based position, skipping
    /// empty slots entirely (so slot 3 alone renders as `"3. <text>"`, not
    /// `"1. <text>"`), joined under [`GOALS_PREAMBLE`].
    /// Test: `goals_tests::render_none_when_all_empty`,
    /// `goals_tests::render_numbers_by_slot_skipping_empty`,
    /// `goals_tests::render_includes_preamble`.
    pub fn render(&self) -> Option<String> {
        let lines: Vec<String> = self
            .slots
            .iter()
            .enumerate()
            .filter_map(|(i, slot)| slot.as_ref().map(|g| format!("{}. {}", i + 1, g.text)))
            .collect();
        if lines.is_empty() {
            return None;
        }
        Some(format!("{GOALS_PREAMBLE}\n{}", lines.join("\n")))
    }

    /// Validate a 1-based slot number, returning its 0-based array index.
    ///
    /// Why: Shared by `set`/`clear` so the range check and error shape can
    /// never drift between the two mutators.
    /// What: `Ok(slot - 1)` for `1..=GOAL_SLOT_COUNT`; `Err(OutOfRange)`
    /// otherwise (covers both `0` and anything past the last slot).
    fn validate(slot: usize) -> Result<usize, GoalSlotError> {
        if slot == 0 || slot > GOAL_SLOT_COUNT {
            return Err(GoalSlotError::OutOfRange { slot });
        }
        Ok(slot - 1)
    }
}

#[cfg(test)]
#[path = "goals_tests.rs"]
mod tests;
