//! Unit tests for `tools::goals` (#2347).

use std::sync::{Arc, Mutex};

use serde_json::json;

use super::*;
use crate::agent_loop::GoalSlots;

/// `set_goal` schema declares `slot`+`text` as required.
///
/// Why: Guards the schema contract the extractor's generic validator relies on.
/// What: Assert `required == ["slot", "text"]`.
/// Test: this test.
#[test]
fn set_goal_schema_has_required_fields() {
    let tool = SetGoalTool::new(Arc::new(Mutex::new(GoalSlots::new())));
    let schema = tool.schema();
    let required: Vec<&str> = schema["function"]["parameters"]["required"]
        .as_array()
        .expect("required array")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(required, vec!["slot", "text"]);
    assert_eq!(schema["function"]["name"], SET_GOAL_TOOL_NAME);
}

/// `clear_goal` schema declares `slot` as required.
///
/// Why: Mirrors `set_goal_schema_has_required_fields` for the other tool.
/// What: Assert `required == ["slot"]`.
/// Test: this test.
#[test]
fn clear_goal_schema_has_required_fields() {
    let tool = ClearGoalTool::new(Arc::new(Mutex::new(GoalSlots::new())));
    let schema = tool.schema();
    let required: Vec<&str> = schema["function"]["parameters"]["required"]
        .as_array()
        .expect("required array")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(required, vec!["slot"]);
    assert_eq!(schema["function"]["name"], CLEAR_GOAL_TOOL_NAME);
}

/// A valid `set_goal` call writes the slot with `GoalSource::Model`, and the
/// SAME shared `GoalSlots` handle reflects it (proving the tool mutates the
/// live state rather than a private copy).
///
/// Why: This is the core wiring contract `task::executor::run_and_record`
/// depends on: the tool and the `Transcript` share one `Arc<Mutex<_>>`.
/// What: Build a shared handle, execute `set_goal`, then read the SAME
/// handle directly.
/// Test: this test.
#[tokio::test]
async fn set_goal_valid_args_writes_slot() {
    let goals = Arc::new(Mutex::new(GoalSlots::new()));
    let tool = SetGoalTool::new(Arc::clone(&goals));

    let result = tool
        .execute(json!({"slot": 2, "text": "ship the release"}))
        .await;
    assert!(!result.is_error(), "expected success: {}", result.content());

    let slot = goals
        .lock()
        .expect("lock")
        .get(2)
        .cloned()
        .expect("slot 2 occupied");
    assert_eq!(slot.text, "ship the release");
    assert_eq!(slot.source, crate::agent_loop::GoalSource::Model);
}

/// `set_goal` with an out-of-range slot returns a recoverable error, not a
/// panic.
///
/// Why: `slot` is model-controlled JSON; an out-of-range value must degrade
/// gracefully.
/// What: `execute({"slot": 0, ...})` and `{"slot": 6, ...}`.
/// Test: this test.
#[tokio::test]
async fn set_goal_out_of_range_slot_is_recoverable_error() {
    let tool = SetGoalTool::new(Arc::new(Mutex::new(GoalSlots::new())));

    for bad_slot in [0, 6, 100] {
        let result = tool.execute(json!({"slot": bad_slot, "text": "x"})).await;
        assert!(result.is_error(), "slot {bad_slot} must be rejected");
        assert!(!result.is_fatal(), "must be recoverable, not fatal");
        assert!(result.content().contains("out of range"));
    }
}

/// `set_goal` with malformed arguments (missing `text`) returns a
/// recoverable error rather than panicking.
///
/// Why: Guards the defensive deserialisation fallback, mirroring
/// `finish_task`'s `execute_bypassing_schema_validation_is_recoverable`.
/// What: `execute({"slot": 1})` — missing required `text`.
/// Test: this test.
#[tokio::test]
async fn set_goal_malformed_args_is_recoverable_error() {
    let tool = SetGoalTool::new(Arc::new(Mutex::new(GoalSlots::new())));
    let result = tool.execute(json!({"slot": 1})).await;
    assert!(result.is_error());
    assert!(!result.is_fatal());
    assert!(
        result
            .content()
            .contains("did not match the expected shape")
    );
}

/// A valid `clear_goal` call empties the shared slot.
///
/// Why: Core round-trip contract for the clear path.
/// What: Set slot 3 via the shared handle directly, `execute(clear_goal)`,
/// assert the shared handle now reports it empty.
/// Test: this test.
#[tokio::test]
async fn clear_goal_valid_args_clears_slot() {
    let goals = Arc::new(Mutex::new(GoalSlots::new()));
    goals
        .lock()
        .expect("lock")
        .set(3, "temp", crate::agent_loop::GoalSource::Model)
        .expect("valid slot");

    let tool = ClearGoalTool::new(Arc::clone(&goals));
    let result = tool.execute(json!({"slot": 3})).await;
    assert!(!result.is_error(), "expected success: {}", result.content());

    assert!(goals.lock().expect("lock").get(3).is_none());
}

/// `clear_goal` with an out-of-range slot returns a recoverable error.
///
/// Why: Mirrors `set_goal_out_of_range_slot_is_recoverable_error`.
/// What: `execute({"slot": 0})` and `{"slot": 6}`.
/// Test: this test.
#[tokio::test]
async fn clear_goal_out_of_range_slot_is_recoverable_error() {
    let tool = ClearGoalTool::new(Arc::new(Mutex::new(GoalSlots::new())));

    for bad_slot in [0, 6] {
        let result = tool.execute(json!({"slot": bad_slot})).await;
        assert!(result.is_error(), "slot {bad_slot} must be rejected");
        assert!(!result.is_fatal());
        assert!(result.content().contains("out of range"));
    }
}
