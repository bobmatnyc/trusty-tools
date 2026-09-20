//! Rendering a structured completion report into dedicated scrollback slots
//! (#8204, #8289).
//!
//! Why: before this, a `finish_task` completion reached the scrollback as one
//! blob of rendered prose, so the changed-file list, the test counts and the
//! captured test output were visible only as sentences inside it. Owner
//! feedback 2026-09-16: "structured responses should be parsed, and shown in
//! dedicated slots." They arrive as data now
//! ([`crate::event::ReplEvent::TaskResult`]); this file is what turns that
//! data into labelled lines, and nothing here parses a string.
//! What: [`slot_lines`] is pure — report in, `(role, text)` pairs out — so
//! the slot layout is testable without a `ReplApp`. [`apply_task_result`] is
//! the reducer arm: it pushes those lines and records the report on
//! [`ReplApp::finished_tasks`], which is the standing, keyed state #8182's
//! subagent panel reads.
//! Test: `crate::app::reduce::tests::task_result_*`.

use super::{ChatLine, ChatRole, ReplApp};
use crate::event::TaskReport;

/// One agent's completion, kept for the panel that renders a standing list
/// (#8182).
///
/// Why: the scrollback lines below scroll away; a panel needs the latest
/// report per agent at any moment. Keyed by `agent_id`, the same key
/// [`crate::event::ReplEvent::DelegationStarted`] opens a block on, so a
/// panel row and its completion match up with no further protocol change.
/// What: the event's three fields, stored verbatim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinishedTask {
    /// The agent's display name.
    pub agent: String,
    /// The opaque per-spawn agent id, or empty when unattributed.
    pub agent_id: String,
    /// The report as the backend built it.
    pub report: TaskReport,
}

/// The verification marker the header carries, if any (#8289).
///
/// Why: a completion whose captured test output states a pass reads
/// differently from one nothing backs, and that difference must be the first
/// thing on the line — not buried under the summary.
/// What: `None` when no test command ran at all, so a run with nothing to
/// verify shows no marker rather than an accusatory "(unverified)".
/// Otherwise `(verified)` iff [`TaskReport::verified`].
/// Test: `crate::app::reduce::tests::task_result_marks_an_unbacked_claim_unverified`,
/// `crate::app::reduce::tests::task_result_with_nothing_to_verify_has_no_marker`.
fn verification_marker(report: &TaskReport) -> Option<&'static str> {
    report.evidence.as_ref().map(|_| {
        if report.verified {
            " (verified)"
        } else {
            " (unverified)"
        }
    })
}

/// Build the labelled slot lines for one completion report.
///
/// Why: kept pure and separate from the reducer so the layout rule this
/// issue is actually about — one slot per kind of fact, and NO slot for a
/// fact the report does not carry — is asserted directly.
/// What: always a header (agent, status, verification marker) and a
/// `summary:` line when the summary is non-empty. Then, only when populated:
/// a `changed files (N):` header with ONE line per path, a `tests:` line, and
/// an `evidence — <command> (<outcome>):` header with one line per captured
/// line. `role` places every line — [`ChatRole::Delegated`] inside an open
/// delegation block, [`ChatRole::Status`] at top level — except a `failed`
/// header, which is [`ChatRole::Error`] so a failure is never quiet.
/// Test: `crate::app::reduce::tests::task_result_renders_each_changed_path_once`,
/// `crate::app::reduce::tests::task_result_without_changes_or_tests_has_no_empty_slots`.
pub(crate) fn slot_lines(
    agent: &str,
    report: &TaskReport,
    role: ChatRole,
) -> Vec<(ChatRole, String)> {
    let marker = verification_marker(report).unwrap_or("");
    let glyph = match (report.status.as_str(), report.verified) {
        ("completed", true) => '✓',
        ("failed", _) => '✗',
        _ => '•',
    };
    let header_role = if report.status == "failed" {
        ChatRole::Error
    } else {
        role
    };
    let mut lines = vec![(
        header_role,
        format!("{glyph} {agent} — {}{marker}", report.status),
    )];

    for line in report.summary.lines().filter(|l| !l.trim().is_empty()) {
        lines.push((role, format!("  summary: {line}")));
    }

    if !report.changes.is_empty() {
        lines.push((role, format!("  changed files ({}):", report.changes.len())));
        for change in &report.changes {
            let counts = match (change.lines_added, change.lines_removed) {
                (None, None) => String::new(),
                (added, removed) => format!(" (+{}/-{})", added.unwrap_or(0), removed.unwrap_or(0)),
            };
            lines.push((role, format!("    {}{counts}", change.file)));
        }
    }

    match (report.tests_run, report.tests_passed) {
        (Some(run), Some(passed)) => lines.push((role, format!("  tests: {passed}/{run} passed"))),
        (Some(run), None) => lines.push((role, format!("  tests: {run} run"))),
        (None, Some(passed)) => lines.push((role, format!("  tests: {passed} passed"))),
        (None, None) => {}
    }

    if let Some(evidence) = &report.evidence {
        lines.push((
            role,
            format!("  evidence — {} ({}):", evidence.command, evidence.outcome),
        ));
        if evidence.truncated {
            lines.push((role, "    … earlier lines omitted".to_string()));
        }
        for line in &evidence.lines {
            lines.push((role, format!("    {line}")));
        }
    }

    lines
}

/// Apply one [`crate::event::ReplEvent::TaskResult`] to `app` (#8204).
///
/// Why: the reducer arm, kept here rather than in `reduce.rs` because that
/// file sits against the crate's production-file cap.
/// What: pushes [`slot_lines`] into the scrollback — indented under an open
/// delegation block when `agent_id` names one — pins the view to the newest
/// content, and records the report on [`ReplApp::finished_tasks`], replacing
/// any earlier report from the SAME `agent_id` so a panel shows one row per
/// agent. An empty `agent_id` never replaces anything, since it identifies
/// nobody.
/// Test: `crate::app::reduce::tests::task_result_renders_each_changed_path_once`,
/// `crate::app::reduce::tests::task_result_replaces_an_earlier_report_from_the_same_agent`.
pub(crate) fn apply_task_result(
    app: &mut ReplApp,
    agent: String,
    agent_id: String,
    report: TaskReport,
) {
    let role = if !agent_id.is_empty() && app.delegations.iter().any(|d| d.agent_id == agent_id) {
        ChatRole::Delegated
    } else {
        ChatRole::Status
    };

    for (role, text) in slot_lines(&agent, &report, role) {
        app.chat.push(ChatLine {
            role,
            text,
            tool: None,
        });
    }
    app.scroll_offset = 0;

    if !agent_id.is_empty() {
        app.finished_tasks.retain(|t| t.agent_id != agent_id);
    }
    app.finished_tasks.push(FinishedTask {
        agent,
        agent_id,
        report,
    });
}
