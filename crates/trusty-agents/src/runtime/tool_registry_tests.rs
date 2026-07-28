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
    let reg = build_assistant_tier_registry(None);
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
    let reg = build_assistant_tier_registry(None);
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

#[test]
fn multi_hop_delegation_taint_narrows_not_widens() {
    // Code-critic HIGH 1 follow-up on PR #4161: taint composition across a
    // 3-hop chain `assistant -> izzie -> engineer`, the peer-assistant lane
    // `ASSISTANT_ALLOWED_DELEGATE_ROLES` explicitly supports.
    //
    // The bug: `build_registry_for_agent` (and therefore izzie's OWN
    // outbound `delegate_to_agent`, built by `build_assistant_delegate_tool`
    // when izzie's role is itself "assistant") is necessarily constructed
    // using izzie's NATIVE `[tools].allow` — `cfg.tools.allowed` (izzie's
    // EFFECTIVE, already-taint-narrowed posture) cannot exist yet at that
    // point, because computing it needs the registry to already contain a
    // `delegate_to_agent` entry. If izzie's outbound taint is never
    // corrected afterward, a tainted izzie forwards its full NATIVE
    // allow-list to whatever it delegates to next — even patterns izzie's
    // OWN inbound taint from `assistant` never granted it. This is
    // exploitable precisely because a native `[tools].allow` glob list is
    // just strings: a pattern like `"shell_exec"` sitting in izzie's TOML
    // matches nothing in izzie's OWN curated registry (which never
    // registers a real `shell_exec` tool), so it is inert for izzie's own
    // turn — but the SAME string, forwarded verbatim as the next hop's
    // taint, matches a real `shell_exec` tool in engineer's registry.
    //
    // This test mirrors, at the pure-function level, the exact composition
    // `runtime::subagent_mode::run_subagent` performs: build the registry
    // with the native posture (unavoidable, first pass), compute the
    // effective posture via `scope_for_delegation`, then recompute the
    // OUTBOUND taint as `cfg.tools.allowed.or(cfg.tools.allow)` — the fixed
    // formula — and contrasts it against the PRE-FIX formula (always
    // forward the native list) to prove the fix is load-bearing, not
    // incidental.
    // Test: this test IS the regression coverage code-critic asked for
    // ("fails without this fix").

    // izzie's native `[tools].allow` (hop 2's own TOML) — deliberately
    // broader than what `assistant` (hop 1) actually grants it, including a
    // pattern ("shell_exec") that resolves to nothing in izzie's OWN
    // registry but WOULD resolve against a coding agent's registry. This
    // models exactly the risk the code-critic flagged: "a user-authored
    // peer persona could reintroduce it."
    let izzie_native_allow = vec![
        "delegate_to_agent".to_string(),
        "web_search".to_string(),
        "shell_exec".to_string(),
    ];

    // The taint `assistant` (hop 1) actually forwards to izzie — narrower:
    // no `shell_exec`.
    let inbound_taint_from_assistant =
        vec!["delegate_to_agent".to_string(), "web_search".to_string()];

    // Hop 1 -> 2: build izzie's registry (first pass, native posture —
    // unavoidable per the doc comment on `build_assistant_delegate_tool`),
    // then compute izzie's EFFECTIVE posture given the inbound taint.
    let izzie_reg = build_registry_for_agent(
        "izzie",
        "assistant",
        None,
        None,
        empty_skill_registry(),
        empty_tag_registry(),
        Some(&izzie_native_allow),
    )
    .expect("izzie (role=assistant) builds a registry");
    assert!(
        !izzie_reg.contains("shell_exec"),
        "fixture assumption: izzie's own curated registry never registers a real \
         shell_exec tool, regardless of what strings its [tools].allow lists"
    );

    let izzie_effective = scope_for_delegation(
        true,
        true,
        None,
        None,
        Some(&inbound_taint_from_assistant),
        Some(&izzie_reg),
    )
    .expect("a tainted assistant-tier spawn is always scoped");
    assert!(
        !izzie_effective.iter().any(|n| n == "shell_exec"),
        "izzie's effective posture must not contain shell_exec: {izzie_effective:?}"
    );

    // Hop 2 -> 3: the FIXED outbound formula (`cfg.tools.allowed.or(cfg.tools.allow)`,
    // `runtime::subagent_mode::run_subagent`'s post-scoping fix-up) vs. the
    // PRE-FIX formula (always the native list, ignoring any inbound taint).
    let izzie_outbound_fixed: Option<Vec<String>> =
        Some(izzie_effective.clone()).or_else(|| Some(izzie_native_allow.clone()));
    let izzie_outbound_pre_fix: Vec<String> = izzie_native_allow.clone();

    let engineer_reg = build_registry_for_agent(
        "local-ops-agent",
        "engineer",
        None,
        None,
        empty_skill_registry(),
        empty_tag_registry(),
        None,
    )
    .expect("engineer-shaped registry builds");
    assert!(
        engineer_reg.contains("shell_exec"),
        "fixture assumption: engineer's registry has a REAL shell_exec tool"
    );

    let fixed_result = scope_for_delegation(
        false,
        true,
        None,
        None,
        izzie_outbound_fixed.as_deref(),
        Some(&engineer_reg),
    )
    .expect("tainted spawn is always scoped");
    assert!(
        !fixed_result.iter().any(|n| n == "shell_exec"),
        "FIXED: a 3-hop assistant -> izzie -> engineer chain must not let engineer reach \
         shell_exec, which izzie's own inbound taint never granted; kept={fixed_result:?}"
    );

    // Prove the test actually distinguishes fixed from buggy behavior: the
    // PRE-FIX formula (izzie always forwarding its native list) DOES leak
    // shell_exec to engineer. If a future change reintroduces the bug (e.g.
    // reverts the `.or_else` fix-up to always use the native list), this
    // assertion pins the exact failure mode `fixed_result` above must never
    // exhibit.
    let buggy_result = scope_for_delegation(
        false,
        true,
        None,
        None,
        Some(&izzie_outbound_pre_fix),
        Some(&engineer_reg),
    )
    .expect("tainted spawn is always scoped");
    assert!(
        buggy_result.iter().any(|n| n == "shell_exec"),
        "sanity: the PRE-FIX formula (forwarding izzie's native allow-list unconditionally) \
         must actually be exploitable in this fixture, or this test proves nothing; \
         kept={buggy_result:?}"
    );
}

