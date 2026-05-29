//! Tests for ctrl `config` + `claude_cli` prompt/credential helpers.

use crate::agents::AgentConfig;
use crate::llm;

use super::super::claude_cli::{filter_project_index_in_prompt, strip_cli_artifacts};
use super::super::config::{
    apply_credential_routing, build_deployment_footer, resolve_agent_config,
};

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
