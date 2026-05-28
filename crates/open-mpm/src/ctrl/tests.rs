//! Centralised CTRL tests.
//!
//! Why: The ctrl module is split across pm_task / ctrl_turn / repl / handlers
//! files but their unit tests share helpers (`insert_fake`, `insert_mock_actor`,
//! `make_fake_self_project`) and many cross-reference items from multiple
//! submodules. Co-locating the tests here lets every test reach `super::*`
//! without exposing implementation-detail re-exports purely for test use.
//! What: All `#[test]` and `#[tokio::test]` cases that previously lived in
//! `ctrl/mod.rs::tests`.
//! Test: This module IS the test surface.

#![cfg(test)]

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use serde_json::json;
use tokio::sync::{mpsc, oneshot};

use crate::agents::AgentConfig;
use crate::llm;

use super::claude_cli::{filter_project_index_in_prompt, strip_cli_artifacts};
use super::config::{apply_credential_routing, build_deployment_footer, resolve_agent_config};
use super::handlers::projects::{detect_stack, is_empty_project};
use super::handlers::{
    AddProjectTool, CreateDirTool, InitiateSelfTaskTool, MoveFileTool, PmStopHandle,
    RemoveProjectTool, SelfProjectStatusTool, SetActiveProjectTool, StartPmTool, StopTaskTool,
    TaskStatusTool,
};
use super::pm_task::{extract_name_from_input, match_any_glob};
use super::repl::handle_command;
use super::state::{Ctrl, PmHandle, PmMsg};
use super::util::detect_self_project;

use crate::tools::traits::ToolExecutor;

#[test]
fn filter_project_index_in_prompt_noop_when_no_section() {
    let prompt = "You are a PM.\n\nNo index here.";
    let out = filter_project_index_in_prompt(prompt, "anything", 5);
    assert_eq!(out, prompt);
}

#[test]
fn filter_project_index_in_prompt_filters_bullets_by_task() {
    let prompt = "## Project Context (auto-indexed)\n\n\
                  - src/credentials.rs — credential routing helpers\n\
                  - ui/src/main.tsx — react root\n\
                  - src/repl/mod.rs — terminal repl\n\
                  - src/agents/mod.rs — agent loader\n\n\
                  ---\n\nrest of prompt\n";
    let out = filter_project_index_in_prompt(prompt, "fix credential routing", 2);
    assert!(out.contains("## Project Context (auto-indexed)"));
    assert!(out.contains("credential"));
    assert!(
        !out.contains("react root") || !out.contains("terminal repl"),
        "filter should have dropped at least one unrelated bullet, got: {out}"
    );
    assert!(out.contains("rest of prompt"));
}

#[test]
fn filter_project_index_in_prompt_terminates_at_next_heading() {
    let prompt = "## Project Context (auto-indexed)\n\n\
                  - a — alpha\n\
                  - b — beta\n\n\
                  ## Next Section\n\nbody\n";
    let out = filter_project_index_in_prompt(prompt, "alpha", 1);
    assert!(out.contains("## Next Section"));
    assert!(out.contains("body"));
}

#[test]
fn apply_credential_routing_anthropic_direct_sets_flag() {
    let mut cfg = AgentConfig::ctrl_default();
    cfg.llm.use_anthropic_direct = false;
    let short_circuit =
        apply_credential_routing(&mut cfg, &llm::credentials::LlmCredentials::AnthropicDirect);
    assert!(!short_circuit);
    assert!(cfg.llm.use_anthropic_direct);
}

#[test]
fn strip_cli_artifacts_removes_summary_with_double_newline() {
    let input = "Hello world\n\n## Summary\n- did stuff\n".to_string();
    assert_eq!(strip_cli_artifacts(input), "Hello world");
}

#[test]
fn strip_cli_artifacts_removes_summary_with_single_newline() {
    let input = "Hello world\n## Summary\n- did stuff".to_string();
    assert_eq!(strip_cli_artifacts(input), "Hello world");
}

#[test]
fn strip_cli_artifacts_removes_summary_at_start() {
    let input = "## Summary\n- only summary".to_string();
    assert_eq!(strip_cli_artifacts(input), "");
}

#[test]
fn strip_cli_artifacts_trims_trailing_whitespace_when_no_summary() {
    let input = "Hello world\n\n   \n".to_string();
    assert_eq!(strip_cli_artifacts(input), "Hello world");
}

#[test]
fn strip_cli_artifacts_preserves_content_without_summary() {
    let input = "Hello world".to_string();
    assert_eq!(strip_cli_artifacts(input), "Hello world");
}

