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
        // #3466: these were migrated off the claude-code runner by #3458 but
        // were missing from this list, so nothing noticed that they had
        // silently dropped to the catch-all branch.
        //
        // `ticketing-agent` is deliberately NOT here: its `[tools] allowed`
        // authorizes only the 12 ticketing tools + `finish_task`, so
        // registering skill tools it may never call would just be bait for a
        // refused call. See `ticketing_agent_registry_matches_its_allowed_list`.
        "engineer",
        "python-engineer",
        "postmortem-agent",
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

/// #3555 CRITICAL follow-up (code-critic) — live wiring check, `#[ignore]`d
/// by default.
///
/// Why: The deterministic regression tests for the role gate itself live in
/// `src/tools/delegate.rs` (`delegate_assistant_role_gate_rejects_*`), using
/// a hand-built `DelegateToAgentTool` + tempdir so they're fast and
/// environment-independent. This test instead exercises the REAL production
/// wiring — `build_assistant_tier_registry()` unmodified, resolving against
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
    let reg = build_assistant_tier_registry();
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

    let reg = build_assistant_tier_registry();
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

// --- #3466 follow-up: TOOL SURFACE regression guard -----------------------
//
// Why: PR #3466 migrated 12 specialists off `runner = "claude-code"` onto the
// native `SubprocessAgentRunner` path. Under the claude-code runner the
// `claude` CLI supplied its own Read/Write/Edit/Bash surface, so
// `build_registry_for_agent` never had to. After the migration the ONLY tool
// surface an agent has is what that function registers — and four migrated
// agents (`engineer`, `python-engineer`, `postmortem-agent`,
// `ticketing-agent`) had no `match` arm, silently dropping to the catch-all
// (`load_skill` + `list_skills` only). The PR's own tests passed because they
// asserted CONFIG SHAPE (`runner != ClaudeCode`, `strict_tool_discipline()`)
// and never the resolved registry, so 11 green CI checks sat on top of an
// `engineer` that could not write a file.
//
// What: a table of (agent, role, must-have tools, must-NOT-have tools) driven
// straight off each agent's prompt contract and `[tools] allowed` list. New
// migrations must add a row here; a future edit that removes a `match` arm
// fails loudly instead of degrading to a three-tool agent.
//
// Test: this function IS the test surface.
#[test]
fn migrated_agents_resolve_their_declared_tool_surface() {
    // (agent name, agent role, tools that MUST be present, tools that MUST NOT be)
    let cases: &[(&str, &str, &[&str], &[&str])] = &[
        // #3466 CRITICAL: `engineer` is the `code` phase of prescriptive.json
        // (`produces_files = true`) — without write_file it reports success
        // having written nothing.
        (
            "engineer",
            "engineer",
            &["read_file", "write_file", "list_dir", "grep_files"],
            &[],
        ),
        // #3466 CRITICAL: postmortem-agent's own prompt names
        // "read_file / list_dir / grep" and "write_file" under "Available Tools".
        (
            "postmortem-agent",
            "analysis",
            &["read_file", "write_file", "list_dir", "grep_files"],
            &[],
        ),
        // #3466: `python-engineer` is DELIBERATELY tool-light. Its prompt
        // mandates returning every file as a prose `## File: <path>` +
        // code-fence section, which `ipc::extract_files_from_content` extracts to
        // disk after the turn. Handing it write_file would invite it to call
        // a tool instead of honoring that contract, so the catch-all
        // (skills-only) registry is CORRECT here — pinned so nobody "fixes"
        // it by adding file tools without also rewriting the prompt.
        (
            "python-engineer",
            "engineer",
            &["load_skill", "list_skills"],
            &["write_file"],
        ),
        // #3466: qa-agent's STEP 0 tells it to inspect the project directory
        // for Cargo.toml / package.json / go.mod before choosing a runner —
        // impossible without read_file + list_dir.
        (
            "qa-agent",
            "qa",
            &["pytest_exec", "read_file", "list_dir"],
            &[],
        ),
        // Pre-existing arms, pinned so the table is the single source of truth.
        ("code-agent", "engineer", &["read_file", "write_file"], &[]),
        (
            "docs-agent",
            "documentation",
            &["read_file", "write_file"],
            &[],
        ),
        (
            "research-agent",
            "researcher",
            &["read_file", "grep_files", "web_search"],
            &[],
        ),
    ];

    for (name, role, required, forbidden) in cases {
        let reg = build_registry_for_agent(
            name,
            role,
            None,
            None,
            empty_skill_registry(),
            empty_tag_registry(),
        )
        .unwrap_or_else(|| panic!("{name}: must build a registry"));

        for tool in *required {
            assert!(
                reg.contains(tool),
                "{name}: `{tool}` missing from the resolved tool surface. \
                 Since #3466 moved this agent off the claude-code runner, \
                 `build_registry_for_agent` is the ONLY source of its tools."
            );
        }
        for tool in *forbidden {
            assert!(
                !reg.contains(tool),
                "{name}: `{tool}` must NOT be registered — see the table comment"
            );
        }
    }
}

