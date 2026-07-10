//! `set_goal` / `clear_goal` tools — model-driven writes to the session's 5
//! fixed privileged goal slots (#2347, epic #2343 pillar 3).
//!
//! Why: `agent_loop::goals::GoalSlots` is the storage; something must let the
//! PM's own model write to it mid-conversation the same way `finish_task`
//! (`tools::finish_task`) lets it signal completion — a schema-validated
//! `ToolExecutor` pair, following that module's stateless-tool pattern except
//! these two DO hold state: a shared `Arc<Mutex<GoalSlots>>` handle onto the
//! live session `Transcript` (see `agent_loop::transcript::Transcript::goals_handle`),
//! rather than an `AgentRunner` or config dir as `delegate_to_agent`
//! (`tools::delegate`) holds. Both tools write with `GoalSource::Model` — the
//! future operator-facing `session.set_goal` RPC (#2348) writes the same
//! `GoalSlots` with `GoalSource::Operator` through a different call path, not
//! through these tools.
//! What: [`SetGoalTool`] and [`ClearGoalTool`] each wrap an
//! `Arc<Mutex<GoalSlots>>`. `execute` parses/validates the JSON args, then
//! calls [`GoalSlots::set`]/[`GoalSlots::clear`] under the lock; an
//! out-of-range slot or a poisoned lock both become a recoverable
//! `ToolResult::err` (never a panic). Registered ONLY in the daemon-session
//! path's PM tool registry (`task::executor::run_and_record`) — `run_task`'s
//! one-shot/bake-off path and delegated engineer registries never see these
//! tools, since goal slots are a session (not sub-agent) feature.
//! Test: `tests::*` — schema shape, valid set/clear round-trip through a
//! shared handle, out-of-range slot error handling for both tools.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::agent_loop::{GoalSlots, GoalSource};
use crate::tools::traits::{ToolExecutor, ToolResult};

/// The `set_goal` tool's registered/advertised name.
pub const SET_GOAL_TOOL_NAME: &str = "set_goal";

/// The `clear_goal` tool's registered/advertised name.
pub const CLEAR_GOAL_TOOL_NAME: &str = "clear_goal";

/// Parsed `set_goal` arguments.
#[derive(Debug, Deserialize)]
struct SetGoalArgs {
    slot: usize,
    text: String,
}

/// Parsed `clear_goal` arguments.
#[derive(Debug, Deserialize)]
struct ClearGoalArgs {
    slot: usize,
}

/// Lock `goals`, degrading a poisoned lock to a recoverable `ToolResult::err`
/// rather than panicking.
///
/// Why: Shared by both tools' `execute` so the poison-handling convention
/// (mirrors `run_task::recorder`'s `if let Ok(guard) = ...lock() {...}`
/// pattern, adapted here to return a value rather than silently no-op, since
/// a dropped goal write is a worse failure mode for a tool call than a
/// dropped accounting record) can never drift between them.
fn lock_or_recoverable_error(
    goals: &Mutex<GoalSlots>,
) -> Result<std::sync::MutexGuard<'_, GoalSlots>, ToolResult> {
    goals
        .lock()
        .map_err(|_| ToolResult::err("goal slots lock was poisoned by a prior panic"))
}

/// `ToolExecutor` for `set_goal` (#2347).
///
/// Why: See module docs.
/// What: Holds the shared `Arc<Mutex<GoalSlots>>`; `execute` writes with
/// `GoalSource::Model`.
/// Test: `tests::set_goal_valid_args_writes_slot`,
/// `tests::set_goal_out_of_range_slot_is_recoverable_error`,
/// `tests::set_goal_malformed_args_is_recoverable_error`.
pub struct SetGoalTool {
    goals: Arc<Mutex<GoalSlots>>,
}

impl SetGoalTool {
    /// Construct with a shared handle onto the session's live goal slots.
    ///
    /// Why: Callers get this handle from `agent_loop::transcript::Transcript::goals_handle`
    /// so this tool and `to_messages`'s rendering share the exact same state.
    /// What: Stores the `Arc` clone.
    /// Test: `tests::set_goal_valid_args_writes_slot`.
    pub fn new(goals: Arc<Mutex<GoalSlots>>) -> Self {
        Self { goals }
    }
}

#[async_trait]
impl ToolExecutor for SetGoalTool {
    fn name(&self) -> &str {
        SET_GOAL_TOOL_NAME
    }

