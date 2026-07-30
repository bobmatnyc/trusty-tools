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
        None,
        crate::agents::AgentTier::L1Standard,
        None,
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
        None,
        crate::agents::AgentTier::L1Standard,
        None,
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
        None,
        crate::agents::AgentTier::L1Standard,
        None,
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
        None,
        crate::agents::AgentTier::L1Standard,
        None,
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
            None,
            crate::agents::AgentTier::L1Standard,
            None,
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
        None,
        crate::agents::AgentTier::L1Standard,
        None,
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
        None,
        crate::agents::AgentTier::L1Standard,
        None,
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
        None,
        crate::agents::AgentTier::L1Standard,
        None,
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
        None,
        crate::agents::AgentTier::L1Standard,
        None,
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
        None,
        crate::agents::AgentTier::L1Standard,
        None,
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
        None,
        crate::agents::AgentTier::L1Standard,
        None,
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
fn assistant_tier_registry_includes_izzie_tools() {
    // #3745 item C regression guard: the `--direct`/`--agent` assistant-tier
    // registry must register the izzie platform-hosted tools so a persona
    // whose `[tools].allow` opts into them (izzie) can actually reach them —
    // exactly as the persona-chat dispatch path
    // (`run_pm_task_with_persona`) already does. Before the fix these three
    // tools were only registered on the REPL `/agent` path, so `--direct
    // izzie` scoped them away and the model answered "weather tool
    // unavailable".
    let reg = build_registry_for_agent(
        "izzie",
        "assistant",
        None,
        None,
        empty_skill_registry(),
        empty_tag_registry(),
        None,
        crate::agents::AgentTier::L1Standard,
        None,
    )
    .expect("assistant-tier agent builds a registry");
    for expected in ["get_weather", "get_train_schedule", "get_train_alerts"] {
        assert!(
            reg.contains(expected),
            "{expected} missing from assistant-tier registry (#3745 item C)"
        );
    }
}

#[test]
fn izzie_allow_list_surfaces_persona_tools_through_scoping() {
    // End-to-end of the two functions the `--direct` path chains: the
    // assistant-tier registry (`build_assistant_tier_registry`) plus the REAL
    // glob-scoping (`scope_assistant_allowed_tools`). With izzie's declared
    // `[tools].allow` including `get_weather`, the scoped result must retain
    // it — proving the fix at the exact seam that dropped it (the allow list
    // was correct; the tool simply was not in the registry to survive the
    // intersection).
    let reg = build_assistant_tier_registry(None, crate::agents::AgentTier::L1Standard, None);
    let allow = vec![
        "delegate_to_agent".to_string(),
        "web_search".to_string(),
        "get_weather".to_string(),
        "get_train_schedule".to_string(),
        "get_train_alerts".to_string(),
    ];
    let kept =
        scope_assistant_allowed_tools(true, None, Some(&allow), Some(&reg)).unwrap_or_default();
    for expected in ["get_weather", "get_train_schedule", "get_train_alerts"] {
        assert!(
            kept.iter().any(|n| n == expected),
            "{expected} declared in [tools].allow must survive assistant-tier scoping, got: {kept:?}"
        );
    }
}

