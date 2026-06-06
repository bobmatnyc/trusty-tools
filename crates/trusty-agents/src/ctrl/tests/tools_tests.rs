//! Tests for the ctrl tool executors (self-project, task-status, project/fs
//! tools introduced in #202) and project-detection helpers.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde_json::json;
use tokio::sync::mpsc;

use super::super::handlers::projects::{detect_stack, is_empty_project};
use super::super::handlers::{
    AddProjectTool, CreateDirTool, InitiateSelfTaskTool, MoveFileTool, PmStopHandle,
    RemoveProjectTool, SelfProjectStatusTool, SetActiveProjectTool, StartPmTool, StopTaskTool,
    TaskStatusTool,
};
use super::super::state::PmMsg;
use super::super::util::detect_self_project;
use super::make_fake_self_project;

use crate::tools::traits::ToolExecutor;

// -- #182: self-project detection + tools --

#[test]
fn detect_self_project_finds_via_env_var() {
    let tmp = tempfile::tempdir().unwrap();
    let root = make_fake_self_project(tmp.path());
    // SAFETY: tests run single-threaded within this process by default.
    unsafe {
        std::env::set_var("TAGENT_PROJECT_DIR", &root);
    }
    let detected = detect_self_project();
    unsafe {
        std::env::remove_var("TAGENT_PROJECT_DIR");
    }
    assert!(detected.is_some(), "expected detection via env var");
}

#[test]
fn detect_self_project_returns_none_when_no_marker() {
    let tmp = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("TAGENT_PROJECT_DIR", tmp.path());
    }
    let detected = detect_self_project();
    unsafe {
        std::env::remove_var("TAGENT_PROJECT_DIR");
    }
    let _ = detected;
}

#[tokio::test]
async fn self_project_status_returns_version_when_path_set() {
    let tmp = tempfile::tempdir().unwrap();
    let root = make_fake_self_project(tmp.path());
    let tool = SelfProjectStatusTool {
        self_path: Some(root.clone()),
    };
    let result = tool.execute(json!({})).await;
    assert!(!result.is_error(), "expected ok, got {}", result.content());
    assert!(
        result.content().contains("9.9.9"),
        "expected version in output: {}",
        result.content()
    );
}

#[tokio::test]
async fn self_project_status_errors_when_no_self_path() {
    let tool = SelfProjectStatusTool { self_path: None };
    let result = tool.execute(json!({})).await;
    assert!(result.is_error());
    assert!(result.content().contains("no self-project detected"));
}

#[tokio::test]
async fn initiate_self_task_queues_self_project_path() {
    let tmp = tempfile::tempdir().unwrap();
    let root = make_fake_self_project(tmp.path());
    let pending_connect: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let pending_self_task: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let tool = InitiateSelfTaskTool {
        self_path: Some(root.clone()),
        pending_connect: pending_connect.clone(),
        pending_self_task: pending_self_task.clone(),
    };
    let result = tool.execute(json!({ "task": "fix bug X" })).await;
    assert!(!result.is_error(), "expected ok, got {}", result.content());
    assert_eq!(
        pending_connect.lock().unwrap().as_deref(),
        Some(root.display().to_string().as_str())
    );
    assert_eq!(
        pending_self_task.lock().unwrap().as_deref(),
        Some("fix bug X")
    );
}

// -- #185: task_status tool --
#[tokio::test]
async fn task_status_returns_known_pm_state() {
    let status = Arc::new(Mutex::new("running".to_string()));
    let last = Arc::new(Mutex::new("write a fastapi app".to_string()));
    let tool = TaskStatusTool {
        snapshot: vec![("alpha".to_string(), status.clone(), last.clone())],
    };
    let result = tool.execute(json!({})).await;
    assert!(!result.is_error(), "expected ok, got {}", result.content());
    let body = result.content();
    assert!(body.contains("alpha"), "missing project name: {body}");
    assert!(body.contains("running"), "missing status: {body}");
    assert!(body.contains("fastapi"), "missing last_message: {body}");
}

#[tokio::test]
async fn initiate_self_task_errors_when_no_self_path() {
    let tool = InitiateSelfTaskTool {
        self_path: None,
        pending_connect: Arc::new(Mutex::new(None)),
        pending_self_task: Arc::new(Mutex::new(None)),
    };
    let result = tool.execute(json!({ "task": "x" })).await;
    assert!(result.is_error());
}

// -- #202 new CTRL tools --

#[tokio::test]
async fn add_project_tool_validates_path() {
    let tool = AddProjectTool;
    let r = tool.execute(json!({})).await;
    assert!(r.is_error(), "expected error for missing path");

    let r = tool
        .execute(json!({ "path": "/definitely/does/not/exist/zzzz" }))
        .await;
    assert!(r.is_error(), "expected error for missing dir");

    let cwd = std::env::current_dir().unwrap();
    let cargo = cwd.join("Cargo.toml");
    let r = tool
        .execute(json!({ "path": cargo.display().to_string() }))
        .await;
    assert!(r.is_error(), "expected error for path-is-file");
}

#[tokio::test]
async fn set_active_project_updates_slot() {
    let active = Arc::new(Mutex::new(None));
    let tool = SetActiveProjectTool {
        active_project: active.clone(),
    };
    let cwd = std::env::current_dir().unwrap();
    let r = tool
        .execute(json!({ "path": cwd.display().to_string() }))
        .await;
    assert!(!r.is_error(), "unexpected: {}", r.content());
    let stored = active.lock().unwrap().clone();
    assert!(stored.is_some());
    assert!(r.content().contains("Active project set"));
}

