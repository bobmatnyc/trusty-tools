//! Tests for #2056's background task-execution orchestration. Split out of
//! `executor.rs` per the crate's `_tests.rs` sibling-file convention (see
//! `intent::classifier_tests` for precedent) to keep the production file
//! under the 500-SLOC cap.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{Value, json};
use tempfile::TempDir;

use super::*;
use crate::agent_loop::telemetry;
use crate::agent_loop::with_cadence_env;
use crate::llm::{ChatRequest, ChatResponse, InferenceAdapter, InferenceError};
use crate::session::{SessionRegistry, SessionStatus};
use crate::task::mock_llm::EchoLlmClient;

/// Agents dir fixture with `pm.md` + `python-engineer.md` (mirrors
/// `run_task::tests::agents_dir`).
///
/// #7948: the PM's `tcode_tools:` allowlist deliberately OMITS
/// `delegate_to_agent`, as stock `pm.md` does, so this daemon path's tests run
/// through the gate's allowlist branch — see
/// `pm_delegates_through_the_tcode_tools_allowlist`.
fn agents_dir() -> TempDir {
    let tmp = tempfile::tempdir().expect("agents tempdir");
    std::fs::write(
        tmp.path().join("pm.md"),
        "---\nname: pm\nmodel: openai/gpt-4o-mini\ntcode_tools: [read_file, bash, finish_task]\n---\n\nYou are the PM.\n",
    )
    .expect("write pm.md");
    std::fs::write(
        tmp.path().join("python-engineer.md"),
        "---\nname: python-engineer\nmodel: deepseek/deepseek-chat\n---\n\nYou are a Python engineer.\n",
    )
    .expect("write python-engineer.md");
    tmp
}

fn params(agents: &TempDir, project: &TempDir, session_id: &str) -> TaskRunParams {
    TaskRunParams {
        permission_broker: None,
        permission_mode: crate::permissions::PermissionMode::Default,
        session_id: session_id.to_string(),
        task: "do something".to_string(),
        agent_name: "pm".to_string(),
        binding: crate::binding::ProjectBinding::resolve(Some(project.path().to_path_buf()))
            .expect("tempdir must bind"),
        agents_dir: agents.path().to_path_buf(),
        model_override: None,
        mode: crate::mode::HarnessMode::default(),
        deadline_secs: None,
        no_delegate: false,
        pm_model: None,
        max_turns: None,
        // #3902: this daemon path (`run_and_record`) always sets `cadence:
        // Some(_)` on the PM loop, so any test built from this shared
        // helper that runs enough turns to trip a real cadence/threshold
        // fire needs an isolated telemetry dir — `AgentLoop::telemetry_data_dir`'s
        // `#[cfg(test)]` guard panics on the first un-injected fire rather
        // than risk the race. Isolating every call unconditionally (rather
        // than only the specific tests known to fire today) means a FUTURE
        // test built on this same helper can never reintroduce the
        // omission either.
        telemetry_data_dir: Some(telemetry::test_temp_dir("task-executor")),
    }
}

/// A second `spawn_task_run` against the SAME session before the first
/// finishes must be rejected — the core "no overlapping runs" guarantee.
#[tokio::test]
async fn spawn_task_run_rejects_second_overlapping_run() {
    let registry = Arc::new(SessionRegistry::new());
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
    let agents = agents_dir();
    let project = tempfile::tempdir().expect("project tempdir");
    let llm: Arc<dyn InferenceAdapter> = Arc::new(EchoLlmClient::new());

    let p = params(&agents, &project, &session.id);

    spawn_task_run(Arc::clone(&registry), Arc::clone(&llm), p.clone())
        .expect("first run must start");
    let err = spawn_task_run(Arc::clone(&registry), llm, p)
        .expect_err("second overlapping run must be rejected");
    assert_eq!(err.code, -32003);

    // Let the background task actually finish so this test doesn't leak a
    // dangling tokio task past its own scope.
    registry
        .shutdown_executions(std::time::Duration::from_secs(5))
        .await;
}

/// `spawn_task_run` against an unknown session must error rather than
/// spawning anything.
#[tokio::test]
async fn spawn_task_run_unknown_session_errors() {
    let registry = Arc::new(SessionRegistry::new());
    let agents = agents_dir();
    let project = tempfile::tempdir().expect("project tempdir");
    let llm: Arc<dyn InferenceAdapter> = Arc::new(EchoLlmClient::new());

    let p = params(&agents, &project, "does-not-exist");
    let err = spawn_task_run(registry, llm, p).unwrap_err();
    assert_eq!(err.code, -32007);
}

// `aggregate_usage_per_role` itself is now `run_task::aggregate_usage_per_role`
// (#2061, #1475 bug 1) — its per-role pricing behaviour is tested once, at
// its actual definition site, in `run_task::report::tests`
// (`aggregate_usage_per_role_prices_each_role_separately`). This module only
// tests THIS daemon path's own wrapper (`resolve_engineer_model`, below).

/// `resolve_engineer_model` resolves via the embedded default roster
/// (#3046) — never panics, and no longer degrades to the literal
/// `"unknown"` — when the on-disk engineer config is missing, because
/// `ENGINEER_AGENT_NAME` (`"python-engineer"`) is one of the 31 embedded
/// roster names.
///
/// Why: before #3046, `resolve_agent_model_slug` read
/// `<agents_dir>/python-engineer.md` directly with no embedded fallback, so
/// a fresh project with no `.claude/agents/` priced every engineer turn
/// under the degraded `"unknown"` slug even though the run itself (once
/// #3046's other fixes land) now actually dispatches a real embedded
/// `python-engineer` config. This pins the corrected behaviour: pricing and
/// dispatch now agree.
/// What: empty agents dir, no `python-engineer.md`; asserts the resolved
/// slug is neither empty nor the literal `"unknown"` fallback string.
/// Test: this test.
#[test]
fn resolve_engineer_model_falls_back_to_embedded_when_disk_config_missing() {
    let empty_agents = tempfile::tempdir().expect("empty agents tempdir");
    let p = TaskRunParams {
        permission_broker: None,
        permission_mode: crate::permissions::PermissionMode::Default,
        session_id: "s".to_string(),
        task: "do something".to_string(),
        agent_name: "pm".to_string(),
        binding: crate::binding::ProjectBinding::resolve(Some(empty_agents.path().to_path_buf()))
            .expect("tempdir must bind"),
        agents_dir: empty_agents.path().to_path_buf(),
        model_override: None,
        mode: crate::mode::HarnessMode::default(),
        deadline_secs: None,
        no_delegate: false,
        pm_model: None,
        max_turns: None,
        // Never reaches an `AgentLoop` run in this test (only
        // `resolve_engineer_model` is called below) — irrelevant here.
        telemetry_data_dir: None,
    };
    let model = resolve_engineer_model(&p);
    assert_ne!(
        model, "unknown",
        "python-engineer is embedded (#3046); a missing disk config must resolve \
         via the embedded fallback, not degrade to unknown"
    );
    assert!(!model.is_empty());
}

/// `daily_driver_skills_catalog` returns `None` under `HarnessMode::Parity`,
/// even when the project has a real `.claude/skills/` catalog (#2069's
/// scope note: "Parity mode should NOT progressively disclose").
#[test]
fn daily_driver_skills_catalog_none_in_parity() {
    let agents = agents_dir();
    let project = tempfile::tempdir().expect("project tempdir");
    let skills_dir = project.path().join(".claude").join("skills").join("demo");
    std::fs::create_dir_all(&skills_dir).expect("mkdir skill dir");
    std::fs::write(
        skills_dir.join("SKILL.md"),
        "---\nname: demo\ndescription: Demo skill\n---\nbody\n",
    )
    .expect("write SKILL.md");

    let mut p = params(&agents, &project, "s");
    p.mode = crate::mode::HarnessMode::Parity;

    assert!(
        daily_driver_skills_catalog(&p, p.binding.root().expect("bound in this test")).is_none()
    );
}

/// `daily_driver_skills_catalog` falls back to the embedded default skill
/// catalog (#2895) when the project has no `.claude/skills/` directory at
/// all, in `HarnessMode::DailyDriver`.
///
/// Why: `skills::discover_skill_metadata`'s embedded-fallback (#2895) means
/// a project with no disk skills is no longer catalog-empty — it now
/// resolves to `crate::assets::DEFAULT_SKILLS`. This test previously asserted
/// `None` here; that assumption is exactly what #2895 changes, so the
/// assertion now pins the new (intended) behavior instead.
/// Test: this test.
#[test]
fn daily_driver_skills_catalog_falls_back_to_embedded_when_no_skills_dir() {
    let agents = agents_dir();
    let project = tempfile::tempdir().expect("project tempdir");

    let mut p = params(&agents, &project, "s");
    p.mode = crate::mode::HarnessMode::DailyDriver;

    let (catalog, resolver) =
        daily_driver_skills_catalog(&p, p.binding.root().expect("bound in this test"))
            .expect("embedded catalog present");
    assert!(catalog.contains("systematic-debugging"));
    assert!(resolver.resolve("systematic-debugging").is_some());
}

