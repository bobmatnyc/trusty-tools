//! The read-only Slack allowlist arm of `tm hook --egress-guard` (#10387).
//!
//! Why: Bob D6c lets one named project's sessions read Slack channels and
//! threads while every other Slack call stays refused (#8454). The grant is
//! `channels.slack_read_allowlist` in the host `config.yaml`, which the #8878
//! trust-anchor floor protects from every session write.
//! What: [`lift`] runs on the guard's first verdict. Only an `egress-family`
//! deny of a `Family("slack")` tool named `mcp__<server>__<tool>`, with
//! `<server>` exactly one of [`SLACK_READ_SERVERS`], is judged. The session's
//! `CLAUDE_PROJECT_DIR` must be set, absolute and canonical; the host ceiling
//! must read and parse strictly; the project must have an entry; and the entry
//! must list `<tool>` by exact, case-sensitive name. All of that lifts the
//! deny to [`Verdict::Allow`] and returns the [`Allowed`] audit fact; any
//! fault keeps the deny and names the fault, never a config value. A
//! trust-anchor deny is never an `egress-family` deny, so the floor stands.
//! The taint arm runs after the lift but judges path tools only, so an
//! allowlisted Slack read stays allowed in a channel-tainted session, by
//! design (Bob D6c, Architect df-D38). See #10387.
//! Test: `egress_guard_slack_read_tests.rs`; end to end in
//! `tests/tm_hook_egress_slack_read_10387.rs`.

use std::ffi::OsStr;
use std::path::Path;

use trusty_channels::policy::{HostError, SlackReadAllowlist, SlackReadEntry};

use crate::commands::egress_guard::Verdict;
use crate::commands::egress_guard_families::{Classification, FamilyTable};
use crate::commands::hook_stdin::HookStdinError;

/// The two Slack MCP server segments the allowlist covers, matched exactly:
/// the native `slack-mcp` and claude.ai's hosted connector.
pub(crate) const SLACK_READ_SERVERS: &[&str] = &["slack-mcp", "claude_ai_Slack"];

/// The check slug of the deny this arm may lift.
const FAMILY_CHECK: &str = "egress-family";

/// The audit slug of an allowed read.
pub(crate) const ALLOWED_AUDIT: &str = "egress-slack-read-allowed";

/// An allowed read, as its audit line names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Allowed {
    /// The entry's display name.
    pub(crate) name: String,
    /// The tool segment, one of the entry's tools.
    pub(crate) tool: String,
    /// The server segment, one of [`SLACK_READ_SERVERS`].
    pub(crate) server: String,
}

impl Allowed {
    /// `egress-slack-read-allowed project=<name> tool=<tool> prefix=<server>`;
    /// no call content.
    pub(crate) fn audit_line(&self) -> String {
        format!(
            "{ALLOWED_AUDIT} project={} tool={} prefix={}",
            self.name, self.tool, self.server
        )
    }
}

/// True when `entry` lists `tool` by exact, case-sensitive name.
///
/// Why (#10387): the one name check of this arm. No prefix, glob or contains
/// match: `slack_read_canvas` must not pass as `slack_read_channel`.
/// Test: `slack_read_posting_tools_stay_refused` (the mutation proof).
fn listed(entry: &SlackReadEntry, tool: &str) -> bool {
    entry.tools().contains(&tool)
}

/// `(server, tool)` of `mcp__<server>__<tool>` when the server is exactly one
/// of [`SLACK_READ_SERVERS`]; `None` for any other spelling.
fn exact_server(tool_name: &str) -> Option<(&str, &str)> {
    let (server, tool) = tool_name.strip_prefix("mcp__")?.split_once("__")?;
    SLACK_READ_SERVERS
        .contains(&server)
        .then_some((server, tool))
}

/// Why `project_dir` cannot identify the session, or `None` when it is a set,
/// absolute, canonical path.
fn project_fault(project_dir: Option<&OsStr>) -> Option<&'static str> {
    let Some(dir) = project_dir.filter(|d| !d.is_empty()) else {
        return Some("CLAUDE_PROJECT_DIR is not set");
    };
    let dir = Path::new(dir);
    let canonical = dir.is_absolute() && std::fs::canonicalize(dir).is_ok_and(|real| real == dir);
    (!canonical).then_some("CLAUDE_PROJECT_DIR is not a canonical absolute path")
}

/// The variant name of a host fault, without any field value.
fn host_fault(error: &HostError) -> String {
    let text = format!("{error:?}");
    let name = text
        .split(|c: char| !c.is_ascii_alphanumeric())
        .next()
        .unwrap_or_default();
    format!("config.yaml could not be used ({name})")
}

/// Lift the guard's first verdict for an allowlisted Slack read.
///
/// Why / What: see the module doc. `load` is called only for a judged call,
/// after the project directory checks.
/// Test: `slack_read_allowlisted_project_reads_both_tools_on_both_prefixes`,
/// `slack_read_posting_tools_stay_refused`, `slack_read_faults_keep_the_deny`,
/// `slack_read_lookalike_names_are_not_judged`,
/// `slack_read_trust_anchor_deny_is_never_lifted`,
/// `slack_read_allow_survives_channel_taint_by_design`.
pub(crate) fn lift(
    first: Verdict,
    read: &Result<serde_json::Value, HookStdinError>,
    table: &FamilyTable,
    project_dir: Option<&OsStr>,
    load: impl FnOnce() -> Result<SlackReadAllowlist, HostError>,
) -> (Verdict, Option<Allowed>) {
    let Verdict::Deny { check, reason } = &first else {
        return (first, None);
    };
    let tool_name = match read {
        Ok(payload) => payload.get("tool_name").and_then(serde_json::Value::as_str),
        Err(_) => None,
    };
    let Some((server, tool)) = tool_name
        .filter(|_| *check == FAMILY_CHECK)
        .filter(|name| table.classify(name) == Classification::Family("slack".into()))
        .and_then(exact_server)
    else {
        return (first, None);
    };
    let keep = |fault: &str| {
        let reason =
            format!("{reason} The #10387 read-only Slack allowlist does not apply: {fault}.");
        (
            Verdict::Deny {
                check: FAMILY_CHECK,
                reason,
            },
            None,
        )
    };
    if let Some(fault) = project_fault(project_dir) {
        return keep(fault);
    }
    let list = match load() {
        Ok(list) => list,
        Err(error) => return keep(&host_fault(&error)),
    };
    let project = Path::new(project_dir.unwrap_or_default());
    let Some(entry) = list.entry_for(project) else {
        return keep("this project has no slack_read_allowlist entry");
    };
    if !listed(entry, tool) {
        return keep("this project's entry does not list this tool");
    }
    let allowed = Allowed {
        name: entry.name().to_owned(),
        tool: tool.to_owned(),
        server: server.to_owned(),
    };
    (Verdict::Allow, Some(allowed))
}

/// The production load: the host ceiling under `home`, strictly parsed.
pub(crate) fn load_allowlist(home: Option<&Path>) -> Result<SlackReadAllowlist, HostError> {
    let home = home.ok_or(HostError::HomeUnknown)?;
    trusty_channels::policy::read_host_ceiling(home).map(|c| c.slack_read_allowlist().clone())
}

#[cfg(test)]
#[path = "egress_guard_slack_read_tests.rs"]
mod tests;
