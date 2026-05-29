//! Unit tests for the claude-code runner and the per-agent dispatcher.
//!
//! Why: Pins the prompt-sanitization, model-normalization, stream-json
//! parsing, and dispatch-routing behavior so refactors can't silently change
//! how claude-code agents are spawned and parsed.
//! What: Exercises helpers directly plus `run_with_config` against a mock
//! `claude` shell script, and the dispatcher's routing fallbacks.
//! Test: This module IS the test surface.

use super::*;
use crate::agents::{
    AgentConfig, AgentInfo, LlmParams, RunnerKind, SystemPrompt, ToolChoice, ToolsConfig,
};
use crate::perf::TokenUsage;
use crate::tools::traits::{AgentOutput, AgentRunner, RunContext};
use anyhow::Result;
use async_trait::async_trait;
use std::sync::Arc;

#[test]
fn prepend_harness_layers_adds_base_and_claude_code() {
    // With use_finish_task=false, expect BASE_PROTOCOL + CLAUDE_CODE_PROTOCOL
    // prepended above the supplied TOML base in that order.
    let out = ClaudeCodeAgentRunner::prepend_harness_layers("TOML BASE", false);
    let base_pos = out
        .find("Output Directory")
        .expect("BASE_PROTOCOL (Output Directory) present");
    let cc_pos = out
        .find("write_file Protocol")
        .expect("CLAUDE_CODE_PROTOCOL (write_file Protocol) present");
    let toml_pos = out.find("TOML BASE").expect("TOML base present");
    assert!(
        base_pos < cc_pos,
        "base protocol precedes claude-code layer"
    );
    assert!(cc_pos < toml_pos, "claude-code layer precedes TOML base");
}

#[test]
fn prepend_harness_layers_finish_task_branch() {
    // With use_finish_task=true, expect BASE_PROTOCOL + FINISH_TASK_PROTOCOL,
    // and the CLAUDE_CODE_PROTOCOL write_file block must be absent.
    let out = ClaudeCodeAgentRunner::prepend_harness_layers("TOML BASE", true);
    assert!(out.contains("Output Directory"), "BASE_PROTOCOL injected");
    assert!(out.contains("finish_task"), "FINISH_TASK_PROTOCOL injected");
    assert!(
        !out.contains("write_file Protocol"),
        "CLAUDE_CODE_PROTOCOL skipped when use_finish_task=true"
    );
}

/// Build a minimal `AgentConfig` for unit tests. Kept here (not in the
/// main module) because nothing else in the codebase needs to fabricate
/// one from scratch — production code always goes through
/// `AgentConfig::load`.
fn test_agent_config(name: &str, model: &str, system_prompt: &str) -> AgentConfig {
    AgentConfig {
        agent: AgentInfo {
            name: name.to_string(),
            role: "engineer".to_string(),
            model: model.to_string(),
            description: "test".to_string(),
            persistent_session: false,
            runner: RunnerKind::ClaudeCode,
            capabilities: None,
            display_name: None,
            prompt_label: None,
        },
        llm: LlmParams {
            temperature: 0.0,
            max_tokens: 1024,
            model_override: None,
            enable_prompt_caching: true,
            max_turns: 5,
            tool_choice: ToolChoice::Auto,
            use_finish_task: false,
            use_anthropic_direct: false,
            claude_allowed_tools: Vec::new(),
            aws_profile: None,
            aws_region: None,
            elevation_threshold: None,
            elevation_model: None,
            stop_sequences: Vec::new(),
            routing_model: None,
            thinking_enabled: None,
        },
        system_prompt: SystemPrompt {
            content: system_prompt.to_string(),
            skills: None,
        },
        tools: ToolsConfig::default(),
        compress: crate::agents::AgentCompressConfig::default(),
        runner_config: crate::agents::RunnerConfig::default(),
        session: crate::agents::SessionCompressionConfig::default(),
        plugins: crate::agents::AgentPluginsConfig::default(),
        rbac: crate::agents::RbacConfig::default(),
        adapter: Arc::new(crate::llm::adapter::GenericAdapter),
    }
}

#[test]
fn normalize_model_strips_anthropic_prefix() {
    assert_eq!(
        ClaudeCodeAgentRunner::normalize_model("anthropic/claude-sonnet-4-6"),
        "claude-sonnet-4-6"
    );
    assert_eq!(ClaudeCodeAgentRunner::normalize_model("sonnet"), "sonnet");
    assert_eq!(
        ClaudeCodeAgentRunner::normalize_model("claude-haiku-4"),
        "claude-haiku-4"
    );
}