#[test]
fn non_opted_in_persona_does_not_get_izzie_tools() {
    // Registering the izzie tools into the shared assistant-tier registry
    // must NOT widen a persona that never declared them: `scope_assistant_
    // allowed_tools` still gates by the persona's own `[tools].allow`. A
    // persona allowing only `web_search` gets only `web_search`, never
    // `get_weather`.
    let reg = build_assistant_tier_registry(None, crate::agents::AgentTier::L1Standard, None);
    let allow = vec!["web_search".to_string()];
    let kept =
        scope_assistant_allowed_tools(true, None, Some(&allow), Some(&reg)).unwrap_or_default();
    assert!(
        !kept.iter().any(|n| n == "get_weather"),
        "a persona that did not opt into get_weather must not receive it, got: {kept:?}"
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
        None,
        crate::agents::AgentTier::L1Standard,
        None,
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
        None,
        crate::agents::AgentTier::L1Standard,
        None,
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

// --- item 2 regression: fail-closed on absent taint/allow signal (#4126) ---

#[test]
fn scope_assistant_allowed_tools_fails_closed_when_allow_absent() {
    // The bug: an assistant-tier agent with no `[tools].allow` resolved at
    // all (misconfigured persona TOML) and no pre-existing `[tools].allowed`
    // used to fall through to `None` here — "every registered tool is
    // callable" — the exact opposite of what the assistant tier's security
    // model requires. Must be `Some(vec![])` (deny-all), never `None`.
    let reg = build_registry_for_agent(
        "assistant",
        "assistant",
        None,
        None,
        empty_skill_registry(),
        empty_tag_registry(),
        None,
        crate::agents::AgentTier::L1Standard,
        None,
    )
    .expect("assistant-tier agent builds a registry");

    let kept = scope_assistant_allowed_tools(true, None, None, Some(&reg));
    assert_eq!(
        kept,
        Some(Vec::new()),
        "an assistant-tier agent with no resolvable allow patterns must be denied \
         every tool, never left unrestricted"
    );
}

#[test]
fn scope_assistant_allowed_tools_fails_closed_when_registry_absent() {
    // Same fail-closed requirement when `registry` itself is unavailable
    // (e.g. the registry failed to build) — still must not fall through to
    // unrestricted `None`.
    let allow = vec!["web_search".to_string()];
    let kept = scope_assistant_allowed_tools(true, None, Some(&allow), None);
    assert_eq!(kept, Some(Vec::new()));
}

#[test]
fn scope_for_delegation_untainted_assistant_tier_fails_closed_without_allow() {
    // End-to-end of item 2 through the actual caller, `scope_for_delegation`,
    // in its `!is_tainted` branch: an assistant-tier agent invoked directly
    // (not via a tainted delegation) with no resolvable `[tools].allow` must
    // still be denied every tool, not handed an unrestricted registry.
    let reg = build_registry_for_agent(
        "assistant",
        "assistant",
        None,
        None,
        empty_skill_registry(),
        empty_tag_registry(),
        None,
        crate::agents::AgentTier::L1Standard,
        None,
    )
    .expect("assistant-tier agent builds a registry");

    let kept = scope_for_delegation(
        /* is_assistant_tier */ true,
        /* is_tainted */ false,
        /* original_allowed */ None,
        /* skill_scoped_allow */ None,
        /* taint_allow */ None,
        Some(&reg),
    );
    assert_eq!(
        kept,
        Some(Vec::new()),
        "an untainted assistant-tier agent with no declared posture must never end up \
         unrestricted"
    );
}

// --- item 1 regression: multi-hop taint composition (#4126) ---
//
// `compose_delegation_taint` is what `run_subagent` (`subagent_mode.rs`) and
// `run_pm_task_with_persona` (`persona.rs`) now call instead of forwarding
// their own native `[tools].allow` verbatim as the outbound taint for
// whatever they spawn next.

#[test]
fn compose_delegation_taint_untainted_forwards_native_unchanged() {
    // Hop 0 (no inbound taint): this agent forwards its OWN posture,
    // byte-identical to the pre-fix `cfg.tools.allow.as_deref()` call.
    let native = vec!["web_search".to_string(), "delegate_to_agent".to_string()];
    let composed = compose_delegation_taint(Some(&native), None);
    assert_eq!(composed, Some(native));
}

#[test]
fn compose_delegation_taint_intersects_native_with_inbound() {
    // Hop 2: an inbound taint narrower than (or disjoint from) this agent's
    // own native posture must NEVER widen back out to the native list.
    let native = vec![
        "delegate_to_agent".to_string(),
        "web_search".to_string(),
        "get_weather".to_string(),
    ];
    let inbound = vec!["delegate_to_agent".to_string(), "web_search".to_string()];
    let composed = compose_delegation_taint(Some(&native), Some(&inbound));
    assert_eq!(
        composed,
        Some(vec![
            "delegate_to_agent".to_string(),
            "web_search".to_string()
        ]),
        "get_weather is native-only (not in the inbound taint) and must be dropped"
    );
}

#[test]
fn compose_delegation_taint_no_native_forwards_inbound_unchanged() {
    // This agent declares no `[tools].allow` of its own — it cannot impose
    // any FURTHER narrowing, so the inbound taint passes through unchanged
    // (never widened, since there is nothing native to widen it with).
    let inbound = vec!["delegate_to_agent".to_string()];
    let composed = compose_delegation_taint(None, Some(&inbound));
    assert_eq!(composed, Some(inbound));
}

#[test]
fn multi_hop_delegation_narrows_at_every_hop() {
    // The concrete scenario named in the fix: `assistant -> izzie ->
    // engineer`. `assistant`'s own posture (A) is broader than `izzie`'s own
    // posture (B) in one dimension (A grants get_gmail_message_content,
    // which izzie does not have) and narrower in another (izzie grants
    // get_weather, which assistant does not have) — proving the hop-2 taint
    // is the TRUE intersection, not either side alone.
    let assistant_allow = vec![
        "delegate_to_agent".to_string(),
        "web_search".to_string(),
        "get_gmail_message_content".to_string(),
    ];

    // Hop 1: assistant -> izzie. No inbound taint at hop 0, so izzie is
    // tainted with assistant's own posture verbatim.
    let hop1_taint =
        compose_delegation_taint(Some(&assistant_allow), None).expect("hop 1 must produce a taint");
    assert_eq!(hop1_taint, assistant_allow);

    // Hop 2: izzie -> engineer. Izzie's OWN native posture (B) includes
    // get_weather (not in A) and omits get_gmail_message_content (which IS
    // in A) — the bug this closes forwarded B verbatim, ignoring hop 1's
    // taint (A) entirely.
    let izzie_allow = vec![
        "delegate_to_agent".to_string(),
        "web_search".to_string(),
        "get_weather".to_string(),
    ];
    let hop2_taint = compose_delegation_taint(Some(&izzie_allow), Some(&hop1_taint))
        .expect("hop 2 must produce a taint");

    assert!(
        hop2_taint.iter().any(|p| p == "delegate_to_agent"),
        "delegate_to_agent is in both A and B and must survive: {hop2_taint:?}"
    );
    assert!(
        hop2_taint.iter().any(|p| p == "web_search"),
        "web_search is in both A and B and must survive: {hop2_taint:?}"
    );
    assert!(
        !hop2_taint.iter().any(|p| p == "get_weather"),
        "get_weather is izzie-only (not in assistant's forwarded taint) and must be \
         dropped, not forwarded to engineer: {hop2_taint:?}"
    );
    assert!(
        !hop2_taint.iter().any(|p| p == "get_gmail_message_content"),
        "get_gmail_message_content is assistant-only (izzie's own posture never \
         granted it) and must be dropped: {hop2_taint:?}"
    );

    // Finally, prove the composed hop-2 taint actually narrows `engineer`'s
    // real registry the same way `scope_for_delegation` would when
    // `run_subagent` applies it: engineer's shell_exec/read_file surface has
    // no overlap with either persona's tool posture, so it must end up with
    // zero callable tools.
    let engineer_reg = build_registry_for_agent(
        "local-ops-agent",
        "engineer",
        None,
        None,
        empty_skill_registry(),
        empty_tag_registry(),
        None,
        crate::agents::AgentTier::L1Standard,
        None,
    )
    .expect("engineer-shaped registry builds");
    assert!(engineer_reg.contains("shell_exec"));

    let engineer_scoped = scope_for_delegation(
        false,
        true,
        None,
        None,
        Some(&hop2_taint),
        Some(&engineer_reg),
    )
    .expect("a tainted engineer spawn must always be scoped");
    assert!(
        !engineer_scoped.iter().any(|n| n == "shell_exec"),
        "engineer must not reach shell_exec via the narrowed hop-2 taint: {engineer_scoped:?}"
    );
}

// --- `scope_for_delegation` (security fix: delegate_to_agent injection-to-RCE path) ---
//
// Chain: an assistant-tier persona holding live external-content tools
// (get_gmail_message_content, web_search, …) plus `delegate_to_agent` could
// hand attacker-controlled text to `delegate_to_agent(agent_name="engineer",
// task=…)`. `engineer` declares no `[tools].allow`, so its spawned
// `claude-code` subprocess got the FULL unrestricted registry — the
// `scope_assistant_allowed_tools` narrowing only ever fired when the SPAWNED
// agent's own role was "assistant". `scope_for_delegation` is the fix.

#[test]
fn tainted_delegation_cannot_reach_tool_delegator_lacked() {
    // `local-ops-agent`'s registry carries a genuinely permissive `shell_exec`
    // tool (`tools::shell::ShellExecTool` — the real, unrestricted local-ops
    // command runner, distinct from qa-agent's narrowly-scoped `pytest_exec`)
    // plus read-only file tools. None of these match a curated assistant
    // persona's typical `[tools].allow` surface.
    let reg = build_registry_for_agent(
        "local-ops-agent",
        "engineer",
        None,
        None,
        empty_skill_registry(),
        empty_tag_registry(),
        None,
        crate::agents::AgentTier::L1Standard,
        None,
    )
    .expect("local-ops-agent builds a registry");
    assert!(
        reg.contains("shell_exec"),
        "fixture assumption: local-ops-agent must register the real shell_exec tool"
    );

    // The delegator's (assistant persona's) own posture — none of these
    // patterns match shell_exec/read_file/list_dir/grep_files.
    let taint_allow = vec!["delegate_to_agent".to_string(), "web_search".to_string()];

    let kept = scope_for_delegation(
        /* is_assistant_tier */ false, // engineer's OWN role, unaffected by taint
        /* is_tainted */ true,
        /* original_allowed */ None,
        /* skill_scoped_allow */ None,
        /* taint_allow */ Some(&taint_allow),
        Some(&reg),
    )
    .expect("a tainted spawn must always be scoped, never left unrestricted");

    assert!(
        !kept.iter().any(|n| n == "shell_exec"),
        "tainted engineer must not reach shell_exec — the delegator never had it; kept={kept:?}"
    );
    assert!(
        !kept
            .iter()
            .any(|n| n == "read_file" || n == "list_dir" || n == "grep_files"),
        "tainted engineer must not reach filesystem tools the delegator lacked; kept={kept:?}"
    );
}

#[test]
fn untainted_delegation_is_unaffected() {
    // A normal (non-delegated, or delegated by a trusted pm/ctrl caller that
    // never taints) engineer spawn must be BYTE-IDENTICAL to pre-fix
    // behavior: fully unrestricted (`None` = "every registered tool is
    // callable" per `ToolsConfig::allowed`'s doc comment).
    let reg = build_registry_for_agent(
        "local-ops-agent",
        "engineer",
        None,
        None,
        empty_skill_registry(),
        empty_tag_registry(),
        None,
        crate::agents::AgentTier::L1Standard,
        None,
    )
    .expect("local-ops-agent builds a registry");

    let kept = scope_for_delegation(false, false, None, None, None, Some(&reg));
    assert_eq!(
        kept, None,
        "untainted, non-assistant-tier spawns must remain unrestricted"
    );
}

#[test]
fn tainted_delegation_intersects_with_existing_allowed() {
    // A taint must only ever narrow, never widen past an explicit
    // `[tools].allowed` (exact list) the spawned agent's own TOML already
    // declared.
    let reg = build_registry_for_agent(
        "docs-agent",
        "documentation",
        None,
        None,
        empty_skill_registry(),
        empty_tag_registry(),
        None,
        crate::agents::AgentTier::L1Standard,
        None,
    )
    .expect("docs-agent builds a registry");
    assert!(reg.contains("write_file"));
    assert!(reg.contains("read_file"));

    let original_allowed = Some(vec!["write_file".to_string(), "read_file".to_string()]);
    let taint_allow = vec!["write_file".to_string()];

    let kept = scope_for_delegation(
        false,
        true,
        original_allowed,
        None,
        Some(&taint_allow),
        Some(&reg),
    )
    .expect("tainted spawn must be scoped");

    assert_eq!(
        kept,
        vec!["write_file".to_string()],
        "result must be the INTERSECTION of the taint and the agent's own allowlist, \
         not either side alone"
    );
}

#[test]
fn assistant_delegate_to_engineer_path_is_scoped() {
    // The SPECIFIC vulnerable path: `assistant` (role "assistant", holding
    // live Gmail/Drive/web tools + `delegate_to_agent`) delegates to a
    // worker whose role is NOT "assistant" and which declares no
    // `[tools].allow` at all — exactly `engineer.toml`'s shape. Before this
    // fix, `is_assistant_tier` (keyed on the SPAWNED agent's role) was
    // `false` here, so `scope_assistant_allowed_tools` no-op'd and the spawn
    // kept its full registry — proved below via the `is_tainted=false`
    // baseline, which is the exact pre-fix call shape `run_subagent` used to
    // make unconditionally.
    let reg = build_registry_for_agent(
        "local-ops-agent",
        "engineer",
        None,
        None,
        empty_skill_registry(),
        empty_tag_registry(),
        None,
        crate::agents::AgentTier::L1Standard,
        None,
    )
    .expect("engineer-shaped registry builds");
    assert!(reg.contains("shell_exec"));

    // Pre-fix behavior (still reachable today for a genuinely untainted
    // spawn, e.g. pm/ctrl delegating directly — NOT a regression, just the
    // baseline this test contrasts against): unrestricted.
    let pre_fix_equivalent = scope_for_delegation(false, false, None, None, None, Some(&reg));
    assert_eq!(
        pre_fix_equivalent, None,
        "sanity: an untainted spawn is unrestricted, matching pre-fix behavior for \
         legitimate (non-assistant-originated) delegation"
    );

    // A representative slice of `assistant/agent.toml`'s actual `[tools].allow`
    // surface (delegate_to_agent, git_*, web_search, gmail — no shell, no
    // filesystem writes).
    let assistant_allow = vec![
        "delegate_to_agent".to_string(),
        "git_log".to_string(),
        "git_status".to_string(),
        "web_search".to_string(),
        "get_gmail_message_content".to_string(),
    ];
    let post_fix =
        scope_for_delegation(false, true, None, None, Some(&assistant_allow), Some(&reg))
            .expect("tainted delegation from an assistant-tier persona must always be scoped");

    assert!(
        !post_fix.iter().any(|n| n == "shell_exec"),
        "the delegate_to_agent(engineer) injection-to-RCE path must be closed: \
         shell_exec must not survive a tainted assistant-tier delegation; kept={post_fix:?}"
    );
    assert!(
        post_fix.is_empty(),
        "local-ops-agent's tool surface (shell_exec, read_file, list_dir, grep_files, \
         load_skill, list_skills) has no overlap with the assistant's own allow-list, so \
         the tainted spawn should be left with NO callable tools; kept={post_fix:?}"
    );
}

/// #3555 CRITICAL follow-up (code-critic) — live wiring check, `#[ignore]`d
/// by default.
///
/// Why: The deterministic regression tests for the role gate itself live in
/// `src/tools/delegate.rs` (`delegate_assistant_role_gate_rejects_*`), using
/// a hand-built `DelegateToAgentTool` + tempdir so they're fast and
/// environment-independent. This test instead exercises the REAL production
/// wiring — `build_assistant_tier_registry(None, AgentTier::L1Standard)` unmodified, resolving against
/// whatever roster is ACTUALLY deployed at `$HOME/.trusty-agents/agents/` on
/// the machine running it (via `agents::bundled::ensure_bundled_agents_deployed`,
/// which every `tagent` invocation runs at startup) — as an end-to-end proof
/// that the role gate is really wired into the registry an assistant-tier
/// sub-agent actually gets, not just proven in isolation. `#[ignore]`d
/// because CI's `$HOME` may not have a deployed roster at all (in which case
/// `pm` simply fails to resolve rather than exercising the role check
/// specifically) — this is a manual/local verification aid, not a CI gate.
/// Run with `cargo test -p trusty-agents --lib -- --ignored
/// live_assistant_registry_rejects_pm_role`.
/// What: builds the real registry, dispatches `delegate_to_agent` with
/// `agent_name: "pm"`, and asserts the result is an error. Never spawns a
/// subprocess (the role gate rejects before the runner is invoked) — no
/// network or credentials required either way.
/// Test: itself (manual verification only).
#[tokio::test]
#[ignore]
async fn live_assistant_registry_rejects_pm_role() {
    let reg = build_assistant_tier_registry(None, crate::agents::AgentTier::L1Standard, None);
    assert!(
        reg.contains("delegate_to_agent"),
        "sanity: delegate_to_agent must be registered"
    );

    let result = reg
        .dispatch(
            "delegate_to_agent",
            serde_json::json!({"agent_name": "pm", "task": "diagnostic ping"}),
        )
        .await;

    assert!(
        result.is_error(),
        "delegate_to_agent(agent_name=\"pm\") against the REAL deployed roster \
         must be rejected by the role gate, got: {}",
        result.content()
    );
}

/// #3556 regression test — closes the exact gap that shipped: bundled
/// template changes (e.g. PR-A's `delegate_to_agent` grant added to the
/// base `assistant` agent) never reached a machine that had already
/// deployed an older copy, because the deploy path only ever wrote MISSING
/// files. Deploying a FRESH tempdir and reading it back would pass
/// identically on pre-#3556 code (it never exercises the refresh path), so
/// this test instead seeds a genuinely STALE `assistant/agent.toml` — an old
/// allow-list WITHOUT `delegate_to_agent`, standing in for a machine that
/// deployed an older binary's bundle — plus a wrong stamp, then drives the
/// REAL refresh path (`ensure_bundled_agents_deployed_in`) before resolving.
/// This assertion sequence WOULD fail on pre-#3556 code (the stale file is
/// never rewritten). It then proves the end-to-end chain a real `tagent
/// --direct assistant` invocation depends on — the refreshed (on-disk)
/// `assistant` TOML, loaded via the REAL production resolver
/// (`AgentConfig::by_name`), scoped through the REAL assistant-tier registry
/// (`build_assistant_tier_registry`) and the REAL `scope_assistant_
/// allowed_tools` glob translation — carries `delegate_to_agent`. Unlike
/// `live_assistant_registry_rejects_pm_role` above, this is NOT `#[ignore]`d:
/// everything happens in an isolated tempdir, so it makes no environment
/// assumptions and runs in CI every time.
///
/// Uses `AgentConfig::by_name` (not `by_name_in`) via a `TAGENT_CONFIG_DIR`
/// override so the test exercises the SAME default-dirs entry point
/// `run_subagent` actually calls — `by_name` is documented as "a thin
/// wrapper over `by_name_in` with the default dirs"
/// (`agents/loader.rs`), and `agents_dir()` honors `TAGENT_CONFIG_DIR`
/// first, so pointing it at the tempdir makes that the sole resolved tier.
#[test]
fn deployed_assistant_config_survives_scoping_with_delegate_to_agent() {
    use crate::agents::AgentConfig;
    use crate::agents::tests::loading::ENV_LOCK;

    let _guard = ENV_LOCK.blocking_lock();
    let tmp = tempfile::tempdir().unwrap();

    // Establish a baseline deploy, then simulate a STALE on-disk
    // `assistant/agent.toml` — e.g. deployed before the `delegate_to_agent`
    // grant existed on the bundled template — the exact scenario #3556
    // fixes.
    let written = crate::agents::bundled::deploy_bundled_agents(tmp.path()).unwrap();
    assert!(
        written > 0,
        "sanity: bundled deploy wrote at least one file"
    );
    let assistant_toml = tmp.path().join("assistant").join("agent.toml");
    let stale_content = "[agent]\nname = \"assistant\"\nrole = \"assistant\"\n\n[tools]\nallow = [\"web_search\"]\n";
    std::fs::write(&assistant_toml, stale_content).unwrap();

    // Drive the REAL refresh path (#3556) — a stale/missing stamp must
    // rewrite the stale file to match the CURRENT embedded template. This is
    // the assertion that actually closes the gap: without the refresh
    // mechanism, `stale_content` (no `delegate_to_agent`) would still be on
    // disk below.
    let report = crate::agents::bundled::ensure_bundled_agents_deployed_in(tmp.path()).unwrap();
    assert!(
        report.refreshed > 0,
        "seeded stale assistant/agent.toml must trigger a refresh, got: {report:?}"
    );

    // SAFETY: guarded by ENV_LOCK, same convention as every other
    // TAGENT_CONFIG_DIR mutator in this crate (see `agents::tests::loading`).
    unsafe {
        std::env::set_var("TAGENT_CONFIG_DIR", tmp.path());
    }
    let cfg = AgentConfig::by_name("assistant");
    // SAFETY: see above.
    unsafe {
        std::env::remove_var("TAGENT_CONFIG_DIR");
    }
    let cfg = cfg.expect("refreshed assistant package must resolve via AgentConfig::by_name");

    assert!(
        cfg.tools
            .allow
            .as_ref()
            .is_some_and(|allow| allow.iter().any(|p| p == "delegate_to_agent")),
        "REFRESHED assistant TOML must declare delegate_to_agent in [tools].allow, got: {:?}",
        cfg.tools.allow
    );

    let reg = build_assistant_tier_registry(None, crate::agents::AgentTier::L1Standard, None);
    let kept = scope_assistant_allowed_tools(
        true,
        cfg.tools.allowed.clone(),
        cfg.tools.allow.as_deref(),
        Some(&reg),
    )
    .unwrap_or_default();

    assert!(
        kept.iter().any(|n| n == "delegate_to_agent"),
        "delegate_to_agent must survive the refresh -> deployed-config -> \
         registry-scoping chain, got: {kept:?}"
    );
}

// --- `parse_delegation_taint_env` (security fix: delegate_to_agent
// injection-to-RCE path) ---

#[test]
fn parse_delegation_taint_env_absent_is_untainted() {
    // The overwhelming majority of spawns: no env var at all, e.g. every
    // pm/ctrl-initiated coding sub-agent. Must be byte-identical to pre-fix
    // behavior — `None` in, `None` out.
    assert_eq!(parse_delegation_taint_env(None), None);
}

#[test]
fn parse_delegation_taint_env_valid_json_is_tainted() {
    let raw = r#"["delegate_to_agent","web_search"]"#.to_string();
    let parsed = parse_delegation_taint_env(Some(raw));
    assert_eq!(
        parsed,
        Some(vec![
            "delegate_to_agent".to_string(),
            "web_search".to_string()
        ])
    );
}

#[test]
fn parse_delegation_taint_env_empty_array_is_tainted_deny_all() {
    // An explicit empty allow-list (e.g. an assistant persona with no
    // `[tools].allow` resolved at all — should not happen in practice, but
    // must fail closed if it does) taints with a deny-all set, not `None`.
    let parsed = parse_delegation_taint_env(Some("[]".to_string()));
    assert_eq!(parsed, Some(Vec::new()));
}

#[test]
fn parse_delegation_taint_env_malformed_fails_closed() {
    // A corrupted/truncated env var (process env tampering, a future
    // encoding bug, …) must NEVER silently disable the taint — that would
    // reopen the exact hole this fix closes. It must fail CLOSED: an empty
    // (deny-all), not `None` (untainted/unrestricted).
    let parsed = parse_delegation_taint_env(Some("not valid json".to_string()));
    assert_eq!(
        parsed,
        Some(Vec::new()),
        "a malformed taint env var must fail closed (deny-all), never silently untainted"
    );
}

// --- `build_assistant_tier_registry`'s `delegator_tier` wiring (#4169,
// epic #4167 — one-directional L0/L1 delegation gate) ---
//
// Mirrors `deployed_assistant_config_survives_scoping_with_delegate_to_agent`'s
// approach: drive the REAL production wiring end-to-end (the actual
// `agents_dir_candidates()` resolution via `TAGENT_CONFIG_DIR`, the REAL
// `build_assistant_tier_registry`, dispatched through `reg.dispatch`) rather
// than constructing a `DelegateToAgentTool` by hand — proving `delegator_tier`
// genuinely reaches the tool this function registers, not just the
// `DelegateToAgentTool` unit tests in `tools::delegate`.
#[tokio::test]
async fn assistant_tier_registry_delegator_tier_blocks_l0_target_end_to_end() {
    use crate::agents::tests::loading::ENV_LOCK;

    let _guard = ENV_LOCK.lock().await;
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("orchestration-assistant.toml"),
        "[agent]\nname = \"orchestration-assistant\"\nrole = \"assistant\"\ntier = \"orchestration\"\nmodel = \"m\"\ndescription = \"d\"\n\n[llm]\ntemperature = 0.2\nmax_tokens = 1024\n\n[system_prompt]\ncontent = \"x\"\n",
    )
    .unwrap();

    // SAFETY: guarded by ENV_LOCK, same convention as every other
    // TAGENT_CONFIG_DIR mutator in this crate.
    unsafe {
        std::env::set_var("TAGENT_CONFIG_DIR", tmp.path());
    }
    let reg = build_assistant_tier_registry(None, crate::agents::AgentTier::L1Standard, None);
    unsafe {
        std::env::remove_var("TAGENT_CONFIG_DIR");
    }

    let result = reg
        .dispatch(
            "delegate_to_agent",
            serde_json::json!({
                "agent_name": "orchestration-assistant",
                "task": "escalate"
            }),
        )
        .await;

    assert!(
        result.is_error(),
        "an L1-tier assistant registry must refuse to delegate to an L0 target \
         resolved via the REAL agents_dir_candidates() wiring, got: {}",
        result.content()
    );
}

/// Counter-test: an L0-declared `delegator_tier` reaching the SAME L0
/// target must NOT be rejected by the tier gate through the identical real
/// wiring.
///
/// Why this doesn't assert overall success: `build_assistant_tier_registry`
/// wires a REAL `SubprocessAgentRunner` (unlike the mock-runner unit tests
/// in `tools::delegate`), so a call that clears pre-flight validation goes
/// on to actually spawn a subprocess — which fails in this test environment
/// for unrelated reasons (no real `orchestration-assistant` binary/roster to
/// spawn), exactly as the pre-existing `live_assistant_registry_rejects_pm_role`
/// test's doc comment notes for the same wiring. What THIS test pins is
/// narrower and fully verifiable without a live spawn: the error, if any,
/// must NOT be the tier-gate's specific refusal text — proving the gate
/// itself let the call through.
#[tokio::test]
async fn assistant_tier_registry_l0_delegator_tier_reaches_l0_target_end_to_end() {
    use crate::agents::tests::loading::ENV_LOCK;

    let _guard = ENV_LOCK.lock().await;
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("orchestration-assistant.toml"),
        "[agent]\nname = \"orchestration-assistant\"\nrole = \"assistant\"\ntier = \"orchestration\"\nmodel = \"m\"\ndescription = \"d\"\n\n[llm]\ntemperature = 0.2\nmax_tokens = 1024\n\n[system_prompt]\ncontent = \"x\"\n",
    )
    .unwrap();

    unsafe {
        std::env::set_var("TAGENT_CONFIG_DIR", tmp.path());
    }
    let reg = build_assistant_tier_registry(None, crate::agents::AgentTier::L0Orchestration, None);
    unsafe {
        std::env::remove_var("TAGENT_CONFIG_DIR");
    }

    let result = reg
        .dispatch(
            "delegate_to_agent",
            serde_json::json!({
                "agent_name": "orchestration-assistant",
                "task": "escalate"
            }),
        )
        .await;

    assert!(
        !result
            .content()
            .contains("orchestration-tier (L0) specialist"),
        "an L0-tier delegator must NOT be refused by the tier gate when reaching \
         an L0 target, got: {}",
        result.content()
    );
}

