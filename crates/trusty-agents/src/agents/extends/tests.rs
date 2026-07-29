//! Unit + integration tests for `extends`-based agent personalization
//! (DOC-41 §2.5 / §2.5.1, issue #3055).
//!
//! Why: pins the §2.5 merge table — base-first prose concatenation, list
//! union with dedup, scalar child-override (including a child setting
//! `display_name` when the base has none), and the cycle / missing-parent /
//! depth failure modes — so a later refactor cannot silently regress
//! personalization semantics.
//! What: direct [`resolve`] / [`merge_extends`] unit tests over hand-built
//! configs, plus end-to-end `AgentRegistry::load` tests over on-disk `.md`
//! fixtures.
//! Test: this module IS the test surface.

use std::collections::HashMap;

use super::{AgentExtendsError, MAX_DEPTH, merge_extends, resolve};
use crate::agents::AgentConfig;
use crate::agents::RunnerKind;
use crate::agents::registry::AgentRegistry;

use tempfile::TempDir;

/// Parse a minimal agent config from TOML for direct merge/resolve tests.
fn cfg(toml: &str) -> AgentConfig {
    toml::from_str(toml).expect("valid test agent TOML")
}

/// Build a config declaring `extends = <base>` plus name/model/desc/body.
fn child(name: &str, extends: &str, body: &str) -> AgentConfig {
    cfg(&format!(
        r#"
[agent]
name = "{name}"
role = "agent"
model = ""
description = ""
extends = "{extends}"

[llm]
temperature = 0.0
max_tokens = 1024

[system_prompt]
content = "{body}"
"#
    ))
}

/// A case-folded name→config lookup closure over a fixed map.
fn lookup_map(configs: Vec<AgentConfig>) -> HashMap<String, AgentConfig> {
    configs
        .into_iter()
        .map(|c| (c.agent.name.to_lowercase(), c))
        .collect()
}

// --- merge_extends unit tests -------------------------------------------

