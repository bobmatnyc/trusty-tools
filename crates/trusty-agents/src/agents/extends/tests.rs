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

/// TODO(#3075 / AUTH-2): once AUTH-1/#3074 adds the `user_authority` field to
/// `AgentConfig`, this test must assert that a child's `user_authority` is
/// NEVER inherited/unioned from the base through `extends` — even when the
/// child extends the authority holder, the merged value is the child's own
/// explicit setting (defaulting `false`). The exclusion hook is stubbed at the
/// `merge_extends` merge site (see the `user_authority` comment there); the
/// field does not exist yet, so this placeholder is `#[ignore]`d.
#[test]
#[ignore = "user_authority field lands in AUTH-1/#3074; assertion filled by AUTH-2/#3075"]
fn extends_does_not_inherit_user_authority() {
    // Intentionally empty: see doc comment. When #3074 lands, build a base
    // with user_authority = true and a child that omits it, then assert the
    // merged config's user_authority is false (the child's own default).
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