/// A project's `.claude/settings.json` `code_harness.cadence_turns` override
/// reaches `resolve_cadence_config` exactly the way `run_and_record` calls it
/// (#2346) — the same `params.project` path a real `spawn_task_run` would use.
///
/// Why: #2346's acceptance criteria explicitly call for an integration-style
/// proof that the settings.json precedence chain changes `cadence_turns`, not
/// just a unit test isolated to `agent_loop::cadence`'s own module (see
/// `cadence::tests::resolve_cadence_config_settings_json_override` for that
/// unit-level coverage) — this test exercises the SAME `TaskRunParams.project`
/// shape `run_and_record`'s `cadence: Some(resolve_cadence_config(&params.project))`
/// wiring consumes.
/// What: Build a project `TempDir` (via the same `params` helper every other
/// executor test uses) with a `.claude/settings.json` overriding
/// `cadence_turns` to `3`; assert `crate::agent_loop::resolve_cadence_config`
/// against `p.project` returns `3`, not the built-in default of `8`.
/// Test: this test.
///
/// Hermeticity: `resolve_cadence_config` reads the process-global
/// `TCODE_CADENCE_TURNS` env var, which the sibling
/// `crate::agent_loop::cadence::tests::resolve_cadence_config_env_wins_over_settings_json`
/// test sets to `"5"` while it runs. This test therefore resolves under
/// `with_cadence_env(None, None, …)` — holding `CADENCE_ENV_LOCK` and forcing
/// the env var unset — so a concurrent env-setting test cannot bleed its
/// `TCODE_CADENCE_TURNS=5` into this resolver and make the `== 3` assertion
/// observe `5` (fixing the pre-existing flake this test guards against).
#[tokio::test]
async fn settings_json_cadence_turns_override_reaches_resolver() {
    with_cadence_env(None, None, || {
        let agents = agents_dir();
        let project = tempfile::tempdir().expect("project tempdir");
        std::fs::create_dir_all(project.path().join(".claude")).expect("mkdir .claude");
        std::fs::write(
            project.path().join(".claude").join("settings.json"),
            r#"{"code_harness": {"cadence_turns": 3}}"#,
        )
        .expect("write settings.json");

        let p = params(&agents, &project, "s");
        let cfg = crate::agent_loop::resolve_cadence_config(
            p.binding.root().expect("bound in this test"),
        );
        assert_eq!(cfg.cadence_turns, 3);
        assert_ne!(
            cfg.cadence_turns,
            crate::agent_loop::CadenceConfig::default().cadence_turns
        );
    })
    .await;
}

/// `daily_driver_skills_catalog` returns the rendered catalog + a working
/// resolver under `HarnessMode::DailyDriver` when skills exist.
#[test]
fn daily_driver_skills_catalog_some_when_skills_exist() {
    let agents = agents_dir();
    let project = tempfile::tempdir().expect("project tempdir");
    let skills_dir = project.path().join(".claude").join("skills").join("demo");
    std::fs::create_dir_all(&skills_dir).expect("mkdir skill dir");
    std::fs::write(
        skills_dir.join("SKILL.md"),
        "---\nname: demo\ndescription: Demo skill\n---\nfull body\n",
    )
    .expect("write SKILL.md");

    let mut p = params(&agents, &project, "s");
    p.mode = crate::mode::HarnessMode::DailyDriver;

    let (catalog, resolver) =
        daily_driver_skills_catalog(&p, p.binding.root().expect("bound in this test"))
            .expect("catalog present");
    assert!(catalog.contains("demo: Demo skill"));
    assert_eq!(resolver.resolve("demo").as_deref(), Some("full body"));
}

/// `ProjectToolFactory::build` threads the run's resolved `HarnessMode` onto
/// the engineer's `EditTool` (#2073) — under `HarnessMode::Parity`, the
/// dispatched `edit` call must prefer a supplied unified-diff payload even
/// when the calling model slug would prefer SEARCH/REPLACE under the plain
/// per-model matrix, proving the daemon path actually wires `with_mode`
/// through, not just that `EditTool` supports it in isolation
/// (`tools::fs::edit::tests::edit_under_parity_mode_prefers_unified_diff_even_for_flagship_model_slug`
/// covers the tool itself).
#[tokio::test]
async fn project_tool_factory_threads_parity_mode_into_edit_tool() {
    let project = tempfile::tempdir().expect("project tempdir");
    std::fs::write(project.path().join("f.py"), "line1\nline2\n").expect("seed file");

    let factory = ProjectToolFactory {
        project: project.path().to_path_buf(),
        mode: crate::mode::HarnessMode::Parity,
        skill_resolver: None,
        mcp: tokio::sync::OnceCell::new(),
    };
    let agent = crate::agents::AgentConfig::default();
    let ctx = crate::tools::RunContext {
        model: Some("anthropic/claude-opus-4-5".to_string()),
        ..Default::default()
    };
    let registry = factory.build(&agent, &ctx).await;

    let result = registry
        .dispatch_gated(
            "edit",
            serde_json::json!({
                "path": "f.py",
                "old_string": "not-present",
                "new_string": "x",
                "diff": "@@ -2,1 +2,1 @@\n-line2\n+line2-diffed\n"
            }),
            None,
        )
        .await;

    assert!(!result.is_error(), "unexpected error: {}", result.content());
    assert!(result.content().contains("unified_diff"));
    let updated = std::fs::read_to_string(project.path().join("f.py")).expect("read");
    assert_eq!(updated, "line1\nline2-diffed\n");
}

/// #7727 review HIGH 1: an embedded roster agent resolved with no disk config
/// carries skill pointers the engineer registry's `read_file` opens, although
/// they lie outside the project root.
#[tokio::test]
async fn engineer_registry_reads_embedded_agent_skill_pointers() {
    use crate::agents::skill_refs::{REFERENCED_SKILL_FILES, user_skill_refs_dir};

    let project = tempfile::tempdir().expect("project tempdir");
    let agent = crate::agents::resolve_agent(project.path(), "rust-engineer")
        .expect("embedded rust-engineer resolves");
    let factory = ProjectToolFactory {
        project: project.path().to_path_buf(),
        mode: crate::mode::HarnessMode::DailyDriver,
        skill_resolver: None,
        mcp: tokio::sync::OnceCell::new(),
    };
    let registry = factory.build(&agent, &RunContext::default()).await;
    for (relative, content) in REFERENCED_SKILL_FILES {
        let pointer = user_skill_refs_dir().join(relative).display().to_string();
        assert!(agent.system_prompt.content.contains(&pointer), "{pointer}");
        let out = registry
            .dispatch("read_file", serde_json::json!({ "path": pointer }))
            .await;
        assert!(!out.is_error(), "{pointer}: {}", out.content());
        assert_eq!(out.content(), *content);
    }
}

/// #2152: `ProjectToolFactory::build` registers `use_skill` when a skill
/// resolver was threaded in — the gap PR #2942 fixed on the legacy
/// `run_task/mod.rs` path but left open on this daemon path (`task::executor`),
/// where the engineer's prompt (`with_skills_catalog`) advertised `use_skill`
/// while its registry omitted the tool, so a following call errored with
/// "no tool registered".
///
/// Why: the positive case (`Some(resolver)` registers the tool) and the
/// negative case (`None` must NOT register it) are both asserted directly
/// against `ProjectToolFactory::build`'s resulting `ToolRegistry`, mirroring
/// `run_task::tests::use_skill_absent_from_engineer_when_no_skills`'s direct
/// (non-end-to-end) style — the embedded-default-skill-catalog fallback
/// (#2895) makes `skill_resolver` virtually always `Some` on any real project
/// directory, so exercising the `None` branch end-to-end would be impractical.
/// What: builds one `ProjectToolFactory` with a resolver present and one with
/// `skill_resolver: None`, calls `.build()` on each, and asserts
/// `registry.contains("use_skill")` accordingly.
/// Test: this test.
#[tokio::test]
async fn engineer_registry_includes_use_skill_when_catalog_present() {
    let agents = agents_dir();
    let project = tempfile::tempdir().expect("project tempdir");
    let skills_dir = project.path().join(".claude").join("skills").join("demo");
    std::fs::create_dir_all(&skills_dir).expect("mkdir skill dir");
    std::fs::write(
        skills_dir.join("SKILL.md"),
        "---\nname: demo\ndescription: Demo skill\n---\nfull body\n",
    )
    .expect("write SKILL.md");

    let mut p = params(&agents, &project, "s");
    p.mode = crate::mode::HarnessMode::DailyDriver;
    let (_, resolver) =
        daily_driver_skills_catalog(&p, p.binding.root().expect("bound")).expect("catalog present");

    let with_resolver = ProjectToolFactory {
        project: project.path().to_path_buf(),
        mode: crate::mode::HarnessMode::DailyDriver,
        skill_resolver: Some(resolver),
        mcp: tokio::sync::OnceCell::new(),
    };
    let registry = with_resolver
        .build(
            &crate::agents::AgentConfig::default(),
            &RunContext::default(),
        )
        .await;
    assert!(
        registry.contains("use_skill"),
        "engineer registry must advertise use_skill when skill_resolver is Some"
    );

    let without_resolver = ProjectToolFactory {
        project: project.path().to_path_buf(),
        mode: crate::mode::HarnessMode::DailyDriver,
        skill_resolver: None,
        mcp: tokio::sync::OnceCell::new(),
    };
    let registry = without_resolver
        .build(
            &crate::agents::AgentConfig::default(),
            &RunContext::default(),
        )
        .await;
    assert!(
        !registry.contains("use_skill"),
        "engineer registry must NOT advertise use_skill when skill_resolver is None"
    );
    assert!(
        registry.contains("finish_task"),
        "sanity check: registry should still contain other always-present tools"
    );
}