// ============================================================================
// #4173 (epic #4167) — the L0-orchestration shell/build/test execution grant.
//
// Shell execution is the capability that made #4126 a P0 (untrusted content ->
// persona -> delegation -> ungated shell). PR #4161 closed that path and
// PR #4200 added the L0/L1 tier boundary; #4173 deliberately hands shell back
// to exactly ONE tier. These tests exercise the REAL registry-construction
// path (`build_registry_for_agent` / `build_assistant_tier_registry`), the
// REAL glob-scoping and taint composition (`scope_assistant_allowed_tools` /
// `scope_for_delegation`), the REAL config loader (`AgentConfig::by_name_in`)
// and the REAL dispatch surface (`ToolRegistry::dispatch*`) — never a
// hand-rolled restatement of the gate.
// ============================================================================

/// Build the assistant-tier registry the way `run_subagent` does, for a
/// persona whose declared `tier` string is `raw` — resolved by the REAL
/// loader + the REAL fail-closed resolver, not by a hand-built `AgentTier`.
fn registry_for_declared_tier(raw: Option<&str>) -> (tempfile::TempDir, ToolRegistry) {
    registry_for_declared_tier_and_role("assistant", raw)
}

/// As above, but with the persona's `role` under test too — needed since
/// ADR-0024 decision 3 made an undeclared tier a function of the KIND.
fn registry_for_declared_tier_and_role(
    role: &str,
    raw: Option<&str>,
) -> (tempfile::TempDir, ToolRegistry) {
    let dir = tempfile::tempdir().unwrap();
    let tier_line = raw.map(|t| format!("tier = \"{t}\"")).unwrap_or_default();
    std::fs::write(
        dir.path().join("fixture-persona.toml"),
        format!(
            "[agent]\nname = \"fixture-persona\"\nrole = \"{role}\"\nmodel = \"m\"\n\
             description = \"d\"\n{tier_line}\n\n[llm]\ntemperature = 0.2\n\
             max_tokens = 1024\n\n[system_prompt]\ncontent = \"x\"\n"
        ),
    )
    .unwrap();
    let cfg =
        crate::agents::AgentConfig::by_name_in(&[dir.path().to_path_buf()], "fixture-persona")
            .expect("fixture agent loads");
    let reg = build_registry_for_agent(
        "fixture-persona",
        &cfg.agent.role,
        None,
        None,
        empty_skill_registry(),
        empty_tag_registry(),
        None,
        cfg.agent.tier(),
        None,
    )
    .expect("assistant-tier agent builds a registry");
    (dir, reg)
}

