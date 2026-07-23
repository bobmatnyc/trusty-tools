//! Unit tests for the `delegate_to_agent` tool (`super::delegate`).
//!
//! Why: split out of `delegate.rs` so that module stays under the 500-SLOC
//! production cap — the #3737 `AgentSpawned`-emit + session-id-gate additions
//! pushed the combined file over. Moved verbatim; `super::*` / `super::` still
//! resolve to the `delegate` module since `#[path]` does not change the module
//! hierarchy (mirrors the sibling `izzie/*_tests.rs` split idiom).
//! Test: this file IS the test module for `delegate`.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use serde_json::json;

use super::DelegateToAgentTool;
use crate::perf::TokenUsage;
use crate::tools::traits::{AgentOutput, AgentRunner, ToolExecutor};

/// Mock runner that records whether it was invoked and with what args.
struct RecordingRunner {
    invoked: std::sync::Mutex<Vec<(String, String)>>,
}

#[async_trait]
impl AgentRunner for RecordingRunner {
    async fn run(&self, agent_name: &str, task: &str) -> Result<AgentOutput> {
        self.invoked
            .lock()
            .unwrap()
            .push((agent_name.to_string(), task.to_string()));
        Ok(AgentOutput {
            content: "ok".into(),
            summary: None,
            usage: TokenUsage::default(),
        })
    }
}

/// #3052 PR A code-critic CRITICAL-2 regression guard: calling
/// `delegate_to_agent` with an unknown agent name (e.g. the hallucinated
/// `code-searcher` from #204) must return a GENERIC error — it must
/// echo back the caller's OWN rejected input, but it must NOT enumerate
/// the on-disk agent roster, name any bundled agent filename, or leak
/// any internal trusty-* system/daemon name. This tool is reachable
/// from black-boxed assistant-tier personas (`assistant`, `izzie`,
/// `cto-assistant`), so its own error text must be as safe to surface
/// to a user as the persona prompt that calls it — the seeded config
/// dir intentionally includes names spanning the full roster (workers,
/// meta/infra agents, and internal system terms) so this test would
/// catch a regression regardless of which category leaked.
#[tokio::test]
async fn unknown_agent_is_rejected_without_naming_the_agent_or_roster() {
    let tmp = tempfile::tempdir().unwrap();
    // Seed a realistic roster — worker + meta/infra agent names — so a
    // regression that reintroduces roster enumeration is caught no
    // matter which name it would have leaked.
    for name in [
        "engineer",
        "python-engineer",
        "qa-agent",
        "research-agent",
        "docs-agent",
        "local-ops-agent",
        "plan-agent",
        "ctrl",
        "pm",
        "postmortem-agent",
    ] {
        std::fs::write(
            tmp.path().join(format!("{name}.toml")),
            format!("[agent]\nname = \"{name}\"\n"),
        )
        .unwrap();
    }

    let runner = Arc::new(RecordingRunner {
        invoked: std::sync::Mutex::new(Vec::new()),
    });
    let tool = DelegateToAgentTool::new(runner.clone()).with_config_dir(tmp.path().to_path_buf());

    let result = tool
        .execute(json!({
            "agent_name": "code-searcher",
            "task": "find the intent classifier"
        }))
        .await;

    assert!(result.is_error(), "must reject unknown agent");
    let msg = result.content();
    assert!(
        msg.contains("code-searcher"),
        "error may echo back the caller's own rejected input, got: {msg}"
    );
    // No roster enumeration: none of the seeded agent names may leak,
    // regardless of whether they're workers or meta/infra agents.
    for leaked in [
        "engineer",
        "python-engineer",
        "qa-agent",
        "research-agent",
        "docs-agent",
        "local-ops-agent",
        "plan-agent",
        "ctrl",
        "postmortem-agent",
    ] {
        assert!(
            !msg.contains(leaked),
            "error must NOT enumerate the on-disk roster (found '{leaked}'), got: {msg}"
        );
    }
    // No internal system/daemon names either.
    for leaked in ["trusty-mpm", "trusty-code", "tcode", "subprocess"] {
        assert!(
            !msg.to_lowercase().contains(&leaked.to_lowercase()),
            "error must NOT leak internal system name '{leaked}', got: {msg}"
        );
    }
    assert!(
        msg.contains("native tools") && msg.contains("search_code"),
        "error must clarify native-vs-agent distinction, got: {msg}"
    );
    // Crucially, the runner must NOT have been invoked — no subprocess spawn.
    assert!(
        runner.invoked.lock().unwrap().is_empty(),
        "runner must not be called when validation fails"
    );
}