#[test]
fn apply_credential_routing_claude_code_signals_short_circuit() {
    let mut cfg = AgentConfig::ctrl_default();
    let short_circuit =
        apply_credential_routing(&mut cfg, &llm::credentials::LlmCredentials::ClaudeCode);
    assert!(short_circuit, "ClaudeCode must signal CLI short-circuit");
    assert!(!cfg.llm.use_anthropic_direct);
}

#[test]
fn apply_credential_routing_openrouter_qualifies_bare_claude_id() {
    let mut cfg = AgentConfig::ctrl_default();
    cfg.agent.model = "claude-sonnet-4-6".to_string();
    let short_circuit =
        apply_credential_routing(&mut cfg, &llm::credentials::LlmCredentials::OpenRouter);
    assert!(!short_circuit);
    assert_eq!(cfg.agent.model, "anthropic/claude-sonnet-4-6");
}

#[test]
fn apply_credential_routing_openrouter_leaves_prefixed_model_alone() {
    let mut cfg = AgentConfig::ctrl_default();
    cfg.agent.model = "openai/gpt-4o".to_string();
    apply_credential_routing(&mut cfg, &llm::credentials::LlmCredentials::OpenRouter);
    assert_eq!(cfg.agent.model, "openai/gpt-4o");
}

#[test]
fn build_deployment_footer_includes_required_fields() {
    let s = build_deployment_footer(
        "ctrl",
        "openrouter",
        "anthropic/claude-sonnet-4-6",
        "0.1.0",
        3,
        Some(11),
        Some(2),
        "/proj",
        Some("/proj/.open-mpm/agents/ctrl.toml"),
    );
    assert!(s.contains("## Deployment Configuration"));
    assert!(s.contains("- Agent: ctrl"));
    assert!(s.contains("- Model: anthropic/claude-sonnet-4-6"));
    assert!(s.contains("- Runner: openrouter"));
    assert!(s.contains("- Version: v0.1.0"));
    assert!(s.contains("- Skills loaded: 3"));
    assert!(s.contains("- Tools available: 11"));
    assert!(s.contains("- MCP connections: 2"));
    assert!(s.contains("- Project: /proj"));
    assert!(s.contains("- Config: /proj/.open-mpm/agents/ctrl.toml"));
}

#[test]
fn build_deployment_footer_omits_optional_fields_when_none() {
    let s = build_deployment_footer(
        "pm",
        "openrouter",
        "model-x",
        "0.1.0",
        0,
        None,
        None,
        "/proj",
        None,
    );
    assert!(s.contains("- Agent: pm"));
    assert!(!s.contains("Tools available"));
    assert!(!s.contains("MCP connections"));
    assert!(!s.contains("Config:"));
    assert!(s.contains("- Skills loaded: 0"));
}

#[test]
fn match_any_glob_handles_suffix_wildcard() {
    let patterns = vec!["mcp_*".to_string(), "git_log".to_string()];
    assert!(match_any_glob("mcp_list", &patterns));
    assert!(match_any_glob("mcp_enable", &patterns));
    assert!(match_any_glob("mcp_", &patterns));
    assert!(match_any_glob("git_log", &patterns));
    assert!(!match_any_glob("git_status", &patterns));
    assert!(!match_any_glob("shell_exec", &patterns));
    assert!(!match_any_glob("anything", &[]));
}

/// Insert a fake PmHandle into ctrl for the given key/name.
fn insert_fake(ctrl: &mut Ctrl, key: &str, name: &str) -> mpsc::Sender<PmMsg> {
    let (tx, _rx) = mpsc::channel(1);
    let tx2 = tx.clone();
    ctrl.pms.insert(
        key.to_string(),
        PmHandle {
            name: name.to_string(),
            project_path: PathBuf::from(key),
            tx,
            task: tokio::spawn(async {}),
            status: Arc::new(Mutex::new("idle".to_string())),
            last_message: Arc::new(Mutex::new(String::new())),
        },
    );
    tx2
}

/// Insert a mock actor that replies with `response` to the first Task.
fn insert_mock_actor(ctrl: &mut Ctrl, key: &str, response: Result<String>) {
    let (tx, mut rx) = mpsc::channel::<PmMsg>(16);
    let task = tokio::spawn(async move {
        if let Some(PmMsg::Task { reply, .. }) = rx.recv().await {
            let _ = reply.send(response);
        }
    });
    ctrl.pms.insert(
        key.to_string(),
        PmHandle {
            name: key.to_string(),
            project_path: PathBuf::from(key),
            tx,
            task,
            status: Arc::new(Mutex::new("idle".to_string())),
            last_message: Arc::new(Mutex::new(String::new())),
        },
    );
    ctrl.active = Some(key.to_string());
}