/// An L1 (standard) persona's registry must not contain the execution grant
/// at all — not merely be denied it at dispatch. L1 holds the full
/// Gmail/Drive/Calendar surface, which is exactly why it never gets a shell.
#[test]
fn l0_grant_absent_from_l1_assistant_registry() {
    let reg = build_registry_for_agent(
        "assistant",
        "assistant",
        None,
        None,
        empty_skill_registry(),
        empty_tag_registry(),
        None,
        crate::agents::AgentTier::L1Standard,
        None,
    )
    .expect("assistant-tier agent builds a registry");
    assert!(
        !reg.contains(crate::tools::l0_exec::L0_SHELL_EXEC),
        "an L1 persona must not have the L0 execution grant registered"
    );
    // The pre-#4173 curated surface is untouched.
    assert!(reg.contains("delegate_to_agent"));
    assert!(reg.contains("web_search"));
}

/// The counter-test: an L0 (orchestration) persona does get it, through the
/// same real construction path.
#[test]
fn l0_grant_present_in_l0_assistant_registry() {
    let reg = build_registry_for_agent(
        "orchestration-assistant",
        "assistant",
        None,
        None,
        empty_skill_registry(),
        empty_tag_registry(),
        None,
        crate::agents::AgentTier::L0Orchestration,
        None,
    )
    .expect("assistant-tier agent builds a registry");
    assert!(
        reg.contains(crate::tools::l0_exec::L0_SHELL_EXEC),
        "an L0 persona must receive the execution grant"
    );
}

