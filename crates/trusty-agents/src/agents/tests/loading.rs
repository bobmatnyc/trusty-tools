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
//
// `pub(crate)`: #3465-followup — `agents::persona::tests` ALSO mutates
// `TAGENT_CONFIG_DIR` and previously defined its OWN separate, same-named
// `ENV_LOCK` static, which is a DIFFERENT object and does not exclude
// against this one — the two files' tests raced on `TAGENT_CONFIG_DIR`
// despite each individually looking "guarded". `agents::persona::tests` now
// takes THIS lock instead of defining its own, so every `TAGENT_CONFIG_DIR`
// mutator in the crate shares one exclusion domain.
pub(crate) static ENV_LOCK: Mutex<()> = Mutex::const_new(());

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

/// #3303 code-critic HIGH-2: `by_name`/`by_name_in` must reject a `name`
/// that path-traverses outside the search dir before it ever reaches
/// `load_agent_package`/the flat-file joins — a name like `"../victim"`
/// previously escaped the intended agents dir entirely via `dir.join(name)`.
///
/// Why: builds a `victim/agent.toml` package as a SIBLING of (not inside)
/// the dir passed to `by_name_in`, so a successful `Ok` would prove the
/// traversal actually reached outside content — the guard must make this an
/// `Err` instead.
/// What: `AgentConfig::by_name_in(&[agents_dir], "../victim")` must return
/// `Err`. No env mutation needed — `by_name_in` takes its search dirs
/// explicitly, so this doesn't need `ENV_LOCK`.
/// Test: Self-explanatory.
#[test]
fn by_name_in_rejects_parent_dir_traversal() {
    let root = tempfile::tempdir().expect("create temp dir");
    let agents = root.path().join("agents");
    std::fs::create_dir_all(&agents).expect("create agents dir");
    let victim = root.path().join("victim");
    std::fs::create_dir(&victim).expect("create victim dir");
    std::fs::write(
        victim.join("agent.toml"),
        r#"
[agent]
name = "victim"
role = "assistant"
model = "anthropic/claude-haiku-4-5"

[llm]
temperature = 0.5
max_tokens = 1024
"#,
    )
    .expect("write agent.toml");
    std::fs::write(victim.join("persona.md"), "secret persona").expect("write persona.md");

    let result = AgentConfig::by_name_in(&[agents], "../victim");
    assert!(
        result.is_err(),
        "path traversal via '../victim' must be rejected, got: {result:?}"
    );
}

/// #3303 code-critic HIGH-2 companion: a name containing an embedded `/`
/// (not just `..`) must also be rejected — `dir.join("a/b")` is a valid
/// nested-path join regardless of whether either segment is `..`.
/// Test: Self-explanatory.
#[test]
fn by_name_in_rejects_nested_path_segment() {
    let tmp = tempfile::tempdir().expect("create temp dir");
    let result = AgentConfig::by_name_in(&[tmp.path().to_path_buf()], "a/b");
    assert!(
        result.is_err(),
        "nested path segment 'a/b' must be rejected, got: {result:?}"
    );
}

/// #3303: `by_name` (the default-dirs wrapper) must apply the same guard as
/// `by_name_in` — the validation must fire before any dir is even consulted,
/// so this needs no `TAGENT_CONFIG_DIR` setup / `ENV_LOCK`.
/// Test: Self-explanatory.
#[test]
fn by_name_rejects_parent_dir_traversal() {
    let result = AgentConfig::by_name("../../etc");
    assert!(
        result.is_err(),
        "by_name must reject path traversal, got: {result:?}"
    );
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
    // #3054/#3055/#3106, corrected by PR A code-critic follow-up (#3052):
    // the Izzie overlay package's `extends = "assistant"` key REALLY
    // resolves now (it previously lived BEFORE the `[agent]` table header
    // in izzie/agent.toml, making it a top-level TOML key that
    // `AgentInfo::extends` never saw — serde silently dropped it, so
    // `by_name("izzie")` returned the package UNMERGED the whole time; the
    // asserts below were written against that broken state and are updated
    // here to match the now-correct merged behavior). The overlay must
    // carry its personal deltas (display name, personal skills, Masa-bound
    // persona body) AND the base's generic tools/skills/guardrail prose via
    // real `extends` union/concatenation.
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
    // The base's generic skills are UNIONED in via real `extends` resolution
    // (not a duplicate declaration — izzie's own `[system_prompt].skills`
    // does not list this).
    assert!(
        skills.iter().any(|s| s == "gworkspace-gmail"),
        "overlay must inherit the base's generic skills via extends union"
    );
    assert!(cfg.system_prompt.content.contains("Masa"));
    // The base's persona body is concatenated in base-first, ahead of
    // izzie's own personal deltas.
    assert!(
        cfg.system_prompt
            .content
            .contains("knowledgeable assistant who is very organized"),
        "overlay persona must carry the base assistant's prose via extends concatenation"
    );

    // `by_name("izzie")` resolves to THIS package (it shadows the flat
    // izzie.toml). The overlay ALSO still carries its own redundant-but-
    // explicit `[tools].allow`/`scopes` (see the header note in
    // izzie/agent.toml) so it stays safe even if ever loaded standalone
    // (bypassing union) — real union with the base only adds to this, never
    // removes from it.
    let allow = cfg
        .tools
        .allow
        .expect("izzie overlay must restrict tools (absent = UNRESTRICTED)");
    assert!(
        allow.iter().any(|t| t == "search_gmail_messages"),
        "overlay must carry the curated gworkspace surface"
    );
    assert!(
        allow.iter().any(|t| t == "granola_*"),
        "overlay must carry the curated tool surface"
    );
    let scopes = cfg.tools.scopes.expect("izzie overlay must declare scopes");
    assert!(scopes.iter().any(|s| s == "memory.write"));
    // Izzie opts into Google WRITE on top of the base's read-only default.
    assert!(
        scopes.iter().any(|s| s == "google.*"),
        "izzie overlay opts into Google write (google.*)"
    );
    // Safety-critical guardrails must be present in the resolved persona body.
    assert!(
        cfg.system_prompt.content.contains("Approval Framing"),
        "overlay persona must carry the approval-framing guardrail"
    );
    assert!(
        cfg.system_prompt.content.contains("Anti-Hallucination"),
        "overlay persona must carry the anti-hallucination guardrail"
    );
}

