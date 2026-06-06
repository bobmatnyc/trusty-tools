//! Model-resolution and on-disk loader tests for `AgentConfig`.
//!
//! Why: Pins the model-resolution priority chain (env > llm_override >
//! agent TOML > default env > fallback), the directory-package loader, the
//! `stop_sequences` validation, and the `[[plugins.python]]` parse path so
//! loader/model refactors can't silently regress these behaviors.
//! What: Tests that mutate process-global env (guarded by `ENV_LOCK`) plus
//! disk-backed `by_name` / package-loading assertions.
//! Test: This module IS the test surface.

use crate::agents::{
    AgentConfig, FALLBACK_MODEL, ModelSource, agent_config_path, agent_env_suffix, resolve_model,
};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

// Serialize model-resolution tests because they mutate process-global env.
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn clear_model_env(agent_name: &str) {
    let suffix = agent_env_suffix(agent_name);
    let new_var = format!("TAGENT_MODEL_{suffix}");
    let old_var = format!("TAGENT_MODEL_{suffix}");
    // SAFETY: test harness, guarded by ENV_LOCK.
    unsafe {
        std::env::remove_var(&new_var);
        std::env::remove_var(&old_var);
        std::env::remove_var("TAGENT_DEFAULT_MODEL");
        std::env::remove_var("TAGENT_DEFAULT_MODEL");
    }
}

#[test]
fn agent_env_suffix_uppercases_and_replaces_hyphens() {
    assert_eq!(agent_env_suffix("python-engineer"), "PYTHON_ENGINEER");
    assert_eq!(agent_env_suffix("pm"), "PM");
    assert_eq!(agent_env_suffix("research-agent"), "RESEARCH_AGENT");
}

#[test]
fn resolve_model_env_var_beats_toml() {
    let _guard = ENV_LOCK.lock().unwrap();
    clear_model_env("python-engineer");
    // SAFETY: guarded by ENV_LOCK
    unsafe {
        std::env::set_var("TAGENT_MODEL_PYTHON_ENGINEER", "env/winner");
    }
    let (m, src) = resolve_model("python-engineer", "toml/model", Some("toml/override"));
    assert_eq!(m, "env/winner");
    assert_eq!(src, ModelSource::AgentEnv);
    clear_model_env("python-engineer");
}

#[test]
fn resolve_model_llm_override_beats_agent_model() {
    let _guard = ENV_LOCK.lock().unwrap();
    clear_model_env("x-agent");
    let (m, src) = resolve_model("x-agent", "toml/agent", Some("toml/override"));
    assert_eq!(m, "toml/override");
    assert_eq!(src, ModelSource::LlmOverride);
}

#[test]
fn resolve_model_uses_agent_model_when_no_override() {
    let _guard = ENV_LOCK.lock().unwrap();
    clear_model_env("y-agent");
    let (m, src) = resolve_model("y-agent", "toml/agent", None);
    assert_eq!(m, "toml/agent");
    assert_eq!(src, ModelSource::AgentToml);
}

#[test]
fn resolve_model_uses_default_env_when_nothing_else() {
    let _guard = ENV_LOCK.lock().unwrap();
    clear_model_env("z-agent");
    // SAFETY: guarded by ENV_LOCK
    unsafe {
        std::env::set_var("TAGENT_DEFAULT_MODEL", "default/model");
    }
    let (m, src) = resolve_model("z-agent", "", None);
    assert_eq!(m, "default/model");
    assert_eq!(src, ModelSource::DefaultEnv);
    // SAFETY: guarded by ENV_LOCK
    unsafe {
        std::env::remove_var("TAGENT_DEFAULT_MODEL");
    }
}

#[test]
fn resolve_model_fallback_when_nothing_set() {
    let _guard = ENV_LOCK.lock().unwrap();
    clear_model_env("q-agent");
    let (m, src) = resolve_model("q-agent", "", None);
    assert_eq!(m, FALLBACK_MODEL);
    assert_eq!(src, ModelSource::Fallback);
}

#[test]
fn resolve_model_empty_llm_override_is_ignored() {
    let _guard = ENV_LOCK.lock().unwrap();
    clear_model_env("r-agent");
    let (m, src) = resolve_model("r-agent", "toml/agent", Some(""));
    assert_eq!(m, "toml/agent");
    assert_eq!(src, ModelSource::AgentToml);
}