/// A valid agent name passes validation and reaches the runner.
#[tokio::test]
async fn known_agent_reaches_runner() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("engineer.toml"),
        "[agent]\nname = \"engineer\"\n",
    )
    .unwrap();

    let runner = Arc::new(RecordingRunner {
        invoked: std::sync::Mutex::new(Vec::new()),
    });
    let tool = DelegateToAgentTool::new(runner.clone()).with_config_dir(tmp.path().to_path_buf());

    let result = tool
        .execute(json!({
            "agent_name": "engineer",
            "task": "do the thing"
        }))
        .await;

    assert!(
        !result.is_error(),
        "valid agent should succeed: {}",
        result.content()
    );
    let invoked = runner.invoked.lock().unwrap();
    assert_eq!(invoked.len(), 1, "runner should be called exactly once");
    assert_eq!(invoked[0].0, "engineer");
}

/// Without `with_config_dir` (legacy callers), validation is skipped —
/// the runner is invoked unchanged. This preserves backward compatibility
/// with `main.rs:1901` and any test double that constructs the tool
/// without a config dir.
#[tokio::test]
async fn no_config_dir_skips_validation() {
    let runner = Arc::new(RecordingRunner {
        invoked: std::sync::Mutex::new(Vec::new()),
    });
    let tool = DelegateToAgentTool::new(runner.clone());

    let result = tool
        .execute(json!({
            "agent_name": "anything-goes",
            "task": "do the thing"
        }))
        .await;

    assert!(!result.is_error(), "legacy mode should bypass validation");
    assert_eq!(runner.invoked.lock().unwrap().len(), 1);
}

/// #3555 delegate-resolve regression guard: the primary use case this
/// fix targets — an assistant launched from a CWD with NO local
/// `.trusty-agents/agents/` (empty tempdir, standing in for e.g. a bare
/// worktree root) must still successfully delegate to a bundled worker
/// agent that only exists in the SECONDARY tier (standing in for
/// `$HOME/.trusty-agents/agents/`, populated by
/// `agents::bundled::ensure_bundled_agents_deployed`). Before the fix,
/// `DelegateToAgentTool` only ever checked a single directory, so this
/// would have rejected the call before the runner was ever invoked —
/// exactly the reported symptom (`is_error=true`, no `engineer` spawn).
#[tokio::test]
async fn delegate_resolves_agent_from_secondary_config_dir() {
    let empty_primary = tempfile::tempdir().unwrap();
    let secondary = tempfile::tempdir().unwrap();
    std::fs::write(
        secondary.path().join("engineer.toml"),
        "[agent]\nname = \"engineer\"\n",
    )
    .unwrap();

    let runner = Arc::new(RecordingRunner {
        invoked: std::sync::Mutex::new(Vec::new()),
    });
    let tool = DelegateToAgentTool::new(runner.clone()).with_config_dirs(vec![
        empty_primary.path().to_path_buf(),
        secondary.path().to_path_buf(),
    ]);

    let result = tool
        .execute(json!({
            "agent_name": "engineer",
            "task": "run cargo check and report back"
        }))
        .await;

    assert!(
        !result.is_error(),
        "agent present only in the secondary (fallback) tier must still resolve: {}",
        result.content()
    );
    let invoked = runner.invoked.lock().unwrap();
    assert_eq!(invoked.len(), 1, "runner should be called exactly once");
    assert_eq!(invoked[0].0, "engineer");
}

/// The multi-tier counterpart to
/// `unknown_agent_is_rejected_without_naming_the_agent_or_roster`: when
/// an agent name is absent from EVERY candidate directory, validation
/// still rejects before the runner is invoked.
#[tokio::test]
async fn delegate_rejects_agent_absent_from_all_config_dirs() {
    let primary = tempfile::tempdir().unwrap();
    let secondary = tempfile::tempdir().unwrap();

    let runner = Arc::new(RecordingRunner {
        invoked: std::sync::Mutex::new(Vec::new()),
    });
    let tool = DelegateToAgentTool::new(runner.clone()).with_config_dirs(vec![
        primary.path().to_path_buf(),
        secondary.path().to_path_buf(),
    ]);

    let result = tool
        .execute(json!({
            "agent_name": "nonexistent-agent",
            "task": "do the thing"
        }))
        .await;

    assert!(result.is_error(), "must reject an agent absent everywhere");
    assert!(runner.invoked.lock().unwrap().is_empty());
}

