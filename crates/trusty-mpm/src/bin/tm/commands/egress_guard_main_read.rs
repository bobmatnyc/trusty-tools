//! The main-session pass of `tm hook --egress-guard` (#11273, #11521).
//!
//! Why: owner ruling 2026-11-14 — "Only PMs (including Architect) should be
//! allowed egress" and "Keep both subagent paths as they are". #11273 also
//! made a send or write need the tm launch record; the owner ruled that
//! policy was never asked for (#11521). It denied a self-cleared Architect,
//! whose new `claude` has no record, every send and write.
//! What: [`lift`] runs after the #10447 launch-record lift and the #11194
//! owner-DM lift. It judges only an `egress-family` deny. The tool must still
//! classify as a family (by its server for a resource tool). When
//! `egress_guard_pm_dm::main_thread_role` names a PM or Architect main
//! session, any tool of the family — read, send or write — lifts to an
//! allow, with no launch record and no bind step. Otherwise only a PM
//! subagent's Gmail or Google Workspace [`Access::Read`] lifts (GP-043).
//! [`audited`] lets each allow stand only once one audit line is written.
//! [`tool_access`] is an explicit per-family list of read tool names, matched
//! exactly and case-sensitively; every other tool of a family is a
//! [`Access::Write`], so a new or renamed tool is never a subagent read.
//! FAIL-CLOSED: an unreadable payload, any other check (Bash included), a
//! subagent (bar the GP-043 read class), an unknown thread and a missing or
//! unknown stamp keep the deny; an audit line that cannot be written denies.
//! Test: `egress_guard_main_read_tests.rs`.

use std::path::Path;

use serde_json::Value;
use trusty_mpm::core::twin_identity::{ThreadKind, thread_kind};

use crate::commands::egress_guard::{RESOURCE_TOOLS, Verdict};
use crate::commands::egress_guard_architect::{audited_line, field};
use crate::commands::egress_guard_families::{Classification, FamilyTable};
use crate::commands::egress_guard_pm_dm::{MainRole, main_thread_role, pm_launch_stamp};
use crate::commands::hook_stdin::HookStdinError;
use crate::commands::pm_guard_trust_anchor::HookEnv;

/// The check slug of the deny this pass may lift.
const FAMILY_CHECK: &str = "egress-family";

/// The audit slug of an allowed main-session read.
pub(crate) const ALLOWED_AUDIT: &str = "egress-main-read-allowed";

/// Read tools of the Slack family: the native `slack-mcp` read surface.
const SLACK_READS: &[&str] = &[
    "slack_read_channel",
    "slack_read_thread",
    "slack_read_canvas",
    "slack_read_file",
    "slack_list_channels",
    "slack_list_channel_members",
    "slack_list_users",
    "slack_get_user",
    "slack_get_reactions",
    "slack_search_messages",
    "slack_search_channels",
    "slack_search_users",
    "slack_search_emojis",
    "slack_canvas_lookup_sections",
];

/// Read tools of the Gmail family (`trusty-gworkspace` and common servers).
/// `download_gmail_attachment` writes to disk, so it is not listed.
const GMAIL_READS: &[&str] = &[
    "search_gmail_messages",
    "get_gmail_message_content",
    "get_gmail_messages_content_batch",
    "get_gmail_thread_content",
    "get_gmail_threads_content_batch",
    "list_gmail_labels",
    "list_message_attachments",
    "read_email",
    "search_emails",
    "list_email_labels",
];

/// Read tools of the Google Workspace messaging family.
const GWORKSPACE_READS: &[&str] = &[
    "list_message_attachments",
    "get_messages",
    "get_message",
    "list_messages",
    "search_messages",
];

/// Read tools of the Notion family (hosted and open-source servers).
const NOTION_READS: &[&str] = &[
    "notion-search",
    "notion-fetch",
    "notion-get-comments",
    "notion-get-users",
    "notion-get-user",
    "notion-get-self",
    "notion-get-teams",
    "API-post-search",
    "API-post-database-query",
    "API-retrieve-a-page",
    "API-retrieve-a-page-property",
    "API-retrieve-a-block",
    "API-get-block-children",
    "API-retrieve-a-database",
    "API-retrieve-a-comment",
    "API-get-users",
    "API-get-user",
    "API-get-self",
];

/// Read tools of the Google Chat family.
const GOOGLECHAT_READS: &[&str] = &[
    "list_spaces",
    "get_space",
    "list_members",
    "get_messages",
    "get_message",
    "list_messages",
    "search_messages",
];

/// Whether a family tool only reads, or may send or write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Access {
    /// Listed as a read, search or list tool of its family.
    Read,
    /// Anything else: a send, a write, or a tool the list does not know.
    Write,
}

/// The read list of `family`, empty for a family this pass does not know.
fn reads_of(family: &str) -> &'static [&'static str] {
    match family {
        "slack" => SLACK_READS,
        "gmail" | "googlemail" => GMAIL_READS,
        "gworkspace" => GWORKSPACE_READS,
        "notion" => NOTION_READS,
        "googlechat" | "gchat" => GOOGLECHAT_READS,
        _ => &[],
    }
}

