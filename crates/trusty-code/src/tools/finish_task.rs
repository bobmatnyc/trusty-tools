//! `finish_task` tool — structured, schema-validated task completion (#2072, §5.8).
//!
//! Why: Before this tool, an `AgentLoop` only recognised completion implicitly —
//! an assistant turn with NO tool calls (the D3 convention, see
//! `agent_loop::run_inner`). That works, but it forces the model to convey
//! "I'm done, here's what happened" as free prose the caller must re-parse.
//! `finish_task` gives the model an explicit, schema-validated tool call for the
//! same signal: a `status` enum, a `summary`, and optional structured `changes`/
//! test-result fields. Malformed arguments (a missing required field, an invalid
//! `status` enum value, syntactically broken JSON) flow through the EXACT SAME
//! `llm::tool_call_extractor` plumbing #1023 built for every other tool —
//! `ToolCallExtractor::parse_and_validate` → `schema::validate_args` — so no new
//! validation or repair code is needed here; `finish_task` is just another
//! `ToolExecutor` whose schema happens to describe a completion report.
//! What: [`FinishTaskTool`] implements `ToolExecutor` with `name()` equal to
//! [`FINISH_TASK_TOOL_NAME`]. `execute` deserialises the (already
//! schema-validated) arguments into [`FinishTaskArgs`] and renders them via
//! [`render_finish_summary`] into the tool's `ToolResult`. `agent_loop::run_inner`
//! (#2072) recognises a successful `finish_task` dispatch and terminates the loop
//! immediately, building the final `AgentOutput` from the SAME `FinishTaskArgs` /
//! `render_finish_summary` this module exposes, instead of waiting for a later
//! no-tool-call turn.
//! Test: `finish_task::tests::*` — schema shape, valid execute, execute with
//! changes/tests fields, and the defensive (should-be-unreachable given upstream
//! schema validation) malformed-shape branch. `agent_loop::tests::*` covers the
//! loop-termination integration end to end.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::tools::traits::{ToolExecutor, ToolResult};

/// The tool's registered/advertised name.
///
/// Why: Shared literal between the `ToolExecutor` impl and `agent_loop`'s
/// finish-detection check, so the two can never drift.
/// Test: `tests::name_matches_constant`.
pub const FINISH_TASK_TOOL_NAME: &str = "finish_task";

/// Completion status reported by the model.
///
/// Why: An enum (rather than a free-form string) lets the shared JSON-Schema
/// `enum` validator (`llm::tool_call_extractor::schema::validate_args`) catch a
/// hallucinated status value BEFORE it ever reaches `execute`.
/// What: Mirrors the three values in the §5.8 schema, serialised/deserialised in
/// `snake_case` to match the wire strings exactly.
/// Test: `tests::status_display_and_roundtrip`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishStatus {
    /// The task was completed successfully.
    Completed,
    /// The task could not be completed.
    Failed,
    /// The task was abandoned/cancelled before completion.
    Cancelled,
}

impl std::fmt::Display for FinishStatus {
    /// Why: `render_finish_summary` needs the lowercase wire form, not Rust's
    /// `Debug` casing.
    /// What: Renders the same lowercase strings the JSON schema's `enum` lists.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            FinishStatus::Completed => "completed",
            FinishStatus::Failed => "failed",
            FinishStatus::Cancelled => "cancelled",
        };
        write!(f, "{s}")
    }
}

/// One entry of the optional `changes` array.
///
/// Why: Lets the model report which files it touched without the caller having
/// to re-parse a diff out of prose. Every field is optional (including `file`)
/// because the JSON schema does not mark any of them `required` inside the
/// array item — a partially-populated entry must still deserialise rather than
/// hard-failing `execute` for something the schema-level validator already let
/// through.
/// What: `file`, `lines_added`, `lines_removed` — all `Option`.
/// Test: `tests::execute_with_changes_and_tests_renders_all_fields`.
#[derive(Debug, Clone, Deserialize)]
pub struct ChangeEntry {
    /// Path of the changed file, if reported.
    pub file: Option<String>,
    /// Lines added, if reported.
    pub lines_added: Option<i64>,
    /// Lines removed, if reported.
    pub lines_removed: Option<i64>,
}

