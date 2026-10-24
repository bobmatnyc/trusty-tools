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
//! trust-anchor floor ([`verdict`]), [`decide`]s, applies the channel-taint
//! arm with its policy readiness gate ([`with_taint_async`], #8433, #9741;
//! the daemon's verdict first, #8454 VC) and, on a ready policy, the CTO
//! tripwire ([`tripwire_verdict`], #8454 S6), and on
//! a deny records it through `pm_guard_deny_log`, prints the deny from
//! `build_pm_guard_deny_response`, and exits 0. It fails closed: an unusable
//! payload and a panic ([`deny_on_panic`]) both deny. Any other tool gets no
//! output, which Claude Code reads as "no decision".
//! Test: `egress_guard_tests.rs`; end to end in
//! `tests/tm_hook_egress_guard_8454.rs`.

use std::panic::AssertUnwindSafe;

use futures::FutureExt as _;

use crate::commands::egress_guard_bash::{
    BASH_CHECK, bash_deny_reason, bash_egress_deny, bash_oversize_reason,
};
use crate::commands::egress_guard_families::{Classification, FamilyTable};
use crate::commands::egress_guard_taint::{POLICY_CHECK, policy_not_ready_reason, taint_verdict};
use crate::commands::egress_guard_tripwire::tripwire_verdict;
use crate::commands::egress_verdict_client::cached_policy_ready;
use crate::commands::hook_stdin::{
    HookStdinError, PM_GUARD_STDIN_TIMEOUT, exit_after_decision, read_stdin_hook_payload_strict,
};
use crate::commands::pm_guard_deny_log::{DenyContext, post_deny_audit, record_deny};
use crate::commands::pm_guard_response::build_pm_guard_deny_response;
use crate::commands::pm_guard_trust_anchor::{HookEnv, TRUST_ANCHOR_RULE, evaluate};
use trusty_mpm::core::channels_readiness::{NotReady, policy_ready};
use trusty_mpm::core::config::MpmConfig;
use trusty_mpm::core::session_profile;

/// The deny reason printed when the guard panics.
const PANIC_REASON: &str = "tm hook --egress-guard failed while deciding this tool call, so \
     it denies it (#8454). A repeat is a bug in the guard; report it.";

/// Claude Code's built-in MCP resource tools, which name their server in
/// `tool_input.server` rather than in `tool_name` (#8454 fix round).
const RESOURCE_TOOLS: &[&str] = &["ReadMcpResourceTool", "ListMcpResourcesTool"];

/// Claude Code's shell tool, judged by its `tool_input.command` (#8454 S3).
const BASH_TOOL: &str = "Bash";

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
/// and a `tool_input` that is not an object each deny. A `Bash` call is
/// judged by [`bash_verdict`] (#8454 S3). A [`RESOURCE_TOOLS`]
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
    // #8454 S3: a Bash command is judged by its text.
    if tool_name == BASH_TOOL {
        return bash_verdict(input);
    }
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

/// The Bash arm of [`decide`] (#8454 S3).
///
/// Why (§4 fail closed): Claude Code always sends a string `command`, so any
/// other shape is a delivery fault, denied like a malformed payload.
/// What: a missing or non-string `tool_input.command` denies as
/// `egress-unclassifiable-payload`; a command over the byte cap denies as
/// [`BASH_CHECK`] naming the cap (#9699); a command [`bash_egress_deny`]
/// denies is a [`BASH_CHECK`] deny with its reason — the plan §6 text, or the
/// walk budget (#9699); anything else allows.
/// Test: `bash_egress_denies_missing_or_non_string_command`,
/// `bash_egress_denies_a_command_over_the_byte_cap_9699`,
/// `bash_egress_trust_anchor_verdict_precedes_the_bash_arm`.
fn bash_verdict(input: &serde_json::Value) -> Verdict {
    let Some(command) = input.get("command").and_then(serde_json::Value::as_str) else {
        return deny(
            "egress-unclassifiable-payload",
            unclassifiable("a Bash `tool_input.command` is missing or not a string"),
        );
    };
    // #9699: an over-cap command is denied before the walk reads any of it.
    if let Some(reason) = bash_oversize_reason(command) {
        return deny(BASH_CHECK, reason);
    }
    match bash_egress_deny(command) {
        Some(found) => deny(BASH_CHECK, bash_deny_reason(&found)),
        None => Verdict::Allow,
    }
}

/// The over-cap deny for a Bash payload, before any parser reads it (#9699).
fn oversize_bash(payload: &serde_json::Value) -> Option<Verdict> {
    if payload.get("tool_name")?.as_str()? != BASH_TOOL {
        return None;
    }
    let command = payload.get("tool_input")?.get("command")?.as_str()?;
    bash_oversize_reason(command).map(|reason| deny(BASH_CHECK, reason))
}

/// The guard's verdict: the trust-anchor floor first, then [`decide`].
///
/// Why (#8454 S2, Architect D1): `[pm_guard] enabled = false` leaves the
/// pm-guard hook out, and with it the #8878 trust-anchor floor; this hook has
/// no off switch, so it runs that floor itself.
/// What: a Bash command over the byte cap denies first, unread (#9699); then,
/// when the read is a payload and `anchor` denies it, a `trust-anchor-write`
/// deny; otherwise [`decide`]'s verdict.
/// Test: `egress_trust_anchor_verdict_precedes_the_family_check`,
/// `bash_egress_cap_precedes_the_trust_anchor_floor_9699`.
pub(crate) fn verdict(
    read: &Result<serde_json::Value, HookStdinError>,
    table: &FamilyTable,
    anchor: impl FnOnce(&serde_json::Value) -> Option<String>,
) -> Verdict {
    if let Ok(payload) = read {
        // #9699: the anchor floor parses the command too, so the cap runs first.
        if let Some(oversize) = oversize_bash(payload) {
            return oversize;
        }
        if let Some(reason) = anchor(payload) {
            return deny(TRUST_ANCHOR_RULE, reason);
        }
    }
    decide(read, table)
}

