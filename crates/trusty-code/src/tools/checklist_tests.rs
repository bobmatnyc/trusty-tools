//! Unit tests for the `todo_write` tool (#8235) — schema shape, every
//! refusal, and the fail-open check that a refused call never reaches the
//! store. The round trip out through `session.get_agents` lives in
//! `task::todo_store::tests` instead, because only that layer has a registry.

use std::sync::Mutex;

use super::*;

/// A [`TodoStore`] double recording every accepted write, optionally failing.
///
/// Why: `rejected_call_never_reaches_the_store` needs to prove a NEGATIVE —
/// that no write happened — which needs a store that can be asked.
struct RecordingStore {
    writes: Mutex<Vec<Vec<TodoItem>>>,
    fail: Option<String>,
}

impl RecordingStore {
    fn new() -> Self {
        Self {
            writes: Mutex::new(Vec::new()),
            fail: None,
        }
    }

    fn failing(reason: &str) -> Self {
        Self {
            writes: Mutex::new(Vec::new()),
            fail: Some(reason.to_string()),
        }
    }

    fn writes(&self) -> Vec<Vec<TodoItem>> {
        self.writes.lock().expect("test store lock").clone()
    }
}

impl TodoStore for RecordingStore {
    fn replace(&self, todos: Vec<TodoItem>) -> Result<(), TodoError> {
        if let Some(reason) = &self.fail {
            return Err(TodoError::Unavailable(reason.clone()));
        }
        self.writes.lock().expect("test store lock").push(todos);
        Ok(())
    }
}

/// Build `{ "todos": [...] }` from `(content, status)` pairs.
fn args(items: &[(&str, &str)]) -> Value {
    json!({
        "todos": items
            .iter()
            .map(|(content, status)| json!({"content": content, "status": status}))
            .collect::<Vec<_>>()
    })
}

fn raw(items: &[(&str, &str)]) -> Vec<RawTodo> {
    items
        .iter()
        .map(|(content, status)| RawTodo {
            content: (*content).to_string(),
            status: (*status).to_string(),
        })
        .collect()
}

// ── schema ───────────────────────────────────────────────────────────────────

/// The tool advertises a registered schema naming `todos` and all three
/// statuses — #4602's failure mode was a tool named in a prompt with no
/// schema, so the schema's shape is part of the contract.
#[test]
fn schema_declares_todos_and_the_three_statuses() {
    let tool = TodoWriteTool::new(Arc::new(RecordingStore::new()));
    let schema = tool.schema();

    assert_eq!(schema["function"]["name"], TODO_WRITE_TOOL_NAME);
    assert_eq!(tool.name(), "todo_write");
    let props = &schema["function"]["parameters"]["properties"]["todos"];
    assert_eq!(props["type"], "array");
    assert_eq!(
        schema["function"]["parameters"]["required"],
        json!(["todos"])
    );
    let item = &props["items"];
    assert_eq!(item["required"], json!(["content", "status"]));
    assert_eq!(
        item["properties"]["status"]["enum"],
        json!(["pending", "in_progress", "completed"])
    );
    let description = schema["function"]["description"]
        .as_str()
        .expect("description is a string");
    assert!(
        description.contains("replaces the stored one"),
        "the description must tell the model the call REPLACES the list: {description}"
    );
}

// ── validation ───────────────────────────────────────────────────────────────

/// A well-formed list parses, preserving order and trimming content.
#[test]
fn accepts_a_valid_list() {
    let parsed = parse_todos(raw(&[
        ("  add the --json flag  ", "completed"),
        ("add a test", "in_progress"),
        ("add a changelog line", "pending"),
    ]))
    .expect("valid list");

    assert_eq!(parsed.len(), 3);
    assert_eq!(parsed[0].content, "add the --json flag");
    assert_eq!(parsed[0].status, TodoStatus::Completed);
    assert_eq!(parsed[1].status, TodoStatus::InProgress);
    assert_eq!(parsed[2].status, TodoStatus::Pending);
}

/// An empty list is valid — it is how the model clears the checklist.
#[test]
fn accepts_an_empty_list() {
    assert_eq!(parse_todos(vec![]).expect("empty list"), vec![]);
}