#[test]
fn normalize_model_openai_untouched() {
    assert_eq!(
        ClaudeCodeAgentRunner::normalize_model("openai/gpt-4.1"),
        "openai/gpt-4.1"
    );
}

#[test]
fn strip_finish_task_instructions_removes_callouts() {
    // #113: Lines that mention `finish_task` must vanish so the claude
    // CLI doesn't spin looking for a tool it doesn't have.
    let input = "\
Steps:
1. Do the thing
2. Write the file
3. Call finish_task when done
4. Exit
";
    let out = ClaudeCodeAgentRunner::strip_finish_task_instructions(input);
    assert!(!out.contains("finish_task"), "should strip: {out}");
    assert!(out.contains("Do the thing"));
    assert!(out.contains("Write the file"));
    assert!(out.contains("Exit"));
}

#[test]
fn strip_finish_task_instructions_is_idempotent() {
    let input = "plain text with no tool reference";
    let once = ClaudeCodeAgentRunner::strip_finish_task_instructions(input);
    let twice = ClaudeCodeAgentRunner::strip_finish_task_instructions(&once);
    assert_eq!(once, twice);
    assert_eq!(once, input);
}

#[test]
fn strip_finish_task_instructions_preserves_unrelated_text() {
    // Multi-line prose that does NOT mention finish_task should pass
    // through unchanged, preserving newlines.
    let input = "line one\nline two\nline three\n";
    let out = ClaudeCodeAgentRunner::strip_finish_task_instructions(input);
    assert_eq!(out, input);
}

#[test]
fn strip_finish_task_instructions_case_insensitive() {
    let input = "Call Finish_Task now\nor FINISH_TASK\nkeep this";
    let out = ClaudeCodeAgentRunner::strip_finish_task_instructions(input);
    assert!(
        !out.to_ascii_lowercase().contains("finish_task"),
        "got: {out}"
    );
    assert!(out.contains("keep this"));
}

#[test]
fn find_claude_errors_when_not_in_path() {
    // Point PATH at an empty temp dir and confirm we get a useful error.
    // SAFETY: this mutates a process-global env var. The test is marked
    // serial-ish by running it as a separate #[test] with no concurrent
    // env writes elsewhere in this module.
    let tmp = tempfile::tempdir().unwrap();
    let old = std::env::var_os("PATH");
    unsafe {
        std::env::set_var("PATH", tmp.path());
    }
    let err = find_claude().unwrap_err();
    assert!(
        err.to_string().contains("not found in PATH"),
        "unexpected error: {err}"
    );
    unsafe {
        match old {
            Some(v) => std::env::set_var("PATH", v),
            None => std::env::remove_var("PATH"),
        }
    }
}

/// Mock-`claude` integration test: write a tiny shell script that emits
/// a stream-json transcript and confirm we parse it into `AgentOutput`.
#[cfg(unix)]
#[tokio::test]
async fn run_parses_stream_json_result() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempfile::tempdir().unwrap();
    let script = tmp.path().join("claude");
    // Script emits two non-result events (system init, assistant) then a
    // final result. Stderr swallows argv so we don't pollute test output.
    // Use `printf %s\\n` to emit each JSON object as one line without
    // shell-specific `echo` differences expanding `\n` escapes (bash vs
    // dash behave differently on macOS/Linux; printf is portable).
    // The result's `result` field embeds literal \n escapes so the
    // JSON parser sees them as newlines inside the string.
    std::fs::write(
        &script,
        "#!/bin/sh\n\
         printf '%s\\n' '{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"test\"}'\n\
         printf '%s\\n' '{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"thinking\"}]}}'\n\
         printf '%s\\n' '{\"type\":\"result\",\"result\":\"Hello from mock claude\\n\\n## Summary\\nMock OK\",\"is_error\":false}'\n",
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

    let runner = ClaudeCodeAgentRunner { claude_bin: script };
    let cfg = test_agent_config("mock-agent", "anthropic/claude-sonnet-4-6", "test prompt");

    let out = runner.run_with_config(&cfg, "say hi").await.expect("runs");
    assert!(
        out.content.starts_with("Hello from mock claude"),
        "unexpected content: {:?}",
        out.content
    );
    assert_eq!(out.summary.as_deref(), Some("Mock OK"));
    assert_eq!(out.usage, TokenUsage::default());
}