/// #3555 CRITICAL follow-up (code-critic) — live wiring check, `#[ignore]`d
/// by default.
///
/// Why: The deterministic regression tests for the role gate itself live in
/// `src/tools/delegate.rs` (`delegate_assistant_role_gate_rejects_*`), using
/// a hand-built `DelegateToAgentTool` + tempdir so they're fast and
/// environment-independent. This test instead exercises the REAL production
/// wiring — `build_assistant_tier_registry(None)` unmodified, resolving against
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
    let reg = build_assistant_tier_registry(None);
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

    let reg = build_assistant_tier_registry(None);
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

// --- `resolve_taint_posture` (security fix: delegate_to_agent
// injection-to-RCE path, code-critic HIGH 2 follow-up on PR #4161) ---

#[test]
fn resolve_taint_posture_defaults_to_deny_all_when_none() {
    // `build_assistant_delegate_tool`'s fail-closed default: `None` (no
    // resolved posture) must become an EMPTY Vec, not skip tainting.
    assert_eq!(resolve_taint_posture(None), Vec::<String>::new());
}

#[test]
fn resolve_taint_posture_preserves_some() {
    let patterns = vec!["delegate_to_agent".to_string(), "web_search".to_string()];
    assert_eq!(resolve_taint_posture(Some(&patterns)), patterns);
}

// --- Structural backstop: every `DelegateToAgentTool::new(...)` construction
// site in the crate (code-critic MEDIUM follow-up on PR #4161) ---