/// #3466 second-pass review (HIGH): registration and authorization are two
/// separate gates. `tool_loop` sends the FULL registry schemas to the model;
/// `dispatch_gated` then refuses any call outside `[tools] allowed`. So a tool
/// that is registered but not allowed is strictly worse than one that is
/// absent — the model sees it, calls it, and burns a turn on
/// "Tool 'x' is not permitted". An earlier revision of the `ticketing-agent`
/// arm registered skill/memory/timer/read_file/list_dir/grep_files against an
/// `allowed` list naming only the 12 ticketing tools + `finish_task`, on a
/// strict-discipline agent with `max_turns = 20`.
///
/// This pins the reconciliation: the sync arm registers NOTHING, so the
/// resolved surface is exactly the tools the TOML authorizes (ticketing tools
/// arrive from the async `subagent_mode` step, `finish_task` from the
/// `use_finish_task` step).
/// Test: this function IS the test surface.
#[test]
fn ticketing_agent_registry_matches_its_allowed_list() {
    let reg = build_registry_for_agent(
        "ticketing-agent",
        "ticketing",
        None,
        None,
        empty_skill_registry(),
        empty_tag_registry(),
    )
    .expect("ticketing-agent must build a registry");

    let cfg = crate::agents::AgentConfig::by_name("ticketing-agent")
        .expect("bundled ticketing-agent must load");
    let allowed = cfg.tools.allowed.clone().unwrap_or_default();
    assert!(
        !allowed.is_empty(),
        "sanity: ticketing-agent must declare an [tools] allowed list"
    );

    // Every tool the sync arm registers must be authorized by `allowed`.
    for schema in reg.schemas() {
        let tool_name = schema
            .get("function")
            .and_then(|f| f.get("name"))
            .and_then(|n| n.as_str())
            .unwrap_or_default()
            .to_string();
        assert!(
            allowed.contains(&tool_name),
            "ticketing-agent registers `{tool_name}` but [tools] allowed does not \
             authorize it — the model would see it, call it, and get \
             \"Tool '{tool_name}' is not permitted\" (#3466)"
        );
    }
}

/// #3466: the ticketing bundle needs an async GitHub client, so registration
/// happens at the async `subagent_mode` call site. The DECISION is the pure
/// predicate `wants_ticketing_tools`, tested here — otherwise the async block
/// has no reachable coverage at all.
/// Test: this function IS the test surface.
#[test]
fn ticketing_scope_predicate_is_fail_closed() {
    use crate::agents::ToolsConfig;

    // Absent scopes -> false.
    assert!(!wants_ticketing_tools(&ToolsConfig::default()));

    // Empty scopes -> false.
    let empty = ToolsConfig {
        scopes: Some(vec![]),
        ..Default::default()
    };
    assert!(!wants_ticketing_tools(&empty));

    // An unrelated scope must NOT grant ticket-mutation tools.
    let other = ToolsConfig {
        scopes: Some(vec!["memory.read".to_string(), "search.read".to_string()]),
        ..Default::default()
    };
    assert!(!wants_ticketing_tools(&other));

    // A near-miss must not match — the gate is exact, not a prefix.
    let near = ToolsConfig {
        scopes: Some(vec!["ticketing".to_string()]),
        ..Default::default()
    };
    assert!(!wants_ticketing_tools(&near));

    // The exact declared scope grants it.
    let yes = ToolsConfig {
        scopes: Some(vec![TICKETING_SCOPE.to_string()]),
        ..Default::default()
    };
    assert!(wants_ticketing_tools(&yes));
}

/// #3466: pins that the bundled `ticketing-agent` actually declares the scope
/// its tools are gated on. Without this, narrowing the registry arm to nothing
/// (above) would leave the agent with no tools at all and no test would notice.
/// Test: this function IS the test surface.
#[test]
fn bundled_ticketing_agent_declares_the_scope_that_gates_its_tools() {
    let cfg = crate::agents::AgentConfig::by_name("ticketing-agent")
        .expect("bundled ticketing-agent must load");
    assert!(
        wants_ticketing_tools(&cfg.tools),
        "ticketing-agent must declare [tools] scopes = [\"{TICKETING_SCOPE}\"] — \
         that declaration is the ONLY thing that registers its 12 ticketing \
         tools on the subagent path (#3466). Got: {:?}",
        cfg.tools.scopes
    );
}

/// #3466: `[tools] ast_native = true` was honored only by the in-process
/// runner, so `engineer.toml`'s flag was a silent no-op on the subprocess path
/// #3458 moved it to. These pin both halves of the fix: the config predicate
/// and the registration effect.
/// Test: this function IS the test surface.
#[test]
fn engineer_registry_includes_ast_bundle_when_declared() {
    let cfg = crate::agents::AgentConfig::by_name("engineer").expect("bundled engineer must load");
    assert!(
        wants_ast_bundle(&cfg.tools),
        "engineer.toml must declare [tools] ast_native = true"
    );

    let mut reg = build_registry_for_agent(
        "engineer",
        "engineer",
        None,
        None,
        empty_skill_registry(),
        empty_tag_registry(),
    )
    .expect("engineer builds a registry");

    // Not present from the name-keyed arm alone...
    assert!(
        !reg.contains("edit_symbol"),
        "sanity: the AST bundle is layered on by the caller, not the arm"
    );

    // ...and present once the caller applies the declared bundle.
    register_ast_bundle(&mut reg);
    for tool in [
        "get_symbol",
        "edit_symbol",
        "insert_symbol",
        "add_import",
        "validate_syntax",
        "apply_patch",
    ] {
        assert!(
            reg.contains(tool),
            "engineer: AST-native tool `{tool}` missing after register_ast_bundle"
        );
    }
}

/// #3466: the AST bundle must not be handed to agents that never asked for it.
/// Test: this function IS the test surface.
#[test]
fn ast_bundle_predicate_is_false_without_the_flag() {
    use crate::agents::ToolsConfig;
    // Guarded: the process-wide `--ast-native` override (#348) would make this
    // true for every agent, and it is global mutable state.
    if crate::ast::is_ast_native_overridden() {
        return;
    }
    assert!(!wants_ast_bundle(&ToolsConfig::default()));

    let cfg = crate::agents::AgentConfig::by_name("qa-agent").expect("bundled qa-agent must load");
    assert!(
        !wants_ast_bundle(&cfg.tools),
        "qa-agent does not declare ast_native and must not receive the bundle"
    );
}
