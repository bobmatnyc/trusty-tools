//! Unit tests for `agent_loop::goals` (#2347).

use super::*;

/// A fresh `GoalSlots` has all five slots empty and renders `None`.
///
/// Why: Guards the baseline `Default`/`new()` state before any mutation.
/// What: `new()` then `get(1..=5)` all `None`, `render()` is `None`.
/// Test: this test.
#[test]
fn new_is_all_empty() {
    let goals = GoalSlots::new();
    for slot in 1..=GOAL_SLOT_COUNT {
        assert!(goals.get(slot).is_none(), "slot {slot} must start empty");
    }
    assert!(goals.render().is_none());
}

/// `set` then `get` round-trips `text`, `source`, and a fresh `updated_at`.
///
/// Why: The core write/read contract every tool call and render() depends on.
/// What: `set(2, "ship the release", Model)`, then assert `get(2)` reflects
/// all three fields.
/// Test: this test.
#[test]
fn set_then_get_round_trips_all_fields() {
    let mut goals = GoalSlots::new();
    let before = Utc::now();
    goals
        .set(2, "ship the release", GoalSource::Model)
        .expect("valid slot");

    let slot = goals.get(2).expect("slot 2 occupied");
    assert_eq!(slot.text, "ship the release");
    assert_eq!(slot.source, GoalSource::Model);
    assert!(slot.updated_at >= before);
}

/// `set` records `GoalSource::Model` distinctly from `Operator`.
///
/// Why: Guards that the two sources aren't accidentally collapsed/aliased.
/// What: Set one slot from each source; assert both are preserved.
/// Test: this test.
#[test]
fn set_records_model_source() {
    let mut goals = GoalSlots::new();
    goals.set(1, "model goal", GoalSource::Model).expect("ok");
    goals
        .set(2, "operator goal", GoalSource::Operator)
        .expect("ok");

    assert_eq!(goals.get(1).expect("slot 1").source, GoalSource::Model);
    assert_eq!(goals.get(2).expect("slot 2").source, GoalSource::Operator);
}

/// A second `set` on an already-occupied slot overwrites it rather than
/// appending or erroring.
///
/// Why: Slots hold the CURRENT goal only — the model updating a standing
/// goal's wording must not accumulate stale copies.
/// What: `set(1, "first")` then `set(1, "second")`; assert only "second"
/// remains.
/// Test: this test.
#[test]
fn set_overwrites_existing_slot() {
    let mut goals = GoalSlots::new();
    goals.set(1, "first", GoalSource::Model).expect("ok");
    goals.set(1, "second", GoalSource::Model).expect("ok");

    assert_eq!(goals.get(1).expect("slot 1").text, "second");
}

/// `set` on slot `0` or a slot past `GOAL_SLOT_COUNT` returns
/// `GoalSlotError::OutOfRange` naming the bad value, not a panic.
///
/// Why: The `slot` argument is ultimately model-controlled (via the
/// `set_goal` tool's JSON args); an out-of-range value must be a
/// recoverable `Result`, never an index-out-of-bounds panic.
/// What: Try slot `0` and `GOAL_SLOT_COUNT + 1`.
/// Test: this test.
#[test]
fn set_out_of_range_slot_errors() {
    let mut goals = GoalSlots::new();
    assert_eq!(
        goals.set(0, "x", GoalSource::Model),
        Err(GoalSlotError::OutOfRange { slot: 0 })
    );
    assert_eq!(
        goals.set(GOAL_SLOT_COUNT + 1, "x", GoalSource::Model),
        Err(GoalSlotError::OutOfRange {
            slot: GOAL_SLOT_COUNT + 1
        })
    );
}

/// `clear` empties a previously-set slot.
///
/// Why: Core retire-a-goal contract.
/// What: `set(3, ...)` then `clear(3)`; assert `get(3)` is `None`.
/// Test: this test.
#[test]
fn clear_empties_slot() {
    let mut goals = GoalSlots::new();
    goals.set(3, "temp goal", GoalSource::Model).expect("ok");
    goals.clear(3).expect("valid slot");
    assert!(goals.get(3).is_none());
}

/// `clear` on an already-empty slot succeeds (idempotent), not an error.
///
/// Why: The model should not have to track which slots are occupied before
/// clearing one defensively.
/// What: `clear(4)` on a fresh `GoalSlots`.
/// Test: this test.
#[test]
fn clear_already_empty_slot_is_ok() {
    let mut goals = GoalSlots::new();
    assert!(goals.clear(4).is_ok());
    assert!(goals.get(4).is_none());
}

/// `clear` on an out-of-range slot returns `GoalSlotError::OutOfRange`.
///
/// Why: Mirrors `set_out_of_range_slot_errors` for the other mutator.
/// What: `clear(0)` and `clear(GOAL_SLOT_COUNT + 1)`.
/// Test: this test.
#[test]
fn clear_out_of_range_slot_errors() {
    let mut goals = GoalSlots::new();
    assert_eq!(goals.clear(0), Err(GoalSlotError::OutOfRange { slot: 0 }));
    assert_eq!(
        goals.clear(GOAL_SLOT_COUNT + 1),
        Err(GoalSlotError::OutOfRange {
            slot: GOAL_SLOT_COUNT + 1
        })
    );
}

