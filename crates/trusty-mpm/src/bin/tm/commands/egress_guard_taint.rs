//! The channel-taint arm of `tm hook --egress-guard` (#8433 P0, D09).
//!
//! Why: a session that received a Slack-originated answer is tainted, and the
//! egress guard must refuse its path tool calls until the policy readiness
//! gate (#8454 S4) and the sensitive-path tripwire (S6) narrow the refusal. A
//! failed taint re-apply at SessionStart leaves the new id tainted, so this is
//! where D09's "egress denied" takes effect.
//! What: [`taint_verdict`] judges a gated tool — the path-touching tools in
//! [`GATED_TOOLS`], and a claude.ai connector tool not proven read-only
//! ([`connector_write`], #10735) — by [`channels_taint::read_state`] for the payload's
//! `session_id`, under the hook's home. A tainted session, a gated call with no
//! string `session_id`, and an unknown home each deny; every other tool is not
//! judged here. #9923: an untainted session under a live owner-answer mark,
//! by its launch directory or live cwd, is tainted and denied; one no mark
//! covers is not judged. Bypass variables and `agent_id`
//! are never read. #9741 (#8454 S4, ruling a): before that deny the guard
//! checks the route policy; a policy that is not ready denies as
//! [`POLICY_CHECK`] with [`policy_not_ready_reason`].
//! Test: `egress_guard_taint_tests.rs`, `egress_guard_policy_tests.rs`; end to
//! end in `tests/tm_hook_session_start_taint_8433.rs`,
//! `tests/tm_hook_egress_guard_8454.rs` and
//! `tests/tm_hook_egress_lazy_taint_9923.rs`.

use std::path::Path;

use serde_json::Value;
use trusty_mpm::core::channels_readiness::{NOT_READY_CHECK, NotReady};
use trusty_mpm::core::channels_taint;

use crate::commands::egress_guard_families::is_claude_ai_connector;

/// The audit slug a taint deny records.
pub(crate) const TAINT_CHECK: &str = "egress-channel-tainted";

/// The audit slug of a deny because the route policy is not ready (#9741).
pub(crate) const POLICY_CHECK: &str = NOT_READY_CHECK;

/// The deny reason for a gated call refused because the policy is not ready.
///
/// What: names the tool and the failed branch, then keeps `taint`, the taint
/// arm's own reason, so neither cause is hidden.
/// Test: `config_missing_denies_a_gated_call`.
pub(crate) fn policy_not_ready_reason(
    payload: &Value,
    not_ready: &NotReady,
    taint: &str,
) -> String {
    let tool = payload
        .get("tool_name")
        .and_then(Value::as_str)
        .unwrap_or("this tool");
    format!(
        "tm hook --egress-guard denies `{tool}`: the channel route policy is not ready — \
         {not_ready} — so the guard refuses this call before its channel-taint check (#9741, \
         #8454). `tm doctor` shows the cause in its `channels_*` and `egress_policy_ready` rows. \
         The taint check refuses it too: {taint}"
    )
}

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

/// Verbs that mark a claude.ai connector tool read-only under taint (#10735).
/// `export` and `download` are absent: their target is not known to be the
/// session itself, so they fail closed.
const CONNECTOR_READ_VERBS: &[&str] = &[
    "read", "get", "list", "search", "query", "fetch", "find", "view", "describe", "lookup",
    "retrieve",
];

/// Words that make a connector tool a write wherever they sit in its name,
/// and the conjunctions that join a second action to a read (#10735).
const CONNECTOR_WRITE_WORDS: &[&str] = &[
    "send", "post", "write", "create", "update", "delete", "remove", "upload", "share", "edit",
    "add", "set", "put", "patch", "move", "copy", "invite", "archive", "publish", "reply", "react",
    "schedule", "draft", "submit", "insert", "modify", "rename", "deploy", "cancel", "promote",
    "rollback", "trigger", "run", "execute", "append", "comment", "forward", "mark", "upsert",
    "save", "import", "export", "transfer", "assign", "and", "or", "then",
];

/// `segment` split on every non-alphanumeric character and at each
/// lower-to-upper camelCase boundary, lowercased.
fn words(segment: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut word = String::new();
    let mut prev_lower = false;
    for c in segment.chars() {
        let boundary = !c.is_ascii_alphanumeric() || (prev_lower && c.is_ascii_uppercase());
        if boundary && !word.is_empty() {
            out.push(std::mem::take(&mut word));
        }
        if c.is_ascii_alphanumeric() {
            word.push(c.to_ascii_lowercase());
        }
        prev_lower = c.is_ascii_lowercase() || c.is_ascii_digit();
    }
    if !word.is_empty() {
        out.push(word);
    }
    out
}

