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
use tokio::sync::Mutex;

// Serialize model-resolution tests because they mutate process-global env.
//
// Why `tokio::sync::Mutex` rather than `std::sync::Mutex`: the async loader
// test (`by_name_async_loads_plan_agent`) must hold this guard across an
// `.await` point so a concurrent test can't mutate `TAGENT_CONFIG_DIR` (or
// the `TAGENT_MODEL_*` / `TAGENT_DEFAULT_MODEL` vars) mid-read. A
// `std::sync::MutexGuard` held across `.await` trips
// `clippy::await_holding_lock`; `tokio::sync::Mutex` is designed for exactly
// this and its guard is `Send`. Sync `#[test]` functions use
// `blocking_lock()`, which is safe here because none of them run inside a
// tokio runtime.
static ENV_LOCK: Mutex<()> = Mutex::const_new(());

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
    let _guard = ENV_LOCK.blocking_lock();
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
    let _guard = ENV_LOCK.blocking_lock();
    clear_model_env("x-agent");
    let (m, src) = resolve_model("x-agent", "toml/agent", Some("toml/override"));
    assert_eq!(m, "toml/override");
    assert_eq!(src, ModelSource::LlmOverride);
}

#[test]
fn resolve_model_uses_agent_model_when_no_override() {
    let _guard = ENV_LOCK.blocking_lock();
    clear_model_env("y-agent");
    let (m, src) = resolve_model("y-agent", "toml/agent", None);
    assert_eq!(m, "toml/agent");
    assert_eq!(src, ModelSource::AgentToml);
}

#[test]
fn resolve_model_uses_default_env_when_nothing_else() {
    let _guard = ENV_LOCK.blocking_lock();
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
    let _guard = ENV_LOCK.blocking_lock();
    clear_model_env("q-agent");
    let (m, src) = resolve_model("q-agent", "", None);
    assert_eq!(m, FALLBACK_MODEL);
    assert_eq!(src, ModelSource::Fallback);
}

#[test]
fn resolve_model_empty_llm_override_is_ignored() {
    let _guard = ENV_LOCK.blocking_lock();
    clear_model_env("r-agent");
    let (m, src) = resolve_model("r-agent", "toml/agent", Some(""));
    assert_eq!(m, "toml/agent");
    assert_eq!(src, ModelSource::AgentToml);
}

