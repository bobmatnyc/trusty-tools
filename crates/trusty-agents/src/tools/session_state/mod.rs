//! Read-only session-state visibility for the L0 orchestration tier
//! (#4171, epic #4167).
//!
//! Why: L0 ("orchestration assistant") exists to do PM work — reconcile a
//! paused session against live git/PR/CI state, verify in-flight work, drive
//! fix rounds. None of that is possible while the agent cannot SEE session
//! state. L1 ("standard assistant") is the opposite case: its documented
//! BLACK-BOX POSTURE excludes `session_list`, `session_status`,
//! `project_list`, `console_metrics` and `system_status` BY NAME (see
//! `.trusty-agents/agents/assistant/agent.toml`, `cto-assistant/agent.toml`)
//! precisely so a persona that ingests untrusted Gmail/Drive/web content
//! never recites internal trusty-* daemon/session mechanics to the user — and
//! never gains a foothold on the orchestration surface. So the SAME names
//! this module grants to L0 are the names L1 is deliberately denied, which
//! makes tier enforcement the entire point of this module, not a detail of it.
//!
//! Until now the L1 exclusion was a CONVENTION: the names were simply absent
//! from each persona's `[tools].allow`. Nothing stopped an L1 persona (or a
//! personalization overlay unioned into one) from re-adding them, and the
//! persona-chat dispatch path registers the trusty-mpm-proxied `session_list`
//! / `session_status` / `project_list` / `console_metrics` executors into
//! EVERY persona's registry (`tools::mcp_service_tools`), gated only by that
//! allow-list. [`retain_tier_permitted`] turns the convention into
//! enforcement: a second, independent, DENY-ONLY gate that strips every
//! L0-only session-state name unless the agent resolves to
//! [`AgentTier::L0Orchestration`] through #4200's fail-closed resolver.
//!
//! What:
//! - [`L0_ONLY_SESSION_STATE_TOOLS`] — the gated name set (read-only only).
//! - [`retain_tier_permitted`] — the deny-only tier gate, applied on BOTH
//!   assistant dispatch paths (`ctrl::pm_task::dispatch::persona` and
//!   `runtime::subagent_mode`).
//! - [`session_state_tools`] — the three NATIVE read-only executors this PR
//!   adds, returned only for L0 so an L1 registry never contains them at all.
//!
//! READ-ONLY BY CONSTRUCTION. Every executor here opens files and returns
//! text. Nothing in this module writes, spawns a process, or talks to a
//! daemon, so there is no code path by which it could mutate, message, stop
//! or decommission a session. `session_send`, `session_stop`,
//! `session_resume`, `session_new`, `session_prune`, `session_delete`,
//! `session_decommission*`, `session_proxy_message`/`_focus`/`_unfocus`,
//! `agent_delegate`, `mcp_enable` and `mcp_disable` are DELIBERATELY absent
//! from [`L0_ONLY_SESSION_STATE_TOOLS`] — issue #4171's acceptance criterion
//! is "output is read-only (no state modification)", so this module grants no
//! mutation and gates none. Adding a mutating tool to that list would be a
//! separate, separately-reviewed decision.
//! Test: `tests.rs` — see `l0_only_list_contains_no_mutating_tool`,
//! `l1_persona_declaring_session_state_tools_gets_none`,
//! `l0_persona_declaring_session_state_tools_gets_them`,
//! `indeterminate_tier_fails_closed_to_no_session_state_tools`.

use std::path::Path;
use std::sync::Arc;

use crate::agents::AgentTier;
use crate::tools::traits::ToolExecutor;

mod list;
mod snapshot;
mod status;
mod store;

pub use list::SessionStateListTool;
pub use snapshot::SessionStateSnapshotTool;
pub use status::SessionStateStatusTool;

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

/// Every tool name that only an [`AgentTier::L0Orchestration`] agent may
/// reach (#4171, epic #4167).
///
/// Why: One list, consulted by one function ([`retain_tier_permitted`]), so
/// "what counts as session state" can never drift between the two assistant
/// dispatch paths that enforce it. The set is the UNION of two groups, and
/// the distinction matters when reading it:
///
/// 1. The three native executors this PR adds (`session_state_*`). These are
///    registered only for L0 ([`session_state_tools`]), so for L1 the gate is
///    belt-and-suspenders rather than the only barrier.
/// 2. Names that ALREADY existed and are ALREADY excluded by name from the
///    L1 black-box allowlists (`session_list`, `session_status`,
///    `session_proxy_summary`, `project_list`, `console_metrics`,
///    `system_status`). For these the gate is the only mechanical barrier —
///    they are registered into every persona registry by
///    `tools::mcp_service_tools` (the trusty-mpm static service), by
///    `tools::mcp_live` (live discovery), or natively
///    (`tools::system_status`), and were held back purely by each persona's
///    `[tools].allow` omission.
///
/// This list therefore does not WIDEN anything for L1 — no shipped persona
/// declares any of these names (pinned by
/// `agents::tests::loading::assistant_tier_grants_delegation_and_blackboxes_internal_tools`)
/// — it makes the existing omission unbypassable.
///
/// What: names only, matched exactly (not as globs). A persona's
/// `[tools].allow` globs are resolved to concrete names BEFORE this gate
/// runs, so `session_*` in an L1 allow-list expands to the concrete
/// `session_list`/`session_status` names and is then stripped here.
/// Test: `l0_only_list_contains_no_mutating_tool`,
/// `l0_only_list_covers_every_native_session_state_tool`,
/// `l0_only_list_covers_the_l1_blackbox_exclusions`.
pub const L0_ONLY_SESSION_STATE_TOOLS: &[&str] = &[
    // --- native, added by #4171 (read-only, registered for L0 only) ------
    "session_state_list",
    "session_state_status",
    "session_state_snapshot",
    // --- pre-existing, excluded by name from the L1 black-box allowlists --
    "session_list",
    "session_status",
    "session_proxy_summary",
    "project_list",
    "console_metrics",
    "system_status",
];