/// How `tool_name`, a tool of `family`, is classified (#11273).
///
/// Why: a read may pass for a main session; a send or write may not without
/// the launch record. A name match on verbs would pass a new tool that
/// happens to start with `get`; an explicit list cannot.
/// What: a [`RESOURCE_TOOLS`] call reads or lists a resource, so it is
/// [`Access::Read`]. Any other tool is [`Access::Read`] only when its last
/// `__` segment is in its family's list, byte for byte; everything else,
/// including a family with no list, is [`Access::Write`].
/// Test: `an_unknown_family_tool_is_a_write_11273`,
/// `every_listed_read_is_a_read_and_sends_are_writes_11273`.
pub(crate) fn tool_access(tool_name: &str, family: &str) -> Access {
    if RESOURCE_TOOLS.contains(&tool_name) {
        return Access::Read;
    }
    let tool = tool_name
        .strip_prefix("mcp__")
        .and_then(|rest| rest.rsplit_once("__"))
        .map(|(_, tool)| tool);
    match tool {
        Some(tool) if reads_of(family).contains(&tool) => Access::Read,
        _ => Access::Write,
    }
}

/// An allowed main-session read, as its audit line names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MainReadAllowed {
    /// The caller's launch role.
    pub(crate) role: MainRole,
    /// The family the tool belongs to.
    pub(crate) family: String,
    /// The tool name; never its input.
    pub(crate) tool: String,
    /// The payload's `session_id`.
    pub(crate) session: String,
}

impl MainReadAllowed {
    /// `egress-main-read-allowed time=<rfc3339> session=<id> role=<pm|architect>
    /// family=<f> tool=<name>`, each field reduced as the Architect line's.
    /// Test: `main_session_read_audit_line_carries_no_input_11273`.
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

/// The family of the judged call, or `None` when it is not a family tool.
fn family_of(tool: &str, payload: &Value, table: &FamilyTable) -> Option<String> {
    let classification = if RESOURCE_TOOLS.contains(&tool) {
        let server = payload.get("tool_input")?.get("server")?.as_str()?;
        table.classify_resource_server(server)
    } else {
        table.classify(tool)
    };
    match classification {
        Classification::Family(family) => Some(family),
        Classification::Unrelated | Classification::Malformed(_) => None,
    }
}

/// The Gmail and Google Workspace families a PM subagent may read (GP-043).
const SUBAGENT_READ_FAMILIES: &[&str] = &["gmail", "googlemail", "gworkspace"];

/// Who may read `family` on this call, or `None` to keep the deny.
///
/// Why: GP-043 (owner ruling 2026-11-16) — a PM's subagents read Gmail and
/// Google Workspace too; every other family keeps #11273's main-only rule.
/// What: [`main_thread_role`] first; otherwise a [`ThreadKind::Subagent`]
/// carrying the PM stamp, for a [`SUBAGENT_READ_FAMILIES`] family only, is
/// [`MainRole::PmSubagent`]. An Architect subagent or unknown thread is `None`.
/// Test: `pm_subagent_gworkspace_reads_pass_gp_043`,
/// `pm_subagent_other_family_and_unlisted_reads_stay_denied_gp_043`.
fn read_role(payload: &Value, env: &HookEnv, family: &str) -> Option<MainRole> {
    if let Some(role) = main_thread_role(payload, env) {
        return Some(role);
    }
    // GP-043: only a PM's subagent, only the Gmail and Workspace read class.
    let subagent = thread_kind(payload, env.sub_agent) == ThreadKind::Subagent;
    (subagent && pm_launch_stamp(env) && SUBAGENT_READ_FAMILIES.contains(&family))
        .then_some(MainRole::PmSubagent)
}

/// Lift a family deny for a PM or Architect main session (#11521).
///
/// Why / What: see the module doc.
/// Test: `main_session_reads_pass_in_every_family_11273`,
/// `subagent_reads_stay_denied_11273`,
/// `unknown_thread_or_unstamped_session_reads_stay_denied_11273`,
/// `main_session_sends_without_a_record_are_allowed_11521`,
/// `self_cleared_architect_writes_slack_and_notion_11521`,
/// `subagent_and_unstamped_writes_stay_denied_11521`,
/// `only_a_family_deny_is_judged_11273`.
pub(crate) fn lift(
    first: Verdict,
    read: &Result<Value, HookStdinError>,
    table: &FamilyTable,
    env: &HookEnv,
) -> (Verdict, Option<MainReadAllowed>) {
    let (Verdict::Deny { check, .. }, Ok(payload)) = (&first, read) else {
        return (first, None);
    };
    if *check != FAMILY_CHECK {
        return (first, None);
    }
    let Some(tool) = payload.get("tool_name").and_then(Value::as_str) else {
        return (first, None);
    };
    let Some(family) = family_of(tool, payload, table) else {
        return (first, None);
    };
    // #11521: a stamped main thread may read, send and write; anyone else
    // (a PM subagent's GP-043 read class aside) keeps the deny.
    let role = match main_thread_role(payload, env) {
        Some(role) => role,
        None if tool_access(tool, &family) == Access::Read => {
            let Some(role) = read_role(payload, env, &family) else {
                return (first, None);
            };
            role
        }
        None => return (first, None),
    };
    let session = payload.get("session_id").and_then(Value::as_str);
    let allowed = MainReadAllowed {
        role,
        family,
        tool: tool.to_owned(),
        session: session.unwrap_or("").to_owned(),
    };
    (Verdict::Allow, Some(allowed))
}

/// The final verdict: a main-session read allow stands only once audited.
///
/// What: `audited_line` over [`MainReadAllowed::audit_line`]; a failed write
/// is the `egress-architect-audit-unwritable` deny (GP-047).
/// Test: `main_session_read_needs_its_audit_line_11273`.
pub(crate) fn audited(
    verdict: Verdict,
    allowed: Option<MainReadAllowed>,
    path: Option<&Path>,
    time: &str,
) -> Verdict {
    let line = allowed.map(|allowed| allowed.audit_line(time));
    audited_line(verdict, line, path, "main-session read (#11273)")
}

#[cfg(test)]
#[path = "egress_guard_main_read_tests.rs"]
mod tests;
