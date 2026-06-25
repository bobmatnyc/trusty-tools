//! Integration tests for the `task_add`, `task_list`, and `task_complete` MCP
//! tools (spec-001 Phase 4, issue #1722).
//!
//! Why: closes the acceptance gap — `DrawerType::Task` drawers existed at the
//! library layer but were unreachable over MCP. These tests drive all three tools
//! through `dispatch_tool` and verify that the dream-cycle protection holds
//! end-to-end via the public MCP surface.
//! What: creates a palace, adds Task drawers via `task_add`, verifies `task_list`
//! returns them, runs a full `dream_cycle` directly on the palace handle,
//! asserts the Task drawer survives while a low-importance normal drawer is
//! pruned, and verifies `task_complete` sets `completed_at` and updates the list.
//! Test: this IS the test module.

use chrono::Utc;
use serde_json::json;
use tempfile::TempDir;
use trusty_common::memory_core::dream::{DreamConfig, Dreamer};
use trusty_common::memory_core::palace::{DrawerType, Palace, PalaceId};
use trusty_common::memory_core::retrieval::{seed_shared_embedder_with_mock, PalaceHandle};
use trusty_memory::tools::dispatch_tool;
use trusty_memory::AppState;

/// Build a ready `AppState` rooted at `root`.
///
/// Why: all tests below need a ready state against a fresh tempdir.
/// What: constructs `AppState` and calls `set_ready()` to skip the embedder
/// warm-up gate (tests seed the mock embedder instead).
/// Test: used by every test below.
fn ready_state(tmp: &TempDir) -> AppState {
    let state = AppState::new(tmp.path().to_path_buf());
    state.set_ready();
    state
}

/// `task_add` creates a Task drawer through the MCP path; `task_list` returns it.
///
/// Why: end-to-end smoke test for the happy path (issue #1722).
/// What: creates a palace, calls task_add, verifies the response shape, then
/// calls task_list and checks the drawer appears with the correct fields.
/// Test: this IS the test.
#[tokio::test]
async fn task_add_and_list_via_mcp() {
    seed_shared_embedder_with_mock();
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = ready_state(&tmp);
    let cwd = tmp.path().to_string_lossy().to_string();

    dispatch_tool(
        &state,
        "palace_create",
        json!({ "name": "tasktest", "force": true, "cwd": cwd }),
    )
    .await
    .expect("create palace");

    // Create a Task drawer via MCP.
    let result = dispatch_tool(
        &state,
        "task_add",
        json!({
            "palace": "tasktest",
            "content": "Ship v2 of the API before Friday",
            "tags": ["deadline", "api"]
        }),
    )
    .await
    .expect("task_add");

    assert_eq!(result["status"], "stored", "task_add must report stored");
    assert_eq!(
        result["drawer_type"], "Task",
        "task_add must report drawer_type=Task"
    );
    let drawer_id = result["drawer_id"].as_str().expect("drawer_id").to_string();
    assert!(!drawer_id.is_empty(), "drawer_id must be non-empty");

    // task_list returns the open task.
    let list = dispatch_tool(&state, "task_list", json!({ "palace": "tasktest" }))
        .await
        .expect("task_list");
    let tasks = list["tasks"].as_array().expect("tasks array");
    assert_eq!(tasks.len(), 1, "exactly one task should be listed");
    assert_eq!(tasks[0]["drawer_id"], drawer_id, "drawer_id must match");
    assert_eq!(
        tasks[0]["content"], "Ship v2 of the API before Friday",
        "content must round-trip"
    );
    assert!(
        tasks[0]["completed_at"].is_null(),
        "open task must have null completed_at"
    );
    assert_eq!(
        tasks[0]["drawer_type"], "Task",
        "listed drawer must have drawer_type=Task"
    );
}