#[test]
fn extends_prose_base_first() {
    let base = cfg(r#"
[agent]
name = "base"
role = "researcher"
model = "m1"
description = "base desc"
[llm]
temperature = 0.0
max_tokens = 1024
[system_prompt]
content = "BASE PROSE"
"#);
    let ch = child("child", "base", "CHILD PROSE");
    let merged = merge_extends(base, ch);
    // Parent prose first, child appended — NOT child-replaces-parent.
    assert_eq!(merged.system_prompt.content, "BASE PROSE\n\nCHILD PROSE");
}

#[test]
fn extends_prose_empty_sides() {
    let base = cfg(r#"
[agent]
name = "base"
role = "r"
model = "m1"
description = "d"
[llm]
temperature = 0.0
max_tokens = 1024
[system_prompt]
content = "ONLY BASE"
"#);
    // Child with empty body inherits just the base prose.
    let ch = child("child", "base", "");
    let merged = merge_extends(base, ch);
    assert_eq!(merged.system_prompt.content, "ONLY BASE");
}

#[test]
fn extends_scalar_child_override() {
    let base = cfg(r#"
[agent]
name = "base"
role = "researcher"
model = "base-model"
description = "base desc"
[llm]
temperature = 0.0
max_tokens = 1024
[system_prompt]
content = "b"
"#);
    // Child overrides model + description (non-empty), inherits role (child
    // role defaults to the neutral "agent") and keeps its own identity name.
    let ch = cfg(r#"
[agent]
name = "mine"
role = "agent"
model = "child-model"
description = "child desc"
extends = "base"
[llm]
temperature = 0.0
max_tokens = 1024
[system_prompt]
content = "c"
"#);
    let merged = merge_extends(base, ch);
    assert_eq!(merged.agent.name, "mine");
    assert_eq!(merged.agent.model, "child-model");
    assert_eq!(merged.agent.description, "child desc");
    // Child role was the neutral default → inherits base's meaningful role.
    assert_eq!(merged.agent.role, "researcher");
    // `extends` is consumed by the merge.
    assert!(merged.agent.extends.is_none());
}

// --- #3052 PR A follow-up: per-key `[llm]` temperature/max_tokens ---------

#[test]
fn extends_llm_child_overrides_temperature_only() {
    // Child declares ONLY `temperature`, omitting `max_tokens` entirely —
    // must override temperature and still inherit the base's max_tokens.
    let base = cfg(r#"
[agent]
name = "base"
role = "r"
model = "m"
description = "d"
[llm]
temperature = 0.7
max_tokens = 1024
[system_prompt]
content = "b"
"#);
    let ch = cfg(r#"
[agent]
name = "child"
role = "agent"
model = ""
description = ""
extends = "base"
[llm]
temperature = 0.3
[system_prompt]
content = "c"
"#);
    let merged = merge_extends(base, ch);
    assert_eq!(merged.llm.temperature, 0.3);
    assert_eq!(
        merged.llm.max_tokens, 1024,
        "omitted max_tokens must inherit the base's"
    );
}

#[test]
fn extends_llm_child_overrides_max_tokens_only() {
    // Symmetric case: child declares ONLY `max_tokens`, omitting `temperature`.
    let base = cfg(r#"
[agent]
name = "base"
role = "r"
model = "m"
description = "d"
[llm]
temperature = 0.7
max_tokens = 1024
[system_prompt]
content = "b"
"#);
    let ch = cfg(r#"
[agent]
name = "child"
role = "agent"
model = ""
description = ""
extends = "base"
[llm]
max_tokens = 4096
[system_prompt]
content = "c"
"#);
    let merged = merge_extends(base, ch);
    assert_eq!(
        merged.llm.temperature, 0.7,
        "omitted temperature must inherit the base's"
    );
    assert_eq!(merged.llm.max_tokens, 4096);
}

#[test]
fn extends_llm_child_inherits_when_omitted() {
    // Child declares an empty `[llm]` table (both fields omitted) — must
    // inherit BOTH temperature and max_tokens from the base unchanged. This
    // is the #469 regression guard in unit-test form: prior to the per-key
    // fix, this behavior was correct only by accident (wholesale-inherit);
    // now it is the explicit "child omits => inherit" path.
    let base = cfg(r#"
[agent]
name = "base"
role = "r"
model = "m"
description = "d"
[llm]
temperature = 0.3
max_tokens = 4096
[system_prompt]
content = "b"
"#);
    let ch = cfg(r#"
[agent]
name = "child"
role = "agent"
model = ""
description = ""
extends = "base"
[llm]
[system_prompt]
content = "c"
"#);
    let merged = merge_extends(base, ch);
    assert_eq!(merged.llm.temperature, 0.3);
    assert_eq!(merged.llm.max_tokens, 4096);
}

#[test]
fn extends_runner_and_persistent_session_child_override() {
    // #3106 MEDIUM: a child's non-default `runner` and opt-in
    // `persistent_session` must override the base (previously silently dropped).
    let base = cfg(r#"
[agent]
name = "base"
role = "r"
model = "m"
description = "d"
[llm]
temperature = 0.0
max_tokens = 1024
[system_prompt]
content = "b"
"#);
    let ch = cfg(r#"
[agent]
name = "child"
role = "agent"
model = ""
description = ""
extends = "base"
runner = "claude-code"
persistent_session = true
[llm]
temperature = 0.0
max_tokens = 1024
[system_prompt]
content = "c"
"#);
    let merged = merge_extends(base, ch);
    assert_eq!(merged.agent.runner, RunnerKind::ClaudeCode);
    assert!(merged.agent.persistent_session);
}

#[test]
fn extends_child_default_runner_inherits_base() {
    // A child that leaves `runner` at the default inherits the base's runner.
    let base = cfg(r#"
[agent]
name = "base"
role = "r"
model = "m"
description = "d"
runner = "claude-code"
[llm]
temperature = 0.0
max_tokens = 1024
[system_prompt]
content = "b"
"#);
    let ch = child("child", "base", "c"); // default runner (subprocess)
    let merged = merge_extends(base, ch);
    assert_eq!(merged.agent.runner, RunnerKind::ClaudeCode);
}

#[test]
fn extends_child_sets_display_name_over_none() {
    // Reverse case (still valid): when a base is nameless (display_name None),
    // a child MUST be able to set its own. The forward case — a nameless child
    // over the now-NAMED base `assistant` — is covered by
    // `extends_nameless_child_does_not_inherit_named_base_display` (#3738).
    let base = cfg(r#"
[agent]
name = "assistant"
role = "assistant"
model = "m"
description = "d"
[llm]
temperature = 0.0
max_tokens = 1024
[system_prompt]
content = "b"
"#);
    assert!(base.agent.display_name.is_none());
    let ch = cfg(r#"
[agent]
name = "izzie"
role = "agent"
model = ""
description = ""
extends = "assistant"
display_name = "Izzie"
[llm]
temperature = 0.0
max_tokens = 1024
[system_prompt]
content = "c"
"#);
    let merged = merge_extends(base, ch);
    assert_eq!(merged.agent.display_name.as_deref(), Some("Izzie"));
    // Base model inherited (child left it empty).
    assert_eq!(merged.agent.model, "m");
}

#[test]
fn extends_nameless_child_does_not_inherit_named_base_display() {
    // #3738: the base `assistant` is now the NAMED generic default
    // ("Assistant"). A child overlay that omits its own display_name must NOT
    // silently inherit that brand — it must fall back to its OWN name (via
    // `display_label`), so `/agent my-overlay` reads "my-overlay", never
    // "Assistant". This fixture matches PRODUCTION (base carries
    // Some("Assistant") + prompt_label "assistant"), unlike the older
    // `extends_child_sets_display_name_over_none` synthetic (base None).
    let base = cfg(r#"
[agent]
name = "assistant"
role = "assistant"
model = "m"
description = "d"
display_name = "Assistant"
prompt_label = "assistant"
[llm]
temperature = 0.0
max_tokens = 1024
[system_prompt]
content = "b"
"#);
    assert_eq!(base.agent.display_name.as_deref(), Some("Assistant"));
    let ch = cfg(r#"
[agent]
name = "my-overlay"
role = "agent"
model = ""
description = ""
extends = "assistant"
[llm]
temperature = 0.0
max_tokens = 1024
[system_prompt]
content = "c"
"#);
    assert!(ch.agent.display_name.is_none());
    let merged = merge_extends(base, ch);
    // Inherited brand cleared → falls back to the child's own identity.
    assert_eq!(
        merged.agent.display_name, None,
        "nameless overlay must not inherit the base's 'Assistant' brand"
    );
    assert_eq!(merged.agent.prompt_label, None);
    assert_eq!(
        merged.agent.display_label(),
        "my-overlay",
        "display_label must resolve to the child's own name, not 'Assistant'"
    );
}

#[test]
fn extends_tools_union_dedup() {
    let base = cfg(r#"
[agent]
name = "base"
role = "r"
model = "m"
description = "d"
[llm]
temperature = 0.0
max_tokens = 1024
[system_prompt]
content = "b"
[tools]
allowed = ["search", "read"]
scopes = ["memory.read"]
"#);
    let ch = cfg(r#"
[agent]
name = "child"
role = "agent"
model = ""
description = ""
extends = "base"
[llm]
temperature = 0.0
max_tokens = 1024
[system_prompt]
content = "c"
[tools]
allowed = ["read", "my_custom_tool"]
scopes = ["memory.read", "search.read"]
"#);
    let merged = merge_extends(base, ch);
    // Union, dedup, base-first order.
    assert_eq!(
        merged.tools.allowed.unwrap(),
        vec!["search", "read", "my_custom_tool"]
    );
    assert_eq!(
        merged.tools.scopes.unwrap(),
        vec!["memory.read", "search.read"]
    );
}

/// #3232: the ticket's own worked example — a base Assistant attaches its
/// project index; a CTO Assistant extending it unions in `cto-projects`
/// WITHOUT re-declaring the base's and without being able to silently drop
/// it. Same base-first-union-with-dedup contract as `allow`/`scopes`.
#[test]
fn extends_unions_search_indexes() {
    let base = cfg(r#"
[agent]
name = "assistant"
role = "r"
model = "m"
description = "d"
[llm]
temperature = 0.0
max_tokens = 1024
[system_prompt]
content = "b"
[tools]
search_indexes = ["trusty-tools", "shared-docs"]
"#);
    let ch = cfg(r#"
[agent]
name = "cto-assistant"
role = "agent"
model = ""
description = ""
extends = "assistant"
[llm]
temperature = 0.0
max_tokens = 1024
[system_prompt]
content = "c"
[tools]
search_indexes = ["shared-docs", "cto-projects", "apex"]
"#);
    let merged = merge_extends(base, ch);
    assert_eq!(
        merged.tools.resolved_search_indexes(),
        vec!["trusty-tools", "shared-docs", "cto-projects", "apex"],
        "base-first union, dedup — the overlay may only ADD reach"
    );
}

/// #3232: a child that declares nothing keeps its base's attached indexes;
/// a base that declares nothing does not erase the child's. Mirrors
/// `extends_union_none_cases` for the pre-existing list fields.
#[test]
fn extends_search_indexes_none_cases() {
    let mk = |name: &str, extends: &str, line: &str| {
        cfg(&format!(
            r#"
[agent]
name = "{name}"
role = "agent"
model = "m"
description = "d"
{extends}
[llm]
temperature = 0.0
max_tokens = 1024
[system_prompt]
content = "x"
[tools]
{line}
"#
        ))
    };
    // Child silent → inherits the base's list.
    let merged = merge_extends(
        mk("base", "", "search_indexes = [\"apex\"]"),
        mk("child", "extends = \"base\"", ""),
    );
    assert_eq!(merged.tools.resolved_search_indexes(), vec!["apex"]);
    // Base silent → the child's own list survives.
    let merged = merge_extends(
        mk("base", "", ""),
        mk("child", "extends = \"base\"", "search_indexes = [\"apex\"]"),
    );
    assert_eq!(merged.tools.resolved_search_indexes(), vec!["apex"]);
    // Neither declares → still absent (the pre-#3232 state).
    let merged = merge_extends(mk("base", "", ""), mk("child", "extends = \"base\"", ""));
    assert!(merged.tools.search_indexes.is_none());
    assert!(!merged.tools.search_indexes_enforced());
}

/// #4009: `enforce_search_indexes` is a POSTURE, not a list — so it does not
/// union. A child that declares it wins (may harden or relax); a child that
/// omits it inherits, so a hardened base cannot be silently un-hardened by
/// an overlay that simply never mentions the key.
#[test]
fn extends_child_overrides_enforce_search_indexes() {
    let mk = |name: &str, extends: &str, line: &str| {
        cfg(&format!(
            r#"
[agent]
name = "{name}"
role = "agent"
model = "m"
description = "d"
{extends}
[llm]
temperature = 0.0
max_tokens = 1024
[system_prompt]
content = "x"
[tools]
search_indexes = ["apex"]
{line}
"#
        ))
    };
    // Hardened base + silent child → stays hardened.
    let merged = merge_extends(
        mk("base", "", "enforce_search_indexes = true"),
        mk("child", "extends = \"base\"", ""),
    );
    assert!(merged.tools.search_indexes_enforced());
    // Hardened base + child explicitly relaxing → child wins.
    let merged = merge_extends(
        mk("base", "", "enforce_search_indexes = true"),
        mk(
            "child",
            "extends = \"base\"",
            "enforce_search_indexes = false",
        ),
    );
    assert!(!merged.tools.search_indexes_enforced());
    // Unenforced base + child hardening → child wins.
    let merged = merge_extends(
        mk("base", "", ""),
        mk(
            "child",
            "extends = \"base\"",
            "enforce_search_indexes = true",
        ),
    );
    assert!(merged.tools.search_indexes_enforced());
}

// --- `tier` merge (#4168, epic #4167 — L0/L1 orchestration model) ---------
//
// Same posture as `enforce_search_indexes` above: child-declares-wins,
// child-omits-inherits. This is a PRIVILEGE boundary though, so each
// direction (an L0 base's tier traveling to a silent child overlay, AND a
// child explicitly downgrading it) is pinned separately.

fn agent_with_tier(name: &str, extends: &str, tier_line: &str) -> AgentConfig {
    cfg(&format!(
        r#"
[agent]
name = "{name}"
role = "agent"
model = "m"
description = "d"
{extends}
{tier_line}
[llm]
temperature = 0.0
max_tokens = 1024
[system_prompt]
content = "x"
"#
    ))
}

#[test]
fn extends_tier_child_inherits_l0_from_base_when_omitted() {
    // An L0 base's tier travels to a child overlay that never redeclares
    // `tier` — the same "hardened base stays hardened under a silent
    // overlay" rule `enforce_search_indexes` follows, applied to a
    // privilege boundary instead of a search-index posture.
    let base = agent_with_tier("base", "", r#"tier = "orchestration""#);
    let child = agent_with_tier("child", "extends = \"base\"", "");
    let merged = merge_extends(base, child);
    assert_eq!(
        merged.agent.tier(),
        crate::agents::AgentTier::L0Orchestration,
        "a child overlay that never declares its own tier must inherit the \
         base's L0 tier, exactly like every other un-redeclared field"
    );
}

#[test]
fn extends_tier_child_can_downgrade_l0_base_to_l1() {
    // A child that explicitly declares its own tier always wins — including
    // downgrading an L0 base to L1.
    let base = agent_with_tier("base", "", r#"tier = "orchestration""#);
    let child = agent_with_tier("child", "extends = \"base\"", r#"tier = "l1""#);
    let merged = merge_extends(base, child);
    assert_eq!(merged.agent.tier(), crate::agents::AgentTier::L1Standard);
}

#[test]
fn extends_tier_omitted_everywhere_resolves_l1() {
    // Neither base nor child ever declares `tier` — the chain must resolve
    // to the fail-closed default, never accidentally L0.
    let base = agent_with_tier("base", "", "");
    let child = agent_with_tier("child", "extends = \"base\"", "");
    let merged = merge_extends(base, child);
    assert_eq!(merged.agent.tier(), crate::agents::AgentTier::L1Standard);
}

#[test]
fn extends_tier_child_can_declare_l0_over_l1_base() {
    // The reverse direction: an L1 (or tier-less) base, and a child that
    // explicitly opts INTO L0 — a deliberate, explicit declaration, which
    // is the only way `tier()` ever resolves to L0.
    let base = agent_with_tier("base", "", r#"tier = "l1""#);
    let child = agent_with_tier("child", "extends = \"base\"", r#"tier = "orchestration""#);
    let merged = merge_extends(base, child);
    assert_eq!(
        merged.agent.tier(),
        crate::agents::AgentTier::L0Orchestration
    );
}

#[test]
fn extends_unions_listener_bindings_by_name() {
    let base = cfg(r#"
[agent]
name = "base"
role = "r"
model = "m"
description = "d"
[llm]
temperature = 0.0
max_tokens = 1024
[system_prompt]
content = "b"
[[listeners]]
name = "calendar-personal"
event_types = ["event.created"]
"#);
    let ch = cfg(r#"
[agent]
name = "child"
role = "agent"
model = ""
description = ""
extends = "base"
[llm]
temperature = 0.0
max_tokens = 1024
[system_prompt]
content = "c"
[[listeners]]
name = "gmail-personal"
event_types = ["message.received"]
"#);
    let merged = merge_extends(base, ch);
    assert_eq!(merged.listeners.len(), 2, "distinct names both retained");
    assert!(
        merged
            .listeners
            .iter()
            .any(|b| b.name == "calendar-personal")
    );
    assert!(merged.listeners.iter().any(|b| b.name == "gmail-personal"));
}

#[test]
fn extends_listener_binding_child_override_replaces_base_filter() {
    let base = cfg(r#"
[agent]
name = "base"
role = "r"
model = "m"
description = "d"
[llm]
temperature = 0.0
max_tokens = 1024
[system_prompt]
content = "b"
[[listeners]]
name = "gmail-personal"
event_types = ["message.received"]
filter = { from = ["*@duetto.com"] }
"#);
    let ch = cfg(r#"
[agent]
name = "child"
role = "agent"
model = ""
description = ""
extends = "base"
[llm]
temperature = 0.0
max_tokens = 1024
[system_prompt]
content = "c"
[[listeners]]
name = "gmail-personal"
event_types = ["message.received"]
filter = { from = ["*@family.com"] }
"#);
    let merged = merge_extends(base, ch);
    assert_eq!(
        merged.listeners.len(),
        1,
        "same-name binding replaces, not appends"
    );
    assert_eq!(merged.listeners[0].filter.from, vec!["*@family.com"]);
}

#[test]
fn extends_union_none_cases() {
    let base_no_tools = cfg(r#"
[agent]
name = "base"
role = "r"
model = "m"
description = "d"
[llm]
temperature = 0.0
max_tokens = 1024
[system_prompt]
content = "b"
"#);
    // Neither declares tools → stays None.
    let ch_no_tools = child("child", "base", "c");
    let merged = merge_extends(base_no_tools.clone(), ch_no_tools);
    assert!(merged.tools.allowed.is_none());

    // Only the child declares tools → the child's list survives.
    let ch_tools = cfg(r#"
[agent]
name = "child"
role = "agent"
model = ""
description = ""
extends = "base"
[llm]
temperature = 0.0
max_tokens = 1024
[system_prompt]
content = "c"
[tools]
allowed = ["only_child"]
"#);
    let merged = merge_extends(base_no_tools, ch_tools);
    assert_eq!(merged.tools.allowed.unwrap(), vec!["only_child"]);
}

#[test]
fn extends_capabilities_union() {
    let base = cfg(r#"
[agent]
name = "base"
role = "r"
model = "m"
description = "d"
[llm]
temperature = 0.0
max_tokens = 1024
[system_prompt]
content = "b"
[agent.capabilities]
languages = ["rust"]
roles = ["engineer"]
frameworks = []
tags = ["general"]
"#);
    let ch = cfg(r#"
[agent]
name = "child"
role = "agent"
model = ""
description = ""
extends = "base"
[llm]
temperature = 0.0
max_tokens = 1024
[system_prompt]
content = "c"
[agent.capabilities]
languages = ["rust", "python"]
roles = []
frameworks = ["axum"]
tags = ["general", "personal"]
"#);
    let merged = merge_extends(base, ch);
    let caps = merged.agent.capabilities.unwrap();
    assert_eq!(caps.languages, vec!["rust", "python"]);
    assert_eq!(caps.roles, vec!["engineer"]);
    assert_eq!(caps.frameworks, vec!["axum"]);
    assert_eq!(caps.tags, vec!["general", "personal"]);
}

/// #3936 (PM-3, DOC-41 §5.5): a child's `user_authority` is NEVER
/// inherited/unioned from the base through `extends` — even when the child
/// extends the authority holder, the merged value is always the child's own
/// explicit setting (defaulting `false`).
#[test]
fn extends_does_not_inherit_user_authority() {
    let mut base = cfg(r#"
[agent]
name = "authority-holder"
role = "assistant"
model = "m1"
description = "d"
[llm]
temperature = 0.0
max_tokens = 1024
[system_prompt]
content = "base"
"#);
    base.permissions.user_authority = true;

    // A child that OMITS `[permissions]` entirely must NOT inherit `true`.
    let ch = child("overlay", "authority-holder", "child");
    let merged = merge_extends(base.clone(), ch);
    assert!(
        !merged.permissions.user_authority,
        "a child must never inherit user_authority=true from its base"
    );

    // A child that explicitly declares its OWN `user_authority = true` keeps
    // its own explicit setting (this was never in question, but pins the
    // "always the child's own value" framing rather than "always false").
    let mut ch2 = child("overlay2", "authority-holder", "child2");
    ch2.permissions.user_authority = true;
    let merged2 = merge_extends(base, ch2);
    assert!(merged2.permissions.user_authority);
}

/// #3936 (DOC-57 §2.3): `[permissions].scopes` unions base-first across the
/// chain, same polarity as legacy `[tools].scopes`.
#[test]
fn extends_unions_permissions_scopes() {
    let mut base = cfg(r#"
[agent]
name = "base"
role = "r"
model = "m1"
description = "d"
[llm]
temperature = 0.0
max_tokens = 1024
[system_prompt]
content = "base"
"#);
    base.permissions.scopes = Some(vec!["memory.read".to_string()]);

    let mut ch = child("child", "base", "child");
    ch.permissions.scopes = Some(vec!["google.gmail.*".to_string()]);

    let merged = merge_extends(base, ch);
    assert_eq!(
        merged.permissions.scopes,
        Some(vec![
            "memory.read".to_string(),
            "google.gmail.*".to_string()
        ])
    );
}

/// #3936 (DOC-57 CC-9 + §2.3): a base declaring only legacy `[tools].scopes`
/// and a child declaring only `[permissions].scopes` must UNION across the
/// chain — the child's migration to the new key must not silently drop the
/// base's un-migrated legacy grant.
#[test]
fn extends_permissions_scopes_union_survives_a_partially_migrated_chain() {
    let base = cfg(r#"
[agent]
name = "base"
role = "r"
model = "m1"
description = "d"
[llm]
temperature = 0.0
max_tokens = 1024
[system_prompt]
content = "base"

[tools]
scopes = ["memory.read"]
"#);

    let mut ch = child("child", "base", "child");
    ch.permissions.scopes = Some(vec!["google.gmail.*".to_string()]);

    let merged = merge_extends(base, ch);
    assert_eq!(
        merged.permissions.scopes,
        Some(vec![
            "memory.read".to_string(),
            "google.gmail.*".to_string()
        ]),
        "the base's un-migrated [tools].scopes must survive the child's [permissions].scopes"
    );
}

/// #3936 (DOC-57 §2.3): `[[permissions.grants]]` unions by `skill`; a child
/// re-declaring the same skill overrides the base's mode for it.
#[test]
fn extends_permission_grants_union_by_skill() {
    use crate::agents::{GrantMode, PermissionGrant};

    let mut base = cfg(r#"
[agent]
name = "base"
role = "r"
model = "m1"
description = "d"
[llm]
temperature = 0.0
max_tokens = 1024
[system_prompt]
content = "base"
"#);
    base.permissions.grants = vec![
        PermissionGrant {
            skill: "gmail".to_string(),
            mode: GrantMode::Ask,
        },
        PermissionGrant {
            skill: "git-status".to_string(),
            mode: GrantMode::Allow,
        },
    ];

    let mut ch = child("child", "base", "child");
    ch.permissions.grants = vec![PermissionGrant {
        skill: "gmail".to_string(),
        mode: GrantMode::Deny,
    }];

    let merged = merge_extends(base, ch);
    assert_eq!(
        merged.permissions.grants.len(),
        2,
        "{:?}",
        merged.permissions.grants
    );
    let gmail = merged
        .permissions
        .grants
        .iter()
        .find(|g| g.skill == "gmail")
        .unwrap();
    assert_eq!(gmail.mode, GrantMode::Deny, "child overrides base's mode");
    assert!(
        merged
            .permissions
            .grants
            .iter()
            .any(|g| g.skill == "git-status" && g.mode == GrantMode::Allow),
        "an un-overridden base grant survives"
    );
}

// --- resolve() walk tests -----------------------------------------------

#[test]
fn extends_two_level_merge() {
    let base = cfg(r#"
[agent]
name = "researcher"
role = "researcher"
model = "base-model"
description = "the base researcher"
[llm]
temperature = 0.0
max_tokens = 1024
[system_prompt]
content = "BASE INSTRUCTIONS"
[tools]
allowed = ["search_papers"]
"#);
    let ch = cfg(r#"
[agent]
name = "my-researcher"
role = "agent"
model = ""
description = "my customized researcher"
extends = "researcher"
[llm]
temperature = 0.0
max_tokens = 1024
[system_prompt]
content = "MY OVERRIDES"
[tools]
allowed = ["my_custom_tool"]
"#);
    let map = lookup_map(vec![base, ch]);
    let lookup = |n: &str| map.get(&n.to_lowercase()).cloned();
    let resolved = resolve("my-researcher", &lookup).expect("resolves");

    assert_eq!(resolved.agent.name, "my-researcher");
    assert_eq!(resolved.agent.model, "base-model"); // inherited
    assert_eq!(resolved.agent.description, "my customized researcher"); // overridden
    assert_eq!(resolved.agent.role, "researcher"); // inherited
    assert_eq!(
        resolved.system_prompt.content,
        "BASE INSTRUCTIONS\n\nMY OVERRIDES"
    );
    assert_eq!(
        resolved.tools.allowed.unwrap(),
        vec!["search_papers", "my_custom_tool"]
    );
    assert!(resolved.agent.extends.is_none());
}

#[test]
fn extends_resolved_config_has_no_extends() {
    // A base-only agent (no extends) resolves to itself with extends None.
    let base = cfg(r#"
[agent]
name = "base"
role = "r"
model = "m"
description = "d"
[llm]
temperature = 0.0
max_tokens = 1024
[system_prompt]
content = "b"
"#);
    let map = lookup_map(vec![base]);
    let lookup = |n: &str| map.get(&n.to_lowercase()).cloned();
    let resolved = resolve("base", &lookup).expect("resolves");
    assert!(resolved.agent.extends.is_none());
}

#[test]
fn extends_case_insensitive_base_resolution() {
    let base = cfg(r#"
[agent]
name = "Researcher"
role = "researcher"
model = "m"
description = "d"
[llm]
temperature = 0.0
max_tokens = 1024
[system_prompt]
content = "base"
"#);
    // Child extends "researcher" (lowercase) but the base is "Researcher".
    let ch = child("child", "researcher", "child");
    let map = lookup_map(vec![base, ch]);
    let lookup = |n: &str| map.get(&n.to_lowercase()).cloned();
    let resolved = resolve("child", &lookup).expect("resolves case-insensitively");
    assert_eq!(resolved.system_prompt.content, "base\n\nchild");
}

#[test]
fn extends_missing_parent_rejected() {
    let ch = child("child", "ghost", "c");
    let map = lookup_map(vec![ch]);
    let lookup = |n: &str| map.get(&n.to_lowercase()).cloned();
    let err = resolve("child", &lookup).expect_err("missing base is an error");
    match err {
        AgentExtendsError::ExtendsNotFound { agent, base } => {
            assert_eq!(agent, "child");
            assert_eq!(base, "ghost");
        }
        other => panic!("expected ExtendsNotFound, got {other:?}"),
    }
}

#[test]
fn extends_cycle_rejected() {
    // a extends b, b extends a.
    let a = child("a", "b", "a");
    let b = child("b", "a", "b");
    let map = lookup_map(vec![a, b]);
    let lookup = |n: &str| map.get(&n.to_lowercase()).cloned();
    let err = resolve("a", &lookup).expect_err("cycle is an error");
    match err {
        AgentExtendsError::ExtendsCycle { chain } => {
            // Walk order base-first with the repeated name appended.
            assert_eq!(chain, vec!["a", "b", "a"]);
        }
        other => panic!("expected ExtendsCycle, got {other:?}"),
    }
}

#[test]
fn extends_depth_limit_rejected() {
    // Build a chain a0 -> a1 -> ... -> a9 (each extends the next); resolving
    // a0 walks past MAX_DEPTH and is rejected.
    let mut configs = Vec::new();
    for i in 0..9 {
        configs.push(child(&format!("a{i}"), &format!("a{}", i + 1), "x"));
    }
    // Terminal base with no extends.
    configs.push(cfg(r#"
[agent]
name = "a9"
role = "r"
model = "m"
description = "d"
[llm]
temperature = 0.0
max_tokens = 1024
[system_prompt]
content = "leaf"
"#));
    let map = lookup_map(configs);
    let lookup = |n: &str| map.get(&n.to_lowercase()).cloned();
    let err = resolve("a0", &lookup).expect_err("over-deep chain is an error");
    match err {
        AgentExtendsError::ExtendsTooDeep { depth, .. } => {
            assert_eq!(depth, MAX_DEPTH);
        }
        other => panic!("expected ExtendsTooDeep, got {other:?}"),
    }
}

// --- AgentRegistry::load integration tests ------------------------------

#[test]
fn registry_resolves_extends_chain() {
    let dir = TempDir::new().unwrap();
    // Base assistant (nameless — no display_name) with its own tools + prose.
    let base = "---\nname: assistant\nrole: assistant\nmodel: anthropic/claude-sonnet-4-6\n\
description: Base assistant\ntools:\n  allowed: [search, read]\n---\n\nYou are a helpful assistant.\n";
    std::fs::write(dir.path().join("assistant.md"), base).unwrap();
    // Personal overlay extending the base, naming it Izzie and adding a tool.
    let overlay = "---\nname: izzie\nrole: agent\nextends: assistant\ndisplay_name: Izzie\n\
tools:\n  allowed: [my_custom_tool]\n---\n\nI prefer concise answers.\n";
    std::fs::write(dir.path().join("izzie.md"), overlay).unwrap();

    let reg = AgentRegistry::load(&[dir.path().to_path_buf()]);
    let izzie = reg.get("izzie").expect("izzie present");
    // Persona name from the child overlay even though the base had none.
    assert_eq!(izzie.agent.display_name.as_deref(), Some("Izzie"));
    // Model inherited from the base (overlay omitted it) and resolved.
    assert_eq!(izzie.agent.model, "anthropic/claude-sonnet-4-6");
    // Tools unioned base-first.
    assert_eq!(
        izzie.tools.allowed.clone().unwrap(),
        vec!["search", "read", "my_custom_tool"]
    );
    // Prose concatenated base-first.
    assert_eq!(
        izzie.system_prompt.content,
        "You are a helpful assistant.\n\nI prefer concise answers."
    );
    // Chain flattened.
    assert!(izzie.agent.extends.is_none());
    // The base is still present, unchanged.
    assert!(reg.get("assistant").is_some());
}

#[test]
fn registry_extends_missing_base_warns_and_keeps_agent() {
    let dir = TempDir::new().unwrap();
    let overlay = "---\nname: orphan\nrole: agent\nextends: nonexistent-base\n---\n\nHello.\n";
    std::fs::write(dir.path().join("orphan.md"), overlay).unwrap();

    let reg = AgentRegistry::load(&[dir.path().to_path_buf()]);
    // Failure-tolerant: the agent is still registered (unresolved), not dropped.
    let orphan = reg.get("orphan").expect("orphan kept despite missing base");
    // Body is the raw (unmerged) md body — it was never run through the merge's
    // prose trimming because resolution failed.
    assert_eq!(orphan.system_prompt.content.trim(), "Hello.");
    // Left unresolved — its extends marker is preserved so the miss is visible.
    assert_eq!(orphan.agent.extends.as_deref(), Some("nonexistent-base"));
}

#[test]
fn registry_extends_error_surfaced_in_summary() {
    // A failed `extends` resolution must be visible in the roster/agents-list,
    // not merely logged (code-critic HIGH, PR #3106).
    let dir = TempDir::new().unwrap();
    let overlay = "---\nname: orphan\nrole: agent\nextends: nonexistent-base\n---\n\nHi.\n";
    std::fs::write(dir.path().join("orphan.md"), overlay).unwrap();

    let reg = AgentRegistry::load(&[dir.path().to_path_buf()]);
    let summary = reg
        .list()
        .into_iter()
        .find(|s| s.name == "orphan")
        .expect("orphan listed");
    let err = summary
        .extends_error
        .expect("broken extends surfaced in summary");
    assert!(
        err.contains("nonexistent-base"),
        "error names the missing base: {err}"
    );
}

/// #3816: unlike listeners/tools (union), a child's `[[stores]]` REPLACES
/// the base's. The spec allows exactly one OKG store per agent, so a
/// personalization overlay declaring its own store must not end up with the
/// BASE's index as the default `vector_search` target.
#[test]
fn extends_child_stores_replace_base_stores() {
    let base = cfg(r#"
[agent]
name = "assistant"
role = "assistant"
model = "m"
description = "d"
[llm]
temperature = 0.0
max_tokens = 1024
[system_prompt]
content = "b"
[[stores]]
name = "assistant-kb"
index = "assistant"
"#);
    let ch = cfg(r#"
[agent]
name = "cto-assistant"
role = "assistant"
model = ""
description = ""
extends = "assistant"
[llm]
temperature = 0.0
max_tokens = 1024
[system_prompt]
content = "c"
[[stores]]
name = "cto-assistant-kb"
index = "cto-assistant"
palace = "cto"
"#);
    let merged = merge_extends(base, ch);
    assert_eq!(
        merged.stores.bindings.len(),
        1,
        "child replaces, not appends"
    );
    assert_eq!(
        merged.stores.default_search_index(),
        Some("cto-assistant"),
        "the overlay's OWN index must be the default search target"
    );
    assert_eq!(merged.stores.bindings[0].palace.as_deref(), Some("cto"));
}

/// A child that declares no stores still inherits the base's binding.
#[test]
fn extends_child_without_stores_inherits_base_binding() {
    let base = cfg(r#"
[agent]
name = "assistant"
role = "assistant"
model = "m"
description = "d"
[llm]
temperature = 0.0
max_tokens = 1024
[system_prompt]
content = "b"
[[stores]]
name = "assistant-kb"
index = "assistant"
"#);
    let ch = cfg(r#"
[agent]
name = "child"
role = "assistant"
model = ""
description = ""
extends = "assistant"
[llm]
temperature = 0.0
max_tokens = 1024
[system_prompt]
content = "c"
"#);
    let merged = merge_extends(base, ch);
    assert_eq!(merged.stores.default_search_index(), Some("assistant"));
}

/// ADR-0024 decision 4: an `extends` child's own `[subagents]` declarations
/// survive the merge, unioned base-first.
///
/// Why: before decision 4 neither `[subagents]` key was merged at all — the
/// base's value survived only because `merged` starts as `base`, and a CHILD's
/// declaration was silently discarded. Harmless while nothing declared the
/// section; not harmless for a whitelist the owner ratified as EDITABLE, since
/// a personalization overlay declaring its own reachable set would have had it
/// dropped. Union cannot widen past the server-owned floor — `resolve`
/// intersects at dispatch — so the safe direction is the §2.5 list rule.
/// What: base declares one floor member, the child the other; the merge carries
/// both, base-first, on both keys.
/// Test: this function IS the test.
#[test]
fn extends_unions_the_subagent_whitelist() {
    let base = cfg(r#"
[agent]
name = "base"
role = "assistant"
model = "m"
description = "d"
[llm]
temperature = 0.0
max_tokens = 1024
[system_prompt]
content = "b"
[subagents]
allowed = ["research"]
delegate_allowed = ["research-agent"]
"#);
    let ch = cfg(r#"
[agent]
name = "child"
role = "assistant"
model = "m"
description = "d"
extends = "base"
[llm]
temperature = 0.0
max_tokens = 1024
[system_prompt]
content = "c"
[subagents]
allowed = ["ticketing"]
delegate_allowed = ["ticketing-agent"]
"#);

    let merged = merge_extends(base, ch);
    assert_eq!(
        merged.subagents.allowed,
        Some(vec!["research".to_string(), "ticketing".to_string()]),
        "the cross-product list must union base-first"
    );
    assert_eq!(
        merged.subagents.delegate_allowed,
        Some(vec![
            "research-agent".to_string(),
            "ticketing-agent".to_string()
        ]),
        "the in-process whitelist must union base-first"
    );
}
