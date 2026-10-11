//! `tm hook --egress-guard`: deny every outbound-channel MCP tool, for every
//! caller (#8454 G3, slice S1).
//!
//! Why: a tm-launched session must not message anyone through Slack, Gmail,
//! Notion or Google Chat on its own; the daemon's owner channel is meant to be
//! the only outbound path. `tm hook --pm-guard` cannot carry this rule: both
//! of its bypass variables lift it, it exempts subagents, and its settings
//! entry can be removed by the pm-guard opt-out. So this is a separate mode
//! that never reads `TRUSTY_MPM_PM_UNRESTRICTED` or `TRUSTY_MPM_DISABLE_HOOKS`.
//! `CLAUDE_MPM_SUB_AGENT` and the payload's `agent_id` are never read to allow:
//! only the trust-anchor floor reads them, to narrow its Architect exemption
//! (#8454 S2, Architect D1). It binds every tm-launched session, subagents
//! included (Bob ruling 3). It never calls
//! `ChannelPolicy::check_egress`: an `Allow` from a channel policy must not
//! read as permission to run a family tool.
//! What: [`egress_guard`] reads the `PreToolUse` payload, runs the #8878
//! trust-anchor floor ([`verdict`]), [`decide`]s, and on a deny records it through `pm_guard_deny_log`, prints the deny from
//! `build_pm_guard_deny_response`, and exits 0. It fails closed: an unusable
//! payload and a panic ([`deny_on_panic`]) both deny. Any other tool gets no
//! output, which Claude Code reads as "no decision".
//! Test: `egress_guard_tests.rs`; end to end in
//! `tests/tm_hook_egress_guard_8454.rs`.

use std::panic::AssertUnwindSafe;

use futures::FutureExt as _;

use crate::commands::egress_guard_families::{Classification, FamilyTable};
use crate::commands::hook_stdin::{
    HookStdinError, PM_GUARD_STDIN_TIMEOUT, exit_after_decision, read_stdin_hook_payload_strict,
};
use crate::commands::pm_guard_deny_log::{DenyContext, post_deny_audit, record_deny};
use crate::commands::pm_guard_response::build_pm_guard_deny_response;
use crate::commands::pm_guard_trust_anchor::{HookEnv, TRUST_ANCHOR_RULE, evaluate};
use trusty_mpm::core::config::MpmConfig;

/// The deny reason printed when the guard panics.
const PANIC_REASON: &str = "tm hook --egress-guard failed while deciding this tool call, so \
     it denies it (#8454). A repeat is a bug in the guard; report it.";

/// Claude Code's built-in MCP resource tools, which name their server in
/// `tool_input.server` rather than in `tool_name` (#8454 fix round).
const RESOURCE_TOOLS: &[&str] = &["ReadMcpResourceTool", "ListMcpResourcesTool"];

/// The guard's decision for one tool call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// No decision: the call goes through the normal permission flow.
    Allow,
    /// Refuse the call; `check` is the audit slug, `reason` the agent's text.
    Deny { check: &'static str, reason: String },
}

/// A [`Verdict::Deny`] for `check`.
fn deny(check: &'static str, reason: String) -> Verdict {
    Verdict::Deny { check, reason }
}

/// The deny reason for a parsed payload whose `detail` field is unusable.
fn unclassifiable(detail: &str) -> String {
    format!(
        "tm hook --egress-guard cannot classify this tool call: {detail}. Claude Code sends \
         every PreToolUse hook a string `tool_name` and an object `tool_input`, so it denies \
         (#8454). Retry the call; a repeat means the hook is not receiving PreToolUse JSON."
    )
}

/// Decide one tool call from its stdin read.
///
/// Why (§4 of the G3 plan): fail closed. Each fault a payload can carry is a
/// deny, never a default — no `unwrap_or_default` on a missing field.
/// What: an `Err` read, a `tool_name` that is missing, empty or not a string,
/// and a `tool_input` that is not an object each deny. A [`RESOURCE_TOOLS`]
/// call is judged by its `tool_input.server`, and denies when that is missing,
/// empty or not a string. Otherwise the [`FamilyTable`] classifies the tool:
/// a family or a malformed `mcp__` name denies, anything else allows.
/// Test: `egress_denies_malformed_payloads`, `egress_denies_each_family_tool_name`,
/// `egress_allows_unrelated_tools`, `egress_denies_family_mcp_resource_tools`,
/// `egress_denies_resource_tools_naming_no_server`.
pub(crate) fn decide(
    read: &Result<serde_json::Value, HookStdinError>,
    table: &FamilyTable,
) -> Verdict {
    let payload = match read {
        Ok(payload) => payload,
        Err(err) => {
            return deny(
                "egress-unreadable-payload",
                format!(
                    "tm hook --egress-guard could not use this tool call's stdin payload \
                     ({err}), so it denies it (#8454). Retry the call; a repeat means the hook \
                     is not receiving PreToolUse JSON."
                ),
            );
        }
    };
    let tool_name = match payload.get("tool_name").and_then(serde_json::Value::as_str) {
        Some(name) if !name.is_empty() => name,
        _ => {
            return deny(
                "egress-unclassifiable-payload",
                unclassifiable("`tool_name` is missing, empty or not a string"),
            );
        }
    };
    let Some(input) = payload.get("tool_input").filter(|v| v.is_object()) else {
        return deny(
            "egress-unclassifiable-payload",
            unclassifiable("`tool_input` is missing or is not an object"),
        );
    };
    let classification = if RESOURCE_TOOLS.contains(&tool_name) {
        // #8454 fix round: these name their server in `tool_input.server`.
        match input.get("server").and_then(serde_json::Value::as_str) {
            Some(server) if !server.trim().is_empty() => table.classify_resource_server(server),
            _ => {
                return deny(
                    "egress-unnamed-resource-server",
                    format!(
                        "tm hook --egress-guard denies `{tool_name}`: its `tool_input.server` \
                         is missing or not a non-empty string, so the call may reach an \
                         outbound-channel server — a listing with no server covers every \
                         server (#8454). Name the server."
                    ),
                );
            }
        }
    } else {
        table.classify(tool_name)
    };
    match classification {
        Classification::Unrelated => Verdict::Allow,
        Classification::Family(family) => deny(
            "egress-family",
            format!(
                "tm hook --egress-guard denies `{tool_name}`: it is a {family} tool, and no \
                 tm-launched session, PM or subagent, may use an outbound message channel \
                 directly (#8454). Read and list tools are denied too. Ask the operator \
                 instead."
            ),
        ),
        Classification::Malformed(detail) => deny(
            "egress-malformed-mcp-name",
            format!(
                "tm hook --egress-guard cannot tell which MCP server `{tool_name}` belongs to \
                 ({detail}), so it denies it (#8454)."
            ),
        ),
    }
}