/// PR A (epic #3052): pins the assistant-tier delegation grant + black-box
/// tool strip so a future edit to `assistant/agent.toml` (or the
/// `extends`-based personas layered on it) can't silently regress either
/// property.
///
/// Why: The base `assistant` gained `delegate_to_agent` (so it can actually
/// bring in a specialist/peer) and lost `session_list`, `session_status`,
/// `project_list`, `console_metrics`, `system_status`, `mcp_list`,
/// `mcp_enable`, `mcp_disable` (all of which either proxy trusty-mpm by name
/// or enumerate internal daemon/MCP-service names to the user). Both `izzie`
/// and `cto-assistant` declare `extends = "assistant"`, and `merge_extends`
/// UNIONS `[tools].allow` base-first — so this test also confirms the grant
/// propagates through inheritance and the strip isn't reintroduced by either
/// overlay's own (now-deltas-only) allowlist.
/// What: Loads `assistant`, `izzie`, and `cto-assistant` via the real
/// `AgentConfig::by_name` dispatch loader (bundled agents dir) and asserts
/// each resolved `[tools].allow` contains `delegate_to_agent` and excludes
/// every black-boxed tool name.
/// Test: This function IS the test.
#[test]
fn assistant_tier_grants_delegation_and_blackboxes_internal_tools() {
    let _guard = ENV_LOCK.blocking_lock();

    let leaked_tools = [
        "session_list",
        "session_status",
        "session_send",
        "project_list",
        "console_metrics",
        "system_status",
        "mcp_list",
        "mcp_enable",
        "mcp_disable",
        "agent_delegate",
    ];

    for agent_name in ["assistant", "izzie", "cto-assistant"] {
        clear_model_env(agent_name);
        // SAFETY: guarded by ENV_LOCK.
        unsafe {
            std::env::set_var("TAGENT_CONFIG_DIR", bundled_agents_dir());
        }
        let cfg = AgentConfig::by_name(agent_name);
        // SAFETY: guarded by ENV_LOCK.
        unsafe {
            std::env::remove_var("TAGENT_CONFIG_DIR");
        }
        let cfg = cfg.unwrap_or_else(|e| panic!("'{agent_name}' must resolve: {e}"));

        let allow = cfg
            .tools
            .allow
            .unwrap_or_else(|| panic!("'{agent_name}' must declare an allowlist"));

        assert!(
            allow.iter().any(|t| t == "delegate_to_agent"),
            "'{agent_name}' resolved allow-list must include delegate_to_agent (got {allow:?})"
        );
        for leaked in leaked_tools {
            assert!(
                !allow.iter().any(|t| t == leaked),
                "'{agent_name}' resolved allow-list must NOT include black-boxed tool \
                 '{leaked}' (got {allow:?})"
            );
        }
    }
}

/// PR A code-critic follow-up (#3052): the functional-fix companion to
/// [`assistant_tier_grants_delegation_and_blackboxes_internal_tools`] — a
/// persona that HAS the `delegate_to_agent` grant but no internal knowledge
/// of which `agent_name` values are legitimate would blind-guess. This pins
/// that `assistant`'s (and its `extends` descendants') resolved system
/// prompt carries a curated internal routing list naming only real,
/// bundled WORKER agent TOMLs (not meta/infra agents like `ctrl`/`pm`/
/// `observe-agent`/`postmortem-agent`, and not model-variant engineers like
/// `bedrock-engineer`/`gpt-engineer`), and that the black-box reminder
/// ("NEVER reveal internal mechanics") is still present alongside it.
/// Test: This function IS the test.
#[test]
fn assistant_tier_persona_carries_curated_worker_routing_list() {
    let _guard = ENV_LOCK.blocking_lock();

    // Every one of these must exist as a real bundled agent TOML — this
    // loop doubles as a "the routing list didn't drift from the bundled
    // roster" check.
    let curated_workers = [
        "engineer",
        "python-engineer",
        "qa-agent",
        "research-agent",
        "docs-agent",
        "local-ops-agent",
        "plan-agent",
    ];
    for worker in curated_workers {
        assert!(
            bundled_agents_dir()
                .join(format!("{worker}.toml"))
                .is_file(),
            "curated worker '{worker}' must be a real bundled agent TOML"
        );
    }
    // Meta/infra + model-variant agents must NOT appear in the curated list
    // (they may still appear elsewhere in the persona body incidentally, so
    // this is enforced structurally above, not via a substring-absence
    // check on the whole persona body).

    for agent_name in ["assistant", "izzie", "cto-assistant"] {
        clear_model_env(agent_name);
        // SAFETY: guarded by ENV_LOCK.
        unsafe {
            std::env::set_var("TAGENT_CONFIG_DIR", bundled_agents_dir());
        }
        let cfg = AgentConfig::by_name(agent_name);
        // SAFETY: guarded by ENV_LOCK.
        unsafe {
            std::env::remove_var("TAGENT_CONFIG_DIR");
        }
        let cfg = cfg.unwrap_or_else(|e| panic!("'{agent_name}' must resolve: {e}"));
        let body = &cfg.system_prompt.content;

        for worker in curated_workers {
            assert!(
                body.contains(worker),
                "'{agent_name}' resolved persona must carry internal routing knowledge \
                 of worker '{worker}'"
            );
        }
        assert!(
            body.contains("NEVER reveal internal mechanics"),
            "'{agent_name}' resolved persona must still carry the black-box reminder \
             alongside the routing list"
        );
    }
}