/// #107: `RunContext.model` must take precedence over
/// `cfg.agent.model` when passing `--model` to the `claude` CLI so a
/// workflow phase's `model` actually reaches the CLI.
#[cfg(unix)]
#[tokio::test]
async fn run_honors_ctx_model_override() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempfile::tempdir().unwrap();
    let argv_log = tmp.path().join("argv.txt");
    let script = tmp.path().join("claude");
    // Record argv then emit a valid result so the runner doesn't error.
    let script_body = format!(
        "#!/bin/sh\n\
         printf '%s\\n' \"$@\" > {log}\n\
         printf '%s\\n' '{{\"type\":\"result\",\"result\":\"ok\",\"is_error\":false}}'\n",
        log = argv_log.display()
    );
    std::fs::write(&script, script_body).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

    let runner = ClaudeCodeAgentRunner { claude_bin: script };
    // TOML says sonnet, ctx overrides to opus.
    let cfg = test_agent_config("mock-agent", "anthropic/claude-sonnet-4-6", "test prompt");
    let ctx = RunContext {
        model: Some("anthropic/claude-opus-4-6".to_string()),
        ..RunContext::default()
    };

    let _out = runner
        .run_with_config_ctx(&cfg, "say hi", &ctx)
        .await
        .expect("runs");

    let argv = std::fs::read_to_string(&argv_log).expect("argv log written");
    // Expect the normalized override (prefix stripped) next to --model.
    assert!(
        argv.contains("--model\nclaude-opus-4-6\n"),
        "expected --model claude-opus-4-6 in argv, got:\n{argv}"
    );
    assert!(
        !argv.contains("claude-sonnet-4-6"),
        "TOML model should have been overridden, got:\n{argv}"
    );
}

/// When `is_error: true`, the runner returns an error whose message
/// surfaces the CLI's complaint.
#[cfg(unix)]
#[tokio::test]
async fn run_propagates_is_error() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempfile::tempdir().unwrap();
    let script = tmp.path().join("claude");
    std::fs::write(
        &script,
        "#!/bin/sh\n\
         printf '%s\\n' '{\"type\":\"result\",\"result\":\"rate limit exceeded\",\"is_error\":true}'\n",
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

    let runner = ClaudeCodeAgentRunner { claude_bin: script };
    let cfg = test_agent_config("mock-agent", "sonnet", "test");
    let err = runner
        .run_with_config(&cfg, "try")
        .await
        .expect_err("should propagate is_error");
    assert!(err.to_string().contains("rate limit"), "unexpected: {err}");
}

/// #113: `error_max_turns` with a non-empty `result` is treated as
/// success — the agent produced work, the CLI just happened to exhaust
/// its turn budget before emitting a clean terminator.
#[cfg(unix)]
#[tokio::test]
async fn run_recovers_from_max_turns_with_content() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempfile::tempdir().unwrap();
    let script = tmp.path().join("claude");
    std::fs::write(
        &script,
        "#!/bin/sh\n\
         printf '%s\\n' '{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"partial work\"}]}}'\n\
         printf '%s\\n' '{\"type\":\"result\",\"subtype\":\"error_max_turns\",\"result\":\"partial work\",\"is_error\":true}'\n",
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

    let runner = ClaudeCodeAgentRunner { claude_bin: script };
    let cfg = test_agent_config("mock-agent", "sonnet", "test");
    let out = runner
        .run_with_config(&cfg, "try")
        .await
        .expect("max_turns with content should be treated as success");
    assert!(out.content.contains("partial work"));
}

/// #113: `error_max_turns` with completely empty content must still
/// propagate as an error — we don't want to hide a truly failed run.
#[cfg(unix)]
#[tokio::test]
async fn run_max_turns_without_content_still_errors() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempfile::tempdir().unwrap();
    let script = tmp.path().join("claude");
    std::fs::write(
        &script,
        "#!/bin/sh\n\
         printf '%s\\n' '{\"type\":\"result\",\"subtype\":\"error_max_turns\",\"result\":\"\",\"is_error\":true}'\n",
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

    let runner = ClaudeCodeAgentRunner { claude_bin: script };
    let cfg = test_agent_config("mock-agent", "sonnet", "test");
    let err = runner
        .run_with_config(&cfg, "try")
        .await
        .expect_err("empty max_turns should still error");
    assert!(
        err.to_string().contains("error_max_turns"),
        "unexpected: {err}"
    );
}