/// Whether `name` is gated to the L0 orchestration tier.
///
/// Why: A named predicate reads better than an inline `.contains()` at the
/// call sites and gives the tests one thing to pin.
/// What: exact-name membership in [`L0_ONLY_SESSION_STATE_TOOLS`].
/// Test: `is_l0_only_matches_exact_names_only`.
pub fn is_l0_only_session_state_tool(name: &str) -> bool {
    L0_ONLY_SESSION_STATE_TOOLS.contains(&name)
}

/// Strip every L0-only session-state name from an already-resolved tool
/// allowlist unless the agent is L0 (#4171, epic #4167).
///
/// Why: THE enforcement point. Both assistant dispatch paths end up holding a
/// concrete `Vec<String>` of the tool names an agent may actually call —
/// `ctrl::pm_task::dispatch::persona`'s `filter_persona_tool_names` result
/// (which feeds both the advertised schema list AND
/// `ToolRegistry::dispatch_gated`) and `runtime::subagent_mode`'s
/// `cfg.tools.allowed` (ditto). Applying the tier gate to that final list —
/// rather than only at registration — is what makes it unbypassable: a name
/// that reaches the list from ANY source (the static trusty-mpm MCP service,
/// live MCP discovery, a native registration, a personalization overlay that
/// unions `session_*` into an L1 persona's `[tools].allow`) is removed.
/// What: DENY-ONLY and order-preserving. `AgentTier::L0Orchestration` returns
/// `names` unchanged — this function never ADDS a tool, so an L0 persona
/// still only gets what its own `[tools].allow` names. Every other tier —
/// which, per #4200's fail-closed resolver, is where an absent, blank or
/// unrecognized `tier = …` declaration lands — has every
/// [`L0_ONLY_SESSION_STATE_TOOLS`] entry removed. Matching on the enum
/// (rather than a boolean the caller computes) is deliberate: a future third
/// tier is denied by default until someone edits this match arm.
/// Test: `l1_persona_declaring_session_state_tools_gets_none`,
/// `l0_persona_declaring_session_state_tools_gets_them`,
/// `indeterminate_tier_fails_closed_to_no_session_state_tools`,
/// `retain_tier_permitted_never_adds_a_tool`,
/// `retain_tier_permitted_preserves_unrelated_tools_and_order`.
pub fn retain_tier_permitted(names: Vec<String>, tier: AgentTier) -> Vec<String> {
    match tier {
        AgentTier::L0Orchestration => names,
        AgentTier::L1Standard => names
            .into_iter()
            .filter(|n| !is_l0_only_session_state_tool(n))
            .collect(),
    }
}

/// The native read-only session-state executors, for an L0 agent only.
///
/// Why: Registration is the FIRST of the two barriers (the second is
/// [`retain_tier_permitted`]). Returning an empty vector for any non-L0 tier
/// means an L1 agent's registry never contains these executors at all, so
/// `runtime::tool_registry::scope_assistant_allowed_tools` — which intersects
/// a persona's `[tools].allow` globs against the REGISTRY's schema names —
/// cannot resolve them even if the persona declares them verbatim. Putting
/// the tier decision inside this constructor (rather than at each call site)
/// keeps both dispatch paths a one-line, identical, un-forgettable call.
/// What: for [`AgentTier::L0Orchestration`], one
/// [`SessionStateListTool`], [`SessionStateStatusTool`] and
/// [`SessionStateSnapshotTool`], the last one rooted at `project_root` (see
/// its doc comment for the path-scoping rules). For every other tier, an
/// empty vector.
/// Test: `session_state_tools_are_empty_for_l1`,
/// `session_state_tools_present_for_l0`,
/// `assistant_tier_registry_omits_session_state_tools_for_l1`,
/// `assistant_tier_registry_includes_session_state_tools_for_l0`.
pub fn session_state_tools(project_root: &Path, tier: AgentTier) -> Vec<Arc<dyn ToolExecutor>> {
    match tier {
        AgentTier::L1Standard => Vec::new(),
        AgentTier::L0Orchestration => vec![
            Arc::new(SessionStateListTool::new()),
            Arc::new(SessionStateStatusTool::new()),
            Arc::new(SessionStateSnapshotTool::new(project_root.to_path_buf())),
        ],
    }
}
