//! Project account names appended to an ops agent's dispatch prompt (#11390).
//!
//! Why: owner ask (2026-11-16) — ops agents must know the account NAMES a
//! project uses (gh, vercel, neon, …) so they never ask for a permission on
//! the wrong account. Deployed agent files are machine-global and
//! checksum-tracked, so project data reaches the agent at dispatch time
//! instead, through the same `hookSpecificOutput.updatedInput` the worktree
//! grant uses.
//! What: [`account_rewrite`] returns the dispatch input with the
//! `## Project accounts` block appended when the dispatch is an `Agent` call
//! whose `subagent_type` has `role: ops` in the roster. [`compose`] applies it
//! on top of a worktree-grant rewrite, so the hook prints ONE `updatedInput`
//! carrying both notes. #7684: every `Agent`/`Task` dispatch, ops or not, gets
//! the project's CLAUDE.md rules (`core::project_rules`), because deployed
//! agents start with `omitClaudeMd: true` and never read the file themselves.
//! Test: `ops_dispatch_gets_the_account_block`, `non_ops_dispatch_is_unchanged`,
//! `task_dispatch_is_unchanged`, `no_roster_means_no_block`,
//! `block_is_appended_once`, `block_composes_with_the_in_place_note`,
//! `any_agent_dispatch_gets_the_project_rules`, `project_rules_are_appended_once`,
//! `no_claude_md_leaves_a_dispatch_unchanged`,
//! `carrier_response_holds_only_the_updated_input`.

use std::path::Path;

use serde_json::Value;
use trusty_mpm::core::content_source::AgentRoster;
use trusty_mpm::core::dispatch_isolation::{agent_role, dispatch_agent};
use trusty_mpm::core::project_accounts::{
    ACCOUNT_BLOCK_HEADING, AccountMap, account_block, accounts_for,
};
use trusty_mpm::core::project_rules::{PROJECT_RULES_HEADING, rules_block, rules_for};

/// The only dispatch tool that receives the account block; the rules block
/// also goes to `Task` (#7684).
const ACCOUNT_DISPATCH_TOOL: &str = "Agent";

/// The roster `role:` whose agents act through third-party accounts.
const OPS_ROLE: &str = "ops";

/// The dispatch input with the account block (ops dispatches) and the project
/// rules block (every `Agent` dispatch, #7684) appended.
///
/// What: `None` when neither block applies: any tool but `Agent`, a
/// promptless input, or blocks already present; the account block also needs
/// a roster naming the agent `role: ops`, the rules block a project CLAUDE.md.
pub(crate) fn account_rewrite(
    roster: Option<&AgentRoster>,
    tool_name: &str,
    tool_input: Option<&Value>,
    cwd: &Path,
) -> Option<Value> {
    let accounts = if is_ops_dispatch(roster, tool_name, tool_input) {
        append_account_block(tool_input?, &accounts_for(cwd))
    } else {
        None
    };
    // #7684: every `Agent` dispatch also carries the project's CLAUDE.md
    // rules, because deployed agents start with `omitClaudeMd: true`.
    match rules_rewrite(tool_name, accounts.as_ref().or(tool_input), cwd) {
        Some(both) => Some(both),
        None => accounts,
    }
}

/// `input` with the project rules block appended, for an `Agent` or `Task` dispatch.
///
/// What: `None` for any other tool, a promptless input, a project with
/// no `CLAUDE.md` rules, or a prompt already carrying the block (#7684).
/// Test: `any_agent_dispatch_gets_the_project_rules`,
/// `project_rules_are_appended_once`, `no_claude_md_leaves_a_dispatch_unchanged`.
fn rules_rewrite(tool_name: &str, input: Option<&Value>, cwd: &Path) -> Option<Value> {
    // #7684: `Task` is the older name of the same dispatch tool.
    if !matches!(tool_name, ACCOUNT_DISPATCH_TOOL | "Task") {
        return None;
    }
    let input = input?;
    let prompt = input.get("prompt").and_then(Value::as_str)?;
    if prompt.contains(PROJECT_RULES_HEADING) {
        return None;
    }
    let rules = rules_for(cwd)?;
    let mut out = input.clone();
    out["prompt"] = Value::String(format!("{prompt}{}", rules_block(&rules)));
    Some(out)
}

