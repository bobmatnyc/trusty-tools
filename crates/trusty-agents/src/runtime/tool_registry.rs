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

use tools::SkillResolver;
use tools::fs_reader::{GrepFilesTool, ListDirTool, ReadFileTool};
#[allow(unused_imports)]
use tools::memory::{MemoryRecallTool, VectorSearchTool};
use tools::phase_audit::PhaseAuditTool;
use tools::shell::ShellExecTool as LocalOpsShellTool;
use tools::skill_loader::{FsSkillResolver, SkillListTool, SkillLoaderTool};
use tools::web_search::{BraveSearchTool, FetchUrlTool};
use tools::write_file::WriteFileTool;
use tools::{ToolRegistry, shell_exec::ShellExecTool};

/// Build a tool registry tailored to a specific agent.
///
/// Why: Different agents need different tools (research -> web_search,
/// load_skill; qa -> pytest_exec). Hardcoding the mapping here keeps it
/// discoverable; a later version could drive it from the agent TOML.
/// What: Returns `Some(ToolRegistry)` for agents that use tools, else None.
/// `out_dir`, if present, is used to register `advance_workflow_phase`.
/// Test: Called during `run_subagent`.
pub(super) fn build_registry_for_agent(
    name: &str,
    out_dir: Option<&std::path::Path>,
    code_dir: Option<&std::path::Path>,
    skill_registry: Arc<skills::SkillRegistry>,
    tag_skill_registry: Arc<skills::registry::SkillRegistry>,
) -> Option<ToolRegistry> {
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

#[cfg(test)]
#[path = "tool_registry_tests.rs"]
mod registry_tests;