#[tokio::test]
async fn by_name_async_loads_plan_agent() {
    // #96: Async loader should produce the same adapter + model as the
    // sync path when TAGENT_CONFIG_DIR is unset (fallback path).
    // Hold the guard across the `.await` below: `by_name_async` reads the
    // process-global `TAGENT_CONFIG_DIR` env var at poll-time, so dropping
    // the guard before the call lets a concurrent test (e.g.
    // `agent_config_path_honors_env_var`) mutate the var mid-read and
    // redirect the lookup out from under this test.
    let _guard = ENV_LOCK.lock().await;
    clear_model_env("plan-agent");
    // SAFETY: guarded by ENV_LOCK for the duration of this guard's scope,
    // which spans the `by_name_async` call below.
    unsafe {
        std::env::remove_var("TAGENT_CONFIG_DIR");
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
    let _guard = ENV_LOCK.blocking_lock();
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
    let _guard = ENV_LOCK.blocking_lock();
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
    let _guard = ENV_LOCK.blocking_lock();
    clear_model_env("plan-agent");
    let cfg = AgentConfig::by_name("plan-agent").expect("plan-agent loads");
    use crate::llm::adapter::Provider;
    assert_eq!(cfg.adapter.provider(), Provider::Anthropic);
}

#[test]
fn agent_config_ctrl_default_loads_with_adapter() {
    // The built-in ctrl default (#240) must parse and populate an adapter
    // so the controller can boot with zero on-disk config.
    let _guard = ENV_LOCK.blocking_lock();
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

/// Path to the bundled `.trusty-agents/agents` directory shipped with the crate.
fn bundled_agents_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(".trusty-agents")
        .join("agents")
}

#[test]
fn base_assistant_package_is_nameless_and_curated() {
    // #3054: The base `assistant` agent must load from its directory-package
    // form, carry the curated gworkspace tool surface + productivity scopes,
    // and be NAMELESS — no persona display_name/prompt_label. Persona identity
    // is supplied later by a user's `extends` overlay (#3055).
    let _guard = ENV_LOCK.blocking_lock();
    clear_model_env("assistant");
    // SAFETY: guarded by ENV_LOCK.
    unsafe {
        std::env::set_var("TAGENT_CONFIG_DIR", bundled_agents_dir());
    }
    let cfg = AgentConfig::by_name("assistant");
    // SAFETY: guarded by ENV_LOCK.
    unsafe {
        std::env::remove_var("TAGENT_CONFIG_DIR");
    }
    let cfg = cfg.expect("base assistant package must load");

    assert_eq!(cfg.agent.name, "assistant");
    assert_eq!(cfg.agent.role, "assistant");
    // Nameless: the base carries NO persona identity.
    assert_eq!(
        cfg.agent.display_name, None,
        "base assistant must be nameless (no display_name)"
    );
    assert_eq!(
        cfg.agent.prompt_label, None,
        "base assistant must be nameless (no prompt_label)"
    );
    // Curated tool surface + scopes are present on the base.
    let allow = cfg
        .tools
        .allow
        .expect("base assistant declares an allowlist");
    assert!(
        allow.iter().any(|t| t == "search_gmail_messages"),
        "base assistant must expose the gworkspace surface"
    );
    let scopes = cfg.tools.scopes.expect("base assistant declares scopes");
    assert!(scopes.iter().any(|s| s == "memory.write"));
    // §5.5: the generic base template defaults to READ-ONLY Google access.
    // Write (`google.*`) is opted into by a personalization overlay, not the base.
    assert!(
        scopes.iter().any(|s| s == "google.read"),
        "base assistant must default to read-only Google (google.read)"
    );
    assert!(
        !scopes.iter().any(|s| s == "google.*"),
        "base assistant must NOT grant Google write by default"
    );
    // Generic productivity skills only — NO user-specific skills on the base.
    let skills = cfg.system_prompt.skills.expect("base declares skills");
    assert!(skills.iter().any(|s| s == "gworkspace-gmail"));
    assert!(
        !skills.iter().any(|s| s.starts_with("izzie-")),
        "base assistant must not carry user-specific (izzie-*) skills"
    );
    // The persona body must not bind to a specific user.
    assert!(
        !cfg.system_prompt.content.contains("Masa"),
        "base assistant persona must not bind to a specific user"
    );
    assert!(
        !cfg.system_prompt.content.contains("Izzie"),
        "base assistant persona must be nameless"
    );
}

#[test]
fn izzie_overlay_package_parses_with_personal_deltas() {
    // #3054: The Izzie overlay package must parse (its `extends = "assistant"`
    // key is ignored until #3055 lands) and carry the personal deltas — the
    // display name, the personal skills, and the Masa-bound persona body.
    let _guard = ENV_LOCK.blocking_lock();
    clear_model_env("izzie");
    // SAFETY: guarded by ENV_LOCK.
    unsafe {
        std::env::set_var("TAGENT_CONFIG_DIR", bundled_agents_dir());
    }
    let cfg = AgentConfig::by_name("izzie");
    // SAFETY: guarded by ENV_LOCK.
    unsafe {
        std::env::remove_var("TAGENT_CONFIG_DIR");
    }
    let cfg = cfg.expect("izzie overlay package must parse");

    assert_eq!(cfg.agent.name, "izzie");
    assert_eq!(cfg.agent.display_name.as_deref(), Some("Izzie"));
    let skills = cfg.system_prompt.skills.expect("overlay declares skills");
    assert!(skills.iter().any(|s| s == "izzie-weather"));
    // Personal deltas only — the overlay does not re-list the base's generic
    // skills (they are inherited under #3055).
    assert!(
        !skills.iter().any(|s| s == "gworkspace-gmail"),
        "overlay must not duplicate the base's generic skills"
    );
    assert!(cfg.system_prompt.content.contains("Masa"));

    // SAFE-STANDALONE regression tripwire (critic BLOCK #3094): `by_name("izzie")`
    // resolves to THIS package (it shadows the flat izzie.toml), and an absent
    // `[tools].allow` means UNRESTRICTED tools. Until the #3055 extends resolver
    // lands, the overlay must carry the curated allowlist + scopes itself so the
    // dispatch surface is restricted. Do NOT relax these asserts by dropping the
    // block — drop it only together with the #3055 inheritance change.
    let allow = cfg
        .tools
        .allow
        .expect("izzie overlay must restrict tools (absent = UNRESTRICTED)");
    assert!(
        allow.iter().any(|t| t == "search_gmail_messages"),
        "overlay must carry the curated gworkspace surface until #3055"
    );
    assert!(
        allow.iter().any(|t| t == "granola_*"),
        "overlay must carry the curated tool surface until #3055"
    );
    let scopes = cfg.tools.scopes.expect("izzie overlay must declare scopes");
    assert!(scopes.iter().any(|s| s == "memory.write"));
    // Izzie opts into Google WRITE on top of the base's read-only default.
    assert!(
        scopes.iter().any(|s| s == "google.*"),
        "izzie overlay opts into Google write (google.*)"
    );
    // Safety-critical guardrails must be present in the standalone persona body.
    assert!(
        cfg.system_prompt.content.contains("Approval Framing"),
        "overlay persona must carry the approval-framing guardrail standalone"
    );
    assert!(
        cfg.system_prompt.content.contains("Anti-Hallucination"),
        "overlay persona must carry the anti-hallucination guardrail standalone"
    );
}

// --- #3055 `extends` end-to-end loader tests -----------------------------
//
// These exercise the REAL dispatch loaders (`AgentConfig::by_name` /
// `by_name_async`) — not the informational `AgentRegistry` — over on-disk
// `extends` chains, closing the code-critic CRITICAL (flat `.md` overlays were
// unreachable at dispatch) and HIGH (by_name/by_name_async had zero extends
// coverage) findings on PR #3106.

/// A base agent flat TOML with an Anthropic model + given prose.
fn write_flat_toml_base(dir: &Path, name: &str, body: &str) {
    let toml = format!(
        r#"
[agent]
name = "{name}"
role = "researcher"
model = "anthropic/claude-sonnet-4-6"
description = "the base {name}"

[llm]
temperature = 0.0
max_tokens = 1024

[system_prompt]
content = "{body}"
"#
    );
    std::fs::write(dir.join(format!("{name}.toml")), toml).expect("write base toml");
}

#[test]
fn extends_resolved_child_inherits_base_model() {
    // A flat `.md` child that OMITS `model` and `extends` a flat-TOML base must
    // dispatch-load via `by_name` with the base's model AND a matching adapter.
    let _guard = ENV_LOCK.blocking_lock();
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path();
    write_flat_toml_base(dir, "researcher", "BASE INSTRUCTIONS");
    // Child overlay: no model, adds a tool, appends prose.
    let child = "---\nname: my-researcher\nrole: agent\nextends: researcher\n\
tools:\n  allowed: [my_tool]\n---\n\nMY OVERRIDES\n";
    std::fs::write(dir.join("my-researcher.md"), child).expect("write child md");

    clear_model_env("my-researcher");
    // SAFETY: guarded by ENV_LOCK.
    unsafe {
        std::env::set_var("TAGENT_CONFIG_DIR", dir);
    }
    let cfg = AgentConfig::by_name("my-researcher").expect("resolves + dispatch-loads");
    // SAFETY: guarded by ENV_LOCK.
    unsafe {
        std::env::remove_var("TAGENT_CONFIG_DIR");
    }

    assert_eq!(cfg.agent.name, "my-researcher");
    // Model inherited from the base and resolved; adapter matches it.
    assert_eq!(cfg.agent.model, "anthropic/claude-sonnet-4-6");
    use crate::llm::adapter::Provider;
    assert_eq!(cfg.adapter.provider(), Provider::Anthropic);
}

#[test]
fn by_name_flat_md_extends_dispatches() {
    // The flat `.md` personalization overlay (the primary #3055 surface) must be
    // reachable from `by_name` with prose concatenated base-first and tools
    // unioned.
    let _guard = ENV_LOCK.blocking_lock();
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path();
    write_flat_toml_base(dir, "researcher", "BASE INSTRUCTIONS");
    let child = "---\nname: my-researcher\nrole: agent\nextends: researcher\n\
tools:\n  allowed: [my_tool]\n---\n\nMY OVERRIDES\n";
    std::fs::write(dir.join("my-researcher.md"), child).expect("write child md");

    clear_model_env("my-researcher");
    // SAFETY: guarded by ENV_LOCK.
    unsafe {
        std::env::set_var("TAGENT_CONFIG_DIR", dir);
    }
    let cfg = AgentConfig::by_name("my-researcher").expect("flat-md extends dispatches");
    // SAFETY: guarded by ENV_LOCK.
    unsafe {
        std::env::remove_var("TAGENT_CONFIG_DIR");
    }

    assert_eq!(
        cfg.system_prompt.content,
        "BASE INSTRUCTIONS\n\nMY OVERRIDES"
    );
    assert_eq!(
        cfg.tools.allowed.as_deref(),
        Some(&["my_tool".to_string()][..])
    );
    assert!(cfg.agent.extends.is_none());
}

#[tokio::test]
async fn by_name_async_flat_md_extends_dispatches() {
    // Same flat-`.md` extends chain must ALSO dispatch via the async loader —
    // proving the sync/async tier symmetry (no asymmetric ExtendsNotFound).
    let _guard = ENV_LOCK.lock().await;
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path();
    write_flat_toml_base(dir, "researcher", "BASE INSTRUCTIONS");
    let child = "---\nname: my-researcher\nrole: agent\nextends: researcher\n---\n\nMY OVERRIDES\n";
    std::fs::write(dir.join("my-researcher.md"), child).expect("write child md");

    clear_model_env("my-researcher");
    // SAFETY: guarded by ENV_LOCK across the await below.
    unsafe {
        std::env::set_var("TAGENT_CONFIG_DIR", dir);
    }
    let cfg = AgentConfig::by_name_async("my-researcher")
        .await
        .expect("flat-md extends dispatches async");
    // SAFETY: guarded by ENV_LOCK.
    unsafe {
        std::env::remove_var("TAGENT_CONFIG_DIR");
    }

    assert_eq!(cfg.agent.name, "my-researcher");
    assert_eq!(cfg.agent.model, "anthropic/claude-sonnet-4-6");
    assert_eq!(
        cfg.system_prompt.content,
        "BASE INSTRUCTIONS\n\nMY OVERRIDES"
    );
}

#[test]
fn by_name_package_extends_shadow_falls_back_to_flat() {
    // A directory package whose `extends` cannot be resolved must NOT shadow a
    // complete flat `<name>.toml` — the flat file wins (with a warn).
    let _guard = ENV_LOCK.blocking_lock();
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path();

    // Package `izzie/agent.toml` with an UNRESOLVABLE extends and NO [tools].
    let pkg = dir.join("izzie");
    std::fs::create_dir(&pkg).expect("mk pkg");
    std::fs::write(
        pkg.join("agent.toml"),
        r#"
[agent]
name = "izzie"
role = "assistant"
model = "anthropic/claude-sonnet-4-6"
description = "partial package"
extends = "ghost-base"

[llm]
temperature = 0.0
max_tokens = 1024
"#,
    )
    .expect("write pkg agent.toml");
    std::fs::write(pkg.join("persona.md"), "PARTIAL PACKAGE").expect("write persona");

    // Complete flat `izzie.toml` alongside it (locked-down tools, no extends).
    std::fs::write(
        dir.join("izzie.toml"),
        r#"
[agent]
name = "izzie"
role = "assistant"
model = "anthropic/claude-sonnet-4-6"
description = "complete flat"

[llm]
temperature = 0.0
max_tokens = 1024

[system_prompt]
content = "COMPLETE FLAT"

[tools]
allowed = ["locked_tool"]
"#,
    )
    .expect("write flat toml");

    clear_model_env("izzie");
    // SAFETY: guarded by ENV_LOCK.
    unsafe {
        std::env::set_var("TAGENT_CONFIG_DIR", dir);
    }
    let cfg = AgentConfig::by_name("izzie").expect("falls back to flat toml");
    // SAFETY: guarded by ENV_LOCK.
    unsafe {
        std::env::remove_var("TAGENT_CONFIG_DIR");
    }

    // Flat file won — NOT the tools-less partial package.
    assert_eq!(cfg.system_prompt.content, "COMPLETE FLAT");
    assert_eq!(
        cfg.tools.allowed.as_deref(),
        Some(&["locked_tool".to_string()][..])
    );
}