/// THE requirement: an L1 persona that DECLARES the grant in `[tools].allow`
/// — by exact name, or by a wildcard that would match it — still does not
/// receive it. Exercised through the real glob-scoping gate and then through
/// the real dispatch surface, both layers.
#[tokio::test]
async fn l1_persona_declaring_the_l0_grant_does_not_receive_it() {
    let reg = build_registry_for_agent(
        "assistant",
        "assistant",
        None,
        None,
        empty_skill_registry(),
        empty_tag_registry(),
        None,
        crate::agents::AgentTier::L1Standard,
        None,
    )
    .expect("assistant-tier agent builds a registry");

    for allow in [
        vec![
            crate::tools::l0_exec::L0_SHELL_EXEC.to_string(),
            "web_search".to_string(),
        ],
        vec!["*".to_string()],
        vec!["l0_*".to_string()],
    ] {
        let kept =
            scope_assistant_allowed_tools(true, None, Some(&allow), Some(&reg)).unwrap_or_default();
        assert!(
            !kept
                .iter()
                .any(|n| n == crate::tools::l0_exec::L0_SHELL_EXEC),
            "declaring {allow:?} must not surface the L0 grant to an L1 persona, got: {kept:?}"
        );
        // Layer 2: even if a caller ignored the scoped list, dispatch has
        // nothing registered under the name.
        let denied = reg
            .dispatch_gated(
                crate::tools::l0_exec::L0_SHELL_EXEC,
                serde_json::json!({"command": "echo pwned"}),
                Some(&kept),
            )
            .await;
        assert!(
            denied.is_error(),
            "dispatch must refuse: {}",
            denied.content()
        );
        let unregistered = reg
            .dispatch(
                crate::tools::l0_exec::L0_SHELL_EXEC,
                serde_json::json!({"command": "echo pwned"}),
            )
            .await;
        assert!(
            unregistered.content().contains("no tool registered"),
            "the tool must not exist in an L1 registry at all, got: {}",
            unregistered.content()
        );
    }
}