/// The guard's verdict: the trust-anchor floor first, then [`decide`].
///
/// Why (#8454 S2, Architect D1): `[pm_guard] enabled = false` leaves the
/// pm-guard hook out, and with it the #8878 trust-anchor floor; this hook has
/// no off switch, so it runs that floor itself.
/// What: when the read is a payload and `anchor` denies it, a
/// `trust-anchor-write` deny; otherwise [`decide`]'s verdict.
/// Test: `egress_trust_anchor_verdict_precedes_the_family_check`.
pub(crate) fn verdict(
    read: &Result<serde_json::Value, HookStdinError>,
    table: &FamilyTable,
    anchor: impl FnOnce(&serde_json::Value) -> Option<String>,
) -> Verdict {
    if let Ok(payload) = read
        && let Some(reason) = anchor(payload)
    {
        return deny(TRUST_ANCHOR_RULE, reason);
    }
    decide(read, table)
}

/// Run one guard pass, answering a deny if it panics.
///
/// Why (§4, Architect Q5): Claude Code treats a non-zero hook exit as
/// non-blocking, so a guard that unwinds out of `main` fails open.
/// What: polls `run` under `catch_unwind`; its own answer on completion, the
/// [`PANIC_REASON`] deny on a panic. A `SIGKILL` or an abort is out of reach;
/// slice S5's `permissions.deny` is the independent layer for those.
/// Test: `egress_injected_panic_still_denies`.
pub(crate) async fn deny_on_panic<F>(run: F) -> Option<serde_json::Value>
where
    F: std::future::Future<Output = Option<serde_json::Value>>,
{
    match AssertUnwindSafe(run).catch_unwind().await {
        Ok(response) => response,
        Err(_) => Some(build_pm_guard_deny_response(PANIC_REASON)),
    }
}

/// Read, decide and audit one call; `Some(deny)` to print, `None` to allow.
///
/// What: the stdin read under `PM_GUARD_STDIN_TIMEOUT`, then [`verdict`]. A
/// deny is recorded and POSTed through `pm_guard_deny_log` first; neither can
/// change the verdict — a failed record is reported on stderr, a failed POST
/// is dropped, and a panic in either is caught by [`deny_on_panic`].
/// Test: `egress_deny_is_audited_without_changing_verdict_when_log_unwritable`.
async fn guard_once(url: &str) -> Option<serde_json::Value> {
    let read = read_stdin_hook_payload_strict(PM_GUARD_STDIN_TIMEOUT).await;
    // #8454 D1: the floor reads the ambient home and user config, as pm-guard's.
    let anchor = |payload: &serde_json::Value| {
        evaluate(payload, &HookEnv::ambient(), MpmConfig::load_default)
    };
    let Verdict::Deny { check, reason } = verdict(&read, &FamilyTable::builtin(), anchor) else {
        return None;
    };
    // An unreadable payload names nothing: audit it with every field empty.
    let null = serde_json::Value::Null;
    let ctx = DenyContext::from_payload(url, read.as_ref().unwrap_or(&null));
    record_deny(&ctx, check, &reason);
    post_deny_audit(&ctx, &reason).await;
    Some(build_pm_guard_deny_response(&reason))
}

/// `tm hook --egress-guard` entry point.
///
/// Why: see the module doc.
/// What: [`guard_once`] under [`deny_on_panic`]. A deny is printed as the one
/// stdout object and the process exits 0 at once, so a stdin that never
/// closed cannot hold the runtime's shutdown past Claude Code's hook timeout.
/// An allow returns `Ok(())` with nothing printed. `url` is used only for the
/// audit POST.
/// Test: `tests/tm_hook_egress_guard_8454.rs`.
pub(crate) async fn egress_guard(url: &str) -> anyhow::Result<()> {
    if let Some(response) = deny_on_panic(guard_once(url)).await {
        println!("{response}");
        exit_after_decision();
    }
    Ok(())
}

#[cfg(test)]
#[path = "egress_guard_tests.rs"]
mod tests;