/// Parsed, validated arguments to a `finish_task` call.
///
/// Why: A typed shape is easier for both `execute` and `agent_loop`'s
/// loop-termination path to work with than a raw `serde_json::Value`.
/// What: `status` and `summary` are required (matching the schema's `required`
/// array); `changes` defaults to empty when absent; `tests_run`/`tests_passed`
/// are optional integers. `tests_run`/`tests_passed` are signed (`i64`) rather
/// than unsigned so an out-of-range value the JSON-Schema-subset validator does
/// not itself reject (it declares no `minimum`) still deserialises instead of
/// hard-failing `execute` for a shape the schema layer already accepted.
/// Test: `tests::execute_valid_args_renders_summary`,
/// `tests::execute_with_changes_and_tests_renders_all_fields`.
#[derive(Debug, Clone, Deserialize)]
pub struct FinishTaskArgs {
    /// Completion status.
    pub status: FinishStatus,
    /// Free-text summary of what was done.
    pub summary: String,
    /// Files/changes made, if reported.
    #[serde(default)]
    pub changes: Vec<ChangeEntry>,
    /// Number of tests run, if reported.
    pub tests_run: Option<i64>,
    /// Number of tests that passed, if reported.
    pub tests_passed: Option<i64>,
}

/// Render a [`FinishTaskArgs`] into human-readable summary text.
///
/// Why: Shared by [`FinishTaskTool::execute`] (so the tool's own `ToolResult`
/// content is meaningful) AND `agent_loop::build_finish_output` (so the final
/// `AgentOutput.content` on explicit finish reflects the SAME structured
/// report) — one rendering, no drift between the two call sites.
/// What: `"Task <status>: <summary>"` followed by an optional `Changes:` list
/// and an optional `Tests:` line.
/// Test: `tests::render_includes_status_and_summary`,
/// `tests::execute_with_changes_and_tests_renders_all_fields`.
pub fn render_finish_summary(args: &FinishTaskArgs) -> String {
    let mut out = format!("Task {}: {}", args.status, args.summary);

    if !args.changes.is_empty() {
        out.push_str("\nChanges:");
        for change in &args.changes {
            let file = change.file.as_deref().unwrap_or("<unknown file>");
            let added = change.lines_added.unwrap_or(0);
            let removed = change.lines_removed.unwrap_or(0);
            out.push_str(&format!("\n  - {file} (+{added}/-{removed})"));
        }
    }

    match (args.tests_run, args.tests_passed) {
        (Some(run), Some(passed)) => out.push_str(&format!("\nTests: {passed}/{run} passed")),
        (Some(run), None) => out.push_str(&format!("\nTests run: {run}")),
        (None, Some(passed)) => out.push_str(&format!("\nTests passed: {passed}")),
        (None, None) => {}
    }

    out
}

/// `ToolExecutor` for the `finish_task` tool (#2072, §5.8).
///
/// Why: Gives the model an explicit, schema-validated way to signal completion
/// instead of relying solely on the implicit no-tool-call convention.
/// What: Stateless — `execute` only formats its (already schema-validated)
/// arguments; it has no side effects on the filesystem, network, or process.
/// Test: `tests::*`.
#[derive(Debug, Default)]
pub struct FinishTaskTool;

