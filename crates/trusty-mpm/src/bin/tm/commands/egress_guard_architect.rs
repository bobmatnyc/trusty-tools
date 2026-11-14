//! The main-thread pass of `tm hook --egress-guard` (#11273), and its audit.
//! The file keeps its #10447 name: the Architect-only lift it held folded into
//! this one pass for every PM and Architect main thread.
//!
//! Why: owner ruling 2026-11-14 — "Only PMs (including Architect) should be
//! allowed egress." and "Keep both subagent paths as they are." The family
//! and Bash rules now bind subagents; a PM or Architect main thread passes
//! them whether or not the Architect is bound to a launch record (#11270).
//! What: [`lift`] runs on the guard's verdict after the #8878 trust-anchor
//! floor and the #10387 read lift. It judges only an `egress-family` deny (an
//! MCP or MCP-resource tool of a family) and an `egress-bash` deny whose
//! command reaches a channel host or runs a channel program; the over-cap and
//! walk-budget Bash denies are never judged, since no rule read those
//! commands. A judged deny is lifted only when
//! `egress_guard_pm_dm::main_thread_role` names a PM or Architect main
//! thread. [`audited`] then lets the allow stand only once one audit line is
//! appended: role, family, tool, time and session, never a message body or a
//! command argument. Every lifted call is audited, reads included.
//! FAIL-CLOSED: a trust-anchor deny is never judged; an unreadable payload,
//! an unknown thread, a subagent and an unknown stamp keep the deny. An audit
//! line that cannot be written turns the allow into an [`AUDIT_CHECK`] deny,
//! which the guard records in its deny log like any other deny.
//! Test: `egress_guard_architect_tests.rs`.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::commands::egress_guard::{BASH_TOOL, RESOURCE_TOOLS, Verdict};
use crate::commands::egress_guard_bash::{
    BASH_CHECK, BashDeny, bash_egress_deny, bash_oversize_reason,
};
use crate::commands::egress_guard_families::{Classification, FamilyTable};
use crate::commands::egress_guard_pm_dm::{MainRole, main_thread_role};
use crate::commands::hook_stdin::HookStdinError;
use crate::commands::pm_guard_trust_anchor::HookEnv;

/// The check slug of an MCP family deny.
const FAMILY_CHECK: &str = "egress-family";

/// The audit slug of an allowed main-thread call (#11273).
pub(crate) const ALLOWED_AUDIT: &str = "egress-main-thread-allowed";

/// The check slug of a deny whose audit line could not be written. The slug
/// keeps its #10447 spelling so GP-047 and existing deny logs still match.
pub(crate) const AUDIT_CHECK: &str = "egress-architect-audit-unwritable";

/// The audit file's name, under the trusty-mpm data directory.
const AUDIT_FILE: &str = "egress-architect-allowed.log";

/// The longest field an audit line carries.
const MAX_FIELD: usize = 128;

/// An allowed main-thread call, as its audit line names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MainThreadAllowed {
    /// The caller's role.
    pub(crate) role: MainRole,
    /// The MCP family, or the service a Bash command reaches.
    pub(crate) family: String,
    /// The tool name; never its input.
    pub(crate) tool: String,
    /// The payload's `session_id`.
    pub(crate) session: String,
}

impl MainThreadAllowed {
    /// `egress-main-thread-allowed time=<rfc3339> session=<id> role=<pm|architect>
    /// family=<f> tool=<name>`, every field reduced to `[A-Za-z0-9._:-]`.
    ///
    /// Why: a payload field could carry a newline and forge a second line.
    /// Test: `main_thread_audit_line_carries_no_body_and_no_newline_11273`.
    pub(crate) fn audit_line(&self, time: &str) -> String {
        format!(
            "{ALLOWED_AUDIT} time={} session={} role={} family={} tool={}",
            field(time),
            field(&self.session),
            self.role.label(),
            field(&self.family),
            field(&self.tool)
        )
    }
}

/// `raw` with every byte outside `[A-Za-z0-9._:-]` dropped, capped at
/// [`MAX_FIELD`]; `-` when nothing is left.
pub(crate) fn field(raw: &str) -> String {
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

/// Lift a family or Bash channel deny for a PM or Architect main thread.
///
/// Why / What: see the module doc. #11273: no config read and no launch
/// record; the identity is `main_thread_role` alone.
/// Test: `pm_and_architect_main_threads_pass_every_family_11273`,
/// `architect_main_thread_passes_without_a_launch_record_11273`,
/// `architect_subagent_stays_denied`,
/// `trust_anchor_deny_holds_for_the_architect_main_thread`,
/// `budget_and_cap_bash_denies_are_never_lifted_11273`.
pub(crate) fn lift(
    first: Verdict,
    read: &Result<Value, HookStdinError>,
    table: &FamilyTable,
    env: &HookEnv,
) -> (Verdict, Option<MainThreadAllowed>) {
    let (Verdict::Deny { check, .. }, Ok(payload)) = (&first, read) else {
        return (first, None);
    };
    let Some(family) = liftable_family(check, payload, table) else {
        return (first, None);
    };
    // #11273: a subagent, an unknown thread or an unknown stamp keeps the deny.
    let Some(role) = main_thread_role(payload, env) else {
        return (first, None);
    };
    let text = |key: &str| payload.get(key).and_then(Value::as_str).unwrap_or("");
    let allowed = MainThreadAllowed {
        role,
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

/// The final verdict: a main-thread allow stands only once audited.
///
/// Why: GP-047 — every lifted call leaves one audit line, so an allow whose
/// line cannot be written must not go through, and a deny is never touched.
/// What: `verdict` unchanged unless it is [`Verdict::Allow`] and `allowed` is
/// set; then the line is appended to `path`, and a `None` path or a failed
/// write is an [`AUDIT_CHECK`] deny naming the role and the error kind. The
/// guard records that deny in its deny log, so the refused call still leaves
/// a record.
/// Test: `main_thread_allow_needs_its_audit_line_11273`,
/// `an_audit_failure_never_turns_a_deny_into_an_allow_11273`.
pub(crate) fn audited(
    verdict: Verdict,
    allowed: Option<MainThreadAllowed>,
    path: Option<&Path>,
    time: &str,
) -> Verdict {
    let (Verdict::Allow, Some(allowed)) = (&verdict, allowed) else {
        return verdict;
    };
    let line = allowed.audit_line(time);
    let written = match path {
        Some(path) => append(path, &line).map_err(|err| err.kind().to_string()),
        None => Err("the trusty-mpm data directory is unknown".to_owned()),
    };
    match written {
        Ok(()) => verdict,
        Err(why) => Verdict::Deny {
            check: AUDIT_CHECK,
            reason: format!(
                "tm hook --egress-guard would allow this {} main-thread call (#11273), but its \
                 audit line could not be written ({why}), so it denies it. Fix the trusty-mpm \
                 data directory and retry.",
                allowed.role.label()
            ),
        },
    }
}

#[cfg(test)]
#[path = "egress_guard_architect_tests.rs"]
mod tests;