/// The fail-open default this fix deliberately keeps (absent taint signal ->
/// `None` -> pre-fix behavior) means a FUTURE construction site that
/// registers `DelegateToAgentTool` for an assistant-tier role but forgets to
/// wire `.with_delegation_taint(...)` fails OPEN — silently, in production —
/// exactly how #4126 happened (`engineer.toml` had no `[tools].allow` and
/// nobody noticed `scope_assistant_allowed_tools` never applied to it).
///
/// Why: A missed call site is a code-review failure mode, not something a
/// pure-function unit test can catch (`scope_for_delegation`,
/// `resolve_taint_posture`, etc. are all correct in isolation — the risk is
/// a NEW site never calling into them at all). This test is a structural
/// scan, not a functional one: it enumerates every real
/// `DelegateToAgentTool::new(` occurrence under `src/`, asserts the found
/// set EXACTLY matches `EXPECTED_SITES` (so an added OR removed site fails
/// the test immediately, forcing a conscious audit — see the assertion
/// message), then for every site flagged `must_taint = true`, asserts the
/// file actually contains a `.with_delegation_taint(` call.
/// What: `EXPECTED_SITES` is a hand-audited table, current as of PR #4161:
///   - `runtime/tool_registry.rs` (`build_assistant_delegate_tool`) — ALWAYS
///     assistant-tier (only ever called when `role == ASSISTANT_TIER_ROLE`).
///   - `ctrl/pm_task/dispatch/persona.rs` (`run_pm_task_with_persona`) —
///     conditionally assistant-tier (any agent name via `/agent`); the file
///     must still contain a `.with_delegation_taint(` call reachable when
///     `persona_cfg.agent.role == "assistant"`.
///   - `ctrl/pm_task/dispatch/history.rs` (`run_pm_task_with_history`) — the
///     CTRL orchestrator's OWN top-level session; role is always
///     `"controller"`/`"orchestrator"`, never assistant-tier. No taint.
///   - `runtime/pm_mode.rs` (`run_pm`) — the `pm` orchestrator's OWN
///     registry; role is always `"orchestrator"`. No taint.
/// Test: this IS the test — `every_delegate_to_agent_construction_site_has_an_audited_taint_posture`.
#[test]
fn every_delegate_to_agent_construction_site_has_an_audited_taint_posture() {
    /// (path relative to the crate root, must this site's file contain a
    /// `.with_delegation_taint(` call).
    const EXPECTED_SITES: &[(&str, bool)] = &[
        ("src/runtime/tool_registry.rs", true),
        ("src/ctrl/pm_task/dispatch/persona.rs", true),
        ("src/ctrl/pm_task/dispatch/history.rs", false),
        ("src/runtime/pm_mode.rs", false),
    ];

    fn collect_rs_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_rs_files(&path, out);
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            // Exclude test-support files (this test's OWN file included —
            // `tool_registry_tests.rs` necessarily contains the literal
            // string `"DelegateToAgentTool::new("` as part of ITS OWN
            // detection logic + doc comments, which would otherwise
            // self-match and corrupt `found_sites`). Production call sites
            // never live in a `*_test.rs`/`*_tests.rs` file.
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default();
            if stem.ends_with("_test") || stem.ends_with("_tests") {
                continue;
            }
            out.push(path);
        }
    }

    let crate_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut rs_files = Vec::new();
    collect_rs_files(&crate_root.join("src"), &mut rs_files);

    let mut found_sites: Vec<String> = Vec::new();
    for file in &rs_files {
        let Ok(content) = std::fs::read_to_string(file) else {
            continue;
        };
        // A REAL construction call, not a doc-comment mention (e.g.
        // `tools/delegate.rs`'s own `/// Test: DelegateToAgentTool::new(...)`
        // doc comment) or this test's own literal string above. `.contains`
        // (not `.starts_with`) because a real call is sometimes nested,
        // e.g. `registry.register(Arc::new(DelegateToAgentTool::new(runner)))`
        // (`runtime/pm_mode.rs`) — only doc-comment lines (`///`/`//!`/`//`)
        // are excluded.
        let has_real_call = content.lines().any(|line| {
            let trimmed = line.trim_start();
            !trimmed.starts_with("//") && trimmed.contains("DelegateToAgentTool::new(")
        });
        if has_real_call {
            let rel = file
                .strip_prefix(crate_root)
                .unwrap_or(file)
                .to_string_lossy()
                .replace('\\', "/");
            found_sites.push(rel);
        }
    }
    found_sites.sort();

    let mut expected: Vec<&str> = EXPECTED_SITES.iter().map(|(p, _)| *p).collect();
    expected.sort();

    assert_eq!(
        found_sites, expected,
        "DelegateToAgentTool::new(...) construction sites in the crate have \
         changed since this test's EXPECTED_SITES table was last audited \
         (PR #4161). A NEW site (or a removed one) must be reflected here \
         WITH an explicit, hand-verified taint-posture decision — do not \
         just add the path to make this test pass. found={found_sites:?} \
         expected={expected:?}"
    );

    for (path, must_taint) in EXPECTED_SITES {
        if !*must_taint {
            continue;
        }
        let content = std::fs::read_to_string(crate_root.join(path))
            .unwrap_or_else(|e| panic!("failed to read {path}: {e}"));
        assert!(
            content.contains(".with_delegation_taint("),
            "{path}: registers DelegateToAgentTool for a path that reaches \
             assistant-tier, but the file has no `.with_delegation_taint(` \
             call anywhere — this is the exact gap that caused #4126"
        );
    }
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