/// Fail-closed on a malformed tier DECLARATION: a typo, or a near-miss that
/// merely CONTAINS "l0" (the shape a typo takes), resolves to L1 through the
/// real loader, so the registry it produces carries no grant.
///
/// ADR-0024 decision 3 (PR #4296) made an ABSENT/blank tier derive from the
/// agent's KIND instead, so those spellings are no longer "indeterminate" and
/// are covered by `l0_grant_follows_the_kind_derivation` below. What must NOT
/// happen — and is what this test pins — is a declared-but-unrecognized string
/// falling THROUGH to that derivation and quietly electing an assistant to L0
/// by way of a typo. The fixture is assistant-kind precisely so that
/// fall-through would be visible here.
#[test]
fn l0_grant_fails_closed_for_malformed_tier_declaration() {
    for raw in [
        Some("bogus"),
        Some("l0-ish"),
        Some("not-l0"),
        Some("l1"),
        Some("L2"),
    ] {
        let (_dir, reg) = registry_for_declared_tier(raw);
        assert!(
            !reg.contains(crate::tools::l0_exec::L0_SHELL_EXEC),
            "declared tier {raw:?} is unrecognized and must be DENIED the execution grant"
        );
    }
    // And the positive control through the same helper, so a broken fixture
    // cannot make the assertions above vacuously pass.
    let (_dir, reg) = registry_for_declared_tier(Some("l0"));
    assert!(reg.contains(crate::tools::l0_exec::L0_SHELL_EXEC));
}

/// With no usable declaration the grant follows ADR-0024 decision 3's KIND
/// derivation, through the real registry-construction path: assistant-kind
/// registers it, every sub-agent kind does not.
///
/// The registration is not by itself a capability — reachability still requires
/// the persona's own `[tools].allow` to name the tool, which no bundled
/// assistant does (pinned by
/// `agents::tests::loading::bundled_assistant_personas_resolve_l0_and_gain_nothing`,
/// extended by this PR to cover the execution grant).
#[test]
fn l0_grant_follows_the_kind_derivation() {
    for raw in [None, Some(""), Some("   ")] {
        let (_dir, reg) = registry_for_declared_tier_and_role("assistant", raw);
        assert!(
            reg.contains(crate::tools::l0_exec::L0_SHELL_EXEC),
            "assistant-kind with tier {raw:?} derives L0 and registers the grant"
        );

        for role in ["engineer", "researcher", "planner"] {
            let (_dir, reg) = registry_for_declared_tier_and_role(role, raw);
            assert!(
                !reg.contains(crate::tools::l0_exec::L0_SHELL_EXEC),
                "sub-agent kind {role:?} with tier {raw:?} must be denied the grant"
            );
        }
    }
}

/// The taint must still NARROW, never widen. When an L0 persona delegates
/// downward to an L1 target, the child is tainted with the L0 delegator's own
/// `[tools].allow` — which legitimately names the execution grant. The child's
/// registry is an L1 registry and never registered that tool, so the real
/// `scope_for_delegation` intersection cannot conjure it: the taint grants
/// only what the child's registry already holds.
#[test]
fn tainted_delegation_cannot_widen_into_the_l0_grant() {
    let child_reg = build_registry_for_agent(
        "assistant",
        "assistant",
        None,
        None,
        empty_skill_registry(),
        empty_tag_registry(),
        None,
        crate::agents::AgentTier::L1Standard,
        None,
    )
    .expect("assistant-tier agent builds a registry");

    let l0_delegator_allow = vec![
        crate::tools::l0_exec::L0_SHELL_EXEC.to_string(),
        "web_search".to_string(),
        "delegate_to_agent".to_string(),
    ];
    let scoped = scope_for_delegation(
        true,
        true,
        None,
        None,
        Some(&l0_delegator_allow),
        Some(&child_reg),
    )
    .unwrap_or_default();

    assert!(
        !scoped
            .iter()
            .any(|n| n == crate::tools::l0_exec::L0_SHELL_EXEC),
        "a taint carrying the L0 grant must not widen an L1 child into it, got: {scoped:?}"
    );
    assert!(
        scoped.iter().any(|n| n == "web_search"),
        "the taint must still admit tools the child registry does hold, got: {scoped:?}"
    );

    // Composition, at the pattern level, is likewise narrowing-only: an L1
    // agent's own native allow-list intersected with an inbound taint can
    // never gain a pattern neither side held.
    let composed =
        compose_delegation_taint(Some(&["web_search".to_string()]), Some(&l0_delegator_allow))
            .unwrap_or_default();
    assert_eq!(composed, vec!["web_search".to_string()]);
}

/// Transitive reach: an L1 persona must not obtain the grant by delegating
/// through an intermediary. Both legs are checked on the real paths — the
/// intermediary's OWN registry (built from its real resolved tier) never
/// carries the grant, and its `delegate_to_agent` refuses the L0 target that
/// would.
#[tokio::test]
async fn l1_cannot_reach_the_l0_grant_through_an_untiered_intermediary() {
    use crate::agents::tests::loading::ENV_LOCK;

    let _guard = ENV_LOCK.lock().await;
    let tmp = tempfile::tempdir().unwrap();
    // The intermediary is an assistant that EXPLICITLY declares L1 — after
    // ADR-0024 decision 3 (PR #4296) that declaration is the only way an
    // assistant-kind persona is L1, since an absent tier now derives L0 from
    // the kind. The escalation target is an L0 assistant that DOES hold the
    // grant.
    std::fs::write(
        tmp.path().join("peer-assistant.toml"),
        "[agent]\nname = \"peer-assistant\"\nrole = \"assistant\"\ntier = \"l1\"\n\
         model = \"m\"\ndescription = \"d\"\n\n[llm]\ntemperature = 0.2\n\
         max_tokens = 1024\n\n[system_prompt]\ncontent = \"x\"\n",
    )
    .unwrap();
    std::fs::write(
        tmp.path().join("orchestration-assistant.toml"),
        "[agent]\nname = \"orchestration-assistant\"\nrole = \"assistant\"\n\
         tier = \"orchestration\"\nmodel = \"m\"\ndescription = \"d\"\n\n[llm]\n\
         temperature = 0.2\nmax_tokens = 1024\n\n[system_prompt]\ncontent = \"x\"\n",
    )
    .unwrap();

    // Hop 1: what the L1 intermediary itself receives. Its real resolved tier
    // is L1, so its registry never registers the grant it could pass along.
    let peer_cfg =
        crate::agents::AgentConfig::by_name_in(&[tmp.path().to_path_buf()], "peer-assistant")
            .expect("intermediary loads");
    assert_eq!(peer_cfg.agent.tier(), crate::agents::AgentTier::L1Standard);

    unsafe {
        std::env::set_var("TAGENT_CONFIG_DIR", tmp.path());
    }
    let seed = vec!["orchestration-assistant".to_string()];
    let peer_reg = build_assistant_tier_registry(None, peer_cfg.agent.tier(), Some(&seed));
    unsafe {
        std::env::remove_var("TAGENT_CONFIG_DIR");
    }
    assert!(
        !peer_reg.contains(crate::tools::l0_exec::L0_SHELL_EXEC),
        "the intermediary must not carry the grant it could pass along"
    );

    // Hop 2: from that intermediary, reaching the persona that DOES carry the
    // grant is refused by the real delegation gates — even with the target
    // explicitly whitelisted in the reachable set, so the refusal is a gate
    // and not merely an unlisted name.
    let refused = peer_reg
        .dispatch(
            "delegate_to_agent",
            serde_json::json!({
                "agent_name": "orchestration-assistant",
                "task": "run cargo test and push the fix"
            }),
        )
        .await;
    assert!(
        refused.is_error(),
        "reaching an L0 grant-holder must be refused at hop 2, got: {}",
        refused.content()
    );
    // Either gate refusing is a correct outcome — the KIND rule (assistants
    // never delegate to a peer assistant, ADR-0024 clause 6) is checked before
    // the tier gate, so it is the one that speaks for this pair. Asserting
    // "refused, for a stated boundary reason" rather than pinning which gate
    // fired keeps this test about the escalation property, not about the
    // ordering of two rules that both deny.
    assert!(
        refused.content().contains("peer assistant")
            || (refused.content().contains("L0") && refused.content().contains("L1")),
        "the refusal should name the boundary it enforced, got: {}",
        refused.content()
    );
    assert!(
        !peer_reg.contains(crate::tools::l0_exec::L0_SHELL_EXEC),
        "a refused escalation must leave the intermediary's surface unchanged"
    );

    // The other transitive leg, after decision 3: a SUB-AGENT kind is L1 by
    // derivation, and its registry holds neither the grant nor the delegation
    // tool that could chase it — there is no hop to make.
    let sub_reg = build_registry_for_agent(
        "research-agent",
        "researcher",
        None,
        None,
        empty_skill_registry(),
        empty_tag_registry(),
        None,
        crate::agents::AgentTier::L1Standard,
        None,
    )
    .expect("sub-agent builds a registry");
    assert!(
        !sub_reg.contains(crate::tools::l0_exec::L0_SHELL_EXEC),
        "a sub-agent kind must never hold the execution grant"
    );
}

