// Pre-existing clippy warnings across this large binary crate.
// Each category below is suppressed at crate level with rationale:
// - dead_code / unused_imports: Many helpers are kept for future use, behind
//   feature flags, or used only on certain platforms / by tests; pruning them
//   is its own refactor and would churn unrelated modules.
// - clippy::collapsible_if / collapsible_else_if: Style preference; nested
//   ifs are often clearer with the existing comments and gating logic.
// - clippy::manual_str_repeat / manual_repeat_n / single_char_add_str: Style
//   nits in display/formatting code where current form reads fine.
// - clippy::too_many_arguments: A few orchestration entry points genuinely
//   need their argument count; signatures are part of internal contracts.
// - clippy::await_holding_lock: Test-only — a std::sync::Mutex serializes
//   tests that mutate process-global env (HOME, etc.). The await points are
//   inside the critical section by design, and tests are single-threaded
//   per-test by virtue of the lock.
// - clippy::clone_on_copy / len_zero / map_or / etc.: Misc style nits in
//   pre-existing code; not worth the churn vs. risk of breaking 1500+ tests.
#![allow(dead_code)]
#![allow(unused_imports)]
#![allow(unused_mut)]
#![allow(unused_assignments)]
#![allow(unused_variables)]
#![allow(clippy::collapsible_if)]
#![allow(clippy::collapsible_else_if)]
#![allow(clippy::manual_str_repeat)]
#![allow(clippy::manual_repeat_n)]
#![allow(clippy::single_char_add_str)]
#![allow(clippy::too_many_arguments)]
#![allow(clippy::await_holding_lock)]
#![allow(clippy::clone_on_copy)]
#![allow(clippy::len_zero)]
#![allow(clippy::unnecessary_map_or)]
#![allow(clippy::manual_map)]
#![allow(clippy::needless_borrows_for_generic_args)]
#![allow(clippy::unnecessary_sort_by)]
#![allow(clippy::if_same_then_else)]
#![allow(clippy::new_without_default)]
#![allow(clippy::manual_split_once)]
#![allow(clippy::needless_splitn)]
#![allow(clippy::single_match_else)]
#![allow(clippy::single_match)]
#![allow(clippy::ptr_arg)]
#![allow(clippy::manual_clamp)]
#![allow(clippy::redundant_closure)]
#![allow(clippy::manual_pattern_char_comparison)]
#![allow(clippy::vec_init_then_push)]
#![allow(clippy::single_component_path_imports)]
#![allow(clippy::derivable_impls)]
#![allow(clippy::match_single_binding)]
#![allow(clippy::redundant_pattern_matching)]

//! Unit + async tests for `build_registry_for_agent` (per-agent tool wiring).

use super::*;
use super::*;

fn empty_skill_registry() -> Arc<skills::SkillRegistry> {
    Arc::new(skills::SkillRegistry::empty())
}

fn empty_tag_registry() -> Arc<skills::registry::SkillRegistry> {
    Arc::new(skills::registry::SkillRegistry::empty())
}

#[test]
fn research_agent_registry_has_web_tools() {
    let reg = build_registry_for_agent(
        "research-agent",
        "researcher",
        None,
        None,
        empty_skill_registry(),
        empty_tag_registry(),
    )
    .expect("research-agent builds a registry");
    assert!(
        reg.contains("web_search"),
        "web_search missing from research-agent registry"
    );
    assert!(
        reg.contains("fetch_url"),
        "fetch_url missing from research-agent registry"
    );
}

#[test]
fn research_agent_registry_has_memory_tools() {
    // #53: memory_recall + vector_search registered for the research agent.
    let reg = build_registry_for_agent(
        "research-agent",
        "researcher",
        None,
        None,
        empty_skill_registry(),
        empty_tag_registry(),
    )
    .expect("research-agent builds a registry");
    assert!(reg.contains("memory_recall"), "memory_recall missing");
    assert!(reg.contains("vector_search"), "vector_search missing");
}

