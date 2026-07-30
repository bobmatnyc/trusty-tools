//! GitHub PR/CI inspection tool surface, grantable to L0 ONLY (#4170, epic
//! #4167).
//!
//! Why: Epic #4167's gap analysis names this as the FIRST reason an L1 persona
//! cannot do PM-tier orchestration work: *"GitHub PR/CI tooling is absent — no
//! `gh pr list/view/checks`, no check-run status queries."* Verified against
//! the tree before writing this module: `crate::tools::git_tools` is twelve
//! LOCAL git operations (`git_log`, `git_status`, `git_branches`,
//! `git_search_commits`, …) with no GitHub API reach at all; the only `gh`
//! invocations anywhere in the crate are the ISSUE-tracker adapter
//! (`ticketing::gh_cli`, driving `gh issue …` behind the `TicketingClient`
//! trait) and `ticketing::actions` (a workflow-file-keyed
//! trigger/status pair). Nothing resembling `gh pr view`, `gh pr checks`, or a
//! check-run status primitive existed.
//!
//! **The tier gate.** [`gh_tools`] takes the caller's resolved
//! [`AgentTier`](crate::agents::AgentTier) and returns an EMPTY vector for
//! anything other than [`AgentTier::L0Orchestration`](crate::agents::AgentTier::L0Orchestration).
//! The gate lives INSIDE the factory rather than at each registration site on
//! purpose: a future call site that forgets to guard its `for tool in
//! gh_tools(...)` loop still registers nothing, and the tier value it must
//! supply is the fail-closed [`AgentInfo::tier`](crate::agents::AgentInfo::tier)
//! resolution from #4200 — absent, blank, or unrecognized `tier =` in an
//! `agent.toml` resolves to `L1Standard` and therefore to no tools. There is
//! no code path by which an L1 persona obtains these tools: because the two
//! persona dispatch paths derive an agent's callable set by INTERSECTING its
//! `[tools].allow` globs with the names actually present in the registry
//! (`runtime::tool_registry::scope_assistant_allowed_tools` and
//! `ctrl::pm_task::dispatch::persona::filter_persona_tool_names`), an L1
//! persona that declares `gh_pr_view` — or `gh_*`, or `*` — matches nothing
//! and is granted nothing.
//!
//! **Read-only by construction.** Every tool here wraps an INSPECTION
//! subcommand: `gh pr list`, `gh pr view`, `gh pr checks`, `gh run list`,
//! `gh run view`. No mutating capability is provided — no `pr create`,
//! `pr merge`, `pr edit`, `pr comment`, `pr close`, `run rerun`, or
//! `workflow run`. #4170's scope list does mention several of those; they are
//! DEFERRED rather than implemented, because merge/comment authority on an
//! explicitly un-sandboxed tier is an owner decision that should be granted
//! deliberately and separately, not folded into the read surface that closes
//! the stated gap. `gh issue view`/`gh issue list` are likewise absent because
//! they are ALREADY present under different names — `get_ticket` /
//! `list_tickets` / `ticket_search`, which resolve to `gh issue …` through
//! `ticketing::gh_cli::GhCliClient` — and adding a second spelling would
//! violate the one-skill-per-tool catalog's 1:1 rule for no new capability.
//!
//! **Credentials.** Each tool shells out to the user's own authenticated `gh`
//! CLI (argv only, never a shell). No `GITHUB_TOKEN` is read or handled here.
//! A missing or unauthenticated `gh` degrades to a legible tool error.
//! What: [`gh_tools`] returns the five `ToolExecutor`s bound to a working-tree
//! `root` (used as `gh`'s current directory for repository auto-detection; a
//! per-call `repo` operand overrides it).
//! Test: `gh_tools_are_denied_to_l1`, `gh_tools_are_denied_by_default_tier`,
//! `gh_tools_are_granted_to_l0`, `gh_tools_expose_only_read_only_subcommands`,
//! and the real-registry-path tests in
//! `crate::runtime::tool_registry_tests` (search `gh_`).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::agents::AgentTier;
use crate::tools::ToolRegistry;
use crate::tools::traits::ToolExecutor;

mod ci;
mod helpers;
mod pr;

#[cfg(test)]
mod tests;

use ci::{GhRunListTool, GhRunViewTool};
use pr::{GhPrChecksTool, GhPrListTool, GhPrViewTool};

/// Every tool name this module can register, in stable order.
///
/// Why: The skill catalog (`skills::manifest::builtin::github`) and the
/// registry tests both need to speak about "the L0 GitHub surface" as a set
/// rather than five hand-copied literals that can drift apart. Naming it once
/// here means a sixth tool added to [`gh_tools`] without a matching skill row
/// fails `every_tool_declared_in_source_has_a_skill`, and a name typo'd in a
/// test fails at compile time.
/// Test: `gh_tool_names_match_the_factory_output`.
pub const GH_TOOL_NAMES: &[&str] = &[
    "gh_pr_list",
    "gh_pr_view",
    "gh_pr_checks",
    "gh_run_list",
    "gh_run_view",
];

/// Build the GitHub PR/CI inspection tools — L0 ONLY, fail-closed.
///
/// Why: The tier check is here, at the single point of construction, so it
/// cannot be forgotten by a caller. See this module's doc comment for why
/// registration is the enforcement boundary and why that is sufficient to keep
/// an L1 persona from obtaining these tools even when it declares them.
/// What: Returns the five read-only executors when `tier` is
/// [`AgentTier::L0Orchestration`]; returns an EMPTY vector for
/// [`AgentTier::L1Standard`] — which is also what a missing, blank, or
/// unrecognized `tier =` declaration resolves to via `AgentInfo::tier()`, so
/// an indeterminate tier denies. `root` becomes `gh`'s working directory for
/// repository auto-detection.
/// Test: `gh_tools_are_denied_to_l1`, `gh_tools_are_denied_by_default_tier`,
/// `gh_tools_are_granted_to_l0`, `gh_tool_names_match_the_factory_output`.
pub fn gh_tools(tier: AgentTier, root: PathBuf) -> Vec<Arc<dyn ToolExecutor>> {
    if tier != AgentTier::L0Orchestration {
        return Vec::new();
    }
    vec![
        Arc::new(GhPrListTool { root: root.clone() }),
        Arc::new(GhPrViewTool { root: root.clone() }),
        Arc::new(GhPrChecksTool { root: root.clone() }),
        Arc::new(GhRunListTool { root: root.clone() }),
        Arc::new(GhRunViewTool { root }),
    ]
}

/// Register the GitHub PR/CI tools into `reg` — L0 ONLY, fail-closed.
///
/// Why: Both persona dispatch paths (`runtime::tool_registry::
/// build_assistant_tier_registry` and `ctrl::pm_task::dispatch::persona::
/// run_pm_task_with_persona`) must register the SAME set, or they diverge the
/// way #3745 item C had to fix for izzie's tools. A single registration
/// helper — rather than a `for` loop copied into each — makes that one line at
/// each site and leaves exactly one place the gate can be reasoned about.
/// What: Delegates to [`gh_tools`], which yields nothing for any tier other
/// than [`AgentTier::L0Orchestration`], so this is a no-op for L1 callers.
/// Test: `register_is_a_no_op_for_l1`, `register_adds_the_full_surface_for_l0`.
pub fn register(reg: &mut ToolRegistry, tier: AgentTier, root: &Path) {
    for tool in gh_tools(tier, root.to_path_buf()) {
        reg.register(tool);
    }
}