// --- `build_assistant_tier_registry`'s `delegator_subagents` wiring
// (ADR-0024 decision 4, owner ratification 2026-07-29) ---
//
// Same end-to-end approach as the `delegator_tier` pair above: drive the REAL
// production wiring (real `agents_dir_candidates()` resolution via
// `TAGENT_CONFIG_DIR`, the REAL `build_assistant_tier_registry`, dispatched
// through `reg.dispatch`) so a call site that dropped the whitelist argument
// fails here, not only in `tools::delegate`'s unit tests.

/// A target this registry's agent whitelisted is NOT refused by the
/// reachable-set gate.
///
/// Why: the fail-closed test below would pass against a registry that refused
/// everything, which would be a worse regression than the hole decision 4
/// closed. This is the non-vacuity half.
/// What: like `assistant_tier_registry_l0_delegator_tier_reaches_l0_target_end_to_end`,
/// this asserts the NEGATIVE (the error, if any, is not the reachable-set
/// refusal) rather than overall success — `build_assistant_tier_registry` wires
/// a REAL `SubprocessAgentRunner`, so a call that clears pre-flight goes on to
/// spawn, which fails in this environment for unrelated reasons.
/// Test: this function IS the test.
#[tokio::test]
async fn assistant_tier_registry_delegation_honors_the_reachable_whitelist() {
    use crate::agents::tests::loading::ENV_LOCK;

    let _guard = ENV_LOCK.lock().await;
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("research-agent.toml"),
        "[agent]\nname = \"research-agent\"\nrole = \"researcher\"\nmodel = \"m\"\ndescription = \"d\"\n\n[llm]\ntemperature = 0.2\nmax_tokens = 1024\n\n[system_prompt]\ncontent = \"x\"\n",
    )
    .unwrap();

    // SAFETY: guarded by ENV_LOCK, same convention as every other
    // TAGENT_CONFIG_DIR mutator in this crate.
    unsafe {
        std::env::set_var("TAGENT_CONFIG_DIR", tmp.path());
    }
    let seed = vec!["research-agent".to_string()];
    let reg =
        build_assistant_tier_registry(None, crate::agents::AgentTier::L0Orchestration, Some(&seed));
    unsafe {
        std::env::remove_var("TAGENT_CONFIG_DIR");
    }

    let result = reg
        .dispatch(
            "delegate_to_agent",
            serde_json::json!({ "agent_name": "research-agent", "task": "investigate" }),
        )
        .await;

    assert!(
        !result.content().contains("available to you"),
        "a whitelisted target must clear the reachable-set gate, got: {}",
        result.content()
    );
}

/// ADR-0024 decision 4 sub-answer (a) through the REAL registry wiring: an
/// agent whose config declares NO whitelist reaches nothing.
///
/// Why: `delegator_subagents` is threaded through two function signatures
/// before it reaches the tool. A regression that dropped it — passing `None`
/// unconditionally — would be invisible to `tools::delegate`'s unit tests,
/// which construct the tool directly. This test is what makes the plumbing
/// itself load-bearing.
/// What: the same fixture as the test above, with `None` for the whitelist, is
/// refused with the reachable-set message.
/// Test: this function IS the test.
#[tokio::test]
async fn assistant_tier_registry_delegation_fails_closed_without_a_whitelist() {
    use crate::agents::tests::loading::ENV_LOCK;

    let _guard = ENV_LOCK.lock().await;
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("research-agent.toml"),
        "[agent]\nname = \"research-agent\"\nrole = \"researcher\"\nmodel = \"m\"\ndescription = \"d\"\n\n[llm]\ntemperature = 0.2\nmax_tokens = 1024\n\n[system_prompt]\ncontent = \"x\"\n",
    )
    .unwrap();

    // SAFETY: guarded by ENV_LOCK.
    unsafe {
        std::env::set_var("TAGENT_CONFIG_DIR", tmp.path());
    }
    let reg = build_assistant_tier_registry(None, crate::agents::AgentTier::L0Orchestration, None);
    unsafe {
        std::env::remove_var("TAGENT_CONFIG_DIR");
    }

    let result = reg
        .dispatch(
            "delegate_to_agent",
            serde_json::json!({ "agent_name": "research-agent", "task": "investigate" }),
        )
        .await;

    assert!(result.is_error());
    assert!(
        result.content().contains("available to you"),
        "an absent whitelist must refuse even a floor member, got: {}",
        result.content()
    );
}

// --- #4170 (epic #4167): the L0-only GitHub PR/CI surface, proven through the
// REAL registry-construction path ---
//
// Why these are here and not only in `tools::gh_tools::tests`: the factory's
// own tier check is easy to prove in isolation, and equally easy to render
// irrelevant by a call site that never consults it. These tests therefore
// exercise the SAME chain `run_subagent` executes for a persona —
// `AgentConfig::by_name` (via a `TAGENT_CONFIG_DIR` override, exactly as
// `deployed_assistant_config_survives_scoping_with_delegate_to_agent` does)
// -> `cfg.agent.tier()` -> `build_registry_for_agent` -> the REAL
// `scope_assistant_allowed_tools` glob translation — so the assertion is
// about what a persona ACTUALLY receives, not about a helper's return value.

