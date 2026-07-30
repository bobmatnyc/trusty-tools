//! Unit + async tests for the L0 execution grant (`super::l0_exec`, #4173).
//!
//! Why: Split out of `l0_exec.rs` so that module stays well under the 500-SLOC
//! production cap, matching the sibling `delegate.rs` / `delegate_tests.rs`
//! idiom (`#[path]` does not change the module hierarchy, so `super::*` still
//! resolves to `l0_exec`).
//! What: The tier gate itself, the containment predicate, the RBAC
//! service-tier denial, and real `execute()` behaviour (including the
//! `echo "hello"` smoke check #4173's acceptance criteria name).
//! Test: this file IS the test module for `l0_exec`.

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::json;

use super::*;
use crate::rbac::UserIdentity;
use crate::tools::ToolRegistry;

// ---------------------------------------------------------------------------
// The tier gate — the security boundary.
// ---------------------------------------------------------------------------

/// L1 (standard tier, the fail-closed default and the tier that holds the
/// Gmail/Drive/Calendar surface) gets NO execution tool at all. This is the
/// invariant #4126 exists to protect: untrusted content reaching an L1
/// persona must not be one tool call away from code execution.
#[test]
fn l0_execution_tools_denies_l1_tier() {
    let tools = l0_execution_tools(AgentTier::L1Standard, PathBuf::from("."));
    assert!(
        tools.is_empty(),
        "L1 must receive no execution tool, got: {:?}",
        tools.iter().map(|t| t.name()).collect::<Vec<_>>()
    );
}

/// L0 (orchestration tier, YOLO posture explicitly accepted by the owner, no
/// untrusted-content surface) does get the shell.
#[test]
fn l0_execution_tools_grants_l0_tier() {
    let tools = l0_execution_tools(AgentTier::L0Orchestration, PathBuf::from("."));
    let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
    assert_eq!(names, vec![L0_SHELL_EXEC]);
}

/// Resolve a persona's tier the way production does: write a real
/// `agent.toml`, load it through `AgentConfig::by_name_in` (the same loader
/// `delegate_to_agent` and `run_subagent` use), and read `AgentInfo::tier()`.
/// Deliberately NOT a hand-built `AgentTier` value — a regression in the
/// loader or in the fail-closed resolver must fail these tests too.
fn tier_from_declared_for_role(role: &str, raw: Option<&str>) -> (tempfile::TempDir, AgentTier) {
    let dir = tempfile::tempdir().unwrap();
    let tier_line = raw.map(|t| format!("tier = \"{t}\"")).unwrap_or_default();
    std::fs::write(
        dir.path().join("fixture-persona.toml"),
        format!(
            r#"
[agent]
name = "fixture-persona"
role = "{role}"
model = "anthropic/claude-sonnet-4-6"
description = "test fixture"
{tier_line}

[llm]
temperature = 0.2
max_tokens = 1024

[system_prompt]
content = "test"
"#
        ),
    )
    .unwrap();
    let cfg =
        crate::agents::AgentConfig::by_name_in(&[dir.path().to_path_buf()], "fixture-persona")
            .expect("fixture agent loads");
    let tier = cfg.agent.tier();
    (dir, tier)
}

/// The assistant-kind spelling of the helper above, for the tests whose subject
/// is the declared string rather than the role.
fn tier_from_declared(raw: Option<&str>) -> (tempfile::TempDir, AgentTier) {
    tier_from_declared_for_role("assistant", raw)
}