#[test]
fn research_agent_registry_has_readonly_fs_tools() {
    // Merged from the former explorer-agent: research-agent is now the
    // single "find out" agent and must be able to read/grep the codebase.
    let reg = build_registry_for_agent(
        "research-agent",
        "researcher",
        None,
        None,
        empty_skill_registry(),
        empty_tag_registry(),
    )
    .expect("research-agent builds a registry");
    assert!(reg.contains("read_file"), "read_file missing");
    assert!(reg.contains("list_dir"), "list_dir missing");
    assert!(reg.contains("grep_files"), "grep_files missing");
}

#[test]
fn plan_agent_registry_has_memory_tools() {
    // #53: plan-agent gets memory_recall + vector_search so it can ground
    // plans in existing code / project knowledge.
    let reg = build_registry_for_agent(
        "plan-agent",
        "planner",
        None,
        None,
        empty_skill_registry(),
        empty_tag_registry(),
    )
    .expect("plan-agent builds a registry");
    assert!(reg.contains("memory_recall"), "memory_recall missing");
    assert!(reg.contains("vector_search"), "vector_search missing");
}

#[test]
fn all_known_agents_get_skill_tools() {
    // #81: every agent that builds a registry should have load_skill and
    // list_skills available, regardless of whether the skill registry is
    // empty or populated. Per-agent `[tools].allowed` still controls which
    // tools are callable at runtime.
    for agent in [
        "research-agent",
        "plan-agent",
        "qa-agent",
        "local-ops-agent",
        "docs-agent",
        // Unknown agent name: default branch also registers skill tools.
        "unknown-agent",
    ] {
        let reg = build_registry_for_agent(
            agent,
            "engineer",
            None,
            None,
            empty_skill_registry(),
            empty_tag_registry(),
        )
        .unwrap_or_else(|| panic!("{agent} should get a registry"));
        assert!(reg.contains("load_skill"), "{agent}: load_skill missing");
        assert!(reg.contains("list_skills"), "{agent}: list_skills missing");
    }
}

#[test]
fn plan_agent_registry_has_write_file_tool() {
    // #87: plan-agent gets write_file so it can emit stub files and
    // assignments.json for interface-first decomposition.
    let reg = build_registry_for_agent(
        "plan-agent",
        "planner",
        None,
        None,
        empty_skill_registry(),
        empty_tag_registry(),
    )
    .expect("plan-agent builds a registry");
    assert!(
        reg.contains("write_file"),
        "write_file missing from plan-agent registry"
    );
}

#[test]
fn docs_agent_registry_has_write_and_read_tools() {
    // #82: docs-agent gets write_file + read-only exploration tools so it
    // can inspect generated code and emit documentation files.
    let reg = build_registry_for_agent(
        "docs-agent",
        "documentation",
        None,
        None,
        empty_skill_registry(),
        empty_tag_registry(),
    )
    .expect("docs-agent builds a registry");
    assert!(reg.contains("write_file"), "write_file missing");
    assert!(reg.contains("read_file"), "read_file missing");
    assert!(reg.contains("list_dir"), "list_dir missing");
    assert!(reg.contains("grep_files"), "grep_files missing");
}

