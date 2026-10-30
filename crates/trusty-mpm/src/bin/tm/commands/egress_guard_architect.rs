//! The Architect main-thread allow arm of `tm hook --egress-guard` (#10447).
//!
//! Why: owner ruling 2026-10-29 — the Architect acts as the owner's proxy, so
//! its own session may reach Slack, Gmail, Notion and Google Chat directly.
//! It supersedes the D9a "no Architect carve-out" ruling (#10416) for the
//! Architect's main thread only; PMs, agents and subagents stay denied.
//! What: [`lift`] runs on the guard's verdict after the #8878 trust-anchor
//! floor. It judges only an `egress-family` deny (an MCP or MCP-resource tool
//! of a family) and an `egress-bash` deny whose command names a channel host
//! or program; the over-cap and walk-budget Bash denies are never judged,
//! since the floor did not read those commands. A judged deny is lifted only
//! when [`architect_main_thread`] holds — the one identity predicate the
//! #8878 floor uses: no payload `agent_id`, `CLAUDE_MPM_SUB_AGENT` unset, the
//! `supervisor` launch stamp, a `CLAUDE_PROJECT_DIR` that requests the
//! supervisor profile and is in the user-level `[supervisor] projects`
//! allowlist after canonicalization, and the hook's nearest `claude`
//! matching a `tm fleet init` / `tm fleet relaunch` launch record on PID and
//! start time. The record keeps a nested `TRUSTY_MPM_SESSION_PROFILE=supervisor
//! claude` in the Architect directory out (Architect ruling 13:03Z).
//! [`audited`] then lets the allow stand only once one audit line is
//! appended: family, tool, time and session, never a message body or a
//! command argument.
//! FAIL-CLOSED: a trust-anchor deny is never judged. An unreadable or
//! malformed user config loads as the default config, whose allowlist is
//! empty; an unreadable `.trusty-mpm.toml` requests the PM profile; either
//! fails the identity, which keeps the deny. An audit line that cannot be
//! written denies.
//! Test: `egress_guard_architect_tests.rs`.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde_json::Value;
use trusty_mpm::core::config::MpmConfig;

use crate::commands::egress_guard::{BASH_TOOL, RESOURCE_TOOLS, Verdict};
use crate::commands::egress_guard_bash::{
    BASH_CHECK, BashDeny, bash_egress_deny, bash_oversize_reason,
};
use crate::commands::egress_guard_families::{Classification, FamilyTable};
use crate::commands::hook_stdin::HookStdinError;
use crate::commands::pm_guard_architect_reason::architect_main_thread;
use crate::commands::pm_guard_trust_anchor::HookEnv;

/// The check slug of an MCP family deny.
const FAMILY_CHECK: &str = "egress-family";

/// The audit slug of an allowed Architect call.
pub(crate) const ALLOWED_AUDIT: &str = "egress-architect-allowed";

/// The check slug of a deny whose audit line could not be written.
pub(crate) const AUDIT_CHECK: &str = "egress-architect-audit-unwritable";

/// The audit file's name, under the trusty-mpm data directory.
const AUDIT_FILE: &str = "egress-architect-allowed.log";

/// The longest field an audit line carries.
const MAX_FIELD: usize = 128;

/// An allowed Architect call, as its audit line names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ArchitectAllowed {
    /// The MCP family, or the service a Bash command reaches.
    pub(crate) family: String,
    /// The tool name; never its input.
    pub(crate) tool: String,
    /// The payload's `session_id`.
    pub(crate) session: String,
}

impl ArchitectAllowed {
    /// `egress-architect-allowed time=<rfc3339> session=<id> family=<f>
    /// tool=<name>`, every field reduced to `[A-Za-z0-9._:-]`.
    ///
    /// Why: a payload field could carry a newline and forge a second line.
    /// Test: `architect_audit_line_carries_no_body_and_no_newline`.
    pub(crate) fn audit_line(&self, time: &str) -> String {
        format!(
            "{ALLOWED_AUDIT} time={} session={} family={} tool={}",
            field(time),
            field(&self.session),
            field(&self.family),
            field(&self.tool)
        )
    }
}

/// `raw` with every byte outside `[A-Za-z0-9._:-]` dropped, capped at
/// [`MAX_FIELD`]; `-` when nothing is left.
fn field(raw: &str) -> String {
    let kept: String = raw
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '-'))
        .take(MAX_FIELD)
        .collect();
    if kept.is_empty() {
        "-".to_owned()
    } else {
        kept
    }
}