/// Fail-closed end to end through the REAL loader + resolver: every EXPLICITLY
/// DECLARED `tier` string that is not a recognized L0 alias — a typo, a
/// plausible near-miss, even values that CONTAIN "l0" — must produce an empty
/// tool set, for EITHER population.
///
/// Checked against the assistant kind as well as a sub-agent kind, because
/// ADR-0024 decision 3 (PR #4296) made an ABSENT tier derive from the role: a
/// declared-but-unrecognized string must still fail closed to `L1Standard` and
/// must NOT fall through to the assistant-kind derivation. That fall-through
/// would be a silent escalation, so it is pinned here for both roles.
#[test]
fn l0_execution_tools_denies_every_fail_closed_tier_string() {
    for role in ["assistant", "engineer"] {
        for raw in [
            "l1",
            "standard",
            "L2",
            "bogus",
            "l0-ish",
            "not-l0",
            "orchestrator",
        ] {
            let (_dir, tier) = tier_from_declared_for_role(role, Some(raw));
            assert_eq!(
                tier,
                AgentTier::L1Standard,
                "role {role:?} + declared tier {raw:?} must resolve L1"
            );
            assert!(
                l0_execution_tools(tier, PathBuf::from(".")).is_empty(),
                "role {role:?} + declared tier {raw:?} must fail closed and grant nothing"
            );
        }
    }
}

/// The DERIVED case, after ADR-0024 decision 3 (PR #4296, on `main`): with no
/// usable `tier =` declaration the tier is a function of the agent's KIND —
/// assistant-kind resolves L0 and therefore holds the grant; every sub-agent
/// kind stays L1 and holds nothing.
///
/// Why this test exists in this file: the grant itself is unchanged — it is
/// still "L0 only, by construction, an empty vector otherwise". What decision 3
/// changed is the POPULATION that resolves L0. That is exactly the kind of
/// upstream change that would otherwise silently redefine this tool's blast
/// radius with no test failing, so the derivation is pinned at the grant site
/// rather than only at the resolver. The observable capability delta for the
/// shipped roster is separately pinned by
/// `agents::tests::loading::bundled_assistant_personas_resolve_l0_and_gain_nothing`,
/// which asserts no bundled assistant's `[tools].allow` can actually reach it.
#[test]
fn l0_execution_tools_follow_the_kind_derivation_when_no_tier_is_declared() {
    for raw in [None, Some(""), Some("   ")] {
        let (_dir, tier) = tier_from_declared_for_role("assistant", raw);
        assert_eq!(
            tier,
            AgentTier::L0Orchestration,
            "assistant-kind with tier {raw:?} derives L0 (ADR-0024 decision 3)"
        );
        assert_eq!(
            l0_execution_tools(tier, PathBuf::from(".")).len(),
            1,
            "an L0-derived assistant registers the grant"
        );

        for role in ["engineer", "researcher", "qa", "planner", ""] {
            let (_dir, tier) = tier_from_declared_for_role(role, raw);
            assert_eq!(
                tier,
                AgentTier::L1Standard,
                "sub-agent kind {role:?} with tier {raw:?} must stay L1"
            );
            assert!(
                l0_execution_tools(tier, PathBuf::from(".")).is_empty(),
                "sub-agent kind {role:?} must be granted nothing"
            );
        }
    }
}

/// The counterpart: the aliases the resolver DOES recognize (in any case,
/// with surrounding whitespace) elevate — again through the real loader.
#[test]
fn l0_execution_tools_grants_the_recognized_l0_aliases() {
    for raw in ["l0", "L0", "orchestration", "Orchestration", "  l0  "] {
        let (_dir, tier) = tier_from_declared(Some(raw));
        assert_eq!(tier, AgentTier::L0Orchestration, "tier {raw:?} must be L0");
        assert_eq!(
            l0_execution_tools(tier, PathBuf::from(".")).len(),
            1,
            "tier {raw:?} must grant the execution tool"
        );
    }
}

// ---------------------------------------------------------------------------
// Containment (cwd scoping).
// ---------------------------------------------------------------------------

#[test]
fn resolve_working_dir_defaults_to_the_root() {
    let tmp = tempfile::tempdir().unwrap();
    let resolved = resolve_working_dir(tmp.path(), None).unwrap();
    assert_eq!(resolved, tmp.path().canonicalize().unwrap());
    // Blank / whitespace is treated as "not supplied", never as "/".
    assert_eq!(
        resolve_working_dir(tmp.path(), Some("   ")).unwrap(),
        tmp.path().canonicalize().unwrap()
    );
}

