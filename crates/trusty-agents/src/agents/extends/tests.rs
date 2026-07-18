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

#[test]
fn extends_child_sets_display_name_over_none() {
    // Owner naming decision 2026-07-18: bases are nameless; the persona name
    // comes from the child overlay. A child MUST be able to set display_name
    // even when the base has NONE.
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