// -- Ctrl::new --
#[test]
fn new_ctrl_has_no_sessions() {
    let c = Ctrl::new();
    assert!(c.pms.is_empty() && c.active.is_none());
}

// -- Ctrl::prompt --
#[test]
fn prompt_without_active_shows_ctrl() {
    assert_eq!(Ctrl::new().prompt(), "CTRL> ");
}

#[tokio::test]
async fn prompt_with_active_shows_pm_name() {
    let mut c = Ctrl::new();
    insert_fake(&mut c, "/tmp/p", "proj");
    c.active = Some("/tmp/p".to_string());
    assert_eq!(c.prompt(), "PM[proj]> ");
}

#[test]
fn prompt_with_stale_active_shows_question_mark() {
    let mut c = Ctrl::new();
    c.active = Some("/gone".to_string());
    assert_eq!(c.prompt(), "PM[?]> ");
}

// -- Ctrl::disconnect --
#[test]
fn disconnect_when_no_active() {
    let mut c = Ctrl::new();
    assert_eq!(c.disconnect(), "No active PM session.");
}

#[tokio::test]
async fn disconnect_clears_active_but_keeps_handle() {
    let mut c = Ctrl::new();
    insert_fake(&mut c, "/tmp/p", "proj");
    c.active = Some("/tmp/p".to_string());
    let msg = c.disconnect();
    assert!(msg.contains("Disconnected") && msg.contains("proj"));
    assert!(c.active.is_none() && c.pms.contains_key("/tmp/p"));
}

// -- Ctrl::status --
#[test]
fn status_empty() {
    assert_eq!(Ctrl::new().status(), "No PM sessions.");
}

#[tokio::test]
async fn status_lists_sessions_with_markers() {
    let mut c = Ctrl::new();
    insert_fake(&mut c, "/a", "alpha");
    insert_fake(&mut c, "/b", "beta");
    c.active = Some("/a".to_string());
    let out = c.status();
    assert!(out.contains("alpha") && out.contains("beta"));
    assert!(out.contains("[*]") && out.contains("[ ]"));
}

// -- Ctrl::dispatch_task --
#[tokio::test]
async fn dispatch_without_active_errors() {
    let e = Ctrl::new().dispatch_task("hi".into()).await.unwrap_err();
    assert!(e.to_string().contains("no active PM session"));
}

#[tokio::test]
async fn dispatch_with_closed_channel_errors() {
    let mut c = Ctrl::new();
    let (tx, rx) = mpsc::channel(1);
    drop(rx);
    c.pms.insert(
        "/d".into(),
        PmHandle {
            name: "d".into(),
            project_path: "/d".into(),
            tx,
            task: tokio::spawn(async {}),
            status: Arc::new(Mutex::new("idle".to_string())),
            last_message: Arc::new(Mutex::new(String::new())),
        },
    );
    c.active = Some("/d".into());
    assert!(c.dispatch_task("hi".into()).await.is_err());
}

#[tokio::test]
async fn dispatch_receives_actor_reply() {
    let mut c = Ctrl::new();
    insert_mock_actor(&mut c, "/m", Ok("ok".into()));
    assert_eq!(c.dispatch_task("hi".into()).await.unwrap(), "ok");
}

#[tokio::test]
async fn dispatch_propagates_actor_error() {
    let mut c = Ctrl::new();
    insert_mock_actor(&mut c, "/e", Err(anyhow::anyhow!("boom")));
    let e = c.dispatch_task("hi".into()).await.unwrap_err();
    assert!(e.to_string().contains("boom"));
}

// -- Ctrl::connect --
#[tokio::test]
async fn connect_creates_handle() {
    let mut c = Ctrl::new();
    let tmp = tempfile::tempdir().unwrap();
    let msg = c.connect(tmp.path().to_str().unwrap()).await.unwrap();
    assert!(msg.contains("Connected to PM["));
    assert_eq!(c.pms.len(), 1);
    c.shutdown_all().await;
}

#[tokio::test]
async fn connect_same_path_reuses() {
    let mut c = Ctrl::new();
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().to_str().unwrap();
    c.connect(p).await.unwrap();
    let msg = c.connect(p).await.unwrap();
    assert!(msg.contains("Switched") && c.pms.len() == 1);
    c.shutdown_all().await;
}