#[tokio::test]
async fn l0_shell_exec_accepts_working_dir_inside_the_root() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir(tmp.path().join("crates")).unwrap();
    let tool = L0ShellExecTool::new(tmp.path().to_path_buf());
    let r = tool
        .execute(json!({"command": "pwd", "working_dir": "crates"}))
        .await;
    assert!(!r.is_error(), "unexpected error: {}", r.content());
    assert!(
        r.content().contains("crates"),
        "expected to run inside the subdirectory, got: {}",
        r.content()
    );
}

#[tokio::test]
async fn l0_shell_exec_refuses_working_dir_outside_the_root() {
    let tmp = tempfile::tempdir().unwrap();
    let tool = L0ShellExecTool::new(tmp.path().to_path_buf());
    let r = tool
        .execute(json!({"command": "pwd", "working_dir": "/"}))
        .await;
    assert!(r.is_error(), "an absolute escape must be refused");
    assert!(
        r.content().contains("outside the project root"),
        "got: {}",
        r.content()
    );
}

#[tokio::test]
async fn l0_shell_exec_refuses_parent_traversal_working_dir() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir(tmp.path().join("inside")).unwrap();
    let tool = L0ShellExecTool::new(tmp.path().join("inside"));
    let r = tool
        .execute(json!({"command": "pwd", "working_dir": "../.."}))
        .await;
    assert!(r.is_error(), "`..` traversal must be refused");
    assert!(
        r.content().contains("outside the project root"),
        "got: {}",
        r.content()
    );
}

#[tokio::test]
async fn l0_shell_exec_refuses_nonexistent_working_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let tool = L0ShellExecTool::new(tmp.path().to_path_buf());
    let r = tool
        .execute(json!({"command": "pwd", "working_dir": "no/such/dir"}))
        .await;
    assert!(r.is_error());
    assert!(
        r.content().contains("not a resolvable directory"),
        "got: {}",
        r.content()
    );
}

// ---------------------------------------------------------------------------
// Execution behaviour (#4173 acceptance criteria).
// ---------------------------------------------------------------------------

/// #4173 acceptance criterion: "Simple test confirms shell execution works
/// (e.g. `echo "hello"`)".
#[tokio::test]
async fn l0_shell_exec_runs_echo() {
    let tmp = tempfile::tempdir().unwrap();
    let tool = L0ShellExecTool::new(tmp.path().to_path_buf());
    let r = tool.execute(json!({"command": "echo hello"})).await;
    assert!(!r.is_error(), "unexpected error: {}", r.content());
    assert!(r.content().contains("hello"), "got: {}", r.content());
    assert!(r.content().contains("[exit 0]"), "got: {}", r.content());
}

#[tokio::test]
async fn l0_shell_exec_runs_in_the_root_by_default() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("marker.txt"), "x").unwrap();
    let tool = L0ShellExecTool::new(tmp.path().to_path_buf());
    let r = tool.execute(json!({"command": "ls"})).await;
    assert!(!r.is_error(), "unexpected error: {}", r.content());
    assert!(r.content().contains("marker.txt"), "got: {}", r.content());
}

/// A failing build/test command must come back with its real exit code and
/// its stderr — #4173 asks for output that is "legible and unfiltered", which
/// is what makes the tool usable for driving a fix round.
#[tokio::test]
async fn l0_shell_exec_reports_exit_code_and_stderr() {
    let tmp = tempfile::tempdir().unwrap();
    let tool = L0ShellExecTool::new(tmp.path().to_path_buf());
    let r = tool
        .execute(json!({"command": "echo boom 1>&2; exit 3"}))
        .await;
    assert!(
        !r.is_error(),
        "a non-zero exit is a result, not a tool error"
    );
    assert!(r.content().contains("[exit 3]"), "got: {}", r.content());
    assert!(r.content().contains("boom"), "got: {}", r.content());
}

#[tokio::test]
async fn l0_shell_exec_rejects_missing_command() {
    let tool = L0ShellExecTool::new(PathBuf::from("."));
    let r = tool.execute(json!({})).await;
    assert!(r.is_error());
    assert!(r.content().contains("required"), "got: {}", r.content());
}

