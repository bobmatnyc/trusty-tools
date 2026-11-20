//! The owner-DM Slack send arm of `tm hook --egress-guard` (#11194).
//!
//! Why: owner instruction 2026-11-13 — a tm-launched PM must be able to post
//! to the owner's Slack DM. Every other Slack target, every other family and
//! every subagent stays denied (#8454); the Architect keeps its wider allow
//! (#10447).
//! What: [`lift`] runs on the guard's verdict after the #10447 Architect lift.
//! It judges only an `egress-family` deny of exactly [`SEND_TOOL`] or
//! [`CANVAS_TOOL`] on the native `slack-mcp` server. The send's `channel` must
//! be the string [`OWNER_DM`], byte for byte; the canvas's `channel_id` must be
//! absent or that string. The call must come from a main thread
//! ([`thread_kind`] is `Main`: no payload `agent_id`, a `session_id`, and
//! `CLAUDE_MPM_SUB_AGENT` unset) — the same thread test the #10447 identity
//! uses. The session must carry the tm PM launch stamp ([`pm_launch_stamp`]);
//! `hook_profile` defaults an unstamped session to PM, so it is not used
//! here. [`audited`] lets the allow stand only once one line is appended to
//! the Architect audit file.
//! FAIL-CLOSED: any other tool, server, key shape or value, a non-object
//! input, an unreadable payload, a subagent, an unknown thread, and a missing,
//! empty or non-PM launch stamp keep the deny. An audit line that cannot be written denies, as for the Architect.
//! #11273: [`main_thread_role`] — the same thread and stamp tests, naming a PM
//! or Architect main session — is the read pass's identity; it lives here so
//! the stamp is compared in one file. No PM launch record exists yet, so this
//! arm stays a PM's only send.
//! Test: `egress_guard_pm_dm_tests.rs`; end to end in
//! `tests/tm_hook_egress_pm_dm_11194.rs`.

use std::path::Path;

use serde_json::Value;
use trusty_mpm::core::session_profile::{SessionProfile, launch_env};
use trusty_mpm::core::twin_identity::{ThreadKind, thread_kind};

use crate::commands::egress_guard::Verdict;
use crate::commands::egress_guard_architect::{audited_line, field};
use crate::commands::hook_stdin::HookStdinError;
use crate::commands::pm_guard_trust_anchor::HookEnv;

/// The owner's Slack DM, the one target this arm allows (#11194).
pub(crate) const OWNER_DM: &str = "D0AJ5P0PRTK";

/// The slack-mcp send tool; its target is `tool_input.channel`.
pub(crate) const SEND_TOOL: &str = "mcp__slack-mcp__slack_send_message";

/// The slack-mcp canvas tool; its optional target is `tool_input.channel_id`.
pub(crate) const CANVAS_TOOL: &str = "mcp__slack-mcp__slack_create_canvas";

/// The check slug of the deny this arm may lift.
const FAMILY_CHECK: &str = "egress-family";

/// The audit slug of an allowed owner-DM call.
pub(crate) const ALLOWED_AUDIT: &str = "egress-pm-dm-allowed";

/// An allowed owner-DM call, as its audit line names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PmDmAllowed {
    /// The tool name; never its input.
    pub(crate) tool: String,
    /// The payload's `session_id`.
    pub(crate) session: String,
    /// The target named, or `None` for a canvas with no `channel_id`.
    pub(crate) channel: Option<&'static str>,
}

impl PmDmAllowed {
    /// `egress-pm-dm-allowed time=<rfc3339> session=<id> family=slack
    /// tool=<name> channel=<id|->`, fields reduced as the Architect line's.
    ///
    /// Test: `pm_dm_audit_line_carries_no_body_and_no_newline`.
    pub(crate) fn audit_line(&self, time: &str) -> String {
        format!(
            "{ALLOWED_AUDIT} time={} session={} family=slack tool={} channel={}",
            field(time),
            field(&self.session),
            field(&self.tool),
            field(self.channel.unwrap_or(""))
        )
    }
}

/// `Some(target)` when `input` names only the owner's DM for `tool`.
///
/// What: [`SEND_TOOL`] needs `channel` to be the string [`OWNER_DM`];
/// [`CANVAS_TOOL`] needs `channel_id` absent (`Some(None)`) or that string.
/// A missing, null, non-string, empty or other value is `None`.
fn owner_dm_target(tool: &str, input: &Value) -> Option<Option<&'static str>> {
    let names_dm = |value: &Value| value.as_str() == Some(OWNER_DM);
    match tool {
        SEND_TOOL => input
            .get("channel")
            .filter(|v| names_dm(v))
            .map(|_| Some(OWNER_DM)),
        CANVAS_TOOL => match input.get("channel_id") {
            None => Some(None),
            Some(value) => names_dm(value).then_some(Some(OWNER_DM)),
        },
        _ => None,
    }
}