    /// JSON schema for `set_goal`.
    ///
    /// Why: The tool's `description` carries the "record standing goals, not
    /// todos" usage guidance directly (#2347 keeps this out of
    /// `prompt/assembler.rs`/`preamble.rs`), since the schema description is
    /// always in the model's context whenever the tool is advertised.
    /// What: `slot` (integer 1-5, required), `text` (string, required).
    /// Test: `tests::set_goal_schema_has_required_fields`.
    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": SET_GOAL_TOOL_NAME,
                "description": "Record or update one of your 5 fixed, privileged standing goals — objectives that persist across turns and survive context compaction. These are NOT a todo list: use this only when your actual, durable objective changes (e.g. the user gives you a new top-level task, or a sub-objective is added). Do not use this for step-by-step task tracking.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "slot": {
                            "type": "integer",
                            "description": "Which of the 5 fixed goal slots to write (1-5). Writing an occupied slot replaces its previous content."
                        },
                        "text": {
                            "type": "string",
                            "description": "The standing goal's description."
                        }
                    },
                    "required": ["slot", "text"],
                    "additionalProperties": false
                }
            }
        })
    }

    /// Write the (already schema-validated) `slot`/`text` into `GoalSlots`.
    ///
    /// Why: By the time `execute` runs, `ToolCallExtractor::parse_and_validate`
    /// has already checked the shape against [`Self::schema`]; the `Err`
    /// deserialisation branch below is a defensive fallback (no panic) for a
    /// shape the loose schema validator accepted but strict `serde` rejects —
    /// mirrors `FinishTaskTool::execute`'s same defensive shape.
    /// What: `Ok` -> `GoalSlots::set(slot, text, GoalSource::Model)`; an
    /// out-of-range slot becomes a recoverable `ToolResult::err` naming the
    /// bad value.
    /// Test: `tests::set_goal_valid_args_writes_slot`,
    /// `tests::set_goal_out_of_range_slot_is_recoverable_error`,
    /// `tests::set_goal_malformed_args_is_recoverable_error`.
    async fn execute(&self, args: Value) -> ToolResult {
        let parsed: SetGoalArgs = match serde_json::from_value(args) {
            Ok(p) => p,
            Err(e) => {
                return ToolResult::err(format!(
                    "set_goal arguments did not match the expected shape ({e}). \
                     'slot' must be an integer 1-5 and 'text' must be a string."
                ));
            }
        };

        let mut guard = match lock_or_recoverable_error(&self.goals) {
            Ok(g) => g,
            Err(err) => return err,
        };

        match guard.set(parsed.slot, parsed.text.clone(), GoalSource::Model) {
            Ok(()) => ToolResult::ok(format!("Goal slot {} set: {}", parsed.slot, parsed.text)),
            Err(e) => ToolResult::err(e.to_string()),
        }
    }
}

/// `ToolExecutor` for `clear_goal` (#2347).
///
/// Why: See module docs.
/// What: Holds the shared `Arc<Mutex<GoalSlots>>`; `execute` clears the
/// named slot.
/// Test: `tests::clear_goal_valid_args_clears_slot`,
/// `tests::clear_goal_out_of_range_slot_is_recoverable_error`.
pub struct ClearGoalTool {
    goals: Arc<Mutex<GoalSlots>>,
}

impl ClearGoalTool {
    /// Construct with a shared handle onto the session's live goal slots.
    ///
    /// Why: Mirrors `SetGoalTool::new` — same shared handle, so both tools
    /// and `Transcript::to_messages` stay in sync.
    /// What: Stores the `Arc` clone.
    /// Test: `tests::clear_goal_valid_args_clears_slot`.
    pub fn new(goals: Arc<Mutex<GoalSlots>>) -> Self {
        Self { goals }
    }
}

#[async_trait]
impl ToolExecutor for ClearGoalTool {
    fn name(&self) -> &str {
        CLEAR_GOAL_TOOL_NAME
    }

    /// JSON schema for `clear_goal`.
    ///
    /// What: `slot` (integer 1-5, required).
    /// Test: `tests::clear_goal_schema_has_required_fields`.
    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": CLEAR_GOAL_TOOL_NAME,
                "description": "Retire one of your 5 fixed, privileged standing goals once it is fully accomplished or no longer relevant, leaving that slot empty.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "slot": {
                            "type": "integer",
                            "description": "Which of the 5 fixed goal slots to clear (1-5)."
                        }
                    },
                    "required": ["slot"],
                    "additionalProperties": false
                }
            }
        })
    }

    /// Clear the (already schema-validated) `slot`.
    ///
    /// What: `Ok` -> `GoalSlots::clear(slot)`; an out-of-range slot becomes
    /// a recoverable `ToolResult::err`.
    /// Test: `tests::clear_goal_valid_args_clears_slot`,
    /// `tests::clear_goal_out_of_range_slot_is_recoverable_error`.
    async fn execute(&self, args: Value) -> ToolResult {
        let parsed: ClearGoalArgs = match serde_json::from_value(args) {
            Ok(p) => p,
            Err(e) => {
                return ToolResult::err(format!(
                    "clear_goal arguments did not match the expected shape ({e}). \
                     'slot' must be an integer 1-5."
                ));
            }
        };

        let mut guard = match lock_or_recoverable_error(&self.goals) {
            Ok(g) => g,
            Err(err) => return err,
        };

        match guard.clear(parsed.slot) {
            Ok(()) => ToolResult::ok(format!("Goal slot {} cleared.", parsed.slot)),
            Err(e) => ToolResult::err(e.to_string()),
        }
    }
}

#[cfg(test)]
#[path = "goals_tests.rs"]
mod tests;