/// A Task drawer created via MCP survives a full `dream_cycle` that prunes a
/// low-importance normal drawer. End-to-end acceptance test for issue #1722.
///
/// Why: verifies `DrawerType::Task.is_protected()` is actually honoured by the
/// dream cycle when the drawer was created through the MCP tool path.
/// What: creates two drawers (one Task via MCP, one normal via MCP), back-dates
/// and lowers the importance of the normal drawer via the in-memory handle so
/// it falls below `prune_importance`, then runs `Dreamer::dream_cycle` directly.
/// Asserts the normal drawer is pruned and the Task drawer survives.
/// Test: this IS the test.
#[tokio::test]
async fn task_drawer_survives_dream_cycle() {
    seed_shared_embedder_with_mock();
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = ready_state(&tmp);
    let cwd = tmp.path().to_string_lossy().to_string();

    dispatch_tool(
        &state,
        "palace_create",
        json!({ "name": "dreamtask", "force": true, "cwd": cwd }),
    )
    .await
    .expect("create palace");

    // Add a Task drawer via MCP.
    let task_result = dispatch_tool(
        &state,
        "task_add",
        json!({
            "palace": "dreamtask",
            "content": "Deploy to production by end of sprint"
        }),
    )
    .await
    .expect("task_add");
    let task_id = task_result["drawer_id"]
        .as_str()
        .expect("task drawer_id")
        .to_string();

    // Add a normal (non-task) drawer via MCP with force so the content filter
    // is bypassed for this deliberately short test string.
    dispatch_tool(
        &state,
        "memory_remember",
        json!({
            "palace": "dreamtask",
            "text": "Stale unimportant note that should be pruned by the dream cycle cleanup pass",
            "force": true,
        }),
    )
    .await
    .expect("memory_remember");

    // Open the palace handle from the same data root to manipulate drawer
    // metadata directly (back-date + lower importance so the prune pass fires).
    let palace_obj = Palace {
        id: PalaceId::new("dreamtask"),
        name: "dreamtask".into(),
        description: None,
        created_at: Utc::now(),
        data_dir: tmp.path().join("dreamtask"),
    };
    let handle = PalaceHandle::open(&palace_obj).expect("open palace handle");

    // Artificially age the non-Task drawer so it falls past the 30-day prune
    // threshold and lower its importance below `prune_importance` (0.05 default).
    {
        let mut drawers = handle.drawers.write();
        for d in drawers.iter_mut() {
            if d.drawer_type != DrawerType::Task {
                d.created_at = Utc::now() - chrono::Duration::days(60);
                d.importance = 0.01;
            }
        }
    }

    let total_before = handle.drawers.read().len();
    assert_eq!(total_before, 2, "should have 2 drawers before dream cycle");

    // Run the dream cycle with no inference backend so it stays deterministic.
    // The prune pass fires because the normal drawer is aged + low-importance;
    // the Task drawer is protected by `is_protected()` and must survive.
    let dreamer = Dreamer::new(DreamConfig {
        dedup_threshold: 0.999, // very high threshold to avoid dedup false positives
        recall_benchmark_enabled: false, // skip benchmark for speed
        ..DreamConfig::default()
    });
    let stats = dreamer
        .dream_cycle(&handle)
        .await
        .expect("dream_cycle must not error");

    // At least the aged normal drawer should have been pruned.
    assert!(
        stats.pruned >= 1,
        "expected at least one normal drawer to be pruned; got stats: pruned={}, merged={}",
        stats.pruned,
        stats.merged
    );

    // The Task drawer must still be present.
    let survivors = handle.drawers.read();
    let task_survivor = survivors.iter().find(|d| d.id.to_string() == task_id);
    assert!(
        task_survivor.is_some(),
        "the Task drawer must survive the dream cycle"
    );
    assert_eq!(
        task_survivor.unwrap().drawer_type,
        DrawerType::Task,
        "surviving drawer must still be Task type"
    );
}

