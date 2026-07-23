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

//! Per-agent tool-registry construction for sub-agent execution.

use std::path::PathBuf;
use std::sync::Arc;

use crate::{skills, tools};

use tools::AgentRunner;
use tools::SkillResolver;
use tools::delegate::DelegateToAgentTool;
use tools::fs_reader::{GrepFilesTool, ListDirTool, ReadFileTool};
#[allow(unused_imports)]
use tools::memory::{MemoryRecallTool, VectorSearchTool};
use tools::phase_audit::PhaseAuditTool;
use tools::shell::ShellExecTool as LocalOpsShellTool;
use tools::skill_loader::{FsSkillResolver, SkillListTool, SkillLoaderTool};
use tools::web_search::{BraveSearchTool, FetchUrlTool};
use tools::write_file::WriteFileTool;
use tools::{ToolRegistry, shell_exec::ShellExecTool};

/// Role string shared by every black-box persona/assistant-tier agent
/// (`assistant`, `cto-assistant`, `izzie`, `personal-assistant`, …).
///
/// Why: `run_subagent` (the `--direct`/`--agent` subprocess dispatch path)
/// previously treated every agent — including personas — as a generic
/// coding sub-agent: full CLAUDE.md project-instruction injection, the
/// binary's coding-harness protocol, and an UNRESTRICTED
/// `list_skills`/`load_skill` tool pair wired to the entire bundled skill
/// catalog (every language/framework/workflow skill, not just the persona's
/// declared `system_prompt.skills`). None of that black-box scoping the
/// persona-chat path (`run_pm_task_with_persona`, #3550) applies here,
/// because this path never routed through it. This constant is the single
/// place both `tool_registry.rs` and `subagent_mode.rs` check so the
/// definition of "assistant tier" can't drift between the two call sites.
/// What: Matches `AgentInfo.role`, not the agent's `name` — every persona
/// TOML/overlay sets `role = "assistant"` regardless of its display name.
/// Test: `assistant_tier_registry_excludes_skill_catalog_tools`,
/// `assistant_tier_registry_includes_curated_tools`.
pub(super) const ASSISTANT_TIER_ROLE: &str = "assistant";

/// Fail-closed allowlist of `agent.role` values an assistant-tier
/// `delegate_to_agent` call may target — see
/// [`crate::tools::delegate::DelegateToAgentTool::with_allowed_target_roles`]
/// for the full rationale.
///
/// Why (#3555 CRITICAL follow-up, code-critic): `agents::agents_dir_candidates()`
/// (wired into `build_assistant_tier_registry` below) makes the ENTIRE
/// bundled roster resolvable from any CWD, including `pm` (role
/// `orchestrator`), `ctrl` (role `controller`), `observe-agent` (role
/// `observer`), and `postmortem-agent`/`analysis-agent` (role `analysis`).
/// `run_subagent`'s registry is keyed on the SPAWNED child's own role, so an
/// assistant delegating to one of those would hand the resulting subprocess
/// an unrestricted orchestrator/controller registry — shell, `write_file`,
/// unrestricted `delegate_to_agent` — a privilege escalation out of the
/// sandboxed assistant tier.
/// What: the exact `role` field values declared in each bundled worker's
/// `agent.toml` (NOT the agent/file name — `qa-agent.toml` declares
/// `role = "qa"`, not `"qa-agent"`) plus [`ASSISTANT_TIER_ROLE`] itself, so
/// the Izzie <-> cto-assistant peer-consult lane keeps working: spawning a
/// peer assistant is safe because IT ALSO gets routed through
/// `build_assistant_tier_registry` (the same restricted registry), never an
/// unrestricted one. Any role not in this list — including anything not yet
/// declared in the bundled roster — is rejected.
/// Test: `delegate_assistant_role_gate_rejects_orchestrator_role`,
/// `delegate_assistant_role_gate_rejects_controller_role`,
/// `delegate_assistant_role_gate_allows_worker_role`,
/// `delegate_assistant_role_gate_allows_peer_assistant_role`
/// (`src/tools/delegate.rs`).
pub(super) const ASSISTANT_ALLOWED_DELEGATE_ROLES: &[&str] = &[
    "engineer",          // engineer.toml, code-agent.toml, python-engineer.toml, …
    "qa",                // qa-agent.toml
    "researcher",        // research-agent.toml
    "documentation",     // docs-agent.toml
    "ops",               // local-ops-agent.toml
    "planner",           // plan-agent.toml
    ASSISTANT_TIER_ROLE, // peer assistant personas: izzie, cto-assistant, personal-assistant, …
];

