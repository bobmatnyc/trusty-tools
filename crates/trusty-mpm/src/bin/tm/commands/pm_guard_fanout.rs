//! `tm hook --pm-guard` — subagent fan-out denial (issue #4784).
//!
//! Why: the orchestration model is one level deep. The PM dispatches
//! specialists; a specialist does its own work and reports back. A subagent
//! that dispatches *further* subagents multiplies context cost, hides the
//! resulting work from the PM's tracking (the PM sees one delegation, not the
//! tree beneath it), and produces exactly the un-owned fan-out ADR-0024 calls
//! a single-edge leaf violation. Agent prose alone does not stop it: a model
//! holding the `Agent`/`Task` tool will use it. The `tools:` frontmatter route
//! cannot express the rule either — it is an ALLOW-list with no "everything
//! except X" grammar, and it gates MCP tools through the same list, so
//! enumerating ~40 agents' tool sets to omit two names would silently break
//! real MCP access. `PreToolUse` is the one seam that can deny a single named
//! tool without touching anything else.
//!
//! What: [`evaluate_subagent_fanout`] denies `SUBAGENT_DISPATCH_TOOLS`
//! (`Task` and `Agent` — both, since Claude Code renamed the tool between
//! releases) when, and only when, the calling session is itself a subagent.
//! [`caller_is_subagent`] answers that from two automatic, spawner/harness-set
//! markers, never from prose or heuristics:
//!
//! 1. `agent_id` in the `PreToolUse` payload — documented by Claude Code as
//!    present "only when the hook fires inside a subagent call", the same
//!    signal [`super::pm_guard::payload_is_subagent_dispatch`] has used since
//!    issue #2014. Reused, not re-derived, so the two can never disagree.
//! 2. `CLAUDE_MPM_SUB_AGENT` in the environment — stamped by trusty-agents on
//!    every subagent process it spawns out-of-band
//!    (`subprocess/spawn.rs`, `agents/claude_code_runner/run.rs`), whose own
//!    Claude Code session is top-level and therefore carries no `agent_id`.
//!    Without this arm the guard would be a no-op for that whole class.
//!
//! **This guard FAILS OPEN, deliberately.** Neither marker present means
//! ALLOW, and every upstream degradation ([`super::pm_guard`]'s missing-stdin
//! and missing-`tool_name` paths) reaches ALLOW before this module is called
//! at all. The asymmetry is intentional: a false DENY lands on the PM and
//! halts every dispatch in the system, while a false ALLOW merely reproduces
//! the behaviour that existed before this guard. Indeterminate is treated as
//! "not a subagent" for that reason, and no future signal may invert it.
//!
//! `SendMessage` is never denied under any circumstance — it is not a member
//! of `SUBAGENT_DISPATCH_TOOLS`, and it is how a blocked agent reports back
//! to the PM and gets resumed. Denying it would strand the very agent this
//! guard redirects.
//!
//! Test: `denies_dispatch_tools_from_a_subagent`,
//! `allows_dispatch_tools_from_the_pm`, `never_denies_send_message`,
//! `allows_every_non_dispatch_tool`, and
//! `fails_open_when_caller_context_is_indeterminate` below; the
//! payload/env marker resolution and the stdin→decision→stdout path run end to
//! end through the real binary in `tests/tm_hook_pm_guard.rs`
//! (`pm_guard_denies_agent_dispatch_from_native_subagent`,
//! `pm_guard_denies_task_dispatch_from_mpm_subagent_env`,
//! `pm_guard_allows_agent_dispatch_from_pm`,
//! `pm_guard_never_denies_send_message`).

use trusty_mpm::core::agent::is_subagent_dispatch_tool;

use crate::commands::misc::SUB_AGENT_ENV;
use crate::commands::pm_guard::payload_is_subagent_dispatch;

/// Deny reason for a subagent attempting to dispatch another subagent.
///
/// Why (#4784): a bare "denied" leaves the model guessing and it retries. The
/// text names both legitimate continuations — do the work directly, or hand
/// the parallelisable units back to the PM, which is the only session
/// authorised to fan out — and states explicitly that `SendMessage` still
/// works, so reporting back is never mistaken for a second blocked path.
/// What: the `permissionDecisionReason` string emitted on a fan-out deny.
/// Test: `denies_dispatch_tools_from_a_subagent`.
pub(crate) const FANOUT_DENY_REASON: &str = "Subagent fan-out denied (#4784): this session is itself a subagent, and a subagent must \
     never dispatch further subagents — orchestration is one level deep, and work dispatched \
     from here is invisible to the PM that owns it. Do this work yourself, or stop and report \
     back to the PM naming the units that should run in parallel and let the PM dispatch them. \
     SendMessage is never blocked — use it to report back or to ask for scope.";

/// Classify a `PreToolUse` call for subagent fan-out: `Some(reason)` denies,
/// `None` allows.
///
/// Why: kept pure — it takes the already-resolved caller context as a `bool`
/// rather than reading the environment itself — so the policy is exhaustively
/// unit-testable without env mutation, which races across test threads. The
/// marker resolution lives next door in [`caller_is_subagent`].
/// What: `Some(`[`FANOUT_DENY_REASON`]`)` when `tool_name` is a
/// `SUBAGENT_DISPATCH_TOOLS` member AND `caller_is_subagent` is true;
/// `None` (ALLOW) otherwise — which covers the PM's own dispatch, every
/// non-dispatch tool including `SendMessage`, and the indeterminate case.
/// Test: `denies_dispatch_tools_from_a_subagent`,
/// `allows_dispatch_tools_from_the_pm`, `never_denies_send_message`,
/// `allows_every_non_dispatch_tool`,
/// `fails_open_when_caller_context_is_indeterminate`.
pub(crate) fn evaluate_subagent_fanout(
    tool_name: &str,
    caller_is_subagent: bool,
) -> Option<&'static str> {
    if !caller_is_subagent {
        return None;
    }
    is_subagent_dispatch_tool(tool_name).then_some(FANOUT_DENY_REASON)
}