#[tokio::test]
async fn set_active_project_rejects_missing_path() {
    let tool = SetActiveProjectTool {
        active_project: Arc::new(Mutex::new(None)),
    };
    let r = tool.execute(json!({})).await;
    assert!(r.is_error());
    let r = tool
        .execute(json!({ "path": "/definitely/does/not/exist/zzzz" }))
        .await;
    assert!(r.is_error());
}

#[tokio::test]
async fn start_pm_falls_back_to_active_project() {
    let pending = Arc::new(Mutex::new(None));
    let active = Arc::new(Mutex::new(Some(PathBuf::from("/tmp/some-active"))));
    let tool = StartPmTool {
        pending: pending.clone(),
        active_project: active,
    };
    let r = tool.execute(json!({})).await;
    assert!(!r.is_error(), "unexpected: {}", r.content());
    let queued = pending.lock().unwrap().clone();
    assert_eq!(queued, Some("/tmp/some-active".to_string()));
}

#[tokio::test]
async fn start_pm_errors_when_no_path_and_no_active() {
    let tool = StartPmTool {
        pending: Arc::new(Mutex::new(None)),
        active_project: Arc::new(Mutex::new(None)),
    };
    let r = tool.execute(json!({})).await;
    assert!(r.is_error());
    assert!(r.content().contains("no active project"));
}

#[tokio::test]
async fn stop_task_tool_records_pending_stop() {
    let (tx, _rx) = mpsc::channel::<PmMsg>(4);
    let snapshot: Vec<PmStopHandle> = vec![("alpha".to_string(), "/tmp/alpha".to_string(), tx)];
    let pending = Arc::new(Mutex::new(None));
    let tool = StopTaskTool {
        snapshot,
        pending_stop: pending.clone(),
    };
    let r = tool.execute(json!({ "session_id": "alpha" })).await;
    assert!(!r.is_error(), "unexpected: {}", r.content());
    assert!(r.content().contains("stopped"));
    let queued = pending.lock().unwrap().clone();
    assert_eq!(queued, Some("alpha".to_string()));
}

#[tokio::test]
async fn stop_task_tool_returns_not_found_for_unknown_id() {
    let tool = StopTaskTool {
        snapshot: Vec::new(),
        pending_stop: Arc::new(Mutex::new(None)),
    };
    let r = tool.execute(json!({ "session_id": "missing" })).await;
    assert!(!r.is_error());
    assert!(r.content().contains("Task not found"));
}

#[tokio::test]
async fn remove_project_tool_returns_not_found_for_unknown_path() {
    let tool = RemoveProjectTool;
    let r = tool
        .execute(json!({ "path": "/zzz-trusty-agents-#202-no-such-project" }))
        .await;
    assert!(!r.is_error() || r.content().contains("registry unavailable"));
}

#[tokio::test]
async fn move_file_tool_renames_basic() {
    let tmp = tempfile::tempdir().unwrap();
    let from = tmp.path().join("a.txt");
    std::fs::write(&from, b"hello").unwrap();
    let to = tmp.path().join("b.txt");
    let tool = MoveFileTool;
    let r = tool
        .execute(json!({ "from": from.to_str().unwrap(), "to": to.to_str().unwrap() }))
        .await;
    assert!(!r.is_error(), "unexpected: {}", r.content());
    assert!(!from.exists());
    assert!(to.exists());
    assert_eq!(std::fs::read_to_string(&to).unwrap(), "hello");
}

#[tokio::test]
async fn move_file_tool_into_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let from = tmp.path().join("script.py");
    std::fs::write(&from, b"x").unwrap();
    let target_dir = tmp.path().join("scripts");
    std::fs::create_dir_all(&target_dir).unwrap();
    let tool = MoveFileTool;
    let r = tool
        .execute(json!({
            "from": from.to_str().unwrap(),
            "to": target_dir.to_str().unwrap()
        }))
        .await;
    assert!(!r.is_error(), "unexpected: {}", r.content());
    assert!(target_dir.join("script.py").exists());
    assert!(!from.exists());
}

#[tokio::test]
async fn move_file_tool_missing_source_errors() {
    let tool = MoveFileTool;
    let r = tool
        .execute(json!({
            "from": "/zzz-no-such-file-trusty-agents",
            "to": "/tmp/whatever"
        }))
        .await;
    assert!(r.is_error());
}

#[tokio::test]
async fn create_dir_tool_makes_nested_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let target = tmp.path().join("a").join("b").join("c");
    let tool = CreateDirTool;
    let r = tool
        .execute(json!({ "path": target.to_str().unwrap() }))
        .await;
    assert!(!r.is_error(), "unexpected: {}", r.content());
    assert!(target.is_dir());
}

#[tokio::test]
async fn create_dir_tool_idempotent_on_existing() {
    let tmp = tempfile::tempdir().unwrap();
    let tool = CreateDirTool;
    let r = tool
        .execute(json!({ "path": tmp.path().to_str().unwrap() }))
        .await;
    assert!(!r.is_error());
    assert!(r.content().contains("already exists"));
}

#[test]
fn detect_stack_finds_rust() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("Cargo.toml"), b"[package]\n").unwrap();
    assert_eq!(detect_stack(tmp.path()), "Rust");
}

#[test]
fn detect_stack_finds_node() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("package.json"), b"{}").unwrap();
    assert_eq!(detect_stack(tmp.path()), "Node.js/TypeScript");
}

#[test]
fn detect_stack_returns_unknown_when_no_indicators() {
    let tmp = tempfile::tempdir().unwrap();
    assert_eq!(detect_stack(tmp.path()), "unknown");
}

#[test]
fn is_empty_project_ignores_dotfiles() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join(".gitignore"), b"").unwrap();
    assert!(is_empty_project(tmp.path()));
    std::fs::write(tmp.path().join("README.md"), b"").unwrap();
    assert!(!is_empty_project(tmp.path()));
}
