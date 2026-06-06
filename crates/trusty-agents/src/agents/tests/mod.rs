//! Config-shape parsing tests for `AgentConfig` and its nested blocks.
//!
//! Why: Pins the TOML schema (field defaults, `[compress]`/`[session]`/
//! `[tools]`/`[rbac]`/`[runner_config]`/`[plugins]` blocks, `RunnerKind`
//! variants) so refactors to the config data shapes can't silently change
//! parse behavior. Model-resolution + on-disk loader tests live in `loading`.
//! What: Pure `toml::from_str` round-trips asserting parsed values.
//! Test: This module IS the test surface.

mod loading;
mod params;

use crate::agents::AgentConfig;

#[test]
fn llm_params_parses_model_override() {
    let toml_str = r#"
[agent]
name = "x"
role = "x"
model = "toml/agent"
description = "x"

[llm]
temperature = 0.0
max_tokens = 1024
model_override = "toml/override"

[system_prompt]
content = "base"
"#;
    let cfg: AgentConfig = toml::from_str(toml_str).expect("parses");
    assert_eq!(cfg.llm.model_override.as_deref(), Some("toml/override"));
}

#[test]
fn compress_config_defaults_enabled() {
    // When no [compress] section is present, the defaults enable compression
    // so all agents benefit from NLP compression without explicit opt-in.
    // compress_task remains false (aggressive task-text compression stays opt-in).
    let toml_str = r#"
[agent]
name = "x"
role = "x"
model = "x"
description = "x"

[llm]
temperature = 0.0
max_tokens = 1024

[system_prompt]
content = "base"
"#;
    let cfg: AgentConfig = toml::from_str(toml_str).expect("parses");
    assert!(cfg.compress.enabled);
    assert_eq!(cfg.compress.token_budget, 32_000);
    assert!(!cfg.compress.compress_task);
}

#[test]
fn compress_config_passthrough_when_disabled() {
    // Explicit enabled = false must disable the pipeline (opt-out path).
    let toml_str = r#"
[agent]
name = "x"
role = "x"
model = "x"
description = "x"

[llm]
temperature = 0.0
max_tokens = 1024

[system_prompt]
content = "base"

[compress]
enabled = false
"#;
    let cfg: AgentConfig = toml::from_str(toml_str).expect("parses");
    assert!(!cfg.compress.enabled);
    assert_eq!(cfg.compress.token_budget, 32_000);
    assert!(!cfg.compress.compress_task);
}

#[test]
fn compress_config_parses_block() {
    // Explicit [compress] block must populate fields.
    let toml_str = r#"
[agent]
name = "x"
role = "x"
model = "x"
description = "x"

[llm]
temperature = 0.0
max_tokens = 1024

[system_prompt]
content = "base"

[compress]
enabled = true
token_budget = 12000
compress_task = true
"#;
    let cfg: AgentConfig = toml::from_str(toml_str).expect("parses");
    assert!(cfg.compress.enabled);
    assert_eq!(cfg.compress.token_budget, 12000);
    assert!(cfg.compress.compress_task);
}

#[test]
fn session_config_defaults_disabled() {
    let toml_str = r#"
[agent]
name = "x"
role = "x"
model = "x"
description = "x"

[llm]
temperature = 0.0
max_tokens = 1024

[system_prompt]
content = "base"
"#;
    let cfg: AgentConfig = toml::from_str(toml_str).expect("parses");
    assert!(!cfg.session.enabled);
    assert_eq!(cfg.session.compression_threshold, 40);
    assert_eq!(cfg.session.keep_recent_turns, 10);
    assert!(cfg.session.compression_model.is_none());
}

#[test]
fn session_config_parses_block() {
    let toml_str = r#"
[agent]
name = "x"
role = "x"
model = "x"
description = "x"

[llm]
temperature = 0.0
max_tokens = 1024

[system_prompt]
content = "base"

[session]
enabled = true
compression_threshold = 60
keep_recent_turns = 12
compression_model = "claude-haiku-4-5"
"#;
    let cfg: AgentConfig = toml::from_str(toml_str).expect("parses");
    assert!(cfg.session.enabled);
    assert_eq!(cfg.session.compression_threshold, 60);
    assert_eq!(cfg.session.keep_recent_turns, 12);
    assert_eq!(
        cfg.session.compression_model.as_deref(),
        Some("claude-haiku-4-5")
    );
}