/// Writes a minimal, fully-parseable agent TOML with the given `role` —
/// the shape `AgentConfig::by_name_in` (used by the role gate) actually
/// requires, unlike the bare `[agent]\nname = "..."` fixtures the
/// existence-only tests above use (those never parse the file).
fn write_agent_toml_with_role(dir: &std::path::Path, name: &str, role: &str) {
    std::fs::write(
        dir.join(format!("{name}.toml")),
        format!(
            r#"
[agent]
name = "{name}"
role = "{role}"
model = "anthropic/claude-sonnet-4-6"
description = "test fixture"

[llm]
temperature = 0.2
max_tokens = 1024

[system_prompt]
content = "test"
"#
        ),
    )
    .unwrap();
}

/// #3555 CRITICAL follow-up (code-critic): privilege escalation via
/// unrestricted delegation target. Widening `config_dirs` to include the
/// `$HOME` fallback tier makes the FULL bundled roster resolvable from
/// any cwd — including `pm` (role `orchestrator`). Without a role gate,
/// an assistant-tier `delegate_to_agent(agent_name="pm")` would pass
/// validation and spawn a subprocess with `run_subagent`'s UNRESTRICTED
/// orchestrator registry (shell, write_file, unrestricted
/// delegate_to_agent) — an escalation straight out of the sandboxed
/// assistant. This test proves the role gate rejects it before the
/// runner is ever invoked, with the SAME generic error text (no role
/// taxonomy leak).
#[tokio::test]
async fn delegate_assistant_role_gate_rejects_orchestrator_role() {
    let dir = tempfile::tempdir().unwrap();
    write_agent_toml_with_role(dir.path(), "pm", "orchestrator");

    let runner = Arc::new(RecordingRunner {
        invoked: std::sync::Mutex::new(Vec::new()),
    });
    let tool = DelegateToAgentTool::new(runner.clone())
        .with_config_dirs(vec![dir.path().to_path_buf()])
        .with_allowed_target_roles(vec!["engineer".to_string(), "assistant".to_string()]);

    let result = tool
        .execute(json!({
            "agent_name": "pm",
            "task": "do orchestrator-tier things"
        }))
        .await;

    assert!(
        result.is_error(),
        "role gate must reject a resolvable but disallowed-role target"
    );
    assert!(
        result.content().contains("pm"),
        "error may echo back the caller's own rejected input, got: {}",
        result.content()
    );
    assert!(
        !result.content().to_lowercase().contains("orchestrator"),
        "error must not leak the target's actual role, got: {}",
        result.content()
    );
    assert!(
        runner.invoked.lock().unwrap().is_empty(),
        "runner must not be called when the role gate rejects"
    );
}

/// Sibling to `delegate_assistant_role_gate_rejects_orchestrator_role`
/// for `ctrl` (role `controller`) — the other privileged meta-agent
/// named in the CRITICAL finding.
#[tokio::test]
async fn delegate_assistant_role_gate_rejects_controller_role() {
    let dir = tempfile::tempdir().unwrap();
    write_agent_toml_with_role(dir.path(), "ctrl", "controller");

    let runner = Arc::new(RecordingRunner {
        invoked: std::sync::Mutex::new(Vec::new()),
    });
    let tool = DelegateToAgentTool::new(runner.clone())
        .with_config_dirs(vec![dir.path().to_path_buf()])
        .with_allowed_target_roles(vec!["engineer".to_string(), "assistant".to_string()]);

    let result = tool
        .execute(json!({
            "agent_name": "ctrl",
            "task": "do controller-tier things"
        }))
        .await;

    assert!(result.is_error(), "role gate must reject role=controller");
    assert!(runner.invoked.lock().unwrap().is_empty());
}

/// The positive case: a worker role (`engineer`) IS in the allowlist and
/// must still reach the runner — the role gate must not become a
/// blanket denial.
#[tokio::test]
async fn delegate_assistant_role_gate_allows_worker_role() {
    let dir = tempfile::tempdir().unwrap();
    write_agent_toml_with_role(dir.path(), "engineer", "engineer");

    let runner = Arc::new(RecordingRunner {
        invoked: std::sync::Mutex::new(Vec::new()),
    });
    let tool = DelegateToAgentTool::new(runner.clone())
        .with_config_dirs(vec![dir.path().to_path_buf()])
        .with_allowed_target_roles(vec!["engineer".to_string(), "assistant".to_string()]);

    let result = tool
        .execute(json!({
            "agent_name": "engineer",
            "task": "run cargo check"
        }))
        .await;

    assert!(
        !result.is_error(),
        "worker role must pass the gate: {}",
        result.content()
    );
    assert_eq!(runner.invoked.lock().unwrap().len(), 1);
}