/// Build a tool registry tailored to a specific agent.
///
/// Why: Different agents need different tools (research -> web_search,
/// load_skill; qa -> pytest_exec). Hardcoding the mapping here keeps it
/// discoverable; a later version could drive it from the agent TOML.
/// What: Returns `Some(ToolRegistry)` for agents that use tools, else None.
/// `out_dir`, if present, is used to register `advance_workflow_phase`.
/// `role` (`AgentInfo.role`) is checked FIRST, ahead of the `name` match
/// below: any agent whose role is [`ASSISTANT_TIER_ROLE`] gets the curated,
/// black-box-safe registry from `build_assistant_tier_registry` instead of
/// falling into the generic coding-subagent branches (or the catch-all,
/// which would otherwise hand it an unrestricted `list_skills`/`load_skill`
/// pair over the ENTIRE bundled skill catalog — see #3550 follow-up).
/// Test: `assistant_tier_registry_excludes_skill_catalog_tools`,
/// `assistant_tier_registry_includes_curated_tools`; called during
/// `run_subagent`.
pub(super) fn build_registry_for_agent(
    name: &str,
    role: &str,
    out_dir: Option<&std::path::Path>,
    code_dir: Option<&std::path::Path>,
    skill_registry: Arc<skills::SkillRegistry>,
    tag_skill_registry: Arc<skills::registry::SkillRegistry>,
) -> Option<ToolRegistry> {
    if role == ASSISTANT_TIER_ROLE {
        return Some(build_assistant_tier_registry());
    }
    // #222: When `code_dir` is set and distinct from `out_dir`, the code-agent
    // and any future tool that writes *generated source files* should root at
    // `code_dir` (the user's project tree). All other agents (plan, docs,
    // observe) keep writing artifacts to `out_dir`. When `code_dir` is None
    // we fall back to `out_dir` for full backward compatibility.
    let code_root = code_dir.or(out_dir);
    // #81: `load_skill` and `list_skills` are registered for every agent that
    // builds a registry. The skill registry itself is loaded once per process
    // (empty when `.trusty-agents/skills/` is absent, so wiring is safe unconditionally).
    // Per-agent `[tools].allowed` lists still gate whether the agent can call
    // these; agents that omit `allowed` get unrestricted access as before.
    //
    // #170: When a non-empty tag-indexed registry (#168) is available, wire it
    // into `list_skills` so `tags=[...]` returns tag-ranked results. The
    // legacy `SkillRegistry` remains as a fallback for rendering when the
    // tag registry yields nothing and for `load_skill`'s frontmatter-aware
    // body rendering.
    let register_skill_tools = |reg: &mut ToolRegistry| {
        let resolver: Arc<dyn tools::SkillResolver> = Arc::new(FsSkillResolver::from_defaults());
        reg.register(Arc::new(SkillLoaderTool::with_registry(
            resolver.clone(),
            skill_registry.clone(),
        )));
        if !tag_skill_registry.is_empty() {
            reg.register(Arc::new(SkillListTool::with_tag_registry(
                resolver,
                Some(skill_registry.clone()),
                tag_skill_registry.clone(),
            )));
        } else {
            reg.register(Arc::new(SkillListTool::with_registry(
                resolver,
                skill_registry.clone(),
            )));
        }
    };
    // #52: `web_search` and `fetch_url` are registered unconditionally for
    // every agent that builds a registry. The per-agent `[tools].allowed`
    // list in TOML governs who is actually permitted to call them; the tool
    // itself degrades gracefully when BRAVE_API_KEY is unset.
    fn register_web_tools(reg: &mut ToolRegistry) {
        reg.register(Arc::new(BraveSearchTool::from_env()));
        reg.register(Arc::new(FetchUrlTool::new()));
    }

    /// #199: `wait_ms` and `poll_until` are universal async-flow tools — every
    /// agent benefits from being able to back off or wait for an external
    /// signal. Per-agent TOML allowlists still gate actual usage.
    fn register_timer_tools(reg: &mut ToolRegistry) {
        reg.register(Arc::new(tools::timer::WaitMsTool::new()));
        reg.register(Arc::new(tools::timer::PollUntilTool::new()));
    }

    // #53: `memory_recall` and `vector_search` are research aids and are
    // registered alongside web tools for any agent that benefits from them.
    // Both degrade gracefully when their underlying stores are missing, so
    // registering them is safe even when the project hasn't been indexed.
    //
    // #71: `memory_search` is a hybrid (vector + BM25) retriever with LLM
    // consolidation over the `.trusty-agents/history/` turn log. Added alongside
    // the existing memory tools for the same gracefully-degrading rationale.
    fn register_memory_tools(reg: &mut ToolRegistry) {
        reg.register(Arc::new(MemoryRecallTool::new()));
        reg.register(Arc::new(VectorSearchTool::new()));
        reg.register(Arc::new(tools::memory_search::MemorySearchTool::from_env()));
    }

    match name {
        "research-agent" => {
            // Unified read-only investigator: web tools + memory/vector tools +
            // skills + read-only filesystem exploration. Merged with the former
            // explorer-agent so research-agent is the single "find out" agent.
            // All tools here are side-effect free; per-agent TOML allowlist
            // governs which are actually callable.
            let mut reg = ToolRegistry::new();
            register_web_tools(&mut reg);
            register_memory_tools(&mut reg);
            register_skill_tools(&mut reg);
            register_timer_tools(&mut reg);
            reg.register(Arc::new(ReadFileTool::new()));
            reg.register(Arc::new(ListDirTool::new()));
            reg.register(Arc::new(GrepFilesTool::new()));
            // #373: research benefits from structural analysis tools.
            for t in tools::analysis::analysis_tools() {
                reg.register(t);
            }
            if let Some(dir) = out_dir {
                reg.register(Arc::new(PhaseAuditTool::new(dir.to_path_buf())));
            }
            Some(reg)
        }
        "analysis-agent" => {
            // #373: code-quality analyst agent. Registers the full analysis
            // tool bundle (complexity, smells, hotspots, dependency cycles,
            // call graphs) plus read-only filesystem + skills + memory so it
            // can dig into specific files when an automated metric flags one.
            let mut reg = ToolRegistry::new();
            register_memory_tools(&mut reg);
            register_skill_tools(&mut reg);
            reg.register(Arc::new(ReadFileTool::new()));
            reg.register(Arc::new(ListDirTool::new()));
            reg.register(Arc::new(GrepFilesTool::new()));
            for t in tools::analysis::analysis_tools() {
                reg.register(t);
            }
            if let Some(dir) = out_dir {
                reg.register(Arc::new(PhaseAuditTool::new(dir.to_path_buf())));
            }
            Some(reg)
        }
        "code-agent" => {
            // Code generation agent. Gets write_file so it can emit files
            // directly as tool calls (avoids plain-text-mid-task retries for
            // large multi-file outputs). Also gets read-only exploration tools
            // so it can inspect existing code and the phase-audit tool for
            // workflow phase management.
            let mut reg = ToolRegistry::new();
            register_skill_tools(&mut reg);
            register_timer_tools(&mut reg);
            reg.register(Arc::new(ReadFileTool::new()));
            reg.register(Arc::new(ListDirTool::new()));
            reg.register(Arc::new(GrepFilesTool::new()));
            // #222: write_file roots at `code_root` (= code_dir when set,
            // else out_dir) so generated source lands in the user's project
            // tree when --project-dir is used. PhaseAuditTool stays anchored
            // at out_dir because the audit trail is an artifact.
            if let Some(dir) = code_root {
                // #88: If `TAGENT_ASSIGNED_FILE` is set, we're inside a
                // per-file wave-loop invocation and must restrict writes to
                // that single path. Otherwise fall through to the legacy
                // unrestricted behavior (full code_root tree writable).
                let mut write_tool = WriteFileTool::new(dir.to_path_buf());
                if let Some(assigned) =
                    crate::env_compat::env_var_os("TAGENT_ASSIGNED_FILE", "OPEN_MPM_ASSIGNED_FILE")
                {
                    write_tool = write_tool.with_allowed_path(PathBuf::from(assigned));
                }
                reg.register(Arc::new(write_tool));
            } else {
                let fallback = std::env::current_dir().unwrap_or_default();
                reg.register(Arc::new(WriteFileTool::new(fallback)));
            }
            if let Some(dir) = out_dir {
                reg.register(Arc::new(PhaseAuditTool::new(dir.to_path_buf())));
            }
            Some(reg)
        }
        "plan-agent" => {
            // #53: planners benefit from memory_recall + vector_search to
            // ground implementation plans in existing code/decisions.
            // #87: plan-agent also gets write_file (scoped to out_dir) so it
            // can emit stub files and assignments.json for interface-first
            // decomposition. When out_dir is absent we fall back to CWD so
            // the tool remains discoverable in schemas.
            let mut reg = ToolRegistry::new();
            register_memory_tools(&mut reg);
            register_skill_tools(&mut reg);
            register_timer_tools(&mut reg);
            if let Some(dir) = out_dir {
                reg.register(Arc::new(WriteFileTool::new(dir.to_path_buf())));
                reg.register(Arc::new(PhaseAuditTool::new(dir.to_path_buf())));
            } else {
                let fallback = std::env::current_dir().unwrap_or_default();
                reg.register(Arc::new(WriteFileTool::new(fallback)));
            }
            Some(reg)
        }
        "qa-agent" => {
            let mut reg = ToolRegistry::new();
            register_web_tools(&mut reg);
            // #71: memory tools so QA can recall prior decisions / failures.
            register_memory_tools(&mut reg);
            register_skill_tools(&mut reg);
            register_timer_tools(&mut reg);
            reg.register(Arc::new(ShellExecTool::new()));
            if let Some(dir) = out_dir {
                reg.register(Arc::new(PhaseAuditTool::new(dir.to_path_buf())));
            }
            Some(reg)
        }
        "local-ops-agent" => {
            // #77: Local operations agent. Registers a permissive (allowlisted)
            // shell executor plus the read-only filesystem tools so the agent
            // can run commands and verify their effects without mutating
            // source files. `finish_task` is auto-registered elsewhere when
            // `use_finish_task = true` in the agent TOML.
            let mut reg = ToolRegistry::new();
            let work_dir = std::env::current_dir().unwrap_or_default();
            reg.register(Arc::new(LocalOpsShellTool::new(work_dir)));
            reg.register(Arc::new(ReadFileTool::new()));
            reg.register(Arc::new(ListDirTool::new()));
            reg.register(Arc::new(GrepFilesTool::new()));
            register_skill_tools(&mut reg);
            if let Some(dir) = out_dir {
                reg.register(Arc::new(PhaseAuditTool::new(dir.to_path_buf())));
            }
            Some(reg)
        }
        "docs-agent" => {
            // #82: Documentation specialist. Reads generated code (read_file /
            // list_dir / grep_files) and writes docs (write_file) scoped to
            // the workflow's out_dir. `finish_task` is auto-registered
            // elsewhere via `use_finish_task = true` in the agent TOML.
            let mut reg = ToolRegistry::new();
            register_skill_tools(&mut reg);
            reg.register(Arc::new(ReadFileTool::new()));
            reg.register(Arc::new(ListDirTool::new()));
            reg.register(Arc::new(GrepFilesTool::new()));
            if let Some(dir) = out_dir {
                reg.register(Arc::new(WriteFileTool::new(dir.to_path_buf())));
                reg.register(Arc::new(PhaseAuditTool::new(dir.to_path_buf())));
            } else {
                // Even without out_dir, register a WriteFileTool rooted at CWD
                // so the tool is discoverable in schemas. In practice workflow
                // mode always provides out_dir; direct mode may not.
                let fallback = std::env::current_dir().unwrap_or_default();
                reg.register(Arc::new(WriteFileTool::new(fallback)));
            }
            Some(reg)
        }
        _ => {
            // #81: Agents without a dedicated tool branch still benefit from
            // skill discovery/loading. Build a minimal registry that just
            // exposes `list_skills` and `load_skill`, plus the phase-audit
            // tool when a workflow out_dir is available. Per-agent allowlists
            // still govern whether any of these can actually be called.
            let mut reg = ToolRegistry::new();
            register_skill_tools(&mut reg);
            if let Some(dir) = out_dir {
                reg.register(Arc::new(PhaseAuditTool::new(dir.to_path_buf())));
            }
            Some(reg)
        }
    }
}