/// Whether the session issuing this `PreToolUse` call is itself a subagent.
///
/// Why: this is the whole difficulty of #4784 — the PM must keep dispatching,
/// so the guard is only as safe as this predicate. Both signals below are
/// AUTOMATIC markers (a harness or a spawn helper sets them; no per-call human
/// or model decision is involved), which is what makes them trustworthy here.
/// No prose, transcript-path shape, or agent-name heuristic is consulted.
/// What: `true` when the payload carries a non-empty `agent_id` (native
/// `Task`/`Agent` dispatch — delegated to
/// [`payload_is_subagent_dispatch`] so the `agent_id` rule has one definition
/// in this crate) OR when [`SUB_AGENT_ENV`] is present (trusty-agents'
/// out-of-band subagent processes). Absent both, `false` — the FAIL-OPEN
/// answer, per this module's doc.
/// Test: the payload arm is pinned by
/// `pm_guard::payload_is_subagent_dispatch_*`; both arms and their `false`
/// case run through the real binary in `tests/tm_hook_pm_guard.rs`
/// (`pm_guard_denies_agent_dispatch_from_native_subagent`,
/// `pm_guard_denies_task_dispatch_from_mpm_subagent_env`,
/// `pm_guard_allows_agent_dispatch_from_pm`), where env is controlled
/// per-process instead of raced across threads.
pub(crate) fn caller_is_subagent(payload: &serde_json::Value) -> bool {
    payload_is_subagent_dispatch(payload) || std::env::var_os(SUB_AGENT_ENV).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use trusty_mpm::core::agent::SUBAGENT_DISPATCH_TOOLS;

    #[test]
    fn denies_dispatch_tools_from_a_subagent() {
        // Both names must be denied: Claude Code called this tool `Task` in
        // earlier releases and `Agent` in current ones (see
        // `core::agent::SUBAGENT_DISPATCH_TOOLS`).
        for tool in SUBAGENT_DISPATCH_TOOLS {
            assert_eq!(
                evaluate_subagent_fanout(tool, true),
                Some(FANOUT_DENY_REASON),
                "expected a subagent's {tool} dispatch to be denied"
            );
        }
        // The reason must actually tell the agent what to do instead.
        assert!(FANOUT_DENY_REASON.contains("report back to the PM"));
        assert!(FANOUT_DENY_REASON.contains("SendMessage is never blocked"));
    }

    #[test]
    fn allows_dispatch_tools_from_the_pm() {
        // The entire orchestration model depends on this arm: the PM is not a
        // subagent, so its dispatches must pass untouched.
        for tool in SUBAGENT_DISPATCH_TOOLS {
            assert_eq!(
                evaluate_subagent_fanout(tool, false),
                None,
                "the PM's own {tool} dispatch must never be denied"
            );
        }
    }

    #[test]
    fn never_denies_send_message() {
        // SendMessage is how a denied agent reports back and gets resumed —
        // denying it would strand the agent this guard redirects. Asserted in
        // BOTH caller contexts, because "never" is the contract.
        for caller_is_subagent in [true, false] {
            assert_eq!(
                evaluate_subagent_fanout("SendMessage", caller_is_subagent),
                None,
                "SendMessage must never be denied (caller_is_subagent={caller_is_subagent})"
            );
        }
    }

    #[test]
    fn allows_every_non_dispatch_tool() {
        // A subagent keeps its full working tool surface; only fan-out is cut.
        for tool in [
            "Read",
            "Edit",
            "Write",
            "Bash",
            "Grep",
            "Glob",
            "TodoWrite",
            "WebFetch",
            "mcp__trusty-search__search",
            // Case-sensitive: an unrelated user tool named `agent`/`task`
            // must not be swept up.
            "agent",
            "task",
        ] {
            assert_eq!(
                evaluate_subagent_fanout(tool, true),
                None,
                "expected {tool} to be allowed inside a subagent"
            );
        }
    }

    #[test]
    fn fails_open_when_caller_context_is_indeterminate() {
        // #4784: a payload carrying neither marker is INDETERMINATE, and
        // indeterminate must resolve to ALLOW — a false deny against the PM
        // halts all orchestration, a false allow merely reproduces the
        // pre-guard behaviour. `caller_is_subagent` reports false for such a
        // payload (env arm is exercised in the integration test).
        let indeterminate = serde_json::json!({"tool_name": "Agent"});
        assert_eq!(
            evaluate_subagent_fanout("Agent", payload_is_subagent_dispatch(&indeterminate)),
            None,
            "an indeterminate caller context must fail OPEN"
        );
        // An empty-string agent_id is equally indeterminate, not a subagent.
        let empty_id = serde_json::json!({"agent_id": "", "tool_name": "Agent"});
        assert_eq!(
            evaluate_subagent_fanout("Agent", payload_is_subagent_dispatch(&empty_id)),
            None
        );
    }
}
