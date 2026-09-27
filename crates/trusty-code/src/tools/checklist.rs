//! The `todo_write` tool — the model-facing write path onto a session's
//! per-agent checklist (#8235).
//!
//! Why: the roster's `todos` field (DOC-39 §5.4, `session::registry_agents`)
//! was reserved and always empty because nothing could write it, so a
//! multi-step task had no in-flight plan anywhere. The #8235 parity capture
//! is the cost: asked for a flag, a test and a changelog line, the agent
//! landed the flag and went idle with the other two silently dropped. Goal
//! slots (`tools::goals`) are not that mechanism — five fixed slots, no
//! per-step state, and their own description tells the model they are "NOT a
//! todo list".
//! What: [`TodoWriteTool`] validates the model's whole-list argument and hands
//! it to a [`TodoStore`]. The store trait lives HERE, in the tools layer, and
//! `task::todo_store::SessionTodoStore` implements it — the same layering
//! `agent_loop::sink::ToolEventSink` uses, so `tools` never depends upward on
//! `session`/`task`. Registered on the daemon-session registry only
//! (`task::executor::run_and_record`), exactly like `set_goal`/`clear_goal`.
//!
//! **Whole-list replace, not a patch.** #8235's own closure condition names
//! "a whole-list-replace call", and the token arithmetic agrees: a patch
//! protocol needs stable per-item ids the model must carry across
//! compaction, plus a companion read tool to recover them after a reset —
//! two calls and a stale-id failure mode to save a handful of tokens on a
//! list that is rarely longer than ten short lines.
//! Test: `tests::*` (this file's sibling `checklist_tests.rs`) for schema,
//! validation and store dispatch; `task::todo_store::tests::*` for the
//! round trip out through `session.get_agents`.

use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use thiserror::Error;

use crate::events::{TodoItem, TodoStatus};
use crate::tools::traits::{ToolExecutor, ToolResult};

/// The `todo_write` tool's registered/advertised name.
///
/// Why: snake_case per this crate's tool-naming convention, but otherwise
/// Claude Code's `TodoWrite` verbatim — the model's prior on that name is
/// worth more than a novel one like `set_todos`.
pub const TODO_WRITE_TOOL_NAME: &str = "todo_write";

/// Hard ceiling on a single checklist's length.
///
/// Why: the list is retained for the life of the session and re-serialised
/// into every roster read and change event, so an unbounded list is
/// unbounded session memory and unbounded wire payload. Twenty steps is well
/// past the point where a plan stops being a plan.
pub const MAX_TODOS: usize = 20;

/// Why a `todo_write` call was refused (#8235).
///
/// Why: every variant is text the MODEL reads and must be able to act on, so
/// each names the offending item and the fix rather than merely reporting
/// that something was wrong. Kept as a `thiserror` enum (not a bare `String`)
/// so `parse_todos` is unit-testable against the specific refusal.
/// What: the first five are validation; [`Self::Unavailable`] is the store
/// refusing the write (in practice: the session ended mid-run).
/// Test: `tests::rejects_unknown_status`, `tests::rejects_empty_content`,
/// `tests::rejects_two_in_progress`, `tests::rejects_more_than_max_todos`,
/// `tests::store_failure_is_a_recoverable_tool_error`.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum TodoError {
    /// An item's `content` was empty or whitespace only.
    #[error(
        "todo {index} has empty content — every step needs a short description of the work, \
         e.g. \"add the --json flag\""
    )]
    EmptyContent { index: usize },
    /// An item's `status` was not one of the three known states.
    #[error(
        "todo {index} has unknown status {status:?} — use \"pending\", \"in_progress\" or \
         \"completed\""
    )]
    UnknownStatus { index: usize, status: String },
    /// More than one item claimed `in_progress`.
    #[error(
        "{count} todos are marked in_progress — at most one step may be in_progress at a time; \
         mark the others pending or completed and send the whole list again"
    )]
    MultipleInProgress { count: usize },
    /// The list was longer than [`MAX_TODOS`].
    #[error(
        "{count} todos exceeds the limit of {max} — keep the checklist to the steps of the \
         task in hand, not every eventual follow-up",
        max = MAX_TODOS
    )]
    TooManyItems { count: usize },
    /// The store could not accept the write; nothing was changed.
    #[error("the checklist was not stored ({0}) — the list is unchanged")]
    Unavailable(String),
}