/// The family a judged deny names, or `None` when the deny is not liftable.
///
/// What: an `egress-family` deny of a tool the table still classifies as a
/// family (by its server for [`RESOURCE_TOOLS`]); an `egress-bash` deny of a
/// command under the byte cap that [`bash_egress_deny`] finds a channel in.
fn liftable_family(check: &str, payload: &Value, table: &FamilyTable) -> Option<String> {
    let tool = payload.get("tool_name")?.as_str()?;
    let input = payload.get("tool_input")?;
    let classification = match check {
        FAMILY_CHECK if RESOURCE_TOOLS.contains(&tool) => {
            table.classify_resource_server(input.get("server")?.as_str()?)
        }
        FAMILY_CHECK => table.classify(tool),
        BASH_CHECK if tool == BASH_TOOL => {
            let command = input.get("command")?.as_str()?;
            // #9699: the floor never read an over-cap command; keep its deny.
            if bash_oversize_reason(command).is_some() {
                return None;
            }
            return match bash_egress_deny(command)? {
                BashDeny::Channel(hit) => Some(hit.service.to_owned()),
                BashDeny::WalkBudget => None,
            };
        }
        _ => return None,
    };
    match classification {
        Classification::Family(family) => Some(family),
        Classification::Unrelated | Classification::Malformed(_) => None,
    }
}

/// Lift a family or Bash channel deny for the Architect's main thread.
///
/// Why / What: see the module doc. `config` is called only for a judged
/// deny, so every other call reads no file and walks no process table.
/// Test: `architect_main_thread_may_send_slack_and_name_the_slack_host`,
/// `architect_relaunch_is_allowed_only_once_recorded`,
/// `nested_supervisor_claude_in_the_architect_dir_is_denied`,
/// `architect_subagent_stays_denied`, `pm_project_with_supervisor_env_stays_denied`,
/// `unreadable_config_or_profile_stays_denied`,
/// `trust_anchor_deny_holds_for_the_architect_main_thread`.
pub(crate) fn lift(
    first: Verdict,
    read: &Result<Value, HookStdinError>,
    table: &FamilyTable,
    env: &HookEnv,
    config: impl FnOnce() -> MpmConfig,
) -> (Verdict, Option<ArchitectAllowed>) {
    let (Verdict::Deny { check, .. }, Ok(payload)) = (&first, read) else {
        return (first, None);
    };
    let Some(family) = liftable_family(check, payload, table) else {
        return (first, None);
    };
    // #10447: the floor's identity predicate; any failed check keeps the deny.
    if architect_main_thread(payload, env, config).is_err() {
        return (first, None);
    }
    let text = |key: &str| payload.get(key).and_then(Value::as_str).unwrap_or("");
    let allowed = ArchitectAllowed {
        family,
        tool: text("tool_name").to_owned(),
        session: text("session_id").to_owned(),
    };
    (Verdict::Allow, Some(allowed))
}

/// The production audit file, or `None` when the data directory is unknown.
pub(crate) fn audit_path() -> Option<PathBuf> {
    trusty_common::resolve_data_dir("trusty-mpm")
        .ok()
        .map(|dir| dir.join(AUDIT_FILE))
}

/// Append `line` and a newline to `path` in one write.
fn append(path: &Path, line: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    file.write_all(format!("{line}\n").as_bytes())
}

/// The final verdict: an Architect allow stands only once audited.
///
/// Why: requirement 3 of #10447 — every allowed call leaves one audit line,
/// so an allow whose line cannot be written must not go through.
/// What: `verdict` unchanged unless it is [`Verdict::Allow`] and `allowed` is
/// set; then the line is appended to `path`, and a `None` path or a failed
/// write is an [`AUDIT_CHECK`] deny.
/// Test: `architect_allow_needs_its_audit_line`.
pub(crate) fn audited(
    verdict: Verdict,
    allowed: Option<ArchitectAllowed>,
    path: Option<&Path>,
    time: &str,
) -> Verdict {
    let (Verdict::Allow, Some(allowed)) = (&verdict, allowed) else {
        return verdict;
    };
    let written = match path {
        Some(path) => append(path, &allowed.audit_line(time)).map_err(|err| err.kind().to_string()),
        None => Err("the trusty-mpm data directory is unknown".to_owned()),
    };
    match written {
        Ok(()) => verdict,
        Err(why) => Verdict::Deny {
            check: AUDIT_CHECK,
            reason: format!(
                "tm hook --egress-guard would allow this Architect call (#10447), but its audit \
                 line could not be written ({why}), so it denies it. Fix the trusty-mpm data \
                 directory and retry."
            ),
        },
    }
}

#[cfg(test)]
#[path = "egress_guard_architect_tests.rs"]
mod tests;