/// PR A follow-up (#3052): pins the resolved `[llm]` `temperature`/
/// `max_tokens` for every bundled assistant-tier agent, proving the per-key
/// `extends` inheritance fix (a) restores `cto-assistant`'s tuned sampling
/// (the #469 regression) and (b) leaves `assistant`/`izzie` — which don't
/// override either field — resolving to EXACTLY the base's values, unchanged
/// from before this fix (no unintended drift for agents that inherit
/// wholesale).
/// What: Loads `assistant`, `izzie`, and `cto-assistant` via the real
/// `AgentConfig::by_name` dispatch loader and asserts each resolved
/// `(temperature, max_tokens)` pair.
/// Test: This function IS the test.
#[test]
fn assistant_tier_llm_sampling_params_pin_per_key_extends_inheritance() {
    let _guard = ENV_LOCK.blocking_lock();

    // (agent name, expected temperature, expected max_tokens)
    let cases: [(&str, f32, u32); 3] = [
        // Base: declares its own values directly (not an extends child) —
        // must be completely unaffected by the per-key merge change.
        ("assistant", 0.7, 1024),
        // izzie: extends "assistant" and declares a REDUNDANT [llm] block
        // matching the base's values (see izzie/agent.toml's header note) —
        // must resolve identically to the base, whether via its own
        // declaration or via inheritance.
        ("izzie", 0.7, 1024),
        // cto-assistant: extends "assistant" and declares ONLY temperature/
        // max_tokens as deltas — must resolve to ITS tuned values (#469),
        // not the base's, restoring the pre-`extends`-conversion behavior.
        ("cto-assistant", 0.3, 4096),
    ];

    for (agent_name, expected_temperature, expected_max_tokens) in cases {
        clear_model_env(agent_name);
        // SAFETY: guarded by ENV_LOCK.
        unsafe {
            std::env::set_var("TAGENT_CONFIG_DIR", bundled_agents_dir());
        }
        let cfg = AgentConfig::by_name(agent_name);
        // SAFETY: guarded by ENV_LOCK.
        unsafe {
            std::env::remove_var("TAGENT_CONFIG_DIR");
        }
        let cfg = cfg.unwrap_or_else(|e| panic!("'{agent_name}' must resolve: {e}"));

        assert_eq!(
            cfg.llm.temperature, expected_temperature,
            "'{agent_name}' resolved temperature mismatch"
        );
        assert_eq!(
            cfg.llm.max_tokens, expected_max_tokens,
            "'{agent_name}' resolved max_tokens mismatch"
        );
    }
}

/// PR A follow-up (#3052): a root (non-`extends`) agent TOML that omits
/// `[llm]` `temperature`/`max_tokens` must be REJECTED at load time — the
/// UNSET-sentinel serde defaults exist so an `extends` CHILD can omit them,
/// not so a root agent can silently ship with `NaN`/`u32::MAX` sampling
/// params reaching the LLM provider.
/// Test: This function IS the test.
#[test]
fn llm_required_fields_missing_on_root_agent_is_rejected() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path();
    std::fs::write(
        dir.join("bad-root.toml"),
        r#"
[agent]
name = "bad-root"
role = "agent"
model = "anthropic/claude-sonnet-4-6"
description = "root agent missing llm fields"

[llm]

[system_prompt]
content = "BODY"
"#,
    )
    .expect("write bad-root.toml");

    let err = AgentConfig::load(&dir.join("bad-root.toml"))
        .expect_err("a root agent omitting [llm] temperature/max_tokens must fail to load");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("temperature") && msg.contains("max_tokens"),
        "error should name both missing fields, got: {msg}"
    );
}