/// Where a validated checklist is written (#8235).
///
/// Why: the seam that keeps `tools` from depending on `session` — the only
/// implementation, `task::todo_store::SessionTodoStore`, holds the
/// `Arc<SessionRegistry>` and the writing agent's identity, so this tool
/// never learns which session or agent it serves. Same rationale as
/// `agent_loop::sink::ToolEventSink`'s module docs.
/// What: [`Self::replace`] is all-or-nothing — it either stores `todos`
/// whole or returns [`TodoError::Unavailable`] having changed nothing. Not
/// `async`: `SessionRegistry`'s writes are synchronous.
/// Test: `task::todo_store::tests::*` (the real store);
/// `tests::*` (a recording double).
pub trait TodoStore: Send + Sync {
    /// Replace the calling agent's whole checklist with `todos`.
    fn replace(&self, todos: Vec<TodoItem>) -> Result<(), TodoError>;
}

/// One raw checklist item as the model sent it.
///
/// Why: `status` is taken as a `String`, not as [`TodoStatus`], on purpose —
/// a serde enum would fail the WHOLE argument deserialisation with
/// "unknown variant" and no item index, and #8235 requires a refusal the
/// model can act on. [`parse_todos`] converts it and names the bad item.
#[derive(Debug, Deserialize)]
struct RawTodo {
    content: String,
    status: String,
}

/// Parsed `todo_write` arguments.
#[derive(Debug, Deserialize)]
struct TodoWriteArgs {
    todos: Vec<RawTodo>,
}

/// Validate and convert the model's raw list into storable [`TodoItem`]s.
///
/// Why: validation is a pure function so every refusal has a test that needs
/// no store, no session and no agent loop — and so `execute` cannot
/// accidentally write a partially-validated list: this returns `Err` before
/// the store is ever touched (#8235's fail-open check).
/// What: trims each `content` and rejects an empty one; maps `status`
/// through [`TodoStatus`]; rejects a list longer than [`MAX_TODOS`] or with
/// more than one `in_progress`. An EMPTY list is valid and means "clear the
/// checklist". The length check runs first so an over-long list is reported
/// as such rather than as whichever item happens to be malformed.
/// Test: `tests::accepts_a_valid_list`, `tests::accepts_an_empty_list`,
/// `tests::rejects_unknown_status`, `tests::rejects_empty_content`,
/// `tests::rejects_two_in_progress`, `tests::rejects_more_than_max_todos`.
fn parse_todos(raw: Vec<RawTodo>) -> Result<Vec<TodoItem>, TodoError> {
    if raw.len() > MAX_TODOS {
        return Err(TodoError::TooManyItems { count: raw.len() });
    }
    let mut items = Vec::with_capacity(raw.len());
    for (i, item) in raw.into_iter().enumerate() {
        let content = item.content.trim();
        if content.is_empty() {
            return Err(TodoError::EmptyContent { index: i });
        }
        let status = parse_status(&item.status).ok_or_else(|| TodoError::UnknownStatus {
            index: i,
            status: item.status.clone(),
        })?;
        items.push(TodoItem {
            content: content.to_string(),
            status,
        });
    }
    let in_progress = items
        .iter()
        .filter(|t| t.status == TodoStatus::InProgress)
        .count();
    if in_progress > 1 {
        return Err(TodoError::MultipleInProgress { count: in_progress });
    }
    Ok(items)
}

/// Map a wire status string onto [`TodoStatus`], or `None` if unknown.
fn parse_status(raw: &str) -> Option<TodoStatus> {
    match raw {
        "pending" => Some(TodoStatus::Pending),
        "in_progress" => Some(TodoStatus::InProgress),
        "completed" => Some(TodoStatus::Completed),
        _ => None,
    }
}

/// One line of feedback describing the stored list.
///
/// Why: the model gets a confirmation it can check against what it sent
/// without the tool echoing the whole list back into context on every call.
/// What: counts per state, plus the `in_progress` step's own text when there
/// is one, since that is the single fact the next turn acts on.
/// Test: `tests::success_text_names_the_in_progress_step`,
/// `tests::success_text_reports_an_empty_list`.
fn summarise(todos: &[TodoItem]) -> String {
    if todos.is_empty() {
        return "Checklist cleared.".to_string();
    }
    let count = |s: TodoStatus| todos.iter().filter(|t| t.status == s).count();
    let mut out = format!(
        "Checklist updated: {} of {} completed, {} pending.",
        count(TodoStatus::Completed),
        todos.len(),
        count(TodoStatus::Pending)
    );
    if let Some(active) = todos
        .iter()
        .find(|t| t.status == TodoStatus::InProgress)
        .map(|t| t.content.as_str())
    {
        out.push_str(&format!(" In progress: {active}"));
    }
    out
}