/// The other positive case: a PEER assistant (role `assistant`, e.g.
/// `cto-assistant`) must still reach the runner — this is the
/// Izzie <-> cto-assistant peer-consult lane the role gate must
/// preserve. Spawning a peer is safe because IT ALSO gets routed
/// through the restricted assistant-tier registry, never an
/// unrestricted one.
#[tokio::test]
async fn delegate_assistant_role_gate_allows_peer_assistant_role() {
    let dir = tempfile::tempdir().unwrap();
    write_agent_toml_with_role(dir.path(), "cto-assistant", "assistant");

    let runner = Arc::new(RecordingRunner {
        invoked: std::sync::Mutex::new(Vec::new()),
    });
    let tool = DelegateToAgentTool::new(runner.clone())
        .with_config_dirs(vec![dir.path().to_path_buf()])
        .with_allowed_target_roles(vec!["engineer".to_string(), "assistant".to_string()]);

    let result = tool
        .execute(json!({
            "agent_name": "cto-assistant",
            "task": "consult on architecture"
        }))
        .await;

    assert!(
        !result.is_error(),
        "peer assistant role must pass the gate: {}",
        result.content()
    );
    assert_eq!(runner.invoked.lock().unwrap().len(), 1);
}

/// Without `with_allowed_target_roles` (pm/ctrl callers), the role gate
/// is a no-op — the existing existence-only check still governs, and a
/// name resolving to ANY role (including `orchestrator`/`controller`)
/// reaches the runner. Pins that widening the assistant tier's gate did
/// NOT change pm/ctrl's own delegation behavior.
#[tokio::test]
async fn delegate_without_role_gate_allows_any_resolvable_role() {
    let dir = tempfile::tempdir().unwrap();
    write_agent_toml_with_role(dir.path(), "pm", "orchestrator");

    let runner = Arc::new(RecordingRunner {
        invoked: std::sync::Mutex::new(Vec::new()),
    });
    let tool =
        DelegateToAgentTool::new(runner.clone()).with_config_dirs(vec![dir.path().to_path_buf()]);

    let result = tool
        .execute(json!({
            "agent_name": "pm",
            "task": "do orchestrator-tier things"
        }))
        .await;

    assert!(
        !result.is_error(),
        "pm/ctrl callers (no role gate) must be unaffected: {}",
        result.content()
    );
    assert_eq!(runner.invoked.lock().unwrap().len(), 1);
}

/// #3737: a successful delegation emits `AgentSpawned { session_id, agent }`
/// when a session id was threaded in via `with_session_id`, so the API
/// server can attribute the answer to the specialist that produced it.
#[tokio::test]
async fn delegate_emits_agent_spawned_with_session_id() {
    use crate::events::Event;
    let sid = "delegate-emit-test-session";
    let mut rx = crate::events::subscribe();

    let runner = Arc::new(RecordingRunner {
        invoked: std::sync::Mutex::new(Vec::new()),
    });
    // No config_dirs → pre-flight validation skipped → runner invoked
    // directly; keeps this focused on the emit behavior.
    let tool = DelegateToAgentTool::new(runner).with_session_id(sid);
    let result = tool
        .execute(json!({ "agent_name": "izzie", "task": "what's the weather" }))
        .await;
    assert!(
        !result.is_error(),
        "delegation should succeed: {}",
        result.content()
    );

    // Drain the bus; find the AgentSpawned for our session.
    let mut found = None;
    while let Ok(ev) = rx.try_recv() {
        if let Event::AgentSpawned { session_id, agent } = ev
            && session_id == sid
        {
            found = Some(agent);
        }
    }
    assert_eq!(
        found.as_deref(),
        Some("izzie"),
        "successful delegation must emit AgentSpawned naming the specialist that answered"
    );
}

/// #3737: the emit is gated on a non-blank session id. The default
/// constructor and a blank `with_session_id` both leave `session_id` as
/// `None`, so delegation emits nothing (byte-identical to pre-#3737
/// behavior); a non-blank id is stored. Asserting on the private field
/// (same-module test) is deterministic on the process-global event bus,
/// where observing "no emit" across concurrent tests would be racy.
#[test]
fn session_id_gate_normalizes_blank_to_none() {
    let runner = Arc::new(RecordingRunner {
        invoked: std::sync::Mutex::new(Vec::new()),
    });
    assert_eq!(DelegateToAgentTool::new(runner.clone()).session_id, None);
    assert_eq!(
        DelegateToAgentTool::new(runner.clone())
            .with_session_id("   ")
            .session_id,
        None
    );
    assert_eq!(
        DelegateToAgentTool::new(runner)
            .with_session_id("task-42")
            .session_id
            .as_deref(),
        Some("task-42")
    );
}
