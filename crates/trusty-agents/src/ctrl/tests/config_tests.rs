//! Tests for ctrl `config` + `claude_cli` prompt/credential helpers.

use crate::agents::AgentConfig;
use crate::llm;

use super::super::claude_cli::{filter_project_index_in_prompt, strip_cli_artifacts};
use super::super::config::{
    apply_credential_routing, build_deployment_footer, build_user_context_prefix,
    render_user_context_block, render_user_datetime, resolve_agent_config,
};

/// Sandbox `$HOME` to a fresh tempdir for the duration of `f`, holding
/// `crate::test_env::HOME_LOCK` so parallel tests never observe each other's
/// redirected `$HOME` (`UserProfile::profile_path` resolves under `$HOME`).
/// Restores the previous `$HOME` (or removes it) before returning.
fn with_sandboxed_home<R>(f: impl FnOnce(&std::path::Path) -> R) -> R {
    let _guard = crate::test_env::HOME_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().expect("tempdir");
    let prev_home = std::env::var_os("HOME");
    // SAFETY: test-only env mutation, serialized by HOME_LOCK.
    unsafe {
        std::env::set_var("HOME", dir.path());
    }
    let result = f(dir.path());
    // SAFETY: restoring the pre-test HOME, still under HOME_LOCK.
    unsafe {
        match prev_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
    }
    result
}

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
        Some("/proj/.trusty-agents/agents/ctrl.toml"),
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
    assert!(s.contains("- Config: /proj/.trusty-agents/agents/ctrl.toml"));
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
    let agents = tmp.path().join(".trusty-agents/agents");
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
    let agents = tmp.path().join(".trusty-agents/agents");
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

// -- render_user_datetime / render_user_context_block (#3052 follow-up) --

/// Why: the motivating defect was a timestamp frozen once at process/REPL
/// startup, causing a stale weekday to leak into later turns ("hope your
/// Sunday's going well" ... "happy Monday!" minutes later). A test that only
/// checks the OUTPUT FORMAT would pass even on that broken, frozen
/// implementation. This test instead proves `render_user_datetime` is a pure
/// function of its `at` parameter — feeding it two different instants (a
/// Sunday and the following Monday, both UTC) must produce two different
/// renderings with the correct weekday in each. Every real call site
/// (`render_user_context_block`) samples `chrono::Utc::now()` fresh on every
/// invocation rather than caching it, so this is the property that actually
/// guarantees "refreshed per turn, not frozen at startup".
/// Test: itself.
#[test]
fn render_user_datetime_reflects_new_instant_on_each_call() {
    use chrono::TimeZone;
    let sunday = chrono::Utc.with_ymd_and_hms(2026, 7, 19, 12, 0, 0).unwrap();
    let monday = chrono::Utc.with_ymd_and_hms(2026, 7, 20, 12, 0, 0).unwrap();
    let s1 = render_user_datetime(sunday, Some("UTC"));
    let s2 = render_user_datetime(monday, Some("UTC"));
    assert_ne!(
        s1, s2,
        "rendering must reflect the instant passed in, not a cached/frozen value"
    );
    assert!(s1.contains("Sunday"), "got: {s1}");
    assert!(s2.contains("Monday"), "got: {s2}");
}

#[test]
fn render_user_datetime_formats_in_named_timezone_with_weekday() {
    use chrono::TimeZone;
    // 2026-07-19T15:30:00Z is a Sunday; America/New_York is UTC-4 in July
    // (EDT), so it's still Sunday there too.
    let at = chrono::Utc
        .with_ymd_and_hms(2026, 7, 19, 15, 30, 0)
        .unwrap();
    let s = render_user_datetime(at, Some("America/New_York"));
    assert!(
        s.starts_with("Current date/time: Sunday, 19 July 2026"),
        "got: {s}"
    );
    assert!(s.contains("America/New_York"), "got: {s}");
}

#[test]
fn render_user_datetime_falls_back_to_utc_when_timezone_missing() {
    let at = chrono::Utc::now();
    let s = render_user_datetime(at, None);
    assert!(s.contains("UTC — no timezone configured"), "got: {s}");
}

#[test]
fn render_user_datetime_falls_back_to_utc_when_timezone_invalid() {
    let at = chrono::Utc::now();
    let s = render_user_datetime(at, Some("Not/AZone"));
    assert!(
        s.contains("UTC — timezone \"Not/AZone\" not recognized"),
        "got: {s}"
    );
}

/// Why: the absolute constraint on `location` is "user-supplied only, never
/// guessed" — a profile that never set it must render a block with NO
/// `location` line at all (not an empty placeholder).
/// Test: itself.
#[test]
fn render_user_context_block_omits_location_when_unset() {
    with_sandboxed_home(|_home| {
        let profile = crate::identity::user_profile::UserProfile {
            name: "Ada".to_string(),
            email: None,
            preferred_model: None,
            timezone: None,
            location: None,
            created_at: "2026-01-01T00:00:00Z".to_string(),
        };
        profile.save().expect("save profile");

        let block = render_user_context_block();
        assert!(block.contains("user_name = \"Ada\""), "got: {block}");
        assert!(!block.contains("location"), "got: {block}");
    });
}

#[test]
fn render_user_context_block_includes_location_when_set() {
    with_sandboxed_home(|_home| {
        let profile = crate::identity::user_profile::UserProfile {
            name: "Ada".to_string(),
            email: None,
            preferred_model: None,
            timezone: None,
            location: Some("New York, NY, USA".to_string()),
            created_at: "2026-01-01T00:00:00Z".to_string(),
        };
        profile.save().expect("save profile");

        let block = render_user_context_block();
        assert!(
            block.contains("location = \"New York, NY, USA\""),
            "got: {block}"
        );
    });
}

#[test]
fn render_user_context_block_unknown_user_still_gets_datetime() {
    with_sandboxed_home(|_home| {
        // No user.toml written — profile is absent.
        let block = render_user_context_block();
        assert!(block.contains("user_name = \"(unknown)\""), "got: {block}");
        assert!(block.contains("Current date/time:"), "got: {block}");
    });
}

#[test]
fn build_user_context_prefix_appends_base_content_after_block() {
    with_sandboxed_home(|_home| {
        let out = build_user_context_prefix("BASE PROMPT CONTENT");
        assert!(out.contains("## User Context"));
        assert!(out.contains("BASE PROMPT CONTENT"));
        assert!(
            out.find("## User Context").unwrap() < out.find("BASE PROMPT CONTENT").unwrap(),
            "user context block must precede the base content: {out}"
        );
    });
}
