//! Who may pass the egress family rule: a PM or Architect main thread
//! (#11273). The file keeps its #11194 name; the owner-DM-only send it held
//! folded into the main-thread pass of `egress_guard_architect`.
//!
//! Why: owner ruling 2026-11-14 — "Only PMs (including Architect) should be
//! allowed egress." The pass must not depend on the Architect's launch-record
//! binding (#11270), and a subagent must never pass.
//! What: [`main_thread_role`] is `Some` only when both hold:
//! - [`thread_kind`] is `Main`: the payload is an object with no `agent_id`
//!   and a non-empty string `session_id`, and `CLAUDE_MPM_SUB_AGENT` is unset.
//!   Claude Code writes `agent_id` on every subagent call and the subagent
//!   cannot remove it; the env marker can only make a call a subagent's.
//! - the launch stamp `TRUSTY_MPM_SESSION_PROFILE` tm puts on the spawned
//!   `claude` names the PM ([`pm_launch_stamp`]) or the Architect
//!   (`architect_compat::is_architect_stamp`). It is the `claude` process's
//!   own environment, which Claude Code passes to its hooks; nothing a tool
//!   call carries, and no `export` in a Bash call, reaches it.
//!
//! FAIL-CLOSED: a non-object payload, a non-string or empty `agent_id`, a
//! missing or empty `session_id`, a subagent marker, and a missing, empty or
//! unknown stamp are each `None`, which keeps the deny. `tool_input` is never
//! read here.
//! Test: `egress_guard_pm_dm_tests.rs`; end to end in
//! `tests/tm_hook_egress_pm_dm_11194.rs`.

use serde_json::Value;
use trusty_mpm::core::architect_compat::is_architect_stamp;
use trusty_mpm::core::session_profile::{SessionProfile, launch_env};
use trusty_mpm::core::twin_identity::{ThreadKind, thread_kind};

use crate::commands::pm_guard_trust_anchor::HookEnv;

/// The role of a main thread that passes the family rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MainRole {
    /// A tm-launched PM's main thread.
    Pm,
    /// The Architect's main thread, bound to a launch record or not.
    Architect,
}

impl MainRole {
    /// The role as the audit line names it.
    pub(crate) fn label(self) -> &'static str {
        match self {
            MainRole::Pm => "pm",
            MainRole::Architect => "architect",
        }
    }
}

/// Whether the session carries the stamp `tm` writes for a PM launch.
///
/// Why: `session_profile::hook_profile` reads a missing stamp as PM, which
/// would fail open here; an unstamped session is not a tm-launched PM.
/// What: `env.stamp` equals the value of `session_profile::launch_env(Pm)`,
/// exactly, as `architect_compat::is_architect_stamp` matches its stamp.
/// Test: `unstamped_or_unknown_stamped_main_thread_stays_denied_11273`.
fn pm_launch_stamp(env: &HookEnv) -> bool {
    let (_, pm) = launch_env(SessionProfile::Pm);
    env.stamp.as_deref() == Some(std::ffi::OsStr::new(&pm))
}

/// The main-thread role of the call `payload` describes, or `None`.
///
/// Why / What: see the module doc. #11273: replaces the #10447 identity
/// (stamp, allowlist and launch record) and the #11194 PM stamp test for the
/// egress pass; neither the user config nor the process table is read.
/// Test: `pm_and_architect_main_threads_pass_every_family_11273`,
/// `subagent_calls_keep_the_deny_11273`,
/// `unknown_thread_or_malformed_payload_stays_denied_11273`,
/// `unstamped_or_unknown_stamped_main_thread_stays_denied_11273`,
/// `tool_input_cannot_present_a_subagent_as_main_11273`.
pub(crate) fn main_thread_role(payload: &Value, env: &HookEnv) -> Option<MainRole> {
    // #11273: the #10447/#11194 thread test; Subagent and Unknown keep the deny.
    if thread_kind(payload, env.sub_agent) != ThreadKind::Main {
        return None;
    }
    if is_architect_stamp(env.stamp.as_deref()) {
        return Some(MainRole::Architect);
    }
    pm_launch_stamp(env).then_some(MainRole::Pm)
}

#[cfg(test)]
#[path = "egress_guard_pm_dm_tests.rs"]
mod tests;