#[tokio::test]
async fn by_name_async_loads_plan_agent() {
    // #96: Async loader should produce the same adapter + model as the
    // sync path when TAGENT_CONFIG_DIR is unset (fallback path).
    // Set up env inside a sync scope so the MutexGuard is dropped
    // before we hit any `.await` (avoids await_holding_lock clippy lint).
    {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_model_env("plan-agent");
        // SAFETY: guarded by ENV_LOCK for the duration of this scope.
        unsafe {
            std::env::remove_var("TAGENT_CONFIG_DIR");
        }
    }
    let cfg = AgentConfig::by_name_async("plan-agent")
        .await
        .expect("plan-agent loads async");
    use crate::llm::adapter::Provider;
    assert_eq!(cfg.adapter.provider(), Provider::Anthropic);
}

#[test]
fn agent_directory_package_loads_correctly() {
    // #482: The directory-package format (`<name>/agent.toml` +
    // `persona.md` + optional `skills.md`) must load with the system
    // prompt sourced from persona.md and skills.md appended.
    let _guard = ENV_LOCK.lock().unwrap();
    let tmp = tempfile::tempdir().expect("create temp dir");
    let agents = tmp.path();
    let pkg = agents.join("cto-assistant");
    std::fs::create_dir(&pkg).expect("create package dir");
    std::fs::write(
        pkg.join("agent.toml"),
        r#"
[agent]
name = "cto-assistant"
role = "assistant"
model = "anthropic/claude-sonnet-4-6"
description = "test agent"

[llm]
temperature = 0.3
max_tokens = 4096
"#,
    )
    .expect("write agent.toml");
    let persona = "You are the CTO Assistant. Be concise and direct.";
    std::fs::write(pkg.join("persona.md"), persona).expect("write persona.md");
    let skills = "## Skill: org chart\nThe SELT has five members.";
    std::fs::write(pkg.join("skills.md"), skills).expect("write skills.md");

    // SAFETY: guarded by ENV_LOCK.
    unsafe {
        std::env::set_var("TAGENT_CONFIG_DIR", agents);
    }
    let cfg = AgentConfig::by_name("cto-assistant").expect("loads package");
    // SAFETY: guarded by ENV_LOCK.
    unsafe {
        std::env::remove_var("TAGENT_CONFIG_DIR");
    }

    assert_eq!(cfg.agent.name, "cto-assistant");
    let expected = format!("{persona}\n\n---\n\n{skills}");
    assert_eq!(cfg.system_prompt.content, expected);
}

#[test]
fn agent_config_path_honors_env_var() {
    // MIN-7 (#104): With TAGENT_CONFIG_DIR set, resolution must use it
    // verbatim instead of the CWD-relative fallback.
    let _guard = ENV_LOCK.lock().unwrap();
    // SAFETY: guarded by ENV_LOCK
    unsafe {
        std::env::set_var("TAGENT_CONFIG_DIR", "/tmp/custom-agents");
    }
    let p = agent_config_path("pm");
    assert_eq!(p, PathBuf::from("/tmp/custom-agents/pm.toml"));
    // SAFETY: guarded by ENV_LOCK
    unsafe {
        std::env::remove_var("TAGENT_CONFIG_DIR");
    }
    let p = agent_config_path("pm");
    assert_eq!(p, PathBuf::from(".trusty-agents/agents/pm.toml"));
}

#[test]
fn agent_config_load_populates_adapter() {
    // Loading a real agent TOML should set `adapter` to match the model.
    // `plan-agent` is configured with an Anthropic model.
    let _guard = ENV_LOCK.lock().unwrap();
    clear_model_env("plan-agent");
    let cfg = AgentConfig::by_name("plan-agent").expect("plan-agent loads");
    use crate::llm::adapter::Provider;
    assert_eq!(cfg.adapter.provider(), Provider::Anthropic);
}

#[test]
fn agent_config_ctrl_default_loads_with_adapter() {
    // The built-in ctrl default (#240) must parse and populate an adapter
    // so the controller can boot with zero on-disk config.
    let _guard = ENV_LOCK.lock().unwrap();
    clear_model_env("ctrl");
    let cfg = AgentConfig::ctrl_default();
    assert_eq!(cfg.agent.name, "ctrl");
    assert_eq!(cfg.agent.role, "controller");
    assert!(cfg.system_prompt.content.contains("Standalone"));
    assert!(cfg.system_prompt.content.contains("delegate_to_agent"));
    // Adapter is populated by from_toml_str.
    use crate::llm::adapter::Provider;
    assert_eq!(cfg.adapter.provider(), Provider::Anthropic);
}