/// Curated tool registry for black-box persona/assistant-tier agents
/// (`role == "assistant"`) dispatched via the `--direct`/`--agent`
/// subprocess path.
///
/// Why: The generic per-agent branches above (and the `_` catch-all) are
/// built for coding sub-agents and unconditionally wire an unrestricted
/// `list_skills`/`load_skill` pair over the ENTIRE bundled skill catalog
/// (every language, framework, and workflow skill — including
/// `workflow/wave-planning.md` and `workflow/delegation.md`, which name
/// internal orchestration concepts the assistant must never recite). A
/// persona's actual domain skills are already injected as system-prompt
/// CONTENT from its declared `system_prompt.skills` list (see
/// `run_subagent`'s skill-resolution loop) — it never needs a
/// catalog-browsing tool. This function gives the assistant tier a small,
/// fixed tool set instead: delegation (so it can still genuinely act on
/// "bring in a specialist") plus read-only git/web lookups, mirroring the
/// tool families the persona-chat path (`run_pm_task_with_persona`)
/// registers. Actual reachability is still narrowed by the caller
/// (`run_subagent`) applying the persona's `[tools].allow` glob patterns —
/// registering a tool here is not the same as granting it.
/// What: Registers `git_log`/`git_status` (when CWD is a git repo),
/// `web_search`, the izzie platform-hosted tools (`get_weather`,
/// `get_train_schedule`, `get_train_alerts` via `crate::tools::izzie::izzie_tools`,
/// mirroring the persona-chat dispatch path — #3745 item C), and
/// `delegate_to_agent` (pre-flight-validated against every
/// tier `agents::agents_dir_candidates()` searches — CWD/`TAGENT_CONFIG_DIR`
/// first, then `$HOME/.trusty-agents/agents`, the SAME tiers the actual
/// sub-agent spawn resolves against — see #3555 delegate-resolve follow-up
/// below — AND gated to [`ASSISTANT_ALLOWED_DELEGATE_ROLES`] via
/// `with_allowed_target_roles`, #3555 CRITICAL follow-up, so a wider
/// resolution search can never turn into a privilege escalation). Deliberately
/// omits `list_skills`, `load_skill`, and every generic coding-agent tool
/// (`write_file`, `run_bash`, analysis tools, …).
/// Test: `assistant_tier_registry_excludes_skill_catalog_tools`,
/// `assistant_tier_registry_includes_curated_tools`,
/// `assistant_tier_registry_includes_izzie_tools`,
/// `izzie_allow_list_surfaces_persona_tools_through_scoping`.
pub(super) fn build_assistant_tier_registry() -> ToolRegistry {
    let mut reg = ToolRegistry::new();
    let cwd = std::env::current_dir().unwrap_or_default();

    if let Ok(repo) = crate::git::GitRepo::open(&cwd) {
        for tool in crate::tools::git_tools::git_tools(repo.root.clone()) {
            reg.register(tool);
        }
    }

    reg.register(Arc::new(BraveSearchTool::from_env()));

    // #3555 delegate-resolve follow-up: previously this was a single
    // hand-rolled `cwd.join(".trusty-agents").join("agents")` — invisible to
    // the bundled worker roster (`engineer`, `qa-agent`, …) that
    // `agents::bundled::ensure_bundled_agents_deployed` deploys to
    // `$HOME/.trusty-agents/agents/` at startup. Any assistant launched from
    // a directory without its OWN local `.trusty-agents/agents/` (e.g. a bare
    // worktree root, `/tmp`) failed `delegate_to_agent`'s pre-flight check
    // before ever reaching the runner — `is_error=true` with no engineer
    // sub-agent ever spawned. `agents_dir_candidates()` is the exact
    // multi-tier list `AgentConfig::by_name` uses for the actual spawn (see
    // `run_subagent`), so validation and spawn now share one source of truth
    // and can never diverge.
    let config_dirs = crate::agents::agents_dir_candidates();
    let primary_config_dir = config_dirs.first().cloned();
    let runner: Arc<dyn AgentRunner> = Arc::new(
        crate::subprocess::SubprocessAgentRunner::new().with_config_dir(primary_config_dir),
    );
    // #3555 CRITICAL follow-up: role-gate the assistant tier's delegation
    // target to workers + peer assistants ONLY — see
    // `ASSISTANT_ALLOWED_DELEGATE_ROLES`. `pm`/`ctrl` build their own
    // `DelegateToAgentTool` elsewhere (`ctrl/pm_task/dispatch/*.rs`) and never
    // call `with_allowed_target_roles`, so they are completely unaffected.
    reg.register(Arc::new(
        DelegateToAgentTool::new(runner)
            .with_config_dirs(config_dirs)
            .with_allowed_target_roles(
                ASSISTANT_ALLOWED_DELEGATE_ROLES
                    .iter()
                    .map(|s| s.to_string())
                    .collect(),
            ),
    ));

    // #3745 item C: register the izzie platform-hosted tools
    // (`get_weather`, `get_train_schedule`, `get_train_alerts`) into the
    // `--direct`/`--agent` assistant-tier registry, exactly as the
    // persona-chat dispatch path already does
    // (`ctrl::pm_task::dispatch::persona::run_pm_task_with_persona`, which
    // calls `crate::tools::izzie::izzie_tools()`). Without this the two
    // assistant dispatch paths DIVERGED: `izzie/agent.toml` declares
    // `get_weather` et al. in `[tools].allow`, but the subprocess path's
    // registry never registered those executors, so
    // `scope_assistant_allowed_tools` intersected them away — leaving only
    // git/web_search/delegate and forcing the model to answer "weather tool
    // unavailable". Registering here only makes the tools REACHABLE; whether a
    // given persona can call them is still gated by its `[tools].allow` globs
    // via `scope_assistant_allowed_tools` (only izzie opts in), so other
    // assistant-tier personas are unaffected.
    for tool in crate::tools::izzie::izzie_tools() {
        reg.register(tool);
    }

    reg
}