/// PR A follow-up (#3052): the SAME omitted-`[llm]` shape that
/// [`llm_required_fields_missing_on_root_agent_is_rejected`] rejects for a
/// root agent must load cleanly — and inherit the base's values — for an
/// `extends` child, exercised end-to-end through `AgentConfig::by_name`.
/// Test: This function IS the test.
#[test]
fn llm_omitted_fields_allowed_on_extends_child() {
    let _guard = ENV_LOCK.blocking_lock();
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path();
    write_flat_toml_base(dir, "researcher", "BASE INSTRUCTIONS");
    // write_flat_toml_base declares temperature = 0.0 / max_tokens = 1024.
    std::fs::write(
        dir.join("my-researcher.toml"),
        r#"
[agent]
name = "my-researcher"
role = "agent"
model = ""
description = ""
extends = "researcher"

[llm]

[system_prompt]
content = "CHILD PROSE"
"#,
    )
    .expect("write child toml");

    clear_model_env("my-researcher");
    // SAFETY: guarded by ENV_LOCK.
    unsafe {
        std::env::set_var("TAGENT_CONFIG_DIR", dir);
    }
    let cfg = AgentConfig::by_name("my-researcher");
    // SAFETY: guarded by ENV_LOCK.
    unsafe {
        std::env::remove_var("TAGENT_CONFIG_DIR");
    }
    let cfg = cfg.expect("extends child omitting [llm] fields must still resolve");

    assert_eq!(
        cfg.llm.temperature, 0.0,
        "must inherit the base's temperature"
    );
    assert_eq!(
        cfg.llm.max_tokens, 1024,
        "must inherit the base's max_tokens"
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

/// #3061: `agents_dir()` resolves a SINGLE directory (`TAGENT_CONFIG_DIR` or
/// the CWD-relative project-local `.trusty-agents/agents`) with no `$HOME`
/// awareness at all — unlike the registry roster
/// (`agent_search_paths`/`registry::mod.rs`), which DOES search
/// `~/.trusty-agents/agents`. So a personalization overlay dropped in
/// `~/.trusty-agents/agents/` showed up in listings but was FILE-NOT-FOUND at
/// dispatch. This proves `agents_dir_candidates()`'s `$HOME` fallback tier
/// fixes it. #3198 code-critic: `TAGENT_CONFIG_DIR` is pinned to a dedicated,
/// deliberately-empty tempdir (rather than left unset to fall back on the
/// real project-local `.trusty-agents/agents/`) — deterministic primary-tier
/// isolation, matching the sibling `TAGENT_CONFIG_DIR`-pinning tests in this
/// file, and independent of `cargo test`'s CWD.
#[test]
fn by_name_finds_flat_md_in_home_tier_when_project_dir_misses() {
    let _guard = ENV_LOCK.blocking_lock();
    // #3465-followup: this file's local `ENV_LOCK` only serializes tests
    // WITHIN this file — it is a different static from
    // `crate::test_env::HOME_LOCK`, so a test here mutating `$HOME` could
    // still race any of the many other files that DO use the shared
    // `HOME_LOCK`. Take it too so this and every other HOME-sandboxing test
    // in the crate are mutually exclusive.
    let _home_guard = crate::test_env::HOME_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let primary_tmp = tempfile::tempdir().expect("primary temp dir");
    let home_tmp = tempfile::tempdir().expect("home temp dir");
    let home_agents = home_tmp.path().join(".trusty-agents").join("agents");
    std::fs::create_dir_all(&home_agents).expect("mkdir home agents");
    let unique_name = format!("home-tier-agent-{}", std::process::id());
    std::fs::write(
        home_agents.join(format!("{unique_name}.md")),
        format!("---\nname: {unique_name}\nrole: agent\n---\nHOME TIER PROSE"),
    )
    .expect("write home overlay");

    clear_model_env(&unique_name);
    let prev_home = std::env::var_os("HOME");
    // SAFETY: guarded by ENV_LOCK.
    unsafe {
        // Primary tier is a fresh, empty tempdir — deliberately isolated from
        // the real project-local `.trusty-agents/agents/`.
        std::env::set_var("TAGENT_CONFIG_DIR", primary_tmp.path());
        std::env::set_var("HOME", home_tmp.path());
    }
    let result = AgentConfig::by_name(&unique_name);
    // SAFETY: guarded by ENV_LOCK.
    unsafe {
        std::env::remove_var("TAGENT_CONFIG_DIR");
        match &prev_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
    }

    let cfg = result.expect("by_name must find the overlay via the $HOME fallback tier");
    assert_eq!(cfg.system_prompt.content, "HOME TIER PROSE");
}

/// Async counterpart of `by_name_finds_flat_md_in_home_tier_when_project_dir_misses`
/// — proves `by_name_async`'s tier set is symmetric with the sync loader after
/// the #3061 fix (both call `agents_dir_candidates()`, not a bare
/// `agents_dir()`).
#[tokio::test]
// Why: `crate::test_env::HOME_LOCK` (a `std::sync::Mutex`) is held
// intentionally across the `.await` below so this test doesn't race other
// `$HOME`-sandboxing tests crate-wide — matches the established, deliberate
// pattern in `api::server::tests` (see that module's identical `#![allow]`).
#[allow(clippy::await_holding_lock)]
async fn by_name_async_finds_flat_md_in_home_tier_when_project_dir_misses() {
    let _guard = ENV_LOCK.lock().await;
    // #3465-followup: also take the shared crate::test_env::HOME_LOCK (see
    // the sync counterpart's comment) across the `.await` below — this
    // crate already holds `std::sync::Mutex` guards across `.await` in
    // other files (`api::server::tests::ctrl_sessions`,
    // `api::server::tests::models`) without issue.
    let _home_guard = crate::test_env::HOME_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let primary_tmp = tempfile::tempdir().expect("primary temp dir");
    let home_tmp = tempfile::tempdir().expect("home temp dir");
    let home_agents = home_tmp.path().join(".trusty-agents").join("agents");
    std::fs::create_dir_all(&home_agents).expect("mkdir home agents");
    let unique_name = format!("home-tier-agent-async-{}", std::process::id());
    std::fs::write(
        home_agents.join(format!("{unique_name}.md")),
        format!("---\nname: {unique_name}\nrole: agent\n---\nHOME TIER ASYNC PROSE"),
    )
    .expect("write home overlay");

    clear_model_env(&unique_name);
    let prev_home = std::env::var_os("HOME");
    // SAFETY: guarded by ENV_LOCK across the await below.
    unsafe {
        std::env::set_var("TAGENT_CONFIG_DIR", primary_tmp.path());
        std::env::set_var("HOME", home_tmp.path());
    }
    let result = AgentConfig::by_name_async(&unique_name).await;
    // SAFETY: guarded by ENV_LOCK.
    unsafe {
        std::env::remove_var("TAGENT_CONFIG_DIR");
        match &prev_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
    }

    let cfg = result.expect("by_name_async must find the overlay via the $HOME fallback tier");
    assert_eq!(cfg.system_prompt.content, "HOME TIER ASYNC PROSE");
}

/// #3198 code-critic MEDIUM fix: proves `agents_dir_candidates()` preserves
/// the documented precedence (explicit `TAGENT_CONFIG_DIR`/project-local >
/// `$HOME`) rather than e.g. merging fields or preferring `$HOME` — when the
/// SAME agent name exists as a flat `.md` in BOTH the primary directory and
/// the `$HOME` fallback tier, the primary tier's content must win.
#[test]
fn same_name_project_local_shadows_home_tier() {
    let _guard = ENV_LOCK.blocking_lock();
    // #3465-followup: also take the shared crate::test_env::HOME_LOCK (see
    // `by_name_finds_flat_md_in_home_tier_when_project_dir_misses`'s comment).
    let _home_guard = crate::test_env::HOME_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let primary_tmp = tempfile::tempdir().expect("primary temp dir");
    let home_tmp = tempfile::tempdir().expect("home temp dir");
    let home_agents = home_tmp.path().join(".trusty-agents").join("agents");
    std::fs::create_dir_all(&home_agents).expect("mkdir home agents");

    let unique_name = format!("shadow-agent-{}", std::process::id());
    std::fs::write(
        primary_tmp.path().join(format!("{unique_name}.md")),
        format!("---\nname: {unique_name}\nrole: agent\n---\nPRIMARY TIER PROSE"),
    )
    .expect("write primary overlay");
    std::fs::write(
        home_agents.join(format!("{unique_name}.md")),
        format!("---\nname: {unique_name}\nrole: agent\n---\nHOME TIER PROSE"),
    )
    .expect("write home overlay");

    clear_model_env(&unique_name);
    let prev_home = std::env::var_os("HOME");
    // SAFETY: guarded by ENV_LOCK.
    unsafe {
        std::env::set_var("TAGENT_CONFIG_DIR", primary_tmp.path());
        std::env::set_var("HOME", home_tmp.path());
    }
    let result = AgentConfig::by_name(&unique_name);
    // SAFETY: guarded by ENV_LOCK.
    unsafe {
        std::env::remove_var("TAGENT_CONFIG_DIR");
        match &prev_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
    }

    let cfg = result.expect("by_name must resolve when both tiers have the name");
    assert_eq!(cfg.system_prompt.content, "PRIMARY TIER PROSE");
}

/// Async counterpart of `same_name_project_local_shadows_home_tier`.
#[tokio::test]
// Why: see `by_name_async_finds_flat_md_in_home_tier_when_project_dir_misses`
// — `HOME_LOCK` is deliberately held across `.await` (established pattern
// also used by `api::server::tests`).
#[allow(clippy::await_holding_lock)]
async fn by_name_async_same_name_project_local_shadows_home_tier() {
    let _guard = ENV_LOCK.lock().await;
    // #3465-followup: also take the shared crate::test_env::HOME_LOCK across
    // the `.await` below (see the sync counterpart's comment).
    let _home_guard = crate::test_env::HOME_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let primary_tmp = tempfile::tempdir().expect("primary temp dir");
    let home_tmp = tempfile::tempdir().expect("home temp dir");
    let home_agents = home_tmp.path().join(".trusty-agents").join("agents");
    std::fs::create_dir_all(&home_agents).expect("mkdir home agents");

    let unique_name = format!("shadow-agent-async-{}", std::process::id());
    std::fs::write(
        primary_tmp.path().join(format!("{unique_name}.md")),
        format!("---\nname: {unique_name}\nrole: agent\n---\nPRIMARY TIER ASYNC PROSE"),
    )
    .expect("write primary overlay");
    std::fs::write(
        home_agents.join(format!("{unique_name}.md")),
        format!("---\nname: {unique_name}\nrole: agent\n---\nHOME TIER ASYNC PROSE"),
    )
    .expect("write home overlay");

    clear_model_env(&unique_name);
    let prev_home = std::env::var_os("HOME");
    // SAFETY: guarded by ENV_LOCK across the await below.
    unsafe {
        std::env::set_var("TAGENT_CONFIG_DIR", primary_tmp.path());
        std::env::set_var("HOME", home_tmp.path());
    }
    let result = AgentConfig::by_name_async(&unique_name).await;
    // SAFETY: guarded by ENV_LOCK.
    unsafe {
        std::env::remove_var("TAGENT_CONFIG_DIR");
        match &prev_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
    }

    let cfg = result.expect("by_name_async must resolve when both tiers have the name");
    assert_eq!(cfg.system_prompt.content, "PRIMARY TIER ASYNC PROSE");
}

/// #3198 code-critic HIGH fix regression: when a directory-package agent
/// resolves from the `$HOME` fallback tier (not the primary directory) and
/// its `extends` fails to resolve, the flat `<name>.toml` shadow-rescue
/// (`extends_shadow_fallback`) must search that SAME tier rather than only
/// the (empty) primary directory. Before the fix this hard-failed even
/// though a valid flat shadow sat right next to the package in `$HOME`.
#[test]
fn extends_shadow_fallback_searches_home_tier_when_package_resolved_there() {
    let _guard = ENV_LOCK.blocking_lock();
    // #3465-followup: also take the shared crate::test_env::HOME_LOCK (see
    // `by_name_finds_flat_md_in_home_tier_when_project_dir_misses`'s comment).
    let _home_guard = crate::test_env::HOME_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let primary_tmp = tempfile::tempdir().expect("primary temp dir");
    let home_tmp = tempfile::tempdir().expect("home temp dir");
    let home_agents = home_tmp.path().join(".trusty-agents").join("agents");
    std::fs::create_dir_all(&home_agents).expect("mkdir home agents");

    let unique_name = format!("home-shadow-agent-{}", std::process::id());

    // Package with an UNRESOLVABLE extends and NO [tools], living ONLY in
    // the $HOME tier.
    let pkg = home_agents.join(&unique_name);
    std::fs::create_dir(&pkg).expect("mk pkg");
    std::fs::write(
        pkg.join("agent.toml"),
        format!(
            "\n[agent]\nname = \"{unique_name}\"\nrole = \"assistant\"\n\
             model = \"anthropic/claude-sonnet-4-6\"\ndescription = \"partial package\"\n\
             extends = \"ghost-base\"\n\n[llm]\ntemperature = 0.0\nmax_tokens = 1024\n"
        ),
    )
    .expect("write pkg agent.toml");
    std::fs::write(pkg.join("persona.md"), "PARTIAL PACKAGE").expect("write persona");

    // Complete flat <name>.toml alongside it, in the SAME $HOME tier.
    std::fs::write(
        home_agents.join(format!("{unique_name}.toml")),
        format!(
            "\n[agent]\nname = \"{unique_name}\"\nrole = \"assistant\"\n\
             model = \"anthropic/claude-sonnet-4-6\"\ndescription = \"complete flat\"\n\n\
             [llm]\ntemperature = 0.0\nmax_tokens = 1024\n\n\
             [system_prompt]\ncontent = \"HOME SHADOW FLAT\"\n\n\
             [tools]\nallowed = [\"locked_tool\"]\n"
        ),
    )
    .expect("write flat toml");

    clear_model_env(&unique_name);
    let prev_home = std::env::var_os("HOME");
    // SAFETY: guarded by ENV_LOCK.
    unsafe {
        // Primary tier is a fresh, empty tempdir — the package/flat pair
        // exists ONLY under the $HOME tier.
        std::env::set_var("TAGENT_CONFIG_DIR", primary_tmp.path());
        std::env::set_var("HOME", home_tmp.path());
    }
    let result = AgentConfig::by_name(&unique_name);
    // SAFETY: guarded by ENV_LOCK.
    unsafe {
        std::env::remove_var("TAGENT_CONFIG_DIR");
        match &prev_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
    }

    let cfg = result.expect("shadow fallback must search the $HOME tier, not just primary");
    assert_eq!(cfg.system_prompt.content, "HOME SHADOW FLAT");
    assert_eq!(
        cfg.tools.allowed.as_deref(),
        Some(&["locked_tool".to_string()][..])
    );
}

// --- #3358: Assistant + persona agents off the claude-code runner --------
//
// Why: The Assistant MVP (epic #3052) staged its move off the claude-code
// CLI runner to the conversational Assistant/PM/persona agents first,
// leaving specialist/task agents (`engineer`, `qa-agent`, …, and especially
// `claude-code-engineer` — deliberately claude-code) untouched. These tests
// pin the resulting `runner`/`model` shape on the bundled configs so a
// future edit can't silently reintroduce `runner = "claude-code"` on this
// agent set, and so the Opus-for-the-base-Assistant /
// Sonnet-for-every-other-persona testing default stays correct.
// What: `bundled_persona_agents_do_not_use_claude_code_runner` loads each
// in-scope agent by name (resolving through `AgentConfig::by_name`, which
// prefers a directory package over a same-named flat file — #482) and
// asserts `runner != ClaudeCode` and the expected model. A second test
// loads the two flat files that are shadowed by a directory package
// (`cto-assistant.toml`, `izzie.toml`) directly by path, since `by_name`
// would never reach them while the package is present — the flat `izzie`
// file additionally serves as the `extends` shadow-fallback safety net
// (see `AgentConfig::extends_shadow_fallback_in`), so it must stay
// consistent even though it isn't the primary load path today.
// Test: This module IS the test surface.

#[test]
fn bundled_persona_agents_do_not_use_claude_code_runner() {
    use crate::agents::RunnerKind;

    let _guard = ENV_LOCK.blocking_lock();
    // (agent name, expected model) — Opus for the base Assistant AND `pm`
    // (the GUI's DEFAULT no-roster-selection chat backend — see
    // `resolve_agent_config` in `src/ctrl/config.rs`, which prefers project
    // `pm.toml` over `ctrl.toml`), Sonnet for every other in-scope persona
    // (owner directive, #3358; `pm` bumped to Opus per the owner's ruling on
    // the code-critic WARN on PR #3360 — the default chat must also get the
    // Opus testing default, not just an explicit `assistant` roster pick).
    let cases = [
        ("assistant", "claude-opus-4-6"),
        ("pm", "claude-opus-4-6"),
        ("personal-assistant", "claude-sonnet-4-6"),
        ("cto-assistant", "claude-sonnet-4-6"),
        ("izzie", "claude-sonnet-4-6"),
    ];
    for (name, expected_model) in cases {
        clear_model_env(name);
        // SAFETY: guarded by ENV_LOCK.
        unsafe {
            std::env::set_var("TAGENT_CONFIG_DIR", bundled_agents_dir());
        }
        let cfg = AgentConfig::by_name(name);
        // SAFETY: guarded by ENV_LOCK.
        unsafe {
            std::env::remove_var("TAGENT_CONFIG_DIR");
        }
        let cfg = cfg.unwrap_or_else(|e| panic!("bundled agent '{name}' must load: {e}"));

        assert_ne!(
            cfg.agent.runner,
            RunnerKind::ClaudeCode,
            "bundled agent '{name}' must not declare runner = \"claude-code\" (#3358)"
        );
        assert_eq!(
            cfg.agent.model, expected_model,
            "bundled agent '{name}' model mismatch"
        );
    }
}

#[test]
fn bundled_persona_shadowed_flat_duplicates_do_not_use_claude_code_runner() {
    use crate::agents::RunnerKind;

    // `cto-assistant.toml` and `izzie.toml` are shadowed by their same-named
    // directory package (`cto-assistant/agent.toml`, `izzie/agent.toml`) and
    // are never reached via `by_name` while the package is present — load
    // them directly by path so this hygiene guard actually exercises them.
    for name in ["cto-assistant", "izzie"] {
        let path = bundled_agents_dir().join(format!("{name}.toml"));
        let cfg = AgentConfig::load(&path)
            .unwrap_or_else(|e| panic!("shadowed flat agent '{name}.toml' must still parse: {e}"));

        assert_ne!(
            cfg.agent.runner,
            RunnerKind::ClaudeCode,
            "shadowed flat agent '{name}.toml' must not declare runner = \"claude-code\" (#3358)"
        );
        assert_eq!(
            cfg.agent.model, "claude-sonnet-4-6",
            "shadowed flat agent '{name}.toml' model mismatch"
        );
    }
}

/// #3358 code-critic MEDIUM: the two tests above assert over a hardcoded
/// name list, so a brand-new persona added later with `runner =
/// "claude-code"` would silently evade both guards. This test instead
/// walks every physical TOML file under `bundled_agents_dir()` — every
/// flat `<name>.toml` AND every directory package's `agent.toml`,
/// independently, not deduplicated by `by_name` — and asserts that any
/// agent NOT on the explicit `claude-code`-eligible allow-list (the
/// specialist/task agents, which stay out of scope for #3358, plus
/// `claude-code-engineer` which is deliberately claude-code) does not
/// declare `runner = "claude-code"`. A future addition — a new persona
/// TOML, or a new directory package — is caught automatically without
/// needing a matching new assertion here.
/// Test: This module IS the test surface.
#[test]
fn all_bundled_agent_tomls_outside_the_specialist_allowlist_avoid_claude_code_runner() {
    use crate::agents::RunnerKind;
    use std::collections::HashSet;

    // Loading each package goes through `resolve_model`, which reads
    // process-global `TAGENT_MODEL_*` / `TAGENT_DEFAULT_MODEL` env vars —
    // guard against concurrent mutation by the env-mutating tests in this
    // file, same as every other test here (this test doesn't itself mutate
    // env, but reads must still be serialized against writers).
    let _guard = ENV_LOCK.blocking_lock();

    // Specialist/task agents (staged OUT of scope for #3358) plus
    // `claude-code-engineer` (deliberately claude-code, never migrate).
    // Every other bundled agent TOML must not declare
    // `runner = "claude-code"`.
    let claude_code_eligible: HashSet<&str> = [
        "analysis-agent",
        "bedrock-engineer",
        "claude-code-engineer",
        "code-agent",
        "docs-agent",
        "engineer",
        "gpt-engineer",
        "gpt5-codex-engineer",
        "local-ops-agent",
        "observe-agent",
        "plan-agent",
        "postmortem-agent",
        "python-engineer",
        "qa-agent",
        "research-agent",
        "ticketing-agent",
    ]
    .into_iter()
    .collect();

    let dir = bundled_agents_dir();
    let mut checked = 0usize;
    for entry in std::fs::read_dir(&dir).expect("read bundled agents dir") {
        let entry = entry.expect("dir entry");
        let path = entry.path();
        let file_type = entry.file_type().expect("file type");

        // Directory-package agent: `<name>/agent.toml` + `persona.md`.
        // `AgentConfig::load` can't parse a package's `agent.toml` in
        // isolation (its system-prompt content lives in the sibling
        // `persona.md`, merged only by the private `load_agent_package`
        // helper) — go through the public `by_name_in`, scoped to just
        // this one directory so it can't wander into $HOME, which resolves
        // packages correctly (#482). Non-agent directories under
        // `.trusty-agents/agents/` (there are none today, but be
        // defensive) are skipped by requiring `agent.toml` to exist.
        let (cfg, display_path) = if file_type.is_dir() {
            let candidate = path.join("agent.toml");
            if !candidate.is_file() {
                continue;
            }
            let dir_name = entry
                .file_name()
                .to_str()
                .expect("bundled agent dir name is valid UTF-8")
                .to_string();
            let cfg = AgentConfig::by_name_in(std::slice::from_ref(&dir), &dir_name)
                .unwrap_or_else(|e| {
                    panic!("bundled package {} must load: {e}", candidate.display())
                });
            (cfg, candidate)
        } else if path.extension().is_some_and(|ext| ext == "toml") {
            let cfg = AgentConfig::load(&path)
                .unwrap_or_else(|e| panic!("bundled TOML {} must parse: {e}", path.display()));
            (cfg, path.clone())
        } else {
            continue;
        };
        checked += 1;

        if claude_code_eligible.contains(cfg.agent.name.as_str()) {
            continue;
        }
        assert_ne!(
            cfg.agent.runner,
            RunnerKind::ClaudeCode,
            "bundled agent '{}' ({}) declares runner = \"claude-code\" but is not on the \
             claude-code-eligible allow-list in this test — either it's a new specialist \
             that needs adding to the allow-list, or it's a persona that must be migrated \
             per #3358",
            cfg.agent.name,
            display_path.display()
        );
    }
    assert!(
        checked >= 20,
        "expected to scan at least 20 bundled agent TOMLs under {}, only found {checked} — \
         bundled_agents_dir() may be resolving the wrong path",
        dir.display()
    );
}

/// #3555 delegate-resolve follow-up: `agent_name_resolves` is the shared
/// predicate `delegate_to_agent`'s pre-flight validation now uses instead of
/// a single hand-rolled directory. These tests pin its three resolution
/// tiers (directory package / flat toml / flat md) plus the negative cases,
/// independent of any `AgentConfig` parsing so they don't need `ENV_LOCK`.
mod agent_name_resolves_tests {
    use crate::agents::{AgentConfig, agent_name_resolves};

    #[test]
    fn finds_flat_toml_in_secondary_dir() {
        let empty_primary = tempfile::tempdir().unwrap();
        let secondary = tempfile::tempdir().unwrap();
        std::fs::write(
            secondary.path().join("engineer.toml"),
            "[agent]\nname = \"engineer\"\n",
        )
        .unwrap();

        let dirs = vec![
            empty_primary.path().to_path_buf(),
            secondary.path().to_path_buf(),
        ];
        assert!(
            agent_name_resolves(&dirs, "engineer"),
            "must find engineer.toml in the second candidate directory"
        );
    }

    #[test]
    fn finds_directory_package() {
        // A real directory-package agent needs BOTH agent.toml AND
        // persona.md (`load_agent_package` reads both, `?`-propagating if
        // either is missing) — write both so this positive case matches the
        // real resolver's requirements exactly.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("assistant")).unwrap();
        std::fs::write(
            dir.path().join("assistant").join("agent.toml"),
            "[agent]\nname = \"assistant\"\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("assistant").join("persona.md"),
            "persona body",
        )
        .unwrap();

        let dirs = vec![dir.path().to_path_buf()];
        assert!(agent_name_resolves(&dirs, "assistant"));
    }

    #[test]
    fn finds_flat_md() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("izzie.md"),
            "---\nname: izzie\nrole: assistant\n---\nbody",
        )
        .unwrap();

        let dirs = vec![dir.path().to_path_buf()];
        assert!(agent_name_resolves(&dirs, "izzie"));
    }

    #[test]
    fn false_when_absent_everywhere() {
        let primary = tempfile::tempdir().unwrap();
        let secondary = tempfile::tempdir().unwrap();
        let dirs = vec![primary.path().to_path_buf(), secondary.path().to_path_buf()];
        assert!(!agent_name_resolves(&dirs, "nonexistent-agent"));
    }

    #[test]
    fn false_for_traversal_name() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("secret.toml"), "[agent]\nname = \"x\"\n").unwrap();
        let dirs = vec![dir.path().to_path_buf()];
        assert!(!agent_name_resolves(&dirs, "../secret"));
        assert!(!agent_name_resolves(&dirs, "..\\secret"));
    }

    #[test]
    fn false_on_empty_dirs() {
        assert!(!agent_name_resolves(&[], "engineer"));
    }

    /// #3555 MEDIUM follow-up (code-critic): a directory `<name>/` present
    /// WITHOUT `agent.toml` inside it makes the REAL resolver
    /// (`AgentConfig::by_name_in`, via `load_agent_package`'s `?`)
    /// hard-abort the ENTIRE search the moment it hits that directory — it
    /// does NOT fall through to a flat `<name>.toml` in a later directory.
    /// `agent_name_resolves` must mirror that short-circuit exactly, or
    /// validation would accept a name the actual spawn then hard-errors on.
    /// This test proves BOTH sides agree in the same scenario.
    #[test]
    fn short_circuits_on_malformed_directory_package_matching_real_resolver() {
        let malformed_primary = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(malformed_primary.path().join("engineer")).unwrap();
        // Deliberately no agent.toml (or persona.md) inside engineer/.

        let secondary_with_valid_flat = tempfile::tempdir().unwrap();
        std::fs::write(
            secondary_with_valid_flat.path().join("engineer.toml"),
            "[agent]\nname = \"engineer\"\n",
        )
        .unwrap();

        let dirs = vec![
            malformed_primary.path().to_path_buf(),
            secondary_with_valid_flat.path().to_path_buf(),
        ];

        assert!(
            !agent_name_resolves(&dirs, "engineer"),
            "must short-circuit to false, matching the real resolver's hard-abort, \
             even though a later dir has a valid flat engineer.toml"
        );

        // Sanity: the real resolver actually agrees — it hard-errors here
        // too, rather than silently falling through to the flat file.
        assert!(
            AgentConfig::by_name_in(&dirs, "engineer").is_err(),
            "sanity: the real resolver must also hard-abort in this scenario"
        );
    }
}