/// Where the channel-taint arm stands before the policy readiness gate.
enum TaintStep<'a> {
    /// The verdict is settled; readiness is not read.
    Done(Verdict),
    /// A tainted gated call: readiness decides which deny it gets.
    Policy {
        payload: &'a serde_json::Value,
        taint: String,
    },
}

/// The taint arm up to the readiness gate: a deny stands, an allow of a
/// readable payload that [`taint_verdict`] refuses needs the policy.
/// #9923: `launch` is the session's launch directory, `None` when unknown.
fn taint_step<'a>(
    first: Verdict,
    read: &'a Result<serde_json::Value, HookStdinError>,
    home: Option<&std::path::Path>,
    launch: Option<&std::path::Path>,
) -> TaintStep<'a> {
    match (first, read) {
        (Verdict::Allow, Ok(payload)) => match taint_verdict(payload, home, launch) {
            None => TaintStep::Done(Verdict::Allow),
            Some(taint) => TaintStep::Policy { payload, taint },
        },
        (first, _) => TaintStep::Done(first),
    }
}

/// The deny for a tainted gated call, given its policy readiness.
fn taint_deny(
    payload: &serde_json::Value,
    home: Option<&std::path::Path>,
    taint: String,
    ready: Result<(), NotReady>,
) -> Verdict {
    // #9741: readiness runs before the taint deny; both arms deny.
    match ready {
        // #8454 S6: a ready policy hands the call to the CTO tripwire.
        Ok(()) => tripwire_verdict(payload, home, taint),
        Err(not_ready) => deny(
            POLICY_CHECK,
            policy_not_ready_reason(payload, &not_ready, &taint),
        ),
    }
}

/// [`verdict`], then the channel-taint arm for a call it allowed (#8433),
/// with the policy readiness gate ahead of the taint deny (#9741).
///
/// Why (#9741, Architect ruling a): a tainted session's gated call is denied
/// either way; a policy that is not ready is reported as the cause first.
/// What: a deny stands. An allow of a readable payload that [`taint_verdict`]
/// refuses runs `ready`: not ready is a [`POLICY_CHECK`] deny naming the
/// branch; ready goes to [`tripwire_verdict`] (#8454 S6), which denies a call
/// in a listed project as `egress-channel-sensitive-path` and any other as
/// `egress-channel-tainted`. `ready` runs for no other call.
/// #8454 VC: the guard runs [`with_taint_async`]; this synchronous form,
/// over the same steps, drives the S4 tests.
/// Test: `with_taint_denies_an_allowed_gated_call_of_a_tainted_session`,
/// `config_missing_denies_a_gated_call`, `ready_policy_falls_through_to_taint_deny`,
/// `untainted_session_never_reads_the_policy`.
#[cfg(test)]
pub(crate) fn with_taint(
    first: Verdict,
    read: &Result<serde_json::Value, HookStdinError>,
    home: Option<&std::path::Path>,
    launch: Option<&std::path::Path>,
    ready: impl FnOnce() -> Result<(), NotReady>,
) -> Verdict {
    match taint_step(first, read, home, launch) {
        TaintStep::Done(verdict) => verdict,
        TaintStep::Policy { payload, taint } => taint_deny(payload, home, taint, ready()),
    }
}

/// `with_taint` with an async readiness gate (#8454 VC).
///
/// Why: the readiness may come from the daemon's verdict, which is a
/// network query; an untainted session must still never read the policy.
/// What: the same steps as `with_taint`; `ready` is called and awaited
/// only for a tainted gated call.
/// Test: `untainted_session_never_queries_the_daemon`,
/// `daemon_unreachable_falls_back_to_inline_load_never_allow`.
pub(crate) async fn with_taint_async<F>(
    first: Verdict,
    read: &Result<serde_json::Value, HookStdinError>,
    home: Option<&std::path::Path>,
    launch: Option<&std::path::Path>,
    ready: impl FnOnce() -> F,
) -> Verdict
where
    F: std::future::Future<Output = Result<(), NotReady>>,
{
    match taint_step(first, read, home, launch) {
        TaintStep::Done(verdict) => verdict,
        TaintStep::Policy { payload, taint } => taint_deny(payload, home, taint, ready().await),
    }
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
    let env = HookEnv::ambient();
    let anchor = |payload: &serde_json::Value| evaluate(payload, &env, MpmConfig::load_default);
    let first = verdict(&read, &FamilyTable::builtin(), anchor);
    let home = env.home.as_deref();
    // #9923 (Architect Q10): the launch directory, which a `cd` does not move.
    let launch = session_profile::hook_project_dir(env.project_dir.clone());
    // #9741: the policy loads under the same home the taint is read from.
    // #8454 VC: the daemon's verdict first; the inline load where none is served.
    let ready = || cached_policy_ready(url, home, || policy_ready(home));
    let verdict = with_taint_async(first, &read, home, launch.as_deref(), ready).await;
    let Verdict::Deny { check, reason } = verdict else {
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
/// An allow returns `Ok(())` with nothing printed. `url` is the daemon the
/// taint arm asks for its policy verdict (#8454 VC) and the audit POST goes to.
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