/// The catastrophic-pattern predicate is defense in depth (the tier gate is
/// the boundary), but it is wired up and must stay wired up.
#[tokio::test]
async fn l0_shell_exec_blocks_catastrophic_pattern() {
    let tool = L0ShellExecTool::new(PathBuf::from("."));
    let r = tool.execute(json!({"command": "rm -rf /"})).await;
    assert!(r.is_error());
    assert!(
        r.content().contains("blocked pattern"),
        "got: {}",
        r.content()
    );
}

// ---------------------------------------------------------------------------
// RBAC service tier — the second, independent dimension.
// ---------------------------------------------------------------------------

/// A read-only transport user (unauthenticated HTTP, guest Slack) must be
/// refused at the REAL dispatch boundary (`ToolRegistry::dispatch_for_user`),
/// even when the persona itself is L0 and the tool is therefore registered.
#[tokio::test]
async fn l0_shell_exec_denied_to_read_only_service_tier() {
    let mut reg = ToolRegistry::new();
    for tool in l0_execution_tools(AgentTier::L0Orchestration, PathBuf::from(".")) {
        reg.register(tool);
    }
    let user = UserIdentity::new("guest", "guest", ServiceTier::ReadOnly);
    let r = reg
        .dispatch_for_user(L0_SHELL_EXEC, json!({"command": "echo hello"}), None, &user)
        .await;
    assert!(r.is_error(), "read-only users must not reach the shell");
    assert!(r.content().contains("access tier"), "got: {}", r.content());
}

#[tokio::test]
async fn l0_shell_exec_denied_to_analytics_service_tier() {
    let mut reg = ToolRegistry::new();
    for tool in l0_execution_tools(AgentTier::L0Orchestration, PathBuf::from(".")) {
        reg.register(tool);
    }
    let user = UserIdentity::new("analyst", "analyst", ServiceTier::Analytics);
    let r = reg
        .dispatch_for_user(L0_SHELL_EXEC, json!({"command": "echo hello"}), None, &user)
        .await;
    assert!(r.is_error(), "analytics users must not reach the shell");
}

#[tokio::test]
async fn l0_shell_exec_allowed_for_operator_service_tier() {
    let tmp = tempfile::tempdir().unwrap();
    let mut reg = ToolRegistry::new();
    for tool in l0_execution_tools(AgentTier::L0Orchestration, tmp.path().to_path_buf()) {
        reg.register(tool);
    }
    let user = UserIdentity::new("owner", "owner", ServiceTier::All);
    let r = reg
        .dispatch_for_user(
            L0_SHELL_EXEC,
            json!({"command": "echo hello"}),
            Some(&[L0_SHELL_EXEC.to_string()]),
            &user,
        )
        .await;
    assert!(!r.is_error(), "unexpected error: {}", r.content());
    assert!(r.content().contains("hello"));
}

/// The schema the model sees must name the tool it dispatches, or the LLM
/// emits a call no registry can route.
#[test]
fn schema_matches_the_dispatch_name() {
    let tool = L0ShellExecTool::new(PathBuf::from("."));
    let schema = tool.schema();
    assert_eq!(schema["function"]["name"], L0_SHELL_EXEC);
    assert_eq!(tool.name(), L0_SHELL_EXEC);
    let required = schema["function"]["parameters"]["required"]
        .as_array()
        .unwrap();
    assert_eq!(required.len(), 1);
    assert_eq!(required[0], "command");
}

/// Guard against a future edit accidentally aliasing this grant onto a tool
/// name some other agent already holds (`run_bash` is granted to `ctrl`/`pm`,
/// `shell_exec` to `local-ops-agent`, `pytest_exec` to `qa-agent`). The L0
/// surface must stay its own greppable name.
#[test]
fn l0_grant_does_not_alias_an_existing_shell_tool_name() {
    let tools: Vec<Arc<dyn ToolExecutor>> =
        l0_execution_tools(AgentTier::L0Orchestration, PathBuf::from("."));
    for t in &tools {
        assert!(
            !["run_bash", "shell_exec", "pytest_exec"].contains(&t.name()),
            "the L0 grant must not reuse an existing shell tool name: {}",
            t.name()
        );
    }
}