#[tokio::test]
async fn list_skills_uses_tag_registry_when_wired() {
    // #170: When `build_registry_for_agent` is called with a non-empty
    // tag-indexed registry, the resulting `list_skills` tool must return
    // tag-ranked JSON (not the legacy float-score format).
    use tempfile::TempDir;
    let dir = TempDir::new().unwrap();
    std::fs::write(
        dir.path().join("fastapi.md"),
        "---\nname: fastapi\ndescription: async routes\ntags: [python, fastapi]\n---\nbody\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("rust.md"),
        "---\nname: rust\ndescription: rust idioms\ntags: [rust]\n---\nbody\n",
    )
    .unwrap();

    let tag_reg = Arc::new(skills::registry::SkillRegistry::load(&[dir
        .path()
        .to_path_buf()]));
    assert!(!tag_reg.is_empty(), "sanity: tag registry loaded skills");

    let reg = build_registry_for_agent(
        "research-agent",
        "researcher",
        None,
        None,
        empty_skill_registry(),
        tag_reg,
    )
    .expect("research-agent builds a registry");
    assert!(reg.contains("list_skills"));

    let result = reg
        .dispatch("list_skills", serde_json::json!({"tags": ["python"]}))
        .await;
    let content = result.content();
    assert!(
        content.contains("\"fastapi\""),
        "expected fastapi in tag-ranked output, got: {content}"
    );
    assert!(
        content.contains("\"match_score\""),
        "expected tag-registry JSON (match_score field), got: {content}"
    );
    assert!(
        !content.contains("\"rust\""),
        "rust has no 'python' tag and must be filtered out: {content}"
    );
}

#[tokio::test]
async fn list_skills_falls_back_to_legacy_when_tag_registry_empty() {
    // #170: Wiring preserves legacy behavior when the tag registry is
    // empty (no `.trusty-agents/skills/` configured). The tool must still
    // register and return a non-panicking response.
    let reg = build_registry_for_agent(
        "research-agent",
        "researcher",
        None,
        None,
        empty_skill_registry(),
        empty_tag_registry(),
    )
    .expect("research-agent builds a registry");
    assert!(reg.contains("list_skills"));
    let result = reg.dispatch("list_skills", serde_json::json!({})).await;
    // Empty legacy + empty tag registry yields the resolver fallback
    // string; just assert the call succeeds without panicking.
    let _ = result.content();
}

#[tokio::test]
async fn web_search_without_api_key_returns_graceful_error() {
    // Ensure no key is set for this scope.
    // SAFETY: removing an env var in a test; other tests do not rely on
    // BRAVE_API_KEY being set. The graceful-error path is what we assert.
    unsafe {
        std::env::remove_var("BRAVE_API_KEY");
    }
    let tool = BraveSearchTool::from_env();
    use tools::ToolExecutor;
    let out = tool
        .execute(serde_json::json!({"query": "rust async"}))
        .await;
    assert!(
        out.is_error(),
        "expected an error result when BRAVE_API_KEY is unset"
    );
    assert!(
        out.content().contains("BRAVE_API_KEY"),
        "error should mention BRAVE_API_KEY, got: {}",
        out.content()
    );
}

// #3550 follow-up: a live smoke test proved the assistant persona still
// leaked internal names (`trusty-mpm`, `subagent`) and recited the entire
// bundled skill catalog (wave planning, git worktree discipline, the full
// engineering skill bench) when dispatched via `tagent --direct assistant`,
// even after PR #3550 hardened the SEPARATE persona-chat dispatch path. Root
// cause: `build_registry_for_agent`'s catch-all branch unconditionally wired
// `list_skills`/`load_skill` to the full tag-indexed skill registry for ANY
// agent without a dedicated branch — including role=="assistant" — and the
// caller never applied the persona's `[tools].allow` glob restriction on
// this path (see `scope_assistant_allowed_tools` below). These tests pin
// the fix: role-keyed dispatch to `build_assistant_tier_registry`.

#[test]
fn assistant_tier_registry_excludes_skill_catalog_tools() {
    // A non-empty tag registry (as production wires via
    // `default_bundled_config_dir()`) must NOT leak `list_skills`/
    // `load_skill` into the assistant-tier registry — those tools are how
    // the assistant could recite the entire engineering skill catalog
    // (languages, frameworks, `workflow/wave-planning.md`,
    // `workflow/delegation.md`, …) it has no business knowing about.
    use tempfile::TempDir;
    let dir = TempDir::new().unwrap();
    std::fs::write(
        dir.path().join("wave-planning.md"),
        "---\nname: wave-planning\ndescription: decompose\ntags: [workflow]\n---\nbody\n",
    )
    .unwrap();
    let tag_reg = Arc::new(skills::registry::SkillRegistry::load(&[dir
        .path()
        .to_path_buf()]));
    assert!(!tag_reg.is_empty(), "sanity: tag registry loaded a skill");

    let reg = build_registry_for_agent(
        "assistant",
        "assistant",
        None,
        None,
        empty_skill_registry(),
        tag_reg,
    )
    .expect("assistant-tier agent builds a registry");

    assert!(
        !reg.contains("list_skills"),
        "list_skills must not be reachable from the assistant tier"
    );
    assert!(
        !reg.contains("load_skill"),
        "load_skill must not be reachable from the assistant tier"
    );
}

