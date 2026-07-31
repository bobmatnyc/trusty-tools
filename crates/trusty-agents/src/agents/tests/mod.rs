//! Config-shape parsing tests for `AgentConfig` and its nested blocks.
//!
//! Why: Pins the TOML schema (field defaults, `[compress]`/`[session]`/
//! `[tools]`/`[rbac]`/`[runner_config]`/`[plugins]` blocks, `RunnerKind`
//! variants) so refactors to the config data shapes can't silently change
//! parse behavior. Model-resolution + on-disk loader tests live in `loading`.
//! What: Pure `toml::from_str` round-trips asserting parsed values.
//! Test: This module IS the test surface.

pub(crate) mod loading;
mod params;

use crate::agents::{AgentConfig, AgentInfo, ToolsConfig};

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
fn workstream_context_config_defaults() {
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
    assert!(cfg.workstreams.enabled);
    assert_eq!(cfg.workstreams.summarize_every, 5);
    assert_eq!(cfg.workstreams.recent_window, 12);
}

#[test]
fn workstream_context_config_parses_block() {
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

[workstreams]
enabled = false
summarize_every = 3
recent_window = 20
"#;
    let cfg: AgentConfig = toml::from_str(toml_str).expect("parses");
    assert!(!cfg.workstreams.enabled);
    assert_eq!(cfg.workstreams.summarize_every, 3);
    assert_eq!(cfg.workstreams.recent_window, 20);
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

/// #3232: `[tools] search_indexes = [...]` — the tier-2 attached-index list
/// (epic #4007). Pinned alongside `allow`/`scopes` because all three are
/// independent capability axes: declaring one must never imply another.
#[test]
fn tools_config_parses_search_indexes() {
    let toml_str = r#"
[agent]
name = "cto-assistant"
role = "x"
model = "x"
description = "x"

[llm]
temperature = 0.0
max_tokens = 1024

[system_prompt]
content = "base"

[tools]
allow = ["vector_search"]
search_indexes = ["apex", "cto-projects"]
"#;
    let cfg: AgentConfig = toml::from_str(toml_str).expect("parses");
    assert_eq!(
        cfg.tools.resolved_search_indexes(),
        vec!["apex".to_string(), "cto-projects".to_string()]
    );
    // Independent axes: attaching indexes neither widens `allow` nor claims
    // an OpenRPC scope.
    assert_eq!(
        cfg.tools.allow.as_deref(),
        Some(&["vector_search".to_string()][..])
    );
    assert!(cfg.tools.scopes.is_none());
}

/// #3232/#4009 DEFAULT-STATE PIN: an agent that declares neither key must
/// come out of parsing in exactly the pre-#3232 state — no attached indexes,
/// enforcement OFF. This is the config-layer half of the "default off =
/// today's behaviour" guarantee (its runtime half is
/// `vector_search_default_is_unenforced`).
#[test]
fn tools_config_without_search_indexes_is_unattached_and_unenforced() {
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
allow = ["vector_search"]
"#;
    let cfg: AgentConfig = toml::from_str(toml_str).expect("parses");
    assert!(cfg.tools.search_indexes.is_none());
    assert!(cfg.tools.resolved_search_indexes().is_empty());
    assert_eq!(cfg.tools.enforce_search_indexes, None);
    assert!(!cfg.tools.search_indexes_enforced());
    // An agent with NO `[tools]` table at all must land in the same state.
    assert!(!ToolsConfig::default().search_indexes_enforced());
    assert!(ToolsConfig::default().resolved_search_indexes().is_empty());
}

/// #4009: the opt-in enforcement knob parses and is readable through its
/// accessor. Owner decision (epic #4007 OQ-2): opt-in, default false.
#[test]
fn tools_config_parses_enforce_search_indexes() {
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
search_indexes = ["apex"]
enforce_search_indexes = true
"#;
    let cfg: AgentConfig = toml::from_str(toml_str).expect("parses");
    assert_eq!(cfg.tools.enforce_search_indexes, Some(true));
    assert!(cfg.tools.search_indexes_enforced());
}

/// #3232: `resolved_search_indexes` is the single normalization point — a
/// hand-edited file (or a deep `extends` chain re-declaring an id) must never
/// produce a blank index id, which would build a `/indexes//search` URL, nor
/// a duplicate the schema would list twice.
#[test]
fn resolved_search_indexes_normalizes_blanks_and_dupes() {
    let tools = ToolsConfig {
        search_indexes: Some(vec![
            "  apex  ".to_string(),
            "".to_string(),
            "   ".to_string(),
            "apex".to_string(),
            "cto-projects".to_string(),
        ]),
        ..Default::default()
    };
    assert_eq!(
        tools.resolved_search_indexes(),
        vec!["apex".to_string(), "cto-projects".to_string()],
        "trimmed, blank-dropped, first-occurrence-wins dedup, order preserved"
    );
}

#[test]
fn skills_config_parses_allow() {
    // `[skills] allow = [...]` (#3933) — capability grants by skill id, parsed
    // alongside `[tools].allow` rather than replacing it. The two are unioned
    // downstream by `skills::manifest::effective_tool_patterns`; here we only
    // pin that BOTH survive deserialization independently, since a `[skills]`
    // table that silently swallowed `[tools]` would be the migration hazard
    // DOC-57 §9.3 exists to prevent.
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
allow = ["git_log"]

[skills]
allow = ["mta-train-time", "handoff-protocol"]
"#;
    let cfg: AgentConfig = toml::from_str(toml_str).expect("parses");
    assert_eq!(
        cfg.skills.allow.expect("skills.allow present"),
        vec!["mta-train-time".to_string(), "handoff-protocol".to_string()]
    );
    assert_eq!(
        cfg.tools.allow.expect("tools.allow present"),
        vec!["git_log".to_string()]
    );
}

#[test]
fn skills_config_absent_is_none_not_empty() {
    // The distinction matters: `None` means "no skill grants declared", which
    // leaves `[tools].allow` as the sole source and keeps behaviour byte-
    // identical to pre-#3933. `Some(vec![])` would mean "grants no skills".
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
    assert!(cfg.skills.allow.is_none());
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

#[test]
fn subagents_config_defaults_empty() {
    // #4026 EMPTY-DEFAULT PIN: an agent TOML with no `[subagents]` section
    // grants NO cross-product reach. An absent section must never be read as
    // "all" — that would be a silent capability grant across a product
    // boundary, the exact failure mode the owner's OQ-7 fail-closed ruling
    // exists to prevent.
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
    assert!(cfg.subagents.allowed.is_none());
    assert!(
        crate::tools::subagent_allow::SubagentAllowSet::over(
            crate::tools::cross_product::NON_CODING_TARGETS,
            cfg.subagents.allowed.as_deref()
        )
        .is_empty(),
        "absent [subagents] must resolve to the EMPTY allow-set"
    );
}

#[test]
fn subagents_config_parses_allowed() {
    // #4026: `[subagents] allowed = [...]` parses alongside `[tools]`/`[skills]`
    // without disturbing either — the same independence `skills_config_parses_allow`
    // pins for the skills section.
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
allow = ["git_log"]

[subagents]
allowed = ["research", "ticketing"]
"#;
    let cfg: AgentConfig = toml::from_str(toml_str).expect("parses");
    assert_eq!(
        cfg.subagents.allowed.as_deref(),
        Some(&["research".to_string(), "ticketing".to_string()][..])
    );
    assert_eq!(
        cfg.tools.allow.as_deref(),
        Some(&["git_log".to_string()][..])
    );
}

/// ADR-0024 decision 4: `[subagents] delegate_allowed = [...]` parses, and is
/// kept SEPARATE from the cross-product `allowed` key.
///
/// Why: the two mechanisms take different names for different things
/// (`research` is a trusty-code specialist, `research-agent` a trusty-agents
/// roster entry). A single flattened key would make one config line mean two
/// capabilities, which is exactly what the API payload keeps apart.
/// What: both keys parse independently; declaring one leaves the other `None`.
/// Test: this function IS the test.
#[test]
fn subagents_config_parses_delegate_allowed() {
    let toml_str = r#"
[agent]
name = "seeded"
role = "assistant"
model = "m"
description = "d"

[llm]
temperature = 0.2
max_tokens = 1024

[system_prompt]
content = "base"

[subagents]
delegate_allowed = ["research-agent", "ticketing-agent"]
"#;
    let cfg: AgentConfig = toml::from_str(toml_str).expect("parses");
    assert_eq!(
        cfg.subagents.delegate_allowed,
        Some(vec![
            "research-agent".to_string(),
            "ticketing-agent".to_string()
        ])
    );
    assert!(
        cfg.subagents.allowed.is_none(),
        "the cross-product key must stay independent"
    );
}

// --- `AgentInfo::tier` (#4168, epic #4167 — L0/L1 orchestration model) ---
//
// Fail-closed contract: absent, blank, or unrecognized `[agent].tier` must
// NEVER resolve to `AgentTier::L0Orchestration` — only an explicit,
// recognized declaration does.

fn agent_toml_with_tier(tier_line: &str) -> String {
    format!(
        r#"
[agent]
name = "x"
role = "x"
model = "x"
description = "x"
{tier_line}

[llm]
temperature = 0.0
max_tokens = 1024

[system_prompt]
content = "base"
"#
    )
}

#[test]
fn agent_tier_defaults_to_l1_when_absent() {
    // No `tier` key at all — the state of every persona TOML shipped before
    // #4168, and the overwhelming majority going forward.
    let cfg: AgentConfig = toml::from_str(&agent_toml_with_tier("")).expect("parses");
    assert_eq!(cfg.agent.tier, None);
    assert_eq!(cfg.agent.tier(), crate::agents::AgentTier::L1Standard);
}

#[test]
fn agent_tier_parses_l0_orchestration_aliases() {
    for alias in ["l0", "L0", "orchestration", "Orchestration", "  l0  "] {
        let cfg: AgentConfig =
            toml::from_str(&agent_toml_with_tier(&format!(r#"tier = "{alias}""#))).expect("parses");
        assert_eq!(
            cfg.agent.tier(),
            crate::agents::AgentTier::L0Orchestration,
            "alias {alias:?} must resolve to L0Orchestration"
        );
    }
}

#[test]
fn agent_tier_parses_l1_standard_aliases() {
    for alias in ["l1", "L1", "standard", "Standard"] {
        let cfg: AgentConfig =
            toml::from_str(&agent_toml_with_tier(&format!(r#"tier = "{alias}""#))).expect("parses");
        assert_eq!(
            cfg.agent.tier(),
            crate::agents::AgentTier::L1Standard,
            "alias {alias:?} must resolve to L1Standard"
        );
    }
}

#[test]
fn agent_tier_unknown_value_fails_closed_to_l1() {
    // The core fail-closed requirement: a malformed/unrecognized value must
    // NEVER silently become the elevated tier.
    for bogus in ["orchestrator", "l2", "yolo", "L0RCHESTRATION", "true"] {
        let cfg: AgentConfig =
            toml::from_str(&agent_toml_with_tier(&format!(r#"tier = "{bogus}""#))).expect("parses");
        assert_eq!(
            cfg.agent.tier(),
            crate::agents::AgentTier::L1Standard,
            "unrecognized tier value {bogus:?} must fail closed to L1Standard, never L0"
        );
    }
}

#[test]
fn agent_tier_blank_value_fails_closed_to_l1() {
    let cfg: AgentConfig =
        toml::from_str(&agent_toml_with_tier(r#"tier = "   ""#)).expect("parses");
    assert_eq!(cfg.agent.tier(), crate::agents::AgentTier::L1Standard);
}

#[test]
fn agent_tier_default_trait_is_l1_standard() {
    // `AgentTier::default()` (used as the fail-closed default in
    // `DelegateToAgentTool::new`) must be `L1Standard`, not derived
    // accidentally onto the wrong variant by a future field reorder.
    assert_eq!(
        crate::agents::AgentTier::default(),
        crate::agents::AgentTier::L1Standard
    );
}

// #4029: `AgentTier::from_declared` extracted `AgentInfo::tier()`'s match so
// the `/api/agents/:name/subagents` route — which must report exactly the
// targets #4169's gate admits — shares ONE parser with the gate instead of
// hand-rolling a second one. These two tests exist to make that extraction a
// pinned equivalence rather than an assumed one.

#[test]
fn agent_tier_from_declared_matches_agent_info_tier() {
    // Every alias and every fail-closed case the tests above cover, asserted to
    // produce the IDENTICAL result through both entry points. If the two ever
    // diverge, the config surface starts advertising a target the delegation
    // gate refuses (or hiding one it allows).
    //
    // ADR-0024 decision 3 scopes this equivalence: it holds for a DECLARED
    // value always, and for an absent/blank value only when the role is not
    // the assistant kind (the fixture's `role = "x"`), because `AgentInfo::
    // tier()` now derives an absent value from kind while `from_declared`
    // stays a pure parser with no role in scope. The assistant-kind divergence
    // is pinned separately by `agent_tier_derives_l0_for_the_assistant_kind`.
    for raw in [
        None,
        Some(""),
        Some("   "),
        Some("l0"),
        Some("L0"),
        Some("  l0  "),
        Some("orchestration"),
        Some("Orchestration"),
        Some("l1"),
        Some("standard"),
        Some("orchestrator"),
        Some("l2"),
        Some("yolo"),
    ] {
        let via_info = AgentInfo {
            tier: raw.map(str::to_string),
            ..toml::from_str::<AgentConfig>(&agent_toml_with_tier(""))
                .expect("parses")
                .agent
        }
        .tier();
        assert_eq!(
            crate::agents::AgentTier::from_declared(raw),
            via_info,
            "from_declared({raw:?}) must equal AgentInfo::tier() for the same raw value"
        );
    }
}

#[test]
fn agent_tier_wire_label_round_trips_through_from_declared() {
    for tier in [
        crate::agents::AgentTier::L0Orchestration,
        crate::agents::AgentTier::L1Standard,
    ] {
        assert_eq!(
            crate::agents::AgentTier::from_declared(Some(tier.wire_label())),
            tier,
            "the wire label the API emits must parse back to the same tier"
        );
    }
    assert_eq!(
        crate::agents::AgentTier::L0Orchestration.wire_label(),
        "l0",
        "the SHORT alias is the wire value, matching what an operator writes"
    );
    assert_eq!(crate::agents::AgentTier::L1Standard.wire_label(), "l1");
}

// --- ADR-0024 decision 3: assistants are tier L0, derived from KIND -------
//
// Ratified by the owner 2026-07-28. The population #4168 shipped is inverted:
// L0 is no longer a rare, unpopulated orchestration tier above the assistants
// — it IS the assistant population, and L1 is the sub-agent population. These
// tests pin the derivation itself; the pane's rendering of it is pinned in
// `api::server::tests::agent_subagents`, and the delegation gate's KIND-first
// behaviour in `tools::delegate`'s tests.

/// Build a config with an explicit role and an optional tier line.
fn agent_toml_with_role_and_tier(role: &str, tier_line: &str) -> String {
    format!(
        r#"
[agent]
name = "x"
role = "{role}"
model = "x"
description = "x"
{tier_line}

[llm]
temperature = 0.0
max_tokens = 1024

[system_prompt]
content = "base"
"#
    )
}

#[test]
fn agent_tier_for_kind_is_l0_for_the_assistant_and_orchestrator_kinds() {
    // #4497: the derivation is now the ONE mechanism that answers "is this
    // agent L0?" for BOTH populations. `orchestrator`/`controller` (`pm`, and
    // a `ctrl` declaring the controller spelling) previously landed on L1
    // here and were handed L0 by a hardcoded literal at the dispatch call
    // site (`ctrl_delegate_posture`) — the second mechanism this rule
    // absorbed. They are still NOT assistants (see
    // `agents::delegation::assistant_and_orchestrator_kinds_are_disjoint`);
    // they share only the tier.
    for l0 in ["assistant", "orchestrator", "controller"] {
        assert_eq!(
            crate::agents::AgentTier::for_kind(l0),
            crate::agents::AgentTier::L0Orchestration,
            "role {l0:?} is an L0 kind"
        );
    }
    // Everything else — including the empty string and every near-miss
    // spelling — still lands on L1. The rule recognizes a NAMED set, so a new
    // specialist role can never fall into L0 by omission.
    for other in [
        "",
        "engineer",
        "qa",
        "researcher",
        "documentation",
        "ops",
        "planner",
        "ticketing",
        "analysis",
        "observer",
        "Assistant",
        "assistant-tier",
        "Orchestrator",
        "orchestration",
        "ctrl",
        "pm",
    ] {
        assert_eq!(
            crate::agents::AgentTier::for_kind(other),
            crate::agents::AgentTier::L1Standard,
            "role {other:?} must NOT derive the elevated tier"
        );
    }
}

#[test]
fn agent_tier_derives_l0_for_the_assistant_kind() {
    // The decision-3 case: an assistant-kind agent that declares no `tier` at
    // all — the state of every bundled assistant TOML — resolves L0.
    let cfg: AgentConfig =
        toml::from_str(&agent_toml_with_role_and_tier("assistant", "")).expect("parses");
    assert_eq!(cfg.agent.tier, None, "no literal is written to the file");
    assert_eq!(
        cfg.agent.tier(),
        crate::agents::AgentTier::L0Orchestration,
        "ADR-0024 decision 3: an assistant IS L0, with no per-file declaration"
    );
    // A blank declaration is "not declared", not "declared as something odd".
    let blank: AgentConfig = toml::from_str(&agent_toml_with_role_and_tier(
        "assistant",
        r#"tier = "   ""#,
    ))
    .expect("parses");
    assert_eq!(
        blank.agent.tier(),
        crate::agents::AgentTier::L0Orchestration
    );
}

#[test]
fn agent_tier_derives_l1_for_every_sub_agent_kind() {
    // The other half of decision 3: sub-agents stay L1. Asserted over the
    // whole `ASSISTANT_ALLOWED_DELEGATE_ROLES` sub-agent vocabulary rather
    // than one sample, so widening that constant without thinking about tier
    // fails here.
    for role in [
        "engineer",
        "qa",
        "researcher",
        "documentation",
        "ops",
        "planner",
    ] {
        let cfg: AgentConfig =
            toml::from_str(&agent_toml_with_role_and_tier(role, "")).expect("parses");
        assert_eq!(
            cfg.agent.tier(),
            crate::agents::AgentTier::L1Standard,
            "sub-agent role {role:?} must stay L1"
        );
    }
}

#[test]
fn agent_tier_explicit_declaration_overrides_the_derived_kind() {
    // The escape hatch, in BOTH directions, so the derivation is a default
    // and not a hard-coded identity: an assistant can be deliberately narrowed
    // to L1, and a non-assistant can be deliberately pinned to L0.
    let narrowed: AgentConfig = toml::from_str(&agent_toml_with_role_and_tier(
        "assistant",
        r#"tier = "l1""#,
    ))
    .expect("parses");
    assert_eq!(narrowed.agent.tier(), crate::agents::AgentTier::L1Standard);

    let pinned: AgentConfig = toml::from_str(&agent_toml_with_role_and_tier(
        "orchestrator",
        r#"tier = "l0""#,
    ))
    .expect("parses");
    assert_eq!(
        pinned.agent.tier(),
        crate::agents::AgentTier::L0Orchestration
    );

    // And the fail-closed rule survives the override: a typo on an assistant
    // NARROWS to L1 rather than being ignored or silently elevating.
    let typo: AgentConfig = toml::from_str(&agent_toml_with_role_and_tier(
        "assistant",
        r#"tier = "L0RCHESTRATION""#,
    ))
    .expect("parses");
    assert_eq!(
        typo.agent.tier(),
        crate::agents::AgentTier::L1Standard,
        "an unrecognized declaration can only ever narrow"
    );
}