/// True when `tool_name` is a claude.ai connector tool not proven read-only.
///
/// Why (#10735, Architect answer to D6d "keep taint"): the connectors are
/// exempt from the family deny, so a tainted session could post through
/// them; the taint arm must refuse their sends and writes and keep their
/// reads.
/// What: an `mcp__<server>__<tool>` name whose server is a claude.ai connector
/// ([`is_claude_ai_connector`]). The tool's [`words`] drop leading words that
/// name the service (`slack` in `slack_send_message`); the first word left is
/// the verb. Read-only means that verb is in [`CONNECTOR_READ_VERBS`] and no
/// word is in [`CONNECTOR_WRITE_WORDS`]. Fail closed: no verb, or any other
/// verb, is a write.
/// Test: `tainted_session_denies_claude_ai_connector_writes_10735`,
/// `tainted_session_allows_claude_ai_connector_reads_10735`,
/// `tainted_session_denies_unknown_claude_ai_connector_verbs_10735`.
fn connector_write(tool_name: &str) -> bool {
    let segments: Vec<&str> = tool_name.split("__").collect();
    if segments.len() < 3 || !segments[0].eq_ignore_ascii_case("mcp") {
        return false;
    }
    let Some((tool, server)) = segments[1..].split_last() else {
        return false;
    };
    let server = server.join("_");
    if !is_claude_ai_connector(&server) {
        return false;
    }
    // `claude_ai_Google_Drive` → service words `google`, `drive`.
    let service: Vec<String> = words(&server).into_iter().skip(2).collect();
    let tool = words(tool);
    let verb = tool.iter().find(|w| !service.contains(w));
    let read_verb = verb.is_some_and(|v| CONNECTOR_READ_VERBS.contains(&v.as_str()));
    !read_verb
        || tool
            .iter()
            .any(|w| CONNECTOR_WRITE_WORDS.contains(&w.as_str()))
}

/// The deny reason for a gated call from a tainted (or unknowable) session,
/// or `None` when the call is not judged or the session is untainted.
///
/// Why (fail closed): only a provably absent taint file allows. A payload
/// that names no session, and a home the guard cannot find, could be a
/// tainted session's, so they deny like one. #9923: an id whose SessionStart
/// never reached the daemon has no taint file either, so a live mark that
/// covers its launch directory taints it here.
/// What: a tainted session denies. An untainted one goes to
/// [`channels_taint::taint_covered`] with `launch` (`CLAUDE_PROJECT_DIR`;
/// a relative one is unknown, which every mark covers) and the payload's
/// absolute `cwd`: a covering mark writes the id's taint file and denies this
/// call, and any error denies. Only "no mark covers it" allows.
/// Test: `untainted_session_gated_call_is_not_judged`,
/// `tainted_session_gated_call_denies`, `gated_call_without_session_id_denies`,
/// `unknown_home_denies_gated_call`, `non_gated_tool_is_not_judged`,
/// `tainted_session_multiedit_denies`,
/// `an_untainted_session_under_a_live_mark_is_tainted_and_denied`,
/// `a_relative_launch_dir_under_a_live_mark_denies`.
pub(crate) fn taint_verdict(
    payload: &Value,
    home: Option<&Path>,
    launch: Option<&Path>,
) -> Option<String> {
    let tool = payload.get("tool_name").and_then(Value::as_str)?;
    // #10735: a claude.ai connector send or write is gated like a path tool.
    if !GATED_TOOLS.contains(&tool) && !connector_write(tool) {
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
    let dir = channels_taint::taint_dir(home);
    let state = channels_taint::read_state(&dir, session);
    if state.is_tainted() {
        return Some(format!(
            "tm hook --egress-guard denies `{tool}`: this session is channel-tainted ({state:?}) — \
             it received an answer through an outbound channel, or its taint could not be \
             re-applied at session start — so its path tool calls and claude.ai connector \
             writes are refused until the managed \
             session ends (#8433). Ask the operator."
        ));
    }
    // #9923: an id no SessionStart reached may run under a live owner-answer mark.
    let launch = launch.filter(|dir| dir.is_absolute());
    let cwd = payload
        .get("cwd")
        .and_then(Value::as_str)
        .map(Path::new)
        .filter(|cwd| cwd.is_absolute());
    match channels_taint::taint_covered(&dir, session, launch, cwd) {
        Ok(None) => None,
        Ok(Some(managed)) => Some(format!(
            "tm hook --egress-guard denies `{tool}`: this session is now channel-tainted — it was \
             launched under the workspace of managed session `{managed}`, which received an \
             answer through an outbound channel, and no SessionStart re-applied that taint — so \
             its path tool calls and claude.ai connector writes are refused until the managed \
             session ends (#9923, #8433). Ask \
             the operator."
        )),
        Err(err) => Some(format!(
            "tm hook --egress-guard denies `{tool}`: the guard could not settle whether a live \
             channel taint covers this session ({err}), so it refuses the call (#9923, #8433). \
             `tm doctor` shows the taint state in its `channels_*` rows."
        )),
    }
}

#[cfg(test)]
#[path = "egress_guard_taint_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "egress_guard_policy_tests.rs"]
mod policy_tests;