/// `task_complete` sets `completed_at`; the task no longer appears in the
/// default `task_list` but does appear with `include_completed=true`.
///
/// Why: verifies the completion lifecycle: mark done, hide from default list,
/// visible with flag (issue #1722).
/// What: creates a task, completes it, checks default list (empty) and then
/// include_completed list (one result with completed_at set).
/// Test: this IS the test.
#[tokio::test]
async fn task_complete_sets_completed_at() {
    seed_shared_embedder_with_mock();
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = ready_state(&tmp);
    let cwd = tmp.path().to_string_lossy().to_string();

    dispatch_tool(
        &state,
        "palace_create",
        json!({ "name": "completetest", "force": true, "cwd": cwd }),
    )
    .await
    .expect("create palace");

    let result = dispatch_tool(
        &state,
        "task_add",
        json!({
            "palace": "completetest",
            "content": "Write the release notes for v2"
        }),
    )
    .await
    .expect("task_add");
    let drawer_id = result["drawer_id"].as_str().expect("drawer_id").to_string();

    // Before completing: appears in default list.
    let list_before = dispatch_tool(&state, "task_list", json!({ "palace": "completetest" }))
        .await
        .expect("task_list before");
    assert_eq!(
        list_before["tasks"].as_array().expect("tasks").len(),
        1,
        "open task should appear in default list"
    );

    // Complete the task.
    let completed = dispatch_tool(
        &state,
        "task_complete",
        json!({ "palace": "completetest", "drawer_id": drawer_id }),
    )
    .await
    .expect("task_complete");
    assert_eq!(
        completed["status"], "completed",
        "task_complete must report status=completed"
    );
    assert_eq!(
        completed["drawer_id"], drawer_id,
        "task_complete must echo back the drawer_id"
    );
    assert!(
        !completed["completed_at"].is_null(),
        "task_complete must set completed_at"
    );

    // After completing: NOT in default list.
    let list_after = dispatch_tool(&state, "task_list", json!({ "palace": "completetest" }))
        .await
        .expect("task_list after");
    assert_eq!(
        list_after["tasks"].as_array().expect("tasks").len(),
        0,
        "completed task must not appear in default list (include_completed defaults to false)"
    );

    // With include_completed=true: appears.
    let list_with = dispatch_tool(
        &state,
        "task_list",
        json!({ "palace": "completetest", "include_completed": true }),
    )
    .await
    .expect("task_list with include_completed");
    let tasks = list_with["tasks"].as_array().expect("tasks");
    assert_eq!(
        tasks.len(),
        1,
        "completed task must appear when include_completed=true"
    );
    assert!(
        !tasks[0]["completed_at"].is_null(),
        "completed task must have a non-null completed_at"
    );
}

/// `task_complete` rejects drawers that are not Task type.
///
/// Why: ensures the type check guards against callers accidentally completing
/// arbitrary drawers — only Task drawers should be completable via this tool.
/// What: creates a normal (UserFact) drawer via `memory_note`, then calls
/// `task_complete` with that drawer's id and asserts the call errors with a
/// message containing "not a Task".
/// Test: this IS the test.
#[tokio::test]
async fn task_complete_rejects_non_task_drawer() {
    seed_shared_embedder_with_mock();
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = ready_state(&tmp);
    let cwd = tmp.path().to_string_lossy().to_string();

    dispatch_tool(
        &state,
        "palace_create",
        json!({ "name": "notasktest", "force": true, "cwd": cwd }),
    )
    .await
    .expect("create palace");

    // Create a normal UserFact drawer via memory_note.
    let note_result = dispatch_tool(
        &state,
        "memory_note",
        json!({
            "palace": "notasktest",
            "content": "This is a regular fact, not a task"
        }),
    )
    .await
    .expect("memory_note");
    let note_id = note_result["drawer_id"].as_str().expect("drawer_id");

    // task_complete must reject it.
    let err = dispatch_tool(
        &state,
        "task_complete",
        json!({ "palace": "notasktest", "drawer_id": note_id }),
    )
    .await;
    assert!(
        err.is_err(),
        "task_complete must error on a non-Task drawer"
    );
    let msg = err.unwrap_err().to_string();
    assert!(
        msg.contains("not a Task"),
        "error message must mention 'not a Task', got: {msg}"
    );
}

/// `task_complete` errors on an unknown drawer_id.
///
/// Why: ensures the not-found path returns a clear error rather than silently
/// succeeding or panicking (issue #1722).
/// What: calls task_complete with a random UUID that was never stored.
/// Test: this IS the test.
#[tokio::test]
async fn task_complete_errors_on_missing_drawer() {
    seed_shared_embedder_with_mock();
    let tmp = tempfile::tempdir().expect("tempdir");
    let state = ready_state(&tmp);
    let cwd = tmp.path().to_string_lossy().to_string();

    dispatch_tool(
        &state,
        "palace_create",
        json!({ "name": "missingtest", "force": true, "cwd": cwd }),
    )
    .await
    .expect("create palace");

    let fake_id = "00000000-0000-0000-0000-000000000000";
    let err = dispatch_tool(
        &state,
        "task_complete",
        json!({ "palace": "missingtest", "drawer_id": fake_id }),
    )
    .await;
    assert!(
        err.is_err(),
        "task_complete with unknown drawer_id must error"
    );
    let msg = err.unwrap_err().to_string();
    assert!(
        msg.contains("not found"),
        "error message must mention 'not found', got: {msg}"
    );
}