/// An unknown status is refused, naming the item and the three valid values.
#[test]
fn rejects_unknown_status() {
    let err = parse_todos(raw(&[("a", "pending"), ("b", "done")])).expect_err("must refuse");

    assert_eq!(
        err,
        TodoError::UnknownStatus {
            index: 1,
            status: "done".to_string()
        }
    );
    let text = err.to_string();
    assert!(text.contains("in_progress"), "not actionable: {text}");
}

/// Empty or whitespace-only content is refused, naming the item.
#[test]
fn rejects_empty_content() {
    let err = parse_todos(raw(&[("a", "pending"), ("   ", "pending")])).expect_err("must refuse");

    assert_eq!(err, TodoError::EmptyContent { index: 1 });
}

/// Two `in_progress` steps are refused — the rule this tool adopts is at most
/// one step in flight, so the roster always has one unambiguous "now".
#[test]
fn rejects_two_in_progress() {
    let err =
        parse_todos(raw(&[("a", "in_progress"), ("b", "in_progress")])).expect_err("must refuse");

    assert_eq!(err, TodoError::MultipleInProgress { count: 2 });
}

/// A list longer than `MAX_TODOS` is refused rather than retained.
#[test]
fn rejects_more_than_max_todos() {
    let items: Vec<(&str, &str)> = (0..MAX_TODOS + 1).map(|_| ("step", "pending")).collect();

    let err = parse_todos(raw(&items)).expect_err("must refuse");

    assert_eq!(
        err,
        TodoError::TooManyItems {
            count: MAX_TODOS + 1
        }
    );
}

// ── dispatch ─────────────────────────────────────────────────────────────────

/// A valid call stores the validated list and reports what it stored.
#[tokio::test]
async fn valid_call_reaches_the_store() {
    let store = Arc::new(RecordingStore::new());
    let tool = TodoWriteTool::new(store.clone());

    let result = tool
        .execute(args(&[
            ("write the test", "in_progress"),
            ("run it", "pending"),
        ]))
        .await;

    assert!(!result.is_error(), "{result:?}");
    let writes = store.writes();
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].len(), 2);
    assert_eq!(writes[0][0].content, "write the test");
}

/// THE fail-open check: a refused call must not write anything. A tool that
/// validated after writing, or that reported success on a dropped write,
/// would pass every other test in this file and fail this one.
#[tokio::test]
async fn rejected_call_never_reaches_the_store() {
    let store = Arc::new(RecordingStore::new());
    let tool = TodoWriteTool::new(store.clone());

    let result = tool
        .execute(args(&[("a", "in_progress"), ("b", "in_progress")]))
        .await;

    assert!(result.is_error(), "two in_progress must be refused");
    assert!(
        store.writes().is_empty(),
        "a refused call wrote to the store: {:?}",
        store.writes()
    );
}

/// A store failure surfaces as a recoverable tool error, never a success.
#[tokio::test]
async fn store_failure_is_a_recoverable_tool_error() {
    let tool = TodoWriteTool::new(Arc::new(RecordingStore::failing("session ended")));

    let result = tool.execute(args(&[("a", "pending")])).await;

    assert!(result.is_error());
    match result {
        ToolResult::Error {
            message,
            recoverable,
        } => {
            assert!(recoverable);
            assert!(message.contains("session ended"), "{message}");
            assert!(message.contains("unchanged"), "{message}");
        }
        other => panic!("expected an error result: {other:?}"),
    }
}

/// A malformed argument shape is a recoverable error naming the shape wanted.
#[tokio::test]
async fn malformed_args_are_a_recoverable_error() {
    let store = Arc::new(RecordingStore::new());
    let tool = TodoWriteTool::new(store.clone());

    let result = tool.execute(json!({"todos": "write the test"})).await;

    assert!(result.is_error());
    assert!(store.writes().is_empty());
}

// ── feedback text ────────────────────────────────────────────────────────────

/// The success text names the in-progress step, which is the one fact the
/// model's next turn acts on.
#[test]
fn success_text_names_the_in_progress_step() {
    let todos = parse_todos(raw(&[
        ("add the flag", "completed"),
        ("add a test", "in_progress"),
    ]))
    .expect("valid");

    let text = summarise(&todos);

    assert!(text.contains("1 of 2 completed"), "{text}");
    assert!(text.contains("In progress: add a test"), "{text}");
}

/// Clearing the list says so rather than reporting zero counts.
#[test]
fn success_text_reports_an_empty_list() {
    assert_eq!(summarise(&[]), "Checklist cleared.");
}