/// `ToolExecutor` for `todo_write` (#8235).
///
/// Why: see module docs.
/// What: holds the [`TodoStore`] the run's registry was built with; `execute`
/// validates then writes, and every failure path returns a recoverable
/// `ToolResult::err` — never a panic, never a success over an unchanged list.
/// Test: `tests::*`.
pub struct TodoWriteTool {
    store: Arc<dyn TodoStore>,
}

impl TodoWriteTool {
    /// Construct against the session's checklist store.
    pub fn new(store: Arc<dyn TodoStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl ToolExecutor for TodoWriteTool {
    fn name(&self) -> &str {
        TODO_WRITE_TOOL_NAME
    }

    /// JSON schema for `todo_write`.
    ///
    /// Why: the description is the only place the model reliably reads about
    /// this tool — `task::executor`'s delegating path scopes the system
    /// prompt to the agent card's own `tcode_tools` and this tool is not one
    /// of them, exactly as `set_goal`/`clear_goal` are not. So the "when to
    /// use it" rule lives here, kept to three sentences.
    /// What: `todos` (array of `{content, status}`, required). `status` is
    /// declared as an enum for the model's benefit; `parse_todos` re-checks
    /// it, because a loose schema validator accepting a bad value must still
    /// produce an indexed refusal rather than a serde error.
    /// Test: `tests::schema_declares_todos_and_the_three_statuses`.
    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": TODO_WRITE_TOOL_NAME,
                "description": "Track the steps of a multi-step task as a checklist. Call it once when you plan the work, then again the moment a step starts or finishes, so the list always shows what is done and what remains. Send the WHOLE list every time — it replaces the stored one — and keep at most one step in_progress.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "todos": {
                            "type": "array",
                            "description": "The complete checklist, in the order the steps should happen. An empty array clears it.",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "content": {
                                        "type": "string",
                                        "description": "The step, as a short imperative phrase."
                                    },
                                    "status": {
                                        "type": "string",
                                        "enum": ["pending", "in_progress", "completed"],
                                        "description": "pending = not started; in_progress = being worked on now (at most one); completed = done."
                                    }
                                },
                                "required": ["content", "status"],
                                "additionalProperties": false
                            }
                        }
                    },
                    "required": ["todos"],
                    "additionalProperties": false
                }
            }
        })
    }

    /// Validate the model's whole list, then replace the stored one.
    ///
    /// Why: ordered validate-then-write so no refusal can leave a partially
    /// applied list behind — #8235's fail-open check.
    /// What: a malformed argument shape, any [`TodoError`] from
    /// [`parse_todos`], and a store refusal each become a recoverable
    /// `ToolResult::err` carrying the model-actionable text.
    /// Test: `tests::valid_call_reaches_the_store`,
    /// `tests::rejected_call_never_reaches_the_store`,
    /// `tests::malformed_args_are_a_recoverable_error`,
    /// `tests::store_failure_is_a_recoverable_tool_error`.
    async fn execute(&self, args: Value) -> ToolResult {
        let parsed: TodoWriteArgs = match serde_json::from_value(args) {
            Ok(p) => p,
            Err(e) => {
                return ToolResult::err(format!(
                    "todo_write arguments did not match the expected shape ({e}). Pass \
                     'todos' as an array of objects, each with a 'content' string and a \
                     'status' of \"pending\", \"in_progress\" or \"completed\"."
                ));
            }
        };
        let todos = match parse_todos(parsed.todos) {
            Ok(t) => t,
            Err(e) => return ToolResult::err(e.to_string()),
        };
        let summary = summarise(&todos);
        match self.store.replace(todos) {
            Ok(()) => ToolResult::ok(summary),
            Err(e) => ToolResult::err(e.to_string()),
        }
    }
}

#[cfg(test)]
#[path = "checklist_tests.rs"]
mod tests;