/// #113: Both `task` and `system prompt` are sanitized before being
/// handed to the CLI; argv should not contain `finish_task`.
#[cfg(unix)]
#[tokio::test]
async fn run_sanitizes_finish_task_from_cli_args() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempfile::tempdir().unwrap();
    let argv_log = tmp.path().join("argv.txt");
    let script = tmp.path().join("claude");
    let script_body = format!(
        "#!/bin/sh\n\
         printf '%s\\n' \"$@\" > {log}\n\
         printf '%s\\n' '{{\"type\":\"result\",\"result\":\"ok\",\"is_error\":false}}'\n",
        log = argv_log.display()
    );
    std::fs::write(&script, script_body).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

    let runner = ClaudeCodeAgentRunner { claude_bin: script };
    let cfg = test_agent_config(
        "mock-agent",
        "sonnet",
        "You are a test agent.\nWhen done, call finish_task with a summary.",
    );
    let _ = runner
        .run_with_config(&cfg, "Do the work.\nThen call finish_task.")
        .await
        .expect("runs");

    let argv = std::fs::read_to_string(&argv_log).expect("argv log written");
    assert!(
        !argv.contains("finish_task"),
        "finish_task leaked into CLI args:\n{argv}"
    );
    // Surrounding content should still be present.
    assert!(argv.contains("Do the work."));
    assert!(argv.contains("You are a test agent."));
}

/// Dispatcher: when no claude-code runner is wired and the agent's TOML
/// doesn't request `claude-code`, calls fall through to the fallback.
#[tokio::test]
async fn dispatcher_routes_subprocess_by_default() {
    struct FakeFallback;
    #[async_trait]
    impl AgentRunner for FakeFallback {
        async fn run(&self, _agent: &str, _task: &str) -> Result<AgentOutput> {
            Ok(AgentOutput {
                content: "fallback-ran".into(),
                summary: None,
                usage: TokenUsage::default(),
            })
        }
    }
    // Use an agent name with no TOML on disk. The dispatcher's config
    // lookup fails and `unwrap_or_default()` yields `RunnerKind::Subprocess`,
    // which routes to the fallback. This keeps the test independent of
    // which runner any shipped agent TOML happens to declare today (e.g.
    // python-engineer migrated to runner="claude-code", which used to
    // break this test).
    let dispatcher = DispatchingAgentRunner::new(Arc::new(FakeFallback), None);
    let out = dispatcher
        .run("__nonexistent_test_agent__", "x")
        .await
        .unwrap();
    assert_eq!(out.content, "fallback-ran");
}

/// Dispatcher: when claude-code is requested but not wired, the call
/// errors with a clear message instead of silently falling through.
#[tokio::test]
async fn dispatcher_errors_when_claude_code_requested_but_unwired() {
    struct FakeFallback;
    #[async_trait]
    impl AgentRunner for FakeFallback {
        async fn run(&self, _agent: &str, _task: &str) -> Result<AgentOutput> {
            unreachable!("fallback should not be called for claude-code agents");
        }
    }
    // We need an agent TOML with runner="claude-code". The test for
    // claude-code-engineer.toml (added in this change) serves as that.
    let dispatcher = DispatchingAgentRunner::new(Arc::new(FakeFallback), None);
    let err = dispatcher
        .run("claude-code-engineer", "x")
        .await
        .expect_err("should error");
    assert!(err.to_string().contains("claude-code"), "unexpected: {err}");
}

/// When the CLI exits without ever emitting a `result` event, the runner
/// surfaces a clear error rather than hanging.
#[cfg(unix)]
#[tokio::test]
async fn run_errors_when_no_result_event() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempfile::tempdir().unwrap();
    let script = tmp.path().join("claude");
    std::fs::write(
        &script,
        "#!/bin/sh\n\
         printf '%s\\n' '{\"type\":\"system\",\"subtype\":\"init\"}'\n",
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

    let runner = ClaudeCodeAgentRunner { claude_bin: script };
    let cfg = test_agent_config("mock-agent", "sonnet", "test");
    let err = runner
        .run_with_config(&cfg, "try")
        .await
        .expect_err("should error without result event");
    assert!(
        err.to_string().contains("without producing a `result`"),
        "unexpected: {err}"
    );
}