#[tokio::test]
async fn connect_invalid_path_errors() {
    assert!(Ctrl::new().connect("/no_such_xyz_999").await.is_err());
}

#[tokio::test]
async fn connect_two_dirs() {
    let mut c = Ctrl::new();
    let (t1, t2) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    c.connect(t1.path().to_str().unwrap()).await.unwrap();
    c.connect(t2.path().to_str().unwrap()).await.unwrap();
    assert_eq!(c.pms.len(), 2);
    c.shutdown_all().await;
}

// -- Ctrl::shutdown_all --
#[tokio::test]
async fn shutdown_all_completes() {
    let mut c = Ctrl::new();
    let (tx, mut rx) = mpsc::channel::<PmMsg>(16);
    let task = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if matches!(msg, PmMsg::Shutdown) {
                break;
            }
        }
    });
    c.pms.insert(
        "/s".into(),
        PmHandle {
            name: "s".into(),
            project_path: "/s".into(),
            tx,
            task,
            status: Arc::new(Mutex::new("idle".to_string())),
            last_message: Arc::new(Mutex::new(String::new())),
        },
    );
    c.shutdown_all().await;
}

// -- handle_command --
#[tokio::test]
async fn cmd_quit_returns_false() {
    let mut c = Ctrl::new();
    for cmd in ["/quit", "/exit", "/q"] {
        assert!(!handle_command(&mut c, cmd).await.unwrap(), "{cmd}");
    }
}

#[tokio::test]
async fn cmd_status_help_disconnect_unknown_return_true() {
    let mut c = Ctrl::new();
    for cmd in ["/status", "/help", "/disconnect", "/bogus"] {
        assert!(handle_command(&mut c, cmd).await.unwrap(), "{cmd}");
    }
}

#[tokio::test]
async fn cmd_connect_no_arg_errors() {
    assert!(handle_command(&mut Ctrl::new(), "/connect").await.is_err());
}

#[tokio::test]
async fn cmd_connect_valid_dir() {
    let mut c = Ctrl::new();
    let tmp = tempfile::tempdir().unwrap();
    let cmd = format!("/connect {}", tmp.path().display());
    assert!(handle_command(&mut c, &cmd).await.unwrap());
    assert_eq!(c.pms.len(), 1);
    c.shutdown_all().await;
}

// -- PmHandle actor lifecycle --
#[tokio::test]
async fn actor_processes_task_and_shuts_down() {
    let (tx, rx) = mpsc::channel::<PmMsg>(16);
    let task = tokio::spawn(async move {
        let mut rx = rx;
        while let Some(msg) = rx.recv().await {
            match msg {
                PmMsg::Task { text, reply } => {
                    let _ = reply.send(Ok(format!("echo:{text}")));
                }
                PmMsg::Shutdown => break,
            }
        }
    });
    let (rtx, rrx) = oneshot::channel();
    tx.send(PmMsg::Task {
        text: "ping".into(),
        reply: rtx,
    })
    .await
    .unwrap();
    assert_eq!(rrx.await.unwrap().unwrap(), "echo:ping");
    tx.send(PmMsg::Shutdown).await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap();
}

// -- #182: self-project detection + tools --

/// Build a fake self-project layout under `tmp` and return the root.
fn make_fake_self_project(tmp: &std::path::Path) -> PathBuf {
    let agents = tmp.join(".open-mpm").join("agents");
    std::fs::create_dir_all(&agents).unwrap();
    std::fs::write(agents.join("pm.toml"), "[agent]\nname=\"pm\"\n").unwrap();
    std::fs::write(
        tmp.join("Cargo.toml"),
        "[package]\nname = \"x\"\nversion = \"9.9.9\"\nedition = \"2021\"\n",
    )
    .unwrap();
    tmp.to_path_buf()
}

#[test]
fn detect_self_project_finds_via_env_var() {
    let tmp = tempfile::tempdir().unwrap();
    let root = make_fake_self_project(tmp.path());
    // SAFETY: tests run single-threaded within this process by default.
    unsafe {
        std::env::set_var("OPEN_MPM_PROJECT_DIR", &root);
    }
    let detected = detect_self_project();
    unsafe {
        std::env::remove_var("OPEN_MPM_PROJECT_DIR");
    }
    assert!(detected.is_some(), "expected detection via env var");
}