/// Whether the session carries the stamp `tm` writes for a PM launch.
///
/// Why: the egress hook also runs in sessions tm did not launch as a PM, and
/// `session_profile::hook_profile` reads a missing stamp as PM (fail open here).
/// What: `env.stamp` equals the value of `session_profile::launch_env(Pm)`,
/// exactly, as `architect_compat::is_architect_stamp` matches its stamp.
/// Test: `pm_unstamped_main_thread_dm_send_stays_denied`,
/// `pm_non_pm_stamped_main_thread_dm_send_stays_denied`.
pub(crate) fn pm_launch_stamp(env: &HookEnv) -> bool {
    let (_, pm) = launch_env(SessionProfile::Pm);
    env.stamp.as_deref() == Some(std::ffi::OsStr::new(&pm))
}

/// The launch role of a main session, as the #11273 read pass names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MainRole {
    /// A session carrying the tm PM launch stamp.
    Pm,
    /// A session carrying an Architect launch stamp, bound or not.
    Architect,
    /// A subagent of a PM session: the PM stamp, a subagent thread (GP-043).
    PmSubagent,
}

impl MainRole {
    /// The role as an audit line names it.
    pub(crate) fn label(self) -> &'static str {
        match self {
            MainRole::Pm => "pm",
            MainRole::Architect => "architect",
            MainRole::PmSubagent => "pm-subagent",
        }
    }
}

/// The role of a main session from payload and stamp evidence, or `None`.
///
/// Why: owner ruling 2026-11-14 (#11273) — "reads pass for any main session;
/// sends and writes need the tm launch record". This is the evidence a read
/// needs; it is not the launch record, so it never lifts a send or a write.
/// What: [`thread_kind`] must be `Main` (no payload `agent_id`, a string
/// `session_id`, `CLAUDE_MPM_SUB_AGENT` unset), and the stamp must be an
/// Architect stamp (`architect_compat::is_architect_stamp`) or exactly the
/// PM stamp ([`pm_launch_stamp`]).
/// FAIL-CLOSED: a subagent, an unknown thread, and a missing, empty or other
/// stamp are each `None`.
/// Test: `main_session_reads_pass_in_every_family_11273`,
/// `subagent_reads_stay_denied_11273`,
/// `unknown_thread_or_unstamped_session_reads_stay_denied_11273`.
pub(crate) fn main_thread_role(payload: &Value, env: &HookEnv) -> Option<MainRole> {
    // #11273: Subagent and Unknown keep the deny.
    if thread_kind(payload, env.sub_agent) != ThreadKind::Main {
        return None;
    }
    if trusty_mpm::core::architect_compat::is_architect_stamp(env.stamp.as_deref()) {
        return Some(MainRole::Architect);
    }
    pm_launch_stamp(env).then_some(MainRole::Pm)
}

/// Lift a Slack family deny for a main thread's send to the owner's DM.
///
/// Why / What: see the module doc.
/// Test: `pm_main_thread_may_send_to_the_owner_dm`,
/// `pm_send_to_any_other_target_stays_denied`,
/// `pm_subagent_dm_send_stays_denied`,
/// `pm_canvas_with_another_channel_id_stays_denied`,
/// `pm_canvas_with_no_channel_id_is_allowed`,
/// `pm_send_with_malformed_input_stays_denied`,
/// `pm_unstamped_main_thread_dm_send_stays_denied`.
pub(crate) fn lift(
    first: Verdict,
    read: &Result<Value, HookStdinError>,
    env: &HookEnv,
) -> (Verdict, Option<PmDmAllowed>) {
    let (Verdict::Deny { check, .. }, Ok(payload)) = (&first, read) else {
        return (first, None);
    };
    if *check != FAMILY_CHECK {
        return (first, None);
    }
    let text = |key: &str| payload.get(key).and_then(Value::as_str);
    let Some(tool) = text("tool_name") else {
        return (first, None);
    };
    let input = payload.get("tool_input").filter(|v| v.is_object());
    let Some(channel) = input.and_then(|input| owner_dm_target(tool, input)) else {
        return (first, None);
    };
    // #11194: the #10447 thread test; a subagent or unknown thread keeps the deny.
    if thread_kind(payload, env.sub_agent) != ThreadKind::Main {
        return (first, None);
    }
    // #11194 fix round: only a tm-launched PM; an unmanaged or non-PM session keeps the deny.
    if !pm_launch_stamp(env) {
        return (first, None);
    }
    let allowed = PmDmAllowed {
        tool: tool.to_owned(),
        session: text("session_id").unwrap_or("").to_owned(),
        channel,
    };
    (Verdict::Allow, Some(allowed))
}

/// The final verdict: an owner-DM allow stands only once audited.
///
/// What: `audited_line` over [`PmDmAllowed::audit_line`]; a failed write is
/// the Architect arm's `egress-architect-audit-unwritable` deny.
/// Test: `pm_dm_allow_needs_its_audit_line`.
pub(crate) fn audited(
    verdict: Verdict,
    allowed: Option<PmDmAllowed>,
    path: Option<&Path>,
    time: &str,
) -> Verdict {
    let line = allowed.map(|allowed| allowed.audit_line(time));
    audited_line(verdict, line, path, "send to the owner's Slack DM (#11194)")
}

#[cfg(test)]
#[path = "egress_guard_pm_dm_tests.rs"]
mod tests;