/// Write a persona TOML whose `[tools].allow` asks for the whole GitHub
/// surface plus the widest possible wildcard, and resolve it through the real
/// loader + registry + scoping chain.
///
/// Why: `allow = ["*", "gh_*", <every literal name>]` is the most generous
/// declaration a persona author could possibly write. If the tier gate holds
/// against THAT, it holds against anything narrower — and using the literal
/// names from `GH_TOOL_NAMES` means a tool added to the factory without a tier
/// check is caught here rather than slipping through a stale hand-copied list.
/// What: Returns the tool names the persona is actually granted.
/// Test: used by `l1_persona_declaring_gh_tools_is_granted_none_through_the_real_path`,
/// `l0_persona_declaring_gh_tools_is_granted_them_through_the_real_path`,
/// `unrecognized_tier_declaration_denies_gh_tools_through_the_real_path`,
/// `gh_tools_follow_the_kind_derivation_when_no_tier_is_declared`.
fn granted_tools_for_persona_with_tier(tier_line: &str) -> Vec<String> {
    use crate::agents::AgentConfig;
    use crate::agents::tests::loading::ENV_LOCK;

    let _guard = ENV_LOCK.blocking_lock();
    let tmp = tempfile::tempdir().unwrap();
    let allow = crate::tools::gh_tools::GH_TOOL_NAMES
        .iter()
        .map(|n| format!("\"{n}\""))
        .collect::<Vec<_>>()
        .join(", ");
    std::fs::write(
        tmp.path().join("gh-probe.toml"),
        format!(
            "[agent]\nname = \"gh-probe\"\nrole = \"assistant\"\n{tier_line}\
             model = \"m\"\ndescription = \"d\"\n\n[llm]\ntemperature = 0.2\n\
             max_tokens = 1024\n\n[tools]\nallow = [\"*\", \"gh_*\", {allow}]\n\n\
             [system_prompt]\ncontent = \"x\"\n"
        ),
    )
    .unwrap();

    // SAFETY: guarded by ENV_LOCK, the same convention every other
    // TAGENT_CONFIG_DIR mutator in this crate follows.
    unsafe {
        std::env::set_var("TAGENT_CONFIG_DIR", tmp.path());
    }
    let cfg = AgentConfig::by_name("gh-probe");
    let cfg = cfg.expect("probe persona must resolve via the real loader");
    let reg = build_registry_for_agent(
        "gh-probe",
        &cfg.agent.role,
        None,
        None,
        empty_skill_registry(),
        empty_tag_registry(),
        cfg.tools.allow.as_deref(),
        cfg.agent.tier(),
        // The same expression `run_subagent` passes (#4314's editable
        // reachable-set whitelist) — this probe declares no `[subagents]`
        // section, so it resolves to `None` and reaches nothing, which is
        // irrelevant to the gh surface but keeps the call on the real path.
        cfg.subagents.delegate_allowed.as_deref(),
    );
    // SAFETY: see above.
    unsafe {
        std::env::remove_var("TAGENT_CONFIG_DIR");
    }
    let reg = reg.expect("an assistant-role persona always gets a registry");
    scope_assistant_allowed_tools(
        true,
        cfg.tools.allowed.clone(),
        cfg.tools.allow.as_deref(),
        Some(&reg),
    )
    .unwrap_or_default()
}

#[test]
fn l1_persona_declaring_gh_tools_is_granted_none_through_the_real_path() {
    // THE requirement: grantable to L0 only, enforced in code. This persona
    // declares every GitHub tool by name AND `gh_*` AND `*`, and still gets
    // none of them, because the registry it is scoped against never contained
    // them.
    let kept = granted_tools_for_persona_with_tier("tier = \"l1\"\n");
    assert!(
        !kept.is_empty(),
        "sanity: an `allow = [\"*\"]` persona must be granted SOMETHING, else \
         this test would pass vacuously"
    );
    for name in crate::tools::gh_tools::GH_TOOL_NAMES {
        assert!(
            !kept.iter().any(|k| k == name),
            "an L1 persona obtained {name} despite the tier gate; granted: {kept:?}"
        );
    }
}

#[test]
fn l0_persona_declaring_gh_tools_is_granted_them_through_the_real_path() {
    // The counter-test that keeps the one above honest: the identical chain
    // with `tier = "l0"` DOES yield the surface, so the L1 denial is the tier
    // gate at work and not a registration bug affecting everyone.
    let kept = granted_tools_for_persona_with_tier("tier = \"l0\"\n");
    for name in crate::tools::gh_tools::GH_TOOL_NAMES {
        assert!(
            kept.iter().any(|k| k == name),
            "an L0 persona must receive {name}; granted: {kept:?}"
        );
    }
}

#[test]
fn unrecognized_tier_declaration_denies_gh_tools_through_the_real_path() {
    // Fail closed: an unrecognized `tier =` value resolves to `L1Standard` via
    // `AgentInfo::tier()` (#4200) and must therefore deny — never "grant
    // because we could not tell", and never fall THROUGH to the assistant-kind
    // derivation ADR-0024 decision 3 (PR #4296) added. This probe persona is
    // assistant-kind, so a fall-through would show up here as a grant.
    for tier_line in [
        "tier = \"L0-ish\"\n",
        "tier = \"not-l0\"\n",
        "tier = \"bogus\"\n",
        "tier = \"l1\"\n",
    ] {
        let kept = granted_tools_for_persona_with_tier(tier_line);
        for name in crate::tools::gh_tools::GH_TOOL_NAMES {
            assert!(
                !kept.iter().any(|k| k == name),
                "tier line {tier_line:?} granted {name}; an unrecognized tier must deny"
            );
        }
    }
}

/// The DERIVED case, after ADR-0024 decision 3 (PR #4296, on `main`): with no
/// usable `tier =` declaration the tier is a function of the agent's KIND.
///
/// Why this lives here: #4170 was written when an absent tier meant L1, so the
/// spellings below used to be deny cases. They are now GRANT cases for
/// assistant-kind — not because this PR widened anything (`gh_tools` still
/// returns an empty vector for every tier but L0), but because `main` changed
/// which personas are L0. Pinning it at the grant site means a future change to
/// that population fails here instead of silently redefining this surface's
/// reach. The tools remain read-only in every case — see
/// `gh_surface_registers_no_mutating_tool_even_for_l0`.
#[test]
fn gh_tools_follow_the_kind_derivation_when_no_tier_is_declared() {
    for tier_line in ["", "tier = \"\"\n", "tier = \"   \"\n"] {
        let kept = granted_tools_for_persona_with_tier(tier_line);
        for name in crate::tools::gh_tools::GH_TOOL_NAMES {
            assert!(
                kept.iter().any(|k| k == name),
                "tier line {tier_line:?} on an assistant-kind persona derives L0 \
                 (ADR-0024 decision 3) and must grant {name}; granted: {kept:?}"
            );
        }
    }

    // And the sub-agent kinds it must NOT reach. A researcher is L1 by
    // derivation, and `build_registry_for_agent` does not even route it into
    // the assistant-tier registry, so the surface is absent twice over.
    let reg = build_registry_for_agent(
        "research-agent",
        "researcher",
        None,
        None,
        empty_skill_registry(),
        empty_tag_registry(),
        None,
        crate::agents::AgentTier::L1Standard,
        None,
    )
    .expect("sub-agent builds a registry");
    for name in crate::tools::gh_tools::GH_TOOL_NAMES {
        assert!(
            !reg.contains(name),
            "a sub-agent kind must never hold {name}"
        );
    }
}

#[test]
fn assistant_tier_registry_omits_gh_tools_for_l1() {
    // The narrower registration-level statement, on the function the
    // `--direct`/`--agent` subprocess path calls.
    let reg = build_assistant_tier_registry(None, crate::agents::AgentTier::L1Standard, None);
    assert!(
        reg.contains("git_log") || reg.contains("web_search"),
        "sanity: the L1 registry is not empty"
    );
    for name in crate::tools::gh_tools::GH_TOOL_NAMES {
        assert!(!reg.contains(name), "{name} must not be registered for L1");
    }
}

#[test]
fn assistant_tier_registry_includes_gh_tools_for_l0() {
    let reg = build_assistant_tier_registry(None, crate::agents::AgentTier::L0Orchestration, None);
    for name in crate::tools::gh_tools::GH_TOOL_NAMES {
        assert!(reg.contains(name), "{name} must be registered for L0");
    }
}

#[test]
fn gh_surface_registers_no_mutating_tool_even_for_l0() {
    // #4170 forbids quietly granting merge power. Asserted against the real
    // L0 registry rather than the factory, so a future edit that adds a
    // mutating tool at THIS call site is caught too.
    let reg = build_assistant_tier_registry(None, crate::agents::AgentTier::L0Orchestration, None);
    for name in [
        "gh_pr_merge",
        "gh_pr_create",
        "gh_pr_comment",
        "gh_pr_edit",
        "gh_pr_close",
        "gh_workflow_run",
        "gh_run_rerun",
    ] {
        assert!(
            !reg.contains(name),
            "{name} is a mutating capability and must not be registered"
        );
    }
}