// ── #2207/#2206: daemon-path deadline wiring + distinct status + telemetry ─────

/// A response in which the assistant calls `finish_task` with a required
/// field (`summary`) missing — recoverable per #2072's schema-validation
/// path, NOT terminal (mirrors `run_task::tests::malformed_finish_task_response`).
fn malformed_finish_task_response() -> Value {
    json!({
        "id": "gen-finish-malformed",
        "choices": [{
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call-missing",
                    "type": "function",
                    "function": {
                        "name": "finish_task",
                        "arguments": json!({"status": "completed"}).to_string()
                    }
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": {"prompt_tokens": 40, "completion_tokens": 10, "total_tokens": 50}
    })
}

/// A response in which the assistant emits final text and stops.
fn stop_response(text: &str) -> Value {
    json!({
        "id": "gen-stop",
        "choices": [{
            "message": { "role": "assistant", "content": text, "tool_calls": [] },
            "finish_reason": "stop"
        }],
        "usage": { "prompt_tokens": 15, "completion_tokens": 5, "total_tokens": 20 }
    })
}

/// A response in which the assistant delegates to `python-engineer` (#7948).
fn delegate_response(task: &str) -> Value {
    json!({
        "id": "gen-delegate",
        "choices": [{
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call-del",
                    "type": "function",
                    "function": {
                        "name": "delegate_to_agent",
                        "arguments": json!({"agent_name": "python-engineer", "task": task}).to_string()
                    }
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": { "prompt_tokens": 50, "completion_tokens": 12, "total_tokens": 62 }
    })
}

/// A response in which the assistant calls `write_file(path, content)` (#7948).
fn write_file_response(path: &str, content: &str) -> Value {
    json!({
        "id": "gen-write",
        "choices": [{
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call-wf",
                    "type": "function",
                    "function": {
                        "name": "write_file",
                        "arguments": json!({"path": path, "content": content}).to_string()
                    }
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": { "prompt_tokens": 30, "completion_tokens": 8, "total_tokens": 38 }
    })
}

/// (#7948 regression) The DAEMON path's delegation survives a `tcode_tools:`
/// allowlist that never names `delegate_to_agent`.
///
/// Why: `run_and_record` wires `delegate_to_agent` into the PM's registry
/// unconditionally, exactly as `run_task::execute_run_task` does, so the
/// harness-registered exemption has to hold on BOTH run-task paths. The
/// sibling proof for the one-shot CLI path is
/// `run_task::tests::pm_delegates_through_the_tcode_tools_allowlist`.
/// What: assert the fixture premise, then script [PM delegate, engineer
/// write_file, engineer stop, PM stop] through the real `spawn_task_run` and
/// assert the engineer's file landed — a refused delegation writes nothing.
/// Test: this test.
#[tokio::test]
async fn pm_delegates_through_the_tcode_tools_allowlist() {
    let registry = Arc::new(SessionRegistry::new());
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
    let agents = agents_dir();
    let project = tempfile::tempdir().expect("project tempdir");

    let pm = crate::agents::resolve_agent(agents.path(), "pm").expect("fixture pm.md loads");
    let allowed = pm
        .tools
        .as_ref()
        .and_then(|t| t.allowed.as_ref())
        .expect("fixture premise: the PM must declare a tcode_tools allowlist");
    assert!(
        !allowed.iter().any(|t| t == "delegate_to_agent"),
        "fixture premise: the allowlist must omit delegate_to_agent, got {allowed:?}"
    );

    let llm: Arc<dyn InferenceAdapter> = Arc::new(ScriptedLlm::from_json(&[
        delegate_response("create delegated.py"),
        write_file_response("delegated.py", "print('dispatched')"),
        stop_response("engineer: done"),
        stop_response("pm: task complete"),
    ]));
    spawn_task_run(
        Arc::clone(&registry),
        llm,
        params(&agents, &project, &session.id),
    )
    .expect("run must start");
    wait_for_terminal(&registry, &session.id).await;

    assert_eq!(
        std::fs::read_to_string(project.path().join("delegated.py")).ok(),
        Some("print('dispatched')".to_string()),
        "a refused delegate_to_agent never reaches the engineer, so nothing is written"
    );
}

/// An `InferenceAdapter` that sleeps before its Nth `chat` call, then replays a
/// scripted response (mirrors `run_task::tests::DeadlineTriggerLlm`).
///
/// Why: deterministically drives the PM's own wall-clock deadline past its
/// configured budget, entirely within the PM's own loop (no delegation), so
/// the daemon path's `run_and_record` observes a genuine
/// `AgentLoopError::Timeout` rather than racing against the delegated
/// engineer's own independently-resolved deadline.
struct DeadlineTriggerLlm {
    responses: Vec<ChatResponse>,
    cursor: AtomicUsize,
    stall_at_call: usize,
    stall_for: std::time::Duration,
}

impl DeadlineTriggerLlm {
    fn new(fixtures: &[Value], stall_at_call: usize, stall_for: std::time::Duration) -> Self {
        let responses = fixtures
            .iter()
            .map(|v| serde_json::from_value(v.clone()).expect("valid ChatResponse fixture"))
            .collect();
        Self {
            responses,
            cursor: AtomicUsize::new(0),
            stall_at_call,
            stall_for,
        }
    }
}

#[async_trait]
impl InferenceAdapter for DeadlineTriggerLlm {
    crate::llm::mock_adapter_identity!("mock-deadline-trigger");

    async fn chat(&self, _req: &ChatRequest) -> Result<ChatResponse, InferenceError> {
        let idx = self.cursor.fetch_add(1, Ordering::SeqCst);
        if idx == self.stall_at_call {
            tokio::time::sleep(self.stall_for).await;
        }
        match self.responses.get(idx) {
            Some(resp) => Ok(resp.clone()),
            None => Err(InferenceError::MissingConfig(format!(
                "scripted LLM exhausted at call {idx}"
            ))),
        }
    }
}

/// A tiny `deadline_secs` override on the daemon path yields
/// `SessionStatus::DeadlineExceeded` (distinct from `Failed`), and the
/// persisted transcript/usage still reflect the turn that completed before
/// the deadline fired (#2207 + #2206's daemon-path equivalent of
/// `run_task::tests::exit_code_reflects_deadline_exceeded_distinct_from_run_failure`).
///
/// Why: `task::executor::run_and_record` calls `registry.set_run_outcome`
/// unconditionally before branching on the loop's `result` (#2206 was
/// already correct here — this test pins that #2207's new
/// `SessionStatus::DeadlineExceeded` arm doesn't regress it), and the
/// deadline must actually reach the PM's `AgentLoopConfig` via
/// `resolve_deadline_secs(params.deadline_secs)`.
/// What: `deadline_secs: Some(1)`; turn 0 is a malformed `finish_task` call
/// (instant, recoverable, recorded with real usage), turn 1 sleeps 3s. Assert
/// the session ends `DeadlineExceeded` (not `Failed`) and its stored usage is
/// non-zero.
/// Test: this test.
#[tokio::test]
async fn spawn_task_run_deadline_exceeded_is_distinct_and_preserves_usage() {
    let registry = Arc::new(SessionRegistry::new());
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
    let agents = agents_dir();
    let project = tempfile::tempdir().expect("project tempdir");

    let llm: Arc<dyn InferenceAdapter> = Arc::new(DeadlineTriggerLlm::new(
        &[
            malformed_finish_task_response(),
            stop_response("recovered (never reached in time)"),
        ],
        1,
        std::time::Duration::from_secs(3),
    ));

    let mut p = params(&agents, &project, &session.id);
    p.deadline_secs = Some(1);

    spawn_task_run(Arc::clone(&registry), llm, p).expect("run must start");

    // Poll for the run to reach a terminal state naturally (via its own 1s
    // deadline), WITHOUT `shutdown_executions` — that flips every tracked
    // execution's cancel flag immediately, which would race the deadline and
    // spuriously report `Cancelled` instead of `DeadlineExceeded`.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let status = registry.status(&session.id).expect("session must exist");
        if status.status.is_terminal() {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "run did not reach a terminal state within 5s"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    let status = registry.status(&session.id).expect("session must exist");
    assert_eq!(
        status.status,
        SessionStatus::DeadlineExceeded,
        "a deadline hit must map to DeadlineExceeded, not Failed"
    );

    let transcript = registry
        .get_transcript(&session.id)
        .expect("transcript must exist");
    assert!(
        transcript.usage.prompt_tokens > 0,
        "the completed first turn must still contribute real usage, got {:?}",
        transcript.usage
    );
    assert!(
        transcript.cost_usd.is_some(),
        "cost must be populated (not None) on the deadline-exceeded path"
    );
}

/// A response in which the assistant calls `set_goal` and never stops —
/// used to drive a scripted LLM through `max_turns` consecutive tool-call
/// turns without ever reaching a natural `stop`/`finish_task` terminal state
/// (#3888's `TurnCapExceeded` regression test below).
fn set_goal_tool_call_response(call_id: &str) -> Value {
    json!({
        "id": format!("gen-{call_id}"),
        "choices": [{
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": call_id,
                    "type": "function",
                    "function": {
                        "name": crate::tools::goals::SET_GOAL_TOOL_NAME,
                        "arguments": json!({"slot": 1, "text": "keep going"}).to_string()
                    }
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}
    })
}

/// An `InferenceAdapter` that replays a fixed script, erroring past the end —
/// mirrors `agent_loop::tests::ScriptedLlm` but scoped to this module so the
/// daemon-path executor tests don't need to reach into `agent_loop`'s
/// `#[cfg(test)]`-private fixtures.
struct ScriptedLlm {
    responses: Vec<ChatResponse>,
    cursor: AtomicUsize,
    /// (#8031) Tool-schema names from the FIRST request, so one fixture can
    /// prove both what the loop advertised and what it then did.
    tool_names: Mutex<Vec<String>>,
    /// (#8030/#8128) Every request's model slug, in call order — the wire
    /// observation both the top-level model override and the turn-cap
    /// override are asserted against.
    models: Mutex<Vec<String>>,
}

impl ScriptedLlm {
    fn from_json(fixtures: &[Value]) -> Self {
        let responses = fixtures
            .iter()
            .map(|v| serde_json::from_value(v.clone()).expect("valid ChatResponse fixture"))
            .collect();
        Self {
            responses,
            cursor: AtomicUsize::new(0),
            tool_names: Mutex::new(Vec::new()),
            models: Mutex::new(Vec::new()),
        }
    }

    /// The tool names advertised on the first request (#8031).
    fn first_tool_names(&self) -> Vec<String> {
        self.tool_names.lock().expect("tool_names lock").clone()
    }

    /// Every model slug the client was asked to use, in call order
    /// (#8030/#8128).
    fn models_seen(&self) -> Vec<String> {
        self.models.lock().expect("models lock").clone()
    }
}

#[async_trait]
impl InferenceAdapter for ScriptedLlm {
    crate::llm::mock_adapter_identity!("mock-scripted");

    async fn chat(&self, req: &ChatRequest) -> Result<ChatResponse, InferenceError> {
        let idx = self.cursor.fetch_add(1, Ordering::SeqCst);
        self.models
            .lock()
            .expect("models lock")
            .push(req.model.clone());
        if idx == 0 {
            *self.tool_names.lock().expect("tool_names lock") = req
                .tools
                .as_ref()
                .map(|tools| tools.iter().map(|t| t.function.name.clone()).collect())
                .unwrap_or_default();
        }
        match self.responses.get(idx) {
            Some(resp) => Ok(resp.clone()),
            None => Err(InferenceError::MissingConfig(format!(
                "scripted LLM exhausted at call {idx}"
            ))),
        }
    }
}

/// (#3888) Exhausting the PM's per-call `max_turns` budget must map to
/// `SessionStatus::TurnCapExceeded`, NOT `SessionStatus::Failed` — and the
/// session must remain resumable via a SECOND `spawn_task_run` against the
/// same `session_id`, exactly like a naturally `Finished` session (#2344).
///
/// Why: this is the exact regression reported in #3888 — before this fix,
/// `task::executor::run_and_record`'s terminal-status match had no
/// `TurnCapExceeded` arm, so `AgentLoopError::TurnCapExceeded` fell through
/// the catch-all into `SessionStatus::Failed`, which
/// `SessionRegistry::begin_execution` permanently rejects — directly
/// regressing epic #2343's infinite-sessions goal (a session must never die
/// just because one call used its whole per-call turn allowance).
/// What: scripts `AgentLoopConfig::default().max_turns` (8) consecutive
/// `set_goal` tool-call turns — the PM never calls `finish_task` or emits a
/// bare `stop`, so the loop can only terminate via the turn cap. Asserts the
/// session lands in `TurnCapExceeded` (not `Failed`), then issues a SECOND
/// `spawn_task_run` against the SAME session and asserts it is accepted (not
/// the `-32003` terminal-session rejection) and reaches a fresh terminal
/// state.
/// Test: this test.
#[tokio::test]
async fn spawn_task_run_turn_cap_exceeded_is_resumable() {
    let registry = Arc::new(SessionRegistry::new());
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
    let agents = agents_dir();
    let project = tempfile::tempdir().expect("project tempdir");

    // 8 responses = `AgentLoopConfig::default().max_turns`; none stop or
    // call `finish_task`, so the PM loop can only end via `TurnCapExceeded`.
    let scripts: Vec<Value> = (0..8)
        .map(|i| set_goal_tool_call_response(&format!("call_{i}")))
        .collect();
    let llm: Arc<dyn InferenceAdapter> = Arc::new(ScriptedLlm::from_json(&scripts));

    let p1 = params(&agents, &project, &session.id);
    spawn_task_run(Arc::clone(&registry), llm, p1).expect("run 1 must start");
    wait_for_terminal(&registry, &session.id).await;

    assert_eq!(
        registry.status(&session.id).unwrap().status,
        SessionStatus::TurnCapExceeded,
        "exhausting max_turns must map to TurnCapExceeded, not Failed (#3888)"
    );

    // The regression: a second `task.run` on this session must be ACCEPTED,
    // not rejected with "session ... is already terminal" (-32003).
    let llm2: Arc<dyn InferenceAdapter> = Arc::new(EchoLlmClient::new());
    let p2 = params(&agents, &project, &session.id);
    spawn_task_run(Arc::clone(&registry), llm2, p2).expect(
        "a TurnCapExceeded session must accept a resuming task.run, \
         not be permanently unresumable (#3888)",
    );
    wait_for_terminal(&registry, &session.id).await;

    // `EchoLlmClient`'s script deterministically ends in a natural stop
    // (`task::mock_llm::EchoLlmClient`'s own docs), so a correctly-resumed
    // run must reach `Finished` exactly — not merely "not Failed", which
    // would also pass if resumption silently regressed to e.g. another
    // `TurnCapExceeded`/`DeadlineExceeded`/`Cancelled`.
    let final_status = registry.status(&session.id).unwrap().status;
    assert_eq!(
        final_status,
        SessionStatus::Finished,
        "the resumed run must reach its own natural Finished terminal state"
    );
}

/// (#4351) A finished run must leave an actionable `TaskResult` on the
/// session, not just a status.
///
/// Why: before #4351 `session.status` handed a caller `id`/`status`/`mode` and
/// nothing it could report to a user. This is the daemon half of the fix: the
/// executor is the only producer, and it must fire on the ordinary success
/// path, not just on the interesting ones.
/// What: runs `EchoLlmClient` (a deterministic natural stop) against a plain,
/// non-git temp project. Non-git means there is no ref to point at, so this
/// also pins the "no diff, no invented refs" half of the contract: `diff_ref`,
/// `branch` and `pr_ref` all `None`, with the status and a real summary still
/// present.
/// Test: this test.
#[tokio::test]
async fn spawn_task_run_records_a_task_result_on_the_session() {
    let registry = Arc::new(SessionRegistry::new());
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
    let agents = agents_dir();
    let project = tempfile::tempdir().expect("project tempdir");

    let llm: Arc<dyn InferenceAdapter> = Arc::new(EchoLlmClient::new());
    spawn_task_run(
        Arc::clone(&registry),
        llm,
        params(&agents, &project, &session.id),
    )
    .expect("run must start");
    wait_for_terminal(&registry, &session.id).await;

    let snapshot = registry.status(&session.id).expect("session must exist");
    assert_eq!(snapshot.status, SessionStatus::Finished);
    let result = snapshot
        .result
        .expect("#4351: a finished run must carry a TaskResult");
    assert_eq!(result.status, crate::session::TaskResultStatus::Success);
    assert_eq!(result.pr_ref, None, "the daemon never opens a pull request");
    assert!(
        result.summary.is_some(),
        "a result must carry a user-facing summary"
    );
}

// ── #2344: persistent session-scoped transcript across task.run calls ──────────

/// Poll `registry.status(id)` until it reaches a terminal state, bounded by
/// a 5s deadline (mirrors the inline poll loop in
/// `spawn_task_run_deadline_exceeded_is_distinct_and_preserves_usage`).
async fn wait_for_terminal(registry: &SessionRegistry, id: &str) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let status = registry.status(id).expect("session must exist");
        if status.status.is_terminal() {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "run did not reach a terminal state within 5s"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

/// A SECOND `spawn_task_run` on the SAME session, issued AFTER the first run
/// has already `Finished`, must be ACCEPTED (not rejected as terminal, #2344)
/// and must APPEND onto the first run's turns/usage rather than overwrite
/// them — this is #2344's headline acceptance criterion exercised through
/// the real daemon wiring (`spawn_task_run` -> `run_and_record` ->
/// `SessionRegistry::begin_pm_transcript`/`begin_execution` resumption /
/// `set_run_outcome` accumulation), not just the unit-level registry/loop
/// tests.
///
/// Why: unit tests already cover each collaborator (`begin_pm_transcript`,
/// `begin_execution`'s `Finished`-resumption, `set_run_outcome`
/// accumulation, `AgentLoop::run_with_transcript`'s no-reseed/output-scoping
/// behaviour) in isolation; this test is the integration proof that
/// `task::executor` actually wires them together correctly end to end.
/// What: run 1 completes via the `EchoLlmClient` script; poll to terminal;
/// snapshot `get_transcript`. Run 2 (a FRESH `EchoLlmClient`, its own script
/// from call 0 — the daemon builds a new LLM client per `task.run`
/// regardless of session) targets the SAME `session_id`; assert
/// `spawn_task_run` accepts it (not `-32003` terminal-session rejection);
/// poll to terminal again; assert the session's cumulative turn count grew,
/// run 1's turns are still present unchanged at the front, and usage
/// accumulated (run 2's usage > 0 on top of run 1's).
/// Test: this test.
#[tokio::test]
async fn spawn_task_run_second_call_after_finish_appends_to_cumulative_transcript() {
    let registry = Arc::new(SessionRegistry::new());
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
    let agents = agents_dir();
    let project = tempfile::tempdir().expect("project tempdir");

    // Run 1: completes the full delegate -> bash -> stop -> stop script.
    let llm1: Arc<dyn InferenceAdapter> = Arc::new(EchoLlmClient::new());
    let p1 = params(&agents, &project, &session.id);
    spawn_task_run(Arc::clone(&registry), llm1, p1).expect("run 1 must start");
    wait_for_terminal(&registry, &session.id).await;
    assert_eq!(
        registry.status(&session.id).unwrap().status,
        SessionStatus::Finished
    );

    let after_run_one = registry
        .get_transcript(&session.id)
        .expect("transcript must exist after run 1");
    assert!(
        !after_run_one.turns.is_empty(),
        "run 1 must have recorded turns"
    );

    // Run 2 against the SAME session_id, issued only AFTER run 1 fully
    // finished — must be ACCEPTED, not rejected as an already-terminal
    // session.
    let llm2: Arc<dyn InferenceAdapter> = Arc::new(EchoLlmClient::new());
    let mut p2 = params(&agents, &project, &session.id);
    p2.task = "do something else".to_string();
    spawn_task_run(Arc::clone(&registry), llm2, p2)
        .expect("a Finished session must accept a follow-up task.run (#2344)");
    wait_for_terminal(&registry, &session.id).await;
    assert_eq!(
        registry.status(&session.id).unwrap().status,
        SessionStatus::Finished,
        "run 2 must also finish successfully"
    );

    let after_run_two = registry
        .get_transcript(&session.id)
        .expect("transcript must exist after run 2");
    assert!(
        after_run_two.turns.len() > after_run_one.turns.len(),
        "run 2 must APPEND more turns onto run 1's, not replace them: {} vs {}",
        after_run_two.turns.len(),
        after_run_one.turns.len()
    );
    assert_eq!(
        after_run_two.turns[..after_run_one.turns.len()],
        after_run_one.turns[..],
        "run 1's turns must still be present, unchanged, at the front of the cumulative list"
    );
    assert!(
        after_run_two.usage.prompt_tokens > after_run_one.usage.prompt_tokens,
        "usage must accumulate across runs, not reset: run1={:?} run2={:?}",
        after_run_one.usage,
        after_run_two.usage
    );
}

/// A scripted `InferenceAdapter` that records the tool-schema names of every
/// request it receives, then immediately stops (no tool calls) — the D3
/// no-tool-call convention `finish_task`'s own docs describe, so a run
/// completes in a single PM turn with no delegation needed.
///
/// Why: #2348's `recall_session` registration is a daemon-session-path-only
/// concern (`task::executor::run_and_record`'s `pm_registry`, never
/// `run_task::execute_run_task`'s). Asserting on the ACTUAL tool schemas the
/// PM's `AgentLoop` sends the LLM (rather than re-deriving the registration
/// logic in the test) proves the real wiring, not a duplicate of it.
struct SchemaCapturingLlm {
    tool_names: Mutex<Vec<String>>,
}

impl SchemaCapturingLlm {
    fn new() -> Self {
        Self {
            tool_names: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl InferenceAdapter for SchemaCapturingLlm {
    crate::llm::mock_adapter_identity!("mock-schema-capturing");

    async fn chat(&self, req: &ChatRequest) -> Result<ChatResponse, InferenceError> {
        let names = req
            .tools
            .as_ref()
            .map(|tools| tools.iter().map(|t| t.function.name.clone()).collect())
            .unwrap_or_default();
        *self.tool_names.lock().expect("tool_names lock") = names;
        let fixture = json!({
            "id": "mock-stop",
            "choices": [{
                "message": {"role": "assistant", "content": "done", "tool_calls": []},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
        });
        serde_json::from_value(fixture).map_err(|e| InferenceError::MissingConfig(e.to_string()))
    }
}

/// The daemon-session path's `pm_registry` (`task::executor::run_and_record`)
/// must register `recall_session` (#2348) alongside `delegate_to_agent` and
/// `finish_task`.
#[tokio::test]
async fn session_path_registers_recall_session_tool() {
    let registry = Arc::new(SessionRegistry::new());
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
    let agents = agents_dir();
    let project = tempfile::tempdir().expect("project tempdir");

    let mock = Arc::new(SchemaCapturingLlm::new());
    let llm: Arc<dyn InferenceAdapter> = Arc::clone(&mock) as Arc<dyn InferenceAdapter>;
    let p = params(&agents, &project, &session.id);

    spawn_task_run(Arc::clone(&registry), llm, p).expect("run must start");
    wait_for_terminal(&registry, &session.id).await;

    let names = mock.tool_names.lock().expect("tool_names lock").clone();
    assert!(
        names.contains(&"recall_session".to_string()),
        "daemon-session path must register recall_session; got {names:?}"
    );
    assert!(
        names.contains(&"finish_task".to_string()),
        "sanity: finish_task must still be registered alongside it; got {names:?}"
    );
}

/// #8031: a `no_delegate` run on the DAEMON path must not advertise
/// `delegate_to_agent`.
///
/// Why: `tcode run-task` routes through this path by default (the thin JSON-RPC
/// client), so the issue's closure condition — no `delegate_to_agent` call in
/// the transcript — is only met if the tool never reaches the wire here. A
/// tool the model is never shown is a tool it cannot call.
/// What: drives `spawn_task_run` with `no_delegate: true` and the existing
/// `SchemaCapturingLlm`, then asserts the PM's advertised schema set. Fails on
/// `origin/main`, where the tool is registered unconditionally.
/// Test: this test.
#[tokio::test]
async fn no_delegate_run_omits_delegate_tool() {
    let registry = Arc::new(SessionRegistry::new());
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
    let agents = agents_dir();
    let project = tempfile::tempdir().expect("project tempdir");

    let mock = Arc::new(SchemaCapturingLlm::new());
    let llm: Arc<dyn InferenceAdapter> = Arc::clone(&mock) as Arc<dyn InferenceAdapter>;
    let p = TaskRunParams {
        no_delegate: true,
        ..params(&agents, &project, &session.id)
    };

    spawn_task_run(Arc::clone(&registry), llm, p).expect("run must start");
    wait_for_terminal(&registry, &session.id).await;

    let names = mock.tool_names.lock().expect("tool_names lock").clone();
    assert!(
        names.contains(&"finish_task".to_string()),
        "sanity: finish_task must still be registered; got {names:?}"
    );
    assert!(
        !names.contains(&"delegate_to_agent".to_string()),
        "a no_delegate run must not advertise delegate_to_agent; got {names:?}"
    );
}

/// #8031 companion: the DEFAULT daemon run still advertises
/// `delegate_to_agent`.
///
/// Why: without this, `no_delegate_run_omits_delegate_tool` would also pass if
/// the tool were dropped from every run — the fix would be indistinguishable
/// from a regression.
/// What: same fixture, `no_delegate` left at its `false` default.
/// Test: this test.
#[tokio::test]
async fn session_path_registers_delegate_tool_by_default() {
    let registry = Arc::new(SessionRegistry::new());
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
    let agents = agents_dir();
    let project = tempfile::tempdir().expect("project tempdir");

    let mock = Arc::new(SchemaCapturingLlm::new());
    let llm: Arc<dyn InferenceAdapter> = Arc::clone(&mock) as Arc<dyn InferenceAdapter>;
    let p = params(&agents, &project, &session.id);

    spawn_task_run(Arc::clone(&registry), llm, p).expect("run must start");
    wait_for_terminal(&registry, &session.id).await;

    let names = mock.tool_names.lock().expect("tool_names lock").clone();
    assert!(
        names.contains(&"delegate_to_agent".to_string()),
        "the default run must keep advertising delegate_to_agent; got {names:?}"
    );
}

// ── #8031: a `--no-delegate` run carries the NAMED agent's own tools ──────────

/// Agents-dir fixture naming a top-level `engineer` a run can target directly.
///
/// Why (#8031): the issue's closure command is `tcode run-task engineer
/// --no-delegate ...`, so the tests below need a real on-disk `engineer.md`
/// whose `tcode_tools:` allowlist they control — that allowlist is the gate a
/// single-agent run must honour exactly as a delegated run does.
/// What: [`agents_dir`]'s `pm.md`/`python-engineer.md` plus an `engineer.md`
/// carrying `tcode_tools`.
/// Test: `no_delegate_run_writes_the_file_without_delegating`,
/// `no_delegate_run_respects_the_agents_tools_allowlist`.
fn engineer_agents_dir(tcode_tools: &str) -> TempDir {
    let tmp = agents_dir();
    std::fs::write(
        tmp.path().join("engineer.md"),
        format!(
            "---\nname: engineer\nmodel: openai/gpt-4o-mini\ntcode_tools: [{tcode_tools}]\n---\n\nYou are the engineer.\n"
        ),
    )
    .expect("write engineer.md");
    tmp
}

/// A single `write_file` tool call (#8031).
fn write_file_tool_call_response(call_id: &str, path: &str, content: &str) -> Value {
    json!({
        "id": "mock-write-file",
        "choices": [{
            "message": {
                "role": "assistant",
                "content": "",
                "tool_calls": [{
                    "id": call_id,
                    "type": "function",
                    "function": {
                        "name": "write_file",
                        "arguments": json!({"path": path, "content": content}).to_string()
                    }
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}
    })
}

/// #8031 CLOSURE TEST: `run-task engineer --no-delegate "create hello.txt
/// containing hello"` writes the file with ZERO `delegate_to_agent` calls,
/// attributed to `engineer`.
///
/// Why: round 1 dropped `delegate_to_agent` but left the top-level loop with
/// only `finish_task`/`set_goal`/`clear_goal`/`use_skill`/`recall_session` —
/// every file tool lived on the DELEGATED agent's per-delegation registry, so
/// a `--no-delegate` run had nothing to do the work with. This is the issue's
/// stated closure condition driven through the real daemon entry point
/// (`spawn_task_run`, what `task.run` calls), not a registry unit test.
/// What: scripts one `write_file` call then a natural stop, and asserts (a)
/// `hello.txt` exists in the bound project with the requested content, (b)
/// neither the advertised schema set nor any emitted tool event names
/// `delegate_to_agent`, and (c) every tool event is attributed to `engineer`.
/// FAILS at 7cdc3b562: `write_file` is not registered, so the call errors and
/// the file is never created.
/// Test: this test.
#[tokio::test]
async fn no_delegate_run_writes_the_file_without_delegating() {
    let registry = Arc::new(SessionRegistry::new());
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
    let agents = engineer_agents_dir("read_file, write_file, edit, bash, finish_task");
    let project = tempfile::tempdir().expect("project tempdir");

    let mock = Arc::new(ScriptedLlm::from_json(&[
        write_file_tool_call_response("call_1", "hello.txt", "hello"),
        stop_response("engineer: wrote hello.txt"),
    ]));
    let llm: Arc<dyn InferenceAdapter> = Arc::clone(&mock) as Arc<dyn InferenceAdapter>;
    let p = TaskRunParams {
        no_delegate: true,
        agent_name: "engineer".to_string(),
        task: "create hello.txt containing hello".to_string(),
        ..params(&agents, &project, &session.id)
    };

    spawn_task_run(Arc::clone(&registry), llm, p).expect("run must start");
    wait_for_terminal(&registry, &session.id).await;

    // (a) the deliverable actually landed on disk.
    let written = std::fs::read_to_string(project.path().join("hello.txt"))
        .expect("#8031: a --no-delegate run must be able to write the file itself");
    assert_eq!(
        written, "hello",
        "the file must carry the requested content"
    );

    // (b) the delegate tool was neither advertised nor called.
    let advertised = mock.first_tool_names();
    assert!(
        advertised.contains(&"write_file".to_string()),
        "the named agent's own write_file must be advertised; got {advertised:?}"
    );
    assert!(
        !advertised.contains(&"delegate_to_agent".to_string()),
        "a --no-delegate run must not advertise delegate_to_agent; got {advertised:?}"
    );

    let calls: Vec<(String, String)> = registry
        .replay(&session.id)
        .expect("session must exist")
        .iter()
        .filter_map(|e| match &e.event {
            crate::events::Event::ToolStarted { agent, tool, .. } => {
                Some((agent.clone(), tool.clone()))
            }
            _ => None,
        })
        .collect();
    assert!(
        calls.iter().any(|(_, tool)| tool == "write_file"),
        "the run must have emitted a write_file tool event; got {calls:?}"
    );
    assert!(
        !calls.iter().any(|(_, tool)| tool == "delegate_to_agent"),
        "a --no-delegate run must emit ZERO delegate_to_agent calls; got {calls:?}"
    );
    // (c) attribution names the top-level agent, not `pm` or the engineer.
    assert!(
        calls.iter().all(|(agent, _)| agent == "engineer"),
        "every tool event must be attributed to the named agent; got {calls:?}"
    );
}

/// #8031 PERMISSION GATE: a `--no-delegate` run honours the named agent's own
/// `tcode_tools` allowlist, exactly as a delegated run of that agent does.
///
/// Why: the single-agent tool set must come through the SAME gate the
/// delegated runner uses (`runner::agent_registry` -> `gate_registry`). An
/// implementation that merged the factory's full output straight onto the
/// top-level registry would advertise `bash`/`edit` to an agent whose author
/// denied them — a silent privilege widening this test is the differential
/// for.
/// What: an `engineer` whose allowlist is `read_file, write_file, finish_task`
/// only. Asserts `write_file` is advertised (an allowlist entry, so the merge
/// really happened) while `bash`, `edit`, `glob` and `grep` — all built by
/// `ProjectToolFactory` — are absent.
/// Test: this test.
#[tokio::test]
async fn no_delegate_run_respects_the_agents_tools_allowlist() {
    let registry = Arc::new(SessionRegistry::new());
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
    let agents = engineer_agents_dir("read_file, write_file, finish_task");
    let project = tempfile::tempdir().expect("project tempdir");

    let mock = Arc::new(ScriptedLlm::from_json(&[stop_response(
        "engineer: nothing to do",
    )]));
    let llm: Arc<dyn InferenceAdapter> = Arc::clone(&mock) as Arc<dyn InferenceAdapter>;
    let p = TaskRunParams {
        no_delegate: true,
        agent_name: "engineer".to_string(),
        ..params(&agents, &project, &session.id)
    };

    spawn_task_run(Arc::clone(&registry), llm, p).expect("run must start");
    wait_for_terminal(&registry, &session.id).await;

    let advertised = mock.first_tool_names();
    assert!(
        advertised.contains(&"write_file".to_string()),
        "an allowlisted tool must reach the single-agent registry; got {advertised:?}"
    );
    for denied in ["bash", "edit", "glob", "grep"] {
        assert!(
            !advertised.contains(&denied.to_string()),
            "{denied} is outside the agent's tcode_tools allowlist and must not be \
             advertised on a --no-delegate run; got {advertised:?}"
        );
    }
}

// ── #8184: the interactive solo session's registry and permission gate ──────

/// #8184: the SOLO run of the stock `pm` agent advertises the file tools.
///
/// Why: `tcode tui`'s default session runs `task.run`'s default agent (`pm`)
/// with `no_delegate`, so "the agent reads and edits the file itself" is true
/// only if the SHIPPED `pm` agent card carries those tools. A fixture agent
/// with a hand-written allowlist would prove nothing about what a user gets.
/// What: an EMPTY agents dir, so `crate::agents::resolve_agent` falls back to
/// the embedded roster's real `pm.md`, and asserts the advertised schema set.
/// Test: this test.
#[tokio::test]
async fn solo_run_of_the_stock_pm_advertises_the_file_tools() {
    let registry = Arc::new(SessionRegistry::new());
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
    let agents = tempfile::tempdir().expect("empty agents tempdir");
    let project = tempfile::tempdir().expect("project tempdir");

    let mock = Arc::new(ScriptedLlm::from_json(&[stop_response(
        "pm: nothing to do",
    )]));
    let llm: Arc<dyn InferenceAdapter> = Arc::clone(&mock) as Arc<dyn InferenceAdapter>;
    let p = TaskRunParams {
        no_delegate: true,
        ..params(&agents, &project, &session.id)
    };

    spawn_task_run(Arc::clone(&registry), llm, p).expect("run must start");
    wait_for_terminal(&registry, &session.id).await;

    let advertised = mock.first_tool_names();
    for tool in ["read_file", "write_file", "edit", "bash"] {
        assert!(
            advertised.contains(&tool.to_string()),
            "the solo session must carry {tool}; got {advertised:?}"
        );
    }
    assert!(
        !advertised.contains(&"delegate_to_agent".to_string()),
        "a solo session must not advertise delegate_to_agent; got {advertised:?}"
    );
}

/// The `request_id` of the first `permission_requested` this session recorded.
///
/// Why (#8184): the ask suspends the run inside the gate, so the answer has to
/// come from a concurrent task reading the session's own event ring — the same
/// place a real client reads it from.
/// Test: `solo_run_of_the_stock_pm_asks_before_write_file`.
async fn await_permission_request(registry: &SessionRegistry, id: &str) -> (String, String) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        for e in registry.replay(id).expect("session must exist") {
            if let crate::events::Event::PermissionRequested {
                request_id, tool, ..
            } = &e.event
            {
                return (request_id.clone(), tool.clone());
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no permission request arrived within 10s"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

/// #8184 / #3422 SAFETY GATE: the STOCK `pm` agent — the one a default
/// interactive session runs — asks before writing a file, and a deny keeps it
/// off disk.
///
/// Why: #8184 gives the top-level agent `write_file`/`edit`/`bash` in the
/// user's real project root. That is only safe if the SHIPPED agent card asks
/// first; a bespoke fixture card with a hand-written rule would prove nothing
/// about what a user gets. This drives the daemon path the TUI drives
/// (`spawn_task_run` with a broker), so the ask, the published event, and the
/// answer are the real ones.
/// What: an EMPTY agents dir, so `resolve_agent` falls back to the embedded
/// `pm.md`; a broker plus a real `session.events` stream — the ONLY thing the
/// TUI holds, and with no `session.attach` anywhere, so this is the client
/// shape (#8184, `tui_client::prompter_claim`) rather than a synthetic
/// `claim_prompter`; a concurrent task answers `deny`. Asserts the request
/// named `write_file` and that nothing was written. FAILS without `pm.md`'s
/// `permissions:` block — no request is ever published and the file lands.
/// Deterministic: the answering task waits for the published event, so there
/// is no window to lose.
/// Test: this test.
#[tokio::test]
async fn solo_run_of_the_stock_pm_asks_before_write_file() {
    let registry = Arc::new(SessionRegistry::new());
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
    let agents = tempfile::tempdir().expect("empty agents tempdir");
    let project = tempfile::tempdir().expect("project tempdir");
    let broker = Arc::new(crate::permissions::PermissionBroker::new());
    // #8100/#8184: an ask only suspends while someone is watching THIS
    // session, and what the TUI holds is an events stream — held open for the
    // whole run, exactly as `prompter_claim` holds it.
    let mut _watching = crate::session::events_stream::open(
        Arc::clone(&registry),
        crate::session::events_stream::SessionEventsParams {
            session_id: session.id.clone(),
            after_seq: None,
        },
    )
    .await
    .expect("the session must be streamable");
    // One frame proves the daemon-side handler ran, so the claim exists before
    // anything can be gated — the client's own handshake.
    let _confirmed = _watching.recv().await;

    let mock = Arc::new(ScriptedLlm::from_json(&[
        write_file_tool_call_response("call_1", "asked.txt", "nope"),
        stop_response("pm: the write was refused"),
    ]));
    let llm: Arc<dyn InferenceAdapter> = Arc::clone(&mock) as Arc<dyn InferenceAdapter>;
    let p = TaskRunParams {
        no_delegate: true,
        task: "create asked.txt".to_string(),
        permission_broker: Some(Arc::clone(&broker)),
        ..params(&agents, &project, &session.id)
    };

    let answering = tokio::spawn({
        let registry = Arc::clone(&registry);
        let broker = Arc::clone(&broker);
        let session_id = session.id.clone();
        async move {
            let (request_id, tool) = await_permission_request(&registry, &session_id).await;
            broker
                .session(&session_id)
                .respond(&request_id, crate::permissions::PermissionDecision::Deny)
                .expect("the request must still be waiting");
            tool
        }
    });

    spawn_task_run(Arc::clone(&registry), llm, p).expect("run must start");
    let tool = answering.await.expect("the answering task must not panic");
    wait_for_terminal(&registry, &session.id).await;

    assert_eq!(
        tool, "write_file",
        "the stock pm must ask before writing a file"
    );
    assert!(
        !project.path().join("asked.txt").exists(),
        "a denied write_file must not reach the disk"
    );
}

// ── #8030 / #8128: top-level model + turn-cap overrides, daemon path ────────

/// A response calling a tool this run's registry does not carry, so the loop
/// answers it with a recoverable error and takes another turn.
///
/// Why: the turn-cap test below needs a PM that never terminates and never
/// spends a script entry on a delegated engineer turn.
/// What: one `tool_calls` response naming a tool registered nowhere.
/// Test: `max_turns_override_reaches_the_pm_loop`.
fn unregistered_tool_response() -> Value {
    json!({
        "id": "gen-unknown",
        "choices": [{
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call-unknown",
                    "type": "function",
                    "function": {
                        "name": "definitely_not_a_registered_tool",
                        "arguments": "{}"
                    }
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": {"prompt_tokens": 5, "completion_tokens": 5, "total_tokens": 10}
    })
}

/// `task.run`'s `pm_model` pins the TOP-LEVEL loop's model on the DAEMON
/// path, ahead of the agent's front-matter `model:` (#8030).
///
/// Why: #8030's evidence is a daemon-path run where `--engineer-model`
/// repointed the engineer and the top-level agent's own calls still hit the
/// old slug. The daemon path is the default `tcode run-task` path, so a fix
/// that only reached `--legacy-in-process` would not close the issue.
/// What: `pm.md` pins `openai/gpt-4o-mini`; the run sets
/// `pm_model: Some("opus")`; assert the first wire request's model is the
/// concrete Claude 5 Opus slug.
/// Test: this test.
#[tokio::test]
async fn pm_model_override_pins_the_top_level_model() {
    let registry = Arc::new(SessionRegistry::new());
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
    let agents = agents_dir();
    let project = tempfile::tempdir().expect("project tempdir");

    let mock = Arc::new(ScriptedLlm::from_json(&[stop_response("pm: done")]));
    let llm: Arc<dyn InferenceAdapter> = Arc::clone(&mock) as Arc<dyn InferenceAdapter>;
    let p = TaskRunParams {
        pm_model: Some("opus".to_string()),
        ..params(&agents, &project, &session.id)
    };

    spawn_task_run(Arc::clone(&registry), llm, p).expect("run must start");
    wait_for_terminal(&registry, &session.id).await;

    let models = mock.models_seen();
    assert_eq!(
        models.first().map(String::as_str),
        Some("anthropic/claude-opus-5"),
        "the top-level loop must use the pinned override, normalised; got {models:?}"
    );
}

/// With no `pm_model`, the daemon path's top-level loop still uses the agent
/// config's own model (#8030 — the override is additive).
///
/// Why: the control that makes the test above meaningful, and the guard that
/// every pre-#8030 `task.run` keeps its behaviour byte-for-byte.
/// What: same fixture, `pm_model: None`; assert `pm.md`'s declared slug.
/// Test: this test.
#[tokio::test]
async fn absent_pm_model_override_uses_the_agent_config_model_on_the_daemon_path() {
    let registry = Arc::new(SessionRegistry::new());
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
    let agents = agents_dir();
    let project = tempfile::tempdir().expect("project tempdir");

    let mock = Arc::new(ScriptedLlm::from_json(&[stop_response("pm: done")]));
    let llm: Arc<dyn InferenceAdapter> = Arc::clone(&mock) as Arc<dyn InferenceAdapter>;

    spawn_task_run(
        Arc::clone(&registry),
        llm,
        params(&agents, &project, &session.id),
    )
    .expect("run must start");
    wait_for_terminal(&registry, &session.id).await;

    let models = mock.models_seen();
    assert_eq!(
        models.first().map(String::as_str),
        Some("openai/gpt-4o-mini"),
        "an absent override must leave the agent config's model in place; got {models:?}"
    );
}

/// `task.run`'s `max_turns` replaces the daemon path's PM turn cap, and its
/// absence leaves that cap at 8 (#8128).
///
/// Why: `run_and_record` never overrode `AgentLoopConfig`'s `max_turns`, so a
/// daemon-driven multi-step delivery task could not be given more turns.
/// Counting the requests the loop issued is the observation that proves the
/// value reached the config literal.
/// What: scripts more never-terminating turns than either cap consumes, runs
/// once with `max_turns: Some(3)` and once with `None`, asserting exactly 3
/// and exactly 8 requests.
/// Test: this test.
#[tokio::test]
async fn max_turns_override_reaches_the_pm_loop() {
    let script: Vec<Value> = (0..12).map(|_| unregistered_tool_response()).collect();

    let registry = Arc::new(SessionRegistry::new());
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
    let agents = agents_dir();
    let project = tempfile::tempdir().expect("project tempdir");
    let capped = Arc::new(ScriptedLlm::from_json(&script));
    let p = TaskRunParams {
        max_turns: Some(3),
        ..params(&agents, &project, &session.id)
    };
    spawn_task_run(
        Arc::clone(&registry),
        Arc::clone(&capped) as Arc<dyn InferenceAdapter>,
        p,
    )
    .expect("run must start");
    wait_for_terminal(&registry, &session.id).await;
    assert_eq!(
        capped.models_seen().len(),
        3,
        "the top-level loop must stop after the overridden cap"
    );

    let registry = Arc::new(SessionRegistry::new());
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
    let defaulted = Arc::new(ScriptedLlm::from_json(&script));
    spawn_task_run(
        Arc::clone(&registry),
        Arc::clone(&defaulted) as Arc<dyn InferenceAdapter>,
        params(&agents, &project, &session.id),
    )
    .expect("run must start");
    wait_for_terminal(&registry, &session.id).await;
    assert_eq!(
        defaulted.models_seen().len(),
        8,
        "an absent override must leave AgentLoopConfig::default()'s cap of 8"
    );
}

/// Every tool name a registry-gated prompt section instructs a call to.
///
/// Why: the two #4602 tests below assert opposite sides of one contract and
/// must range over the same name set the assembler gates on — reading it from
/// `GATED_SECTIONS` means a section added there cannot escape these tests.
/// What: flattens every declared name list in `prompt::GATED_SECTIONS`.
/// Test: used by the two tests below.
fn gated_tool_names() -> Vec<&'static str> {
    crate::prompt::GATED_SECTIONS
        .iter()
        .flat_map(|(_, names, _)| names.iter().copied())
        .collect()
}

/// The delegating PM — the agent `tcode tui` drives — is told to call no tool
/// its registry lacks (#4602).
///
/// Why: this path registers `delegate_to_agent`/`finish_task`/the goal tools
/// and nothing that touches the filesystem, yet its prompt used to carry BASE's
/// `## File discovery` block AND its batch-write instructions; the PM obeyed
/// them and every `list_dir`/`glob`/`write_files` call came back as
/// `ToolCallExtractError::UnknownTool`, rendered in the TUI as
/// `<name>(<invalid-arguments>)`.
/// What: resolves the PM config this path loads, asserts `pm_prompt_tools`
/// yields no registry for a delegating run, and asserts the prompt assembled
/// from it names none of the gated tools. `BASE_PREAMBLE` is excised first —
/// it still names `write_file` as an `e.g.` illustration of batching, which
/// `prompt::tests::base_preamble_instructs_no_registry_specific_tool` covers.
/// Test: this test.
#[tokio::test]
async fn delegating_pm_prompt_names_no_gated_tool() {
    let agents = agents_dir();
    let project = tempfile::tempdir().expect("project tempdir");
    let p = params(&agents, &project, "s-4602-delegating");
    let pm = crate::agents::resolve_agent(&p.agents_dir, "pm").expect("pm config resolves");

    let tools = pm_prompt_tools(&p, project.path(), &pm, None).await;
    assert!(
        tools.is_none(),
        "a delegating run gives the top-level agent no project tools"
    );

    let prompt = assemble_system_prompt_for_mode(p.mode, &pm, None, None, None, tools.as_deref());
    let appended = prompt.replace(crate::prompt::BASE_PREAMBLE, "");
    for name in gated_tool_names() {
        assert!(
            !appended.contains(&format!("`{name}`")),
            "the delegating PM's prompt must not name `{name}`:\n{prompt}"
        );
    }
    assert!(
        !prompt.contains("`write_files`"),
        "the delegating PM holds no write tool and must never be told to batch \
         into `write_files`:\n{prompt}"
    );
}

/// A `--no-delegate` run's top-level agent DOES carry the gated tools, and its
/// prompt still says so (#4602).
///
/// Why: scoping the guidance must not silence it for the agent that can act on
/// it — this is the other half of the contract, and it runs through the same
/// helper, so prompt and registry cannot drift apart.
/// What: resolves an agent with no `tcode_tools` allowlist, asserts
/// `pm_prompt_tools` yields a registry carrying every gated tool, and asserts
/// the assembled prompt carries all three gated sections.
/// Test: this test.
#[tokio::test]
async fn no_delegate_pm_prompt_names_its_gated_tools() {
    let agents = agents_dir();
    let project = tempfile::tempdir().expect("project tempdir");
    let p = TaskRunParams {
        no_delegate: true,
        agent_name: "python-engineer".to_string(),
        ..params(&agents, &project, "s-4602-no-delegate")
    };
    let agent =
        crate::agents::resolve_agent(&p.agents_dir, &p.agent_name).expect("agent config resolves");

    let tools = pm_prompt_tools(&p, project.path(), &agent, None)
        .await
        .expect("a --no-delegate run builds the agent's own registry");
    for name in gated_tool_names() {
        assert!(tools.contains(name), "registry must carry `{name}`");
    }

    let prompt =
        assemble_system_prompt_for_mode(p.mode, &agent, None, None, None, Some(tools.as_ref()));
    for (section, _, _) in crate::prompt::GATED_SECTIONS {
        assert!(
            prompt.contains(section),
            "an agent holding every gated tool must still get every gated section"
        );
    }
}

/// A single `todo_write` tool call (#8235).
fn todo_write_tool_call_response(call_id: &str, todos: Value) -> Value {
    json!({
        "id": "mock-todo-write",
        "choices": [{
            "message": {
                "role": "assistant",
                "content": "",
                "tool_calls": [{
                    "id": call_id,
                    "type": "function",
                    "function": {
                        "name": "todo_write",
                        "arguments": json!({"todos": todos}).to_string()
                    }
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}
    })
}

/// #8235 CLOSURE TEST: the daemon-session agent holds `todo_write` with a real
/// advertised schema, and its call lands in `session.get_agents`'s `todos`.
///
/// Why: #4602's failure was a tool named in a prompt with no registered
/// schema. This drives the OTHER direction through the real entry point
/// (`spawn_task_run`, what `task.run` calls): the schema the model receives
/// must carry `todo_write`, and calling it must change the roster the TUI
/// reads. A registration that existed only in a unit test — or a schema that
/// was never advertised — passes neither assertion.
/// What: scripts one `todo_write` call then a natural stop; asserts (a)
/// `todo_write` is among the advertised tool names, (b) the call emitted a
/// `ToolStarted`, and (c) the roster row for this session's PM carries the
/// three steps with their statuses. FAILS before #8235: the tool is not
/// registered, so the call comes back `UnknownTool` and `todos` stays `[]`.
/// Test: this test.
#[tokio::test]
async fn todo_write_is_registered_and_reaches_the_roster() {
    let registry = Arc::new(SessionRegistry::new());
    let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
    let agents = agents_dir();
    let project = tempfile::tempdir().expect("project tempdir");

    let mock = Arc::new(ScriptedLlm::from_json(&[
        todo_write_tool_call_response(
            "call_1",
            json!([
                {"content": "add the --json flag", "status": "completed"},
                {"content": "add a test", "status": "in_progress"},
                {"content": "add a changelog line", "status": "pending"}
            ]),
        ),
        stop_response("pm: planned the work"),
    ]));
    let llm: Arc<dyn InferenceAdapter> = Arc::clone(&mock) as Arc<dyn InferenceAdapter>;
    let p = params(&agents, &project, &session.id);

    spawn_task_run(Arc::clone(&registry), llm, p).expect("run must start");
    wait_for_terminal(&registry, &session.id).await;

    // (a) the model was actually offered the tool.
    let advertised = mock.first_tool_names();
    assert!(
        advertised.contains(&"todo_write".to_string()),
        "#8235: todo_write must be advertised to the session agent; got {advertised:?}"
    );

    // (b) the call ran rather than failing as an unknown tool.
    let errors: Vec<String> = registry
        .replay(&session.id)
        .expect("session must exist")
        .iter()
        .filter_map(|e| match &e.event {
            crate::events::Event::ToolFinished {
                tool,
                success,
                result_preview,
                ..
            } if tool == "todo_write" && !success => Some(result_preview.clone()),
            _ => None,
        })
        .collect();
    assert!(errors.is_empty(), "todo_write call failed: {errors:?}");

    // (c) the checklist reached the read path the TUI polls.
    let roster = registry.get_agents(&session.id).expect("roster");
    let row = roster
        .iter()
        .find(|a| a.agent_id == format!("pm-{}", session.id))
        .unwrap_or_else(|| panic!("the PM's roster row must exist: {roster:?}"));
    assert_eq!(row.todos.len(), 3, "got {:?}", row.todos);
    assert_eq!(row.todos[1].content, "add a test");
    assert_eq!(row.todos[1].status, crate::events::TodoStatus::InProgress);
}