/// `render()` is `None` when every slot is empty.
///
/// Why: Pins the acceptance criterion "goals message absent when all slots
/// empty" at the `GoalSlots` level (the `Transcript` level is covered in
/// `transcript_tests`).
/// What: Fresh `GoalSlots::new()`, assert `render().is_none()`.
/// Test: this test.
#[test]
fn render_none_when_all_empty() {
    assert!(GoalSlots::new().render().is_none());
}

/// `render()` numbers occupied slots by their 1-based slot position,
/// skipping empty slots entirely rather than renumbering sequentially.
///
/// Why: A model reading "3. finish the migration" must be able to refer
/// back to "slot 3" in a subsequent `clear_goal`/`set_goal` call; if empty
/// slots were skipped in the NUMBERING (not just omitted from output) the
/// numbers would drift from the real slot indices.
/// What: Occupy slots 1 and 3 only; assert the rendered lines are exactly
/// `"1. <text>"` and `"3. <text>"`, with no `"2."` line.
/// Test: this test.
#[test]
fn render_numbers_by_slot_skipping_empty() {
    let mut goals = GoalSlots::new();
    goals.set(1, "goal one", GoalSource::Model).expect("ok");
    goals.set(3, "goal three", GoalSource::Model).expect("ok");

    let rendered = goals.render().expect("non-empty render");
    assert!(rendered.contains("1. goal one"));
    assert!(rendered.contains("3. goal three"));
    assert!(!rendered.contains("2."));
}

/// `occupied()` returns only occupied slots, paired with their 1-based
/// index, skipping gaps.
///
/// Why: #2350's `session.get_goals`/`TranscriptRecord.goals` both build on
/// this snapshot; a gap in the middle (slot 2 empty) must not shift slot 3's
/// reported index down to 2.
/// What: Occupy slots 1 and 3 only; assert `occupied()` returns exactly
/// `[(1, ...), (3, ...)]` in that order.
/// Test: this test.
#[test]
fn occupied_skips_empty_slots_and_preserves_index() {
    let mut goals = GoalSlots::new();
    goals.set(1, "goal one", GoalSource::Model).expect("ok");
    goals
        .set(3, "goal three", GoalSource::Operator)
        .expect("ok");

    let occupied = goals.occupied();
    assert_eq!(occupied.len(), 2);
    assert_eq!(occupied[0].0, 1);
    assert_eq!(occupied[0].1.text, "goal one");
    assert_eq!(occupied[1].0, 3);
    assert_eq!(occupied[1].1.source, GoalSource::Operator);
}

/// `occupied()` on a fresh `GoalSlots` returns an empty vec, not an error.
///
/// Why: `session.get_goals` on a session whose transcript has never had a
/// goal set must return `[]`, matching `session.get_transcript`'s
/// never-run-session convention.
/// What: Fresh `GoalSlots::new()`, assert `occupied()` is empty.
/// Test: this test.
#[test]
fn occupied_empty_when_all_slots_empty() {
    assert!(GoalSlots::new().occupied().is_empty());
}

/// `GoalSource` serialises to the stable snake_case wire strings
/// `session.get_goals`/`TranscriptRecord.goals` expose over JSON-RPC.
///
/// Why: #2350 exposes `GoalSource` directly on the wire (unlike #2347, which
/// only used it in-process); the wire string must be stable and
/// human-readable independent of Rust variant naming, matching
/// `SessionStatus`'s `#[serde(rename_all = "snake_case")]` convention.
/// What: `Model` -> `"model"`, `Operator` -> `"operator"`.
/// Test: this test.
#[test]
fn goal_source_serialises_snake_case() {
    assert_eq!(
        serde_json::to_value(GoalSource::Model).unwrap(),
        serde_json::json!("model")
    );
    assert_eq!(
        serde_json::to_value(GoalSource::Operator).unwrap(),
        serde_json::json!("operator")
    );
}

/// `render()` prefixes the fixed preamble before the numbered slots.
///
/// Why: Guards that the model-facing framing text ("privileged, standing
/// goals... not a todo list") survives, since #2347 deliberately keeps this
/// guidance out of `prompt/assembler.rs`/`preamble.rs`.
/// What: Assert the rendered block starts with the `"## Active Goals"`
/// heading.
/// Test: this test.
#[test]
fn render_includes_preamble() {
    let mut goals = GoalSlots::new();
    goals.set(1, "goal one", GoalSource::Model).expect("ok");
    let rendered = goals.render().expect("non-empty render");
    assert!(rendered.starts_with("## Active Goals"));
    assert!(rendered.contains("not a todo list"));
}