impl FinishTaskTool {
    /// Construct a new `FinishTaskTool`.
    ///
    /// Why: Matches the `Tool::new()` constructor convention every other tool in
    /// this module follows, even though there is no state to inject.
    /// What: Returns `Self`.
    /// Test: `tests::execute_valid_args_renders_summary`.
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ToolExecutor for FinishTaskTool {
    fn name(&self) -> &str {
        FINISH_TASK_TOOL_NAME
    }

    /// JSON schema for `finish_task`, matching §5.8 of the vision spec exactly.
    ///
    /// Why: This is the schema `ToolRegistry::schema_for` hands to
    /// `llm::ToolCallExtractor` (#1023), which validates every call's arguments
    /// against it before `execute` ever runs.
    /// What: `status` (enum, required), `summary` (string, required), `changes`
    /// (optional array of `{file, lines_added, lines_removed}`), `tests_run` /
    /// `tests_passed` (optional integers). `additionalProperties: false` matches
    /// this crate's convention for every other tool's schema.
    /// Test: `tests::schema_has_required_fields_and_enum`.
    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": FINISH_TASK_TOOL_NAME,
                "description": "Signal task completion with a structured, machine-readable report. Call this exactly once when the task is finished (successfully, failed, or cancelled) instead of ending your turn with prose.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "status": {
                            "type": "string",
                            "enum": ["completed", "failed", "cancelled"],
                            "description": "Outcome of the task."
                        },
                        "summary": {
                            "type": "string",
                            "description": "Free-text summary of what was done."
                        },
                        "changes": {
                            "type": "array",
                            "description": "Files changed, if any.",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "file": { "type": "string" },
                                    "lines_added": { "type": "integer" },
                                    "lines_removed": { "type": "integer" }
                                }
                            }
                        },
                        "tests_run": {
                            "type": "integer",
                            "description": "Number of tests run, if applicable."
                        },
                        "tests_passed": {
                            "type": "integer",
                            "description": "Number of tests that passed, if applicable."
                        }
                    },
                    "required": ["status", "summary"],
                    "additionalProperties": false
                }
            }
        })
    }

    /// Render the (already schema-validated) arguments into a `ToolResult`.
    ///
    /// Why: By the time `execute` runs, `agent_loop::dispatch_all` has already
    /// validated `args` against [`Self::schema`] via #1023's
    /// `ToolCallExtractor::parse_and_validate` — a missing required field or an
    /// invalid `status` enum value never reaches here at all; those are caught
    /// upstream and fed back through the SAME recoverable-error + repair path
    /// every other tool uses. The `Err` branch below is therefore a defensive
    /// fallback (no `unwrap`/`panic`) for a shape the loose JSON-Schema-subset
    /// validator accepted but strict `serde` deserialisation still rejects —
    /// exercised directly in `tests::execute_bypassing_schema_validation_is_recoverable`.
    /// What: `Ok` → `ToolResult::ok(render_finish_summary(&parsed))`. `Err` →
    /// `ToolResult::err` naming the deserialisation failure so the model can
    /// retry with a corrected call.
    /// Test: `tests::execute_valid_args_renders_summary`,
    /// `tests::execute_with_changes_and_tests_renders_all_fields`,
    /// `tests::execute_bypassing_schema_validation_is_recoverable`.
    async fn execute(&self, args: Value) -> ToolResult {
        match serde_json::from_value::<FinishTaskArgs>(args) {
            Ok(parsed) => ToolResult::ok(render_finish_summary(&parsed)),
            Err(e) => ToolResult::err(format!(
                "finish_task arguments did not match the expected shape ({e}). \
                 'status' must be one of completed/failed/cancelled and 'summary' must be a string."
            )),
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// `FinishTaskTool::name()` matches the exported constant.
    ///
    /// Why: `agent_loop`'s finish-detection check compares against
    /// `FINISH_TASK_TOOL_NAME`; a drift here would silently break termination.
    /// What: Trivial equality assertion.
    /// Test: this test.
    #[test]
    fn name_matches_constant() {
        let tool = FinishTaskTool::new();
        assert_eq!(tool.name(), FINISH_TASK_TOOL_NAME);
        assert_eq!(FINISH_TASK_TOOL_NAME, "finish_task");
    }

    /// The schema declares the exact `required` set and `status` enum from §5.8.
    ///
    /// Why: Guards the schema contract the extractor's generic validator relies
    /// on — a drift here would silently change what's rejected upstream.
    /// What: Assert `required == ["status", "summary"]` and the enum values.
    /// Test: this test.
    #[test]
    fn schema_has_required_fields_and_enum() {
        let schema = FinishTaskTool::new().schema();
        let params = &schema["function"]["parameters"];
        let required: Vec<&str> = params["required"]
            .as_array()
            .expect("required array")
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert_eq!(required, vec!["status", "summary"]);

        let enum_values: Vec<&str> = params["properties"]["status"]["enum"]
            .as_array()
            .expect("enum array")
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert_eq!(enum_values, vec!["completed", "failed", "cancelled"]);

        assert_eq!(params["additionalProperties"], json!(false));
    }

    /// `FinishStatus` renders its lowercase wire form via `Display`.
    ///
    /// Why: `render_finish_summary` depends on this exact casing.
    /// What: Format each variant, assert the lowercase string.
    /// Test: this test.
    #[test]
    fn status_display_and_roundtrip() {
        assert_eq!(FinishStatus::Completed.to_string(), "completed");
        assert_eq!(FinishStatus::Failed.to_string(), "failed");
        assert_eq!(FinishStatus::Cancelled.to_string(), "cancelled");
    }

    /// `render_finish_summary` includes the status and summary text.
    ///
    /// Why: Guard the minimal happy-path rendering before layering on optional
    /// fields.
    /// What: A `FinishTaskArgs` with no changes/tests; assert the rendered text.
    /// Test: this test.
    #[test]
    fn render_includes_status_and_summary() {
        let args = FinishTaskArgs {
            status: FinishStatus::Completed,
            summary: "did the thing".to_string(),
            changes: vec![],
            tests_run: None,
            tests_passed: None,
        };
        let rendered = render_finish_summary(&args);
        assert_eq!(rendered, "Task completed: did the thing");
    }

    /// A valid, minimal `finish_task` call executes and renders the summary.
    ///
    /// Why: The primary happy path — only the two required fields present.
    /// What: `execute({"status": "completed", "summary": "..."})`.
    /// Test: this test.
    #[tokio::test]
    async fn execute_valid_args_renders_summary() {
        let tool = FinishTaskTool::new();
        let result = tool
            .execute(json!({"status": "completed", "summary": "wrote the feature"}))
            .await;
        assert!(!result.is_error());
        assert_eq!(result.content(), "Task completed: wrote the feature");
    }

    /// A call with `changes` and both test-count fields renders every section.
    ///
    /// Why: Guards the full-shape rendering path, not just the minimal one.
    /// What: `execute` with two changes entries and matching test counts.
    /// Test: this test.
    #[tokio::test]
    async fn execute_with_changes_and_tests_renders_all_fields() {
        let tool = FinishTaskTool::new();
        let result = tool
            .execute(json!({
                "status": "completed",
                "summary": "added the widget",
                "changes": [
                    {"file": "src/widget.rs", "lines_added": 42, "lines_removed": 3},
                    {"file": "src/lib.rs", "lines_added": 1, "lines_removed": 0}
                ],
                "tests_run": 12,
                "tests_passed": 12
            }))
            .await;
        assert!(!result.is_error());
        let content = result.content();
        assert!(content.contains("Task completed: added the widget"));
        assert!(content.contains("src/widget.rs (+42/-3)"));
        assert!(content.contains("src/lib.rs (+1/-0)"));
        assert!(content.contains("Tests: 12/12 passed"));
    }

    /// A `failed` status with only `tests_run` (no `tests_passed`) renders that
    /// single line, not the combined `x/y passed` form.
    ///
    /// Why: Guards the partial-test-info branch of the match in
    /// `render_finish_summary`.
    /// What: `execute` with `tests_run` only.
    /// Test: this test.
    #[tokio::test]
    async fn execute_with_only_tests_run_renders_run_count() {
        let tool = FinishTaskTool::new();
        let result = tool
            .execute(json!({
                "status": "failed",
                "summary": "tests did not all pass",
                "tests_run": 5
            }))
            .await;
        assert!(!result.is_error());
        assert!(result.content().contains("Tests run: 5"));
        assert!(!result.content().contains("passed\n"));
    }

    /// Arguments that bypass schema validation (e.g. called directly, as in
    /// this test) but do not match `FinishTaskArgs`'s shape return a
    /// recoverable error, never a panic.
    ///
    /// Why: `execute` must never `unwrap`/panic on attacker- or model-controlled
    /// input, even though in the real dispatch path this shape would already
    /// have been rejected by `schema::validate_args` before reaching here.
    /// What: `execute({"status": "completed"})` — missing the required
    /// `summary` field that `FinishTaskArgs` (unlike the schema's own leniency
    /// elsewhere) requires as non-optional.
    /// Test: this test.
    #[tokio::test]
    async fn execute_bypassing_schema_validation_is_recoverable() {
        let tool = FinishTaskTool::new();
        let result = tool.execute(json!({"status": "completed"})).await;
        assert!(result.is_error());
        assert!(!result.is_fatal(), "must be recoverable, not fatal");
        assert!(
            result
                .content()
                .contains("did not match the expected shape")
        );
    }

    /// An unrecognised `status` value bypassing schema validation also reports
    /// a recoverable error rather than panicking.
    ///
    /// Why: Guards the enum-deserialisation failure path specifically.
    /// What: `execute({"status": "in_progress", "summary": "..."})`.
    /// Test: this test.
    #[tokio::test]
    async fn execute_unknown_status_bypassing_schema_validation_is_recoverable() {
        let tool = FinishTaskTool::new();
        let result = tool
            .execute(json!({"status": "in_progress", "summary": "still going"}))
            .await;
        assert!(result.is_error());
        assert!(!result.is_fatal());
    }
}