#[test]
fn assistant_tier_registry_includes_curated_tools() {
    // The assistant tier still needs SOME way to genuinely act on "bring in
    // a specialist" rather than only ever talking about it — delegation and
    // web search are registered (actual reachability is still gated by
    // `[tools].allow` at the `run_subagent` call site).
    let reg = build_registry_for_agent(
        "assistant",
        "assistant",
        None,
        None,
        empty_skill_registry(),
        empty_tag_registry(),
    )
    .expect("assistant-tier agent builds a registry");
    assert!(
        reg.contains("delegate_to_agent"),
        "delegate_to_agent missing from assistant-tier registry"
    );
    assert!(
        reg.contains("web_search"),
        "web_search missing from assistant-tier registry"
    );
}

#[test]
fn role_takes_precedence_over_name_for_assistant_tier() {
    // Role gating is checked BEFORE the `name`-based match — an agent named
    // like a coding sub-agent (e.g. a persona overlay someone accidentally
    // names "docs-agent") must still get the curated assistant-tier
    // registry, never the generic coding-agent tool set, when its role is
    // "assistant".
    let reg = build_registry_for_agent(
        "docs-agent",
        "assistant",
        None,
        None,
        empty_skill_registry(),
        empty_tag_registry(),
    )
    .expect("builds a registry");
    assert!(
        !reg.contains("write_file"),
        "assistant-tier role must not get docs-agent's write_file tool"
    );
    assert!(reg.contains("delegate_to_agent"));
}

#[test]
fn scope_assistant_allowed_tools_filters_by_glob() {
    // The exact gap that caused the leak: `[tools].allow` (glob) must be
    // translated into `[tools].allowed` (exact names) for the assistant
    // tier, or `chat_with_tools_gated` treats the persona as unrestricted.
    let reg = build_registry_for_agent(
        "assistant",
        "assistant",
        None,
        None,
        empty_skill_registry(),
        empty_tag_registry(),
    )
    .expect("assistant-tier agent builds a registry");

    let allow = vec!["delegate_to_agent".to_string()];
    let kept =
        scope_assistant_allowed_tools(true, None, Some(&allow), Some(&reg)).unwrap_or_default();

    assert_eq!(kept, vec!["delegate_to_agent".to_string()]);
    assert!(
        !kept.iter().any(|n| n == "web_search"),
        "web_search not in the allow list must not survive scoping"
    );
}

#[test]
fn scope_assistant_allowed_tools_noop_for_non_assistant() {
    // Coding sub-agents rely on `allowed == None` meaning "unrestricted"
    // (see `ToolsConfig::allowed` doc comment) — the assistant-tier gate
    // must never touch that default for role != "assistant".
    let allow = vec!["git_*".to_string()];
    let kept = scope_assistant_allowed_tools(false, None, Some(&allow), None);
    assert_eq!(kept, None);
}

#[test]
fn scope_assistant_allowed_tools_noop_when_allowed_already_set() {
    // An assistant-tier agent that already sets `[tools].allowed` explicitly
    // keeps that list untouched rather than being overwritten by the
    // glob-derived set.
    let existing = Some(vec!["only_this_tool".to_string()]);
    let allow = vec!["*".to_string()];
    let kept = scope_assistant_allowed_tools(true, existing.clone(), Some(&allow), None);
    assert_eq!(kept, existing);
}
