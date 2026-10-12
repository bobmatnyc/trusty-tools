//! The channel-taint arm of `tm hook --egress-guard` (#8433 P0, D09).
//!
//! Why: a session that received a Slack-originated answer is tainted, and the
//! egress guard must refuse its path tool calls until the policy readiness
//! gate (#8454 S4) and the sensitive-path tripwire (S6) narrow the refusal. A
//! failed taint re-apply at SessionStart leaves the new id tainted, so this is
//! where D09's "egress denied" takes effect.
//! What: [`taint_verdict`] judges a gated tool — the path-touching tools in
//! [`GATED_TOOLS`] — by [`channels_taint::read_state`] for the payload's
//! `session_id`, under the hook's home. A tainted session, a gated call with no
//! string `session_id`, and an unknown home each deny; an untainted session
//! and every other tool are not judged here. Bypass variables and `agent_id`
//! are never read.
//! Test: `egress_guard_taint_tests.rs`; end to end in
//! `tests/tm_hook_session_start_taint_8433.rs`.

use std::path::Path;

use serde_json::Value;
use trusty_mpm::core::channels_taint;

/// The audit slug a taint deny records.
pub(crate) const TAINT_CHECK: &str = "egress-channel-tainted";

/// The tools whose calls can reach a path (the S4 plan's gated class).
const GATED_TOOLS: &[&str] = &[
    "Read",
    "Edit",
    // #8433 fix round: writes a path like `Edit` (`pm_guard.rs` EDIT_TOOLS).
    "MultiEdit",
    "Write",
    "NotebookEdit",
    "Glob",
    "Grep",
    "Bash",
];

/// The deny reason for a gated call from a tainted (or unknowable) session,
/// or `None` when the call is not judged or the session is untainted.
///
/// Why (fail closed): only a provably absent taint file allows. A payload
/// that names no session, and a home the guard cannot find, could be a
/// tainted session's, so they deny like one.
/// Test: `untainted_session_gated_call_is_not_judged`,
/// `tainted_session_gated_call_denies`, `gated_call_without_session_id_denies`,
/// `unknown_home_denies_gated_call`, `non_gated_tool_is_not_judged`,
/// `tainted_session_multiedit_denies`.
pub(crate) fn taint_verdict(payload: &Value, home: Option<&Path>) -> Option<String> {
    let tool = payload.get("tool_name").and_then(Value::as_str)?;
    if !GATED_TOOLS.contains(&tool) {
        return None;
    }
    let Some(session) = payload.get("session_id").and_then(Value::as_str) else {
        return Some(format!(
            "tm hook --egress-guard denies `{tool}`: the call names no `session_id`, so the guard \
             cannot rule out a channel-tainted session (#8433). Claude Code sends one on every \
             hook; a repeat means the hook is not receiving PreToolUse JSON."
        ));
    };
    let Some(home) = home else {
        return Some(format!(
            "tm hook --egress-guard denies `{tool}`: the home directory is unknown, so the guard \
             cannot read the channel taint for this session (#8433)."
        ));
    };
    let state = channels_taint::read_state(&channels_taint::taint_dir(home), session);
    state.is_tainted().then(|| {
        format!(
            "tm hook --egress-guard denies `{tool}`: this session is channel-tainted ({state:?}) — \
             it received an answer through an outbound channel, or its taint could not be \
             re-applied at session start — so its path tool calls are refused until the managed \
             session ends (#8433). Ask the operator."
        )
    })
}

#[cfg(test)]
#[path = "egress_guard_taint_tests.rs"]
mod tests;