/// The `PreToolUse` response that carries the project rules on a dispatch, or `None`.
///
/// Why (#7684): deployed agents start with `omitClaudeMd: true`, so the rules
/// carrier must run on every hook-running tm path. A pm-guard bypass
/// (`TRUSTY_MPM_DISABLE_HOOKS`, `TRUSTY_MPM_PM_UNRESTRICTED`, divert workers)
/// is hooks off and skips it, and `[pm_guard] enabled = false` removes the
/// pm-guard entry, where the always-installed egress guard carries the rules.
/// What: [`rules_rewrite`] over the payload's `tool_name`/`tool_input`, with
/// the hook `cwd` (falling back to `fallback_cwd`), rendered as one
/// `updatedInput` object with no permission decision.
/// Test: `an_unbypassed_dispatch_carries_the_rules`,
/// `the_egress_carrier_runs_only_when_pm_guard_is_off`.
pub(crate) fn rules_carrier_response(payload: &Value, fallback_cwd: &Path) -> Option<Value> {
    let tool_name = payload.get("tool_name").and_then(Value::as_str)?;
    let cwd = payload
        .get("cwd")
        .and_then(Value::as_str)
        .map_or(fallback_cwd, Path::new);
    let input = rules_rewrite(tool_name, payload.get("tool_input"), cwd)?;
    Some(serde_json::json!({
        "hookSpecificOutput": {"hookEventName": "PreToolUse", "updatedInput": input}
    }))
}

/// The egress guard's share of the rules carrier (#7684).
///
/// What: [`rules_carrier_response`] only when `pm_guard_enabled()` is false,
/// because an enabled pm-guard carries the rules itself (unless bypassed) and
/// a hook prints one object; two carriers would rewrite the same input twice.
/// The config is read only for a dispatch tool.
/// Test: `the_egress_carrier_runs_only_when_pm_guard_is_off`.
pub(crate) fn egress_rules_carrier(
    payload: Option<&Value>,
    pm_guard_enabled: impl FnOnce() -> bool,
) -> Option<Value> {
    let payload = payload?;
    let tool = payload.get("tool_name").and_then(Value::as_str)?;
    if !matches!(tool, ACCOUNT_DISPATCH_TOOL | "Task") || pm_guard_enabled() {
        return None;
    }
    let cwd = std::env::current_dir().unwrap_or_default();
    rules_carrier_response(payload, &cwd)
}

/// `updated` with the account block added when the dispatch is an ops one.
///
/// Why: a `PreToolUse` hook prints one object, so the worktree-grant rewrite
/// and this one must merge rather than compete.
pub(crate) fn compose(
    roster: Option<&AgentRoster>,
    tool_name: &str,
    updated: Value,
    cwd: &Path,
) -> Value {
    account_rewrite(roster, tool_name, Some(&updated), cwd).unwrap_or(updated)
}

/// Is this an `Agent` dispatch of an agent the roster declares `role: ops`?
fn is_ops_dispatch(roster: Option<&AgentRoster>, tool_name: &str, input: Option<&Value>) -> bool {
    let (Some(roster), Some(agent)) = (roster, dispatch_agent(input)) else {
        return false;
    };
    tool_name == ACCOUNT_DISPATCH_TOOL && agent_role(roster, agent).as_deref() == Some(OPS_ROLE)
}

/// `input` with the rendered block appended to its string `prompt`, once.
fn append_account_block(input: &Value, accounts: &AccountMap) -> Option<Value> {
    let prompt = input.get("prompt").and_then(Value::as_str)?;
    // #11390: idempotent — a re-evaluated dispatch never gets a second block.
    if prompt.contains(ACCOUNT_BLOCK_HEADING) {
        return None;
    }
    let mut out = input.clone();
    out["prompt"] = Value::String(format!("{prompt}{}", account_block(accounts)));
    Some(out)
}

#[cfg(test)]
#[path = "pm_guard_accounts_tests.rs"]
mod tests;