/// Resolve the effective `[tools].allowed` override for the assistant-tier
/// glob-scoping gate.
///
/// Why: `run_subagent_with_tools`/`run_subagent_single_shot` pass
/// `cfg.tools.allowed` (an exact-name allowlist, `None` = unrestricted)
/// straight into `chat_with_tools_gated`. Persona TOMLs (`assistant`,
/// `cto-assistant`, `izzie`, `personal-assistant`) declare `[tools].allow`
/// instead — GLOB patterns, the field the persona-chat dispatch path
/// (`run_pm_task_with_persona` / `filter_persona_tool_names`) reads. Because
/// `run_subagent` never consulted `allow`, an assistant-tier agent dispatched
/// via `--direct`/`--agent` got `allowed_tools = None` — no restriction at
/// all — regardless of its curated `allow` list. This pure function is the
/// fix, pulled out of `run_subagent` so the glob-matching behavior is
/// unit-testable without an LLM call.
/// What: When `is_assistant_tier` is true, `existing_allowed` is `None`, and
/// `allow` is `Some(patterns)`, returns `Some(<names from `registry` matching
/// any pattern>)`. Otherwise returns `existing_allowed` unchanged — every
/// non-persona agent, and any persona that already sets `allowed` explicitly,
/// is untouched.
/// Test: `scope_assistant_allowed_tools_filters_by_glob`,
/// `scope_assistant_allowed_tools_noop_for_non_assistant`,
/// `scope_assistant_allowed_tools_noop_when_allowed_already_set`.
pub(super) fn scope_assistant_allowed_tools(
    is_assistant_tier: bool,
    existing_allowed: Option<Vec<String>>,
    allow: Option<&[String]>,
    registry: Option<&ToolRegistry>,
) -> Option<Vec<String>> {
    if !is_assistant_tier || existing_allowed.is_some() {
        return existing_allowed;
    }
    let (Some(patterns), Some(reg)) = (allow, registry) else {
        return existing_allowed;
    };
    let kept: Vec<String> = reg
        .schemas()
        .into_iter()
        .filter_map(|s| {
            s.get("function")
                .and_then(|f| f.get("name"))
                .and_then(|n| n.as_str())
                .map(String::from)
        })
        .filter(|n| crate::ctrl::pm_task::match_any_glob(n, patterns))
        .collect();
    Some(kept)
}

#[cfg(test)]
#[path = "tool_registry_tests.rs"]
mod registry_tests;