#[test]
fn tools_config_parses_allowed() {
    let toml_str = r#"
[agent]
name = "x"
role = "x"
model = "x"
description = "x"

[llm]
temperature = 0.0
max_tokens = 1024

[system_prompt]
content = "base"

[tools]
allowed = ["web_search", "fetch_url"]
"#;
    let cfg: AgentConfig = toml::from_str(toml_str).expect("parses");
    let list = cfg.tools.allowed.expect("allowed present");
    assert_eq!(
        list,
        vec!["web_search".to_string(), "fetch_url".to_string()]
    );
}

#[test]
fn rbac_config_defaults_unrestricted() {
    // No [rbac] block -> default config -> both effective tiers are All.
    let toml_str = r#"
[agent]
name = "x"
role = "x"
model = "x"
description = "x"

[llm]
temperature = 0.0
max_tokens = 1024

[system_prompt]
content = "base"
"#;
    let cfg: AgentConfig = toml::from_str(toml_str).expect("parses");
    assert_eq!(
        cfg.rbac.effective_default_tier(),
        crate::rbac::ServiceTier::All
    );
    assert_eq!(
        cfg.rbac.effective_unauthenticated_tier(),
        crate::rbac::ServiceTier::All
    );
    assert!(cfg.rbac.allowed_users_env.is_none());
}

#[test]
fn rbac_config_parses_block() {
    let toml_str = r#"
[agent]
name = "x"
role = "x"
model = "x"
description = "x"

[llm]
temperature = 0.0
max_tokens = 1024

[system_prompt]
content = "base"

[rbac]
allowed_users_env = "BOT_ALLOWED_USERS"
default_tier = "all"
unauthenticated_tier = "read_only"
"#;
    let cfg: AgentConfig = toml::from_str(toml_str).expect("parses");
    assert_eq!(
        cfg.rbac.allowed_users_env.as_deref(),
        Some("BOT_ALLOWED_USERS")
    );
    assert_eq!(
        cfg.rbac.effective_default_tier(),
        crate::rbac::ServiceTier::All
    );
    assert_eq!(
        cfg.rbac.effective_unauthenticated_tier(),
        crate::rbac::ServiceTier::ReadOnly
    );
}

#[test]
fn tools_config_parses_ast_native_shorthand() {
    // #347: `[tools] ast_native = true` shorthand resolves through
    // `effective_ast_native()`.
    let toml_str = r#"
[agent]
name = "x"
role = "x"
model = "x"
description = "x"

[llm]
temperature = 0.0
max_tokens = 1024

[system_prompt]
content = "base"

[tools]
ast_native = true
"#;
    let cfg: AgentConfig = toml::from_str(toml_str).expect("parses");
    assert!(cfg.tools.effective_ast_native());
}

#[test]
fn tools_config_parses_ast_native_nested() {
    // #347: `[tools.native] ast_native = true` is the long form.
    let toml_str = r#"
[agent]
name = "x"
role = "x"
model = "x"
description = "x"

[llm]
temperature = 0.0
max_tokens = 1024

[system_prompt]
content = "base"

[tools.native]
ast_native = true
"#;
    let cfg: AgentConfig = toml::from_str(toml_str).expect("parses");
    assert!(cfg.tools.effective_ast_native());
}

#[test]
fn tools_config_parses_allow_globs() {
    // `[tools] allow = [...]` (#255) — glob patterns for persona agents.
    let toml_str = r#"
[agent]
name = "x"
role = "x"
model = "x"
description = "x"

[llm]
temperature = 0.0
max_tokens = 1024

[system_prompt]
content = "base"

[tools]
allow = ["mcp_*", "git_log", "git_status"]
"#;
    let cfg: AgentConfig = toml::from_str(toml_str).expect("parses");
    let list = cfg.tools.allow.expect("allow present");
    assert_eq!(
        list,
        vec![
            "mcp_*".to_string(),
            "git_log".to_string(),
            "git_status".to_string(),
        ]
    );
    // `allowed` (legacy exact-match) is independent of `allow` (globs).
    assert!(cfg.tools.allowed.is_none());
}

#[test]
fn skills_section_is_ignored_gracefully() {
    // MIN-8 (#105): The `[skills]` section was removed because it was
    // never consumed. Existing TOMLs in the wild may still contain the
    // section; serde should silently tolerate it (we don't set
    // `deny_unknown_fields` on AgentConfig) so agents keep loading until
    // operators clean up their configs.
    let toml_str = r#"
[agent]
name = "x"
role = "x"
model = "x"
description = "x"

[llm]
temperature = 0.0
max_tokens = 1024

[system_prompt]
content = "base"

[skills]
auto_load = true
max_auto = 2
"#;
    let cfg: AgentConfig = toml::from_str(toml_str).expect("tolerates legacy [skills]");
    assert_eq!(cfg.agent.name, "x");
}