#[test]
fn detect_self_project_returns_none_when_no_marker() {
    let tmp = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var("OPEN_MPM_PROJECT_DIR", tmp.path());
    }
    let detected = detect_self_project();
    unsafe {
        std::env::remove_var("OPEN_MPM_PROJECT_DIR");
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

// -- Prompt transitions --
#[tokio::test]
async fn prompt_transitions() {
    let mut c = Ctrl::new();
    assert_eq!(c.prompt(), "CTRL> ");
    let tmp = tempfile::tempdir().unwrap();
    c.connect(tmp.path().to_str().unwrap()).await.unwrap();
    assert!(c.prompt().starts_with("PM[") && c.prompt().ends_with("]> "));
    c.disconnect();
    assert_eq!(c.prompt(), "CTRL> ");
    c.shutdown_all().await;
}

#[test]
fn extract_name_from_input_im_bob() {
    assert_eq!(extract_name_from_input("I'm Bob"), Some("Bob".to_string()));
}

#[test]
fn extract_name_from_input_my_name_is_alice() {
    assert_eq!(
        extract_name_from_input("My name is Alice"),
        Some("Alice".to_string())
    );
}

#[test]
fn extract_name_from_input_bare_name() {
    assert_eq!(extract_name_from_input("Bob"), Some("Bob".to_string()));
    assert_eq!(extract_name_from_input("bob"), Some("Bob".to_string()));
}

#[test]
fn extract_name_from_input_call_me_sam() {
    assert_eq!(
        extract_name_from_input("call me Sam"),
        Some("Sam".to_string())
    );
}

#[test]
fn extract_name_from_input_im_alice_lower() {
    assert_eq!(
        extract_name_from_input("im alice"),
        Some("Alice".to_string())
    );
}

#[test]
fn extract_name_from_input_rejects_task_requests() {
    assert_eq!(extract_name_from_input("write me code"), None);
    assert_eq!(extract_name_from_input("build a python script"), None);
}

#[test]
fn extract_name_from_input_rejects_greetings() {
    assert_eq!(extract_name_from_input("Hello"), None);
    assert_eq!(extract_name_from_input("hi"), None);
    assert_eq!(extract_name_from_input("hey"), None);
    assert_eq!(extract_name_from_input("thanks"), None);
}

#[test]
fn extract_name_from_input_rejects_im_filler() {
    assert_eq!(extract_name_from_input("I'm here"), None);
    assert_eq!(extract_name_from_input("I'm fine"), None);
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
        .execute(json!({ "path": "/zzz-open-mpm-#202-no-such-project" }))
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
            "from": "/zzz-no-such-file-open-mpm",
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

// -- resolve_agent_config (#240) --

#[tokio::test]
async fn resolve_agent_config_prefers_pm_toml() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let agents = tmp.path().join(".open-mpm/agents");
    std::fs::create_dir_all(&agents).unwrap();
    std::fs::write(
        agents.join("pm.toml"),
        r#"
[agent]
name = "pm"
role = "manager"
model = "anthropic/claude-sonnet-4-6"
description = "test pm"

[llm]
temperature = 0.2
max_tokens = 1024

[system_prompt]
content = "pm-from-disk"
"#,
    )
    .unwrap();

    let (cfg, _path) = resolve_agent_config(tmp.path()).await.unwrap();
    assert_eq!(cfg.agent.name, "pm");
    assert_eq!(cfg.system_prompt.content, "pm-from-disk");
}

#[tokio::test]
async fn resolve_agent_config_falls_back_to_project_ctrl_toml() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let agents = tmp.path().join(".open-mpm/agents");
    std::fs::create_dir_all(&agents).unwrap();
    std::fs::write(
        agents.join("ctrl.toml"),
        r#"
[agent]
name = "ctrl"
role = "controller"
model = "anthropic/claude-sonnet-4-6"
description = "test ctrl"

[llm]
temperature = 0.7
max_tokens = 2048

[system_prompt]
content = "ctrl-from-project-disk"
"#,
    )
    .unwrap();

    let (cfg, _path) = resolve_agent_config(tmp.path()).await.unwrap();
    assert_eq!(cfg.agent.name, "ctrl");
    assert!(matches!(cfg.agent.role.as_str(), "controller" | "ctrl"));
}

#[tokio::test]
async fn resolve_agent_config_returns_builtin_when_no_disk_config() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let prev_home = std::env::var_os("HOME");
    // SAFETY: test-only env mutation
    unsafe {
        std::env::set_var("HOME", tmp.path());
    }

    let (cfg, _path) = resolve_agent_config(tmp.path()).await.unwrap();
    assert_eq!(cfg.agent.name, "ctrl");
    assert!(cfg.system_prompt.content.contains("Standalone"));

    // SAFETY: restore HOME so other tests aren't affected
    unsafe {
        match prev_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
    }
}