#[test]
fn stop_sequences_too_many_is_rejected() {
    let seqs: Vec<String> = (0..9).map(|i| format!("seq{}", i)).collect();
    let seqs_toml = seqs
        .iter()
        .map(|s| format!("\"{}\"", s))
        .collect::<Vec<_>>()
        .join(", ");
    let toml_str = format!(
        r#"
[agent]
name = "test-agent"
role = "engineer"
model = "anthropic/claude-sonnet-4-6"
description = "test"

[llm]
temperature = 0.2
max_tokens = 1024
stop_sequences = [{}]

[system_prompt]
content = "test"
"#,
        seqs_toml
    );
    let result = AgentConfig::from_toml_str(&toml_str, Path::new("test.toml"));
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("stop_sequences"),
        "error should mention stop_sequences: {}",
        msg
    );
}

#[test]
fn stop_sequences_over_length_limit_is_rejected() {
    let long_seq = "x".repeat(8192); // one over the limit
    let toml_str = format!(
        r#"
[agent]
name = "test-agent"
role = "engineer"
model = "anthropic/claude-sonnet-4-6"
description = "test"

[llm]
temperature = 0.2
max_tokens = 1024
stop_sequences = ["{}"]

[system_prompt]
content = "test"
"#,
        long_seq
    );
    let result = AgentConfig::from_toml_str(&toml_str, Path::new("test.toml"));
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("stop_sequences"),
        "error should mention stop_sequences: {}",
        msg
    );
}

/// Why: #446 — agent TOML must accept the new `[[plugins.python]]` table and
/// produce a structured `AgentPluginsConfig` with one entry per declaration.
/// What: Parse a minimal agent TOML with two plugin entries (one using
/// `schema_file`, one using inline `[plugins.python.schema]`) and assert the
/// parsed fields, including `restricted_tiers` for the RBAC override path.
#[test]
fn plugins_python_section_parses() {
    let toml_str = r#"
[agent]
name = "test"
role = "engineer"
model = "anthropic/claude-sonnet-4-6"
description = "test"

[llm]
temperature = 0.2
max_tokens = 1024

[system_prompt]
content = "test"

[[plugins.python]]
name = "gfa_report"
description = "Git Flow Analytics"
script = "scripts/gfa.py"
schema_file = "scripts/gfa_schema.json"
timeout_secs = 30

[[plugins.python]]
name = "search_email"
description = "Search priority emails"
script = "scripts/email.py"
timeout_secs = 10
restricted_tiers = ["analytics", "read_only"]

[plugins.python.schema]
type = "object"

[plugins.python.schema.properties]
query = { type = "string" }
"#;
    let cfg = AgentConfig::from_toml_str(toml_str, Path::new("test.toml"))
        .expect("plugins.python section must parse");

    assert_eq!(cfg.plugins.python.len(), 2);

    let gfa = &cfg.plugins.python[0];
    assert_eq!(gfa.name, "gfa_report");
    assert_eq!(
        gfa.schema_file.as_deref(),
        Some(std::path::Path::new("scripts/gfa_schema.json"))
    );
    assert_eq!(gfa.timeout_secs, Some(30));
    assert!(gfa.restricted_tiers.is_empty());

    let email = &cfg.plugins.python[1];
    assert_eq!(email.name, "search_email");
    assert_eq!(email.timeout_secs, Some(10));
    assert_eq!(
        email.restricted_tiers,
        vec!["analytics".to_string(), "read_only".to_string()]
    );
    assert!(email.schema.is_some(), "inline schema must be parsed");
}

/// Why: An agent TOML with no `[plugins]` section must continue to load
/// cleanly — the field defaults to an empty `AgentPluginsConfig`. Pins
/// backward compatibility for the ~30 existing agent TOMLs.
#[test]
fn plugins_section_defaults_empty() {
    let toml_str = r#"
[agent]
name = "test"
role = "engineer"
model = "anthropic/claude-sonnet-4-6"
description = "test"

[llm]
temperature = 0.2
max_tokens = 1024

[system_prompt]
content = "test"
"#;
    let cfg = AgentConfig::from_toml_str(toml_str, Path::new("test.toml"))
        .expect("no plugins section must still parse");
    assert!(cfg.plugins.python.is_empty());
}
