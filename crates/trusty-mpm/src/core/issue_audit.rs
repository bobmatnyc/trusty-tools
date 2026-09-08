//! Mechanical verification that a filed issue satisfies the ticketing standard
//! (#7097).
//!
//! Why: #7067 made a milestone, a project, and native relationships mandatory
//! on every new issue, but the only thing enforcing that was the filing agent's
//! own self-check. An issue filed outside a `ticketing` dispatch — #7092 and
//! #7093 were the trigger — could carry or miss any of it with nothing
//! producing evidence either way. This module is the check that reads the
//! filed artifact back and says which requirements it actually meets.
//!
//! What: the [`IssueFacts`] wire shape `gh issue view --json` /
//! `gh issue list --json` return, [`audit_issue`] folding one issue's facts
//! against a [`ResolvedTicketing`] into an [`IssueAudit`], and the two
//! renderers ([`render_audit`] for one issue, [`render_summary`] for a batch).
//! Everything here is pure — the `gh` reads live in
//! [`crate::core::issue_audit_gh`], so every verdict is unit-testable against
//! fixture JSON with no network.
//!
//! # What is and is not a failure
//!
//! A missing milestone or project is a FAIL only when the operator's
//! `agents.ticketing` block requires it; an unset requirement renders as INFO
//! so the report never invents a rule the config does not carry. A
//! `no-milestone: <reason>` comment is the standard's own escape hatch and
//! renders as SKIP with the reason quoted, never as a pass — a reader must be
//! able to tell "no milestone was needed, here is why" from "a milestone is
//! present". Relationships are always INFO: a standalone issue legitimately has
//! no parent, no blocker, and no sub-issues, so their absence is a fact to
//! report and never a violation.
//!
//! Test: `issue_audit_tests.rs`.

use serde::Deserialize;

use crate::core::policy_labels::policy_labels_configured;
use crate::core::trusty_tools_config::ResolvedTicketing;

/// The `--json` field list both `gh issue view` and `gh issue list` are asked
/// for.
///
/// Why: the audit's two entry points must read the SAME facts, or a batch run
/// would silently evaluate fewer requirements than a single-issue run. Naming
/// the list once is what keeps them in agreement.
/// What: the seven fields the requirements read, plus the three relationship
/// fields. `gh` 2.98 serves `parent`, `blockedBy` and `subIssues` from both
/// verbs, so the audit needs no separate `gh api` call for them.
/// Test: `json_fields_cover_every_audited_requirement`.
pub const AUDIT_JSON_FIELDS: &str =
    "number,milestone,projectItems,labels,comments,state,createdAt,parent,blockedBy,subIssues";

/// The comment prefix that excuses an unset milestone (#7067's escape hatch).
pub const NO_MILESTONE_PREFIX: &str = "no-milestone:";

/// A `{"title": …}` node — a milestone or a project item.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct TitleRef {
    /// The milestone or project title.
    #[serde(default)]
    pub title: String,
}

/// A `{"name": …}` node — a label.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct NameRef {
    /// The label name.
    #[serde(default)]
    pub name: String,
}

/// A `{"body": …}` node — an issue comment.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CommentRef {
    /// The comment body.
    #[serde(default)]
    pub body: String,
}

/// A `{"number": …}` node — the `parent` link.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct NumberRef {
    /// The linked issue's number.
    #[serde(default)]
    pub number: u64,
}

/// A GraphQL connection (`blockedBy`, `subIssues`).
///
/// Why: gh renders these as `{"nodes":[…],"totalCount":N}`, and `totalCount`
/// can exceed the returned `nodes` page. Reporting the count rather than the
/// node list keeps the INFO line honest about a truncated page.
/// Test: `relationships_report_what_is_set` — which failed until the
/// `camelCase` rename landed here, since `rename_all` on [`IssueFacts`] does
/// not reach a nested type and `totalCount` silently parsed as 0.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Connection {
    /// The nodes gh returned on this page.
    #[serde(default)]
    pub nodes: Vec<NumberRef>,
    /// The server-side total, which may exceed `nodes.len()`.
    #[serde(default)]
    pub total_count: u64,
}

/// One issue's audited facts, as `gh` returns them.
///
/// Why: the audit reads exactly what GitHub holds, so a fixture of this shape
/// is a faithful stand-in for a live issue and every verdict is testable
/// offline.
/// What: deserialized from a `gh issue view --json <AUDIT_JSON_FIELDS>` object,
/// or one element of the `gh issue list` array. Every field defaults, so a
/// caller that asks for fewer fields still parses.
/// Test: `facts_parse_a_live_view_payload`, `facts_parse_a_list_payload`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct IssueFacts {
    /// Issue number.
    #[serde(default)]
    pub number: u64,
    /// The single milestone, or `None` when unset.
    #[serde(default)]
    pub milestone: Option<TitleRef>,
    /// The GitHub Projects (v2) items the issue belongs to.
    #[serde(default)]
    pub project_items: Vec<TitleRef>,
    /// Every label on the issue.
    #[serde(default)]
    pub labels: Vec<NameRef>,
    /// Every comment body, searched for the `no-milestone:` escape hatch.
    #[serde(default)]
    pub comments: Vec<CommentRef>,
    /// `OPEN` / `CLOSED`.
    #[serde(default)]
    pub state: String,
    /// RFC-3339 creation timestamp, used by the `--since` window.
    #[serde(default)]
    pub created_at: String,
    /// The parent issue, when this is a sub-issue.
    #[serde(default)]
    pub parent: Option<NumberRef>,
    /// Issues this one is blocked by.
    #[serde(default)]
    pub blocked_by: Connection,
    /// Issues nested under this one.
    #[serde(default)]
    pub sub_issues: Connection,
}

/// One requirement's verdict.
///
/// Why: PASS/FAIL alone cannot express the two states the standard actually
/// has beyond them — a requirement waived on the record, and a fact reported
/// without a rule attached. Collapsing either into PASS is what would let a
/// waived milestone read as a set one.
/// What: `Pass`, `Fail` (the only verdict that exits 1), `Skip` (satisfied by
/// the recorded escape hatch), `Info` (reported, never gating).
/// Test: `an_unset_milestone_with_a_reason_comment_is_a_skip`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// The requirement is met.
    Pass,
    /// The requirement is violated — the audit exits 1.
    Fail,
    /// Waived by a recorded reason.
    Skip,
    /// Reported for the reader; never gating.
    Info,
}

impl Verdict {
    /// The four-or-fewer-character tag printed in a report line.
    #[must_use]
    pub fn tag(self) -> &'static str {
        match self {
            Self::Pass => "PASS",
            Self::Fail => "FAIL",
            Self::Skip => "SKIP",
            Self::Info => "INFO",
        }
    }
}

/// One audited requirement.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct AuditRow {
    /// The requirement's stable name (`project`, `milestone`, …).
    pub requirement: &'static str,
    /// Its verdict.
    pub verdict: Verdict,
    /// The evidence behind the verdict — the value found, or why it is absent.
    pub detail: String,
}

/// One issue's complete audit.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct IssueAudit {
    /// The audited issue's number.
    pub number: u64,
    /// One row per requirement, in report order.
    pub rows: Vec<AuditRow>,
}

impl IssueAudit {
    /// Whether any requirement FAILED.
    ///
    /// Why: this is the exit-code decision, and it must read only [`Verdict::Fail`]
    /// — a SKIP is a satisfied requirement and an INFO was never a requirement.
    /// Test: `a_skip_does_not_fail_the_audit`, `a_missing_project_fails`.
    #[must_use]
    pub fn failed(&self) -> bool {
        self.rows.iter().any(|r| r.verdict == Verdict::Fail)
    }

    /// The names of the failing requirements, for a one-line summary.
    /// Test: `a_missing_project_fails`.
    #[must_use]
    pub fn failing_requirements(&self) -> Vec<&'static str> {
        self.rows
            .iter()
            .filter(|r| r.verdict == Verdict::Fail)
            .map(|r| r.requirement)
            .collect()
    }
}

/// Requirement names, in the order [`audit_issue`] emits them.
const REQ_PROJECT: &str = "project";
const REQ_MILESTONE: &str = "milestone";
const REQ_COMPONENT: &str = "component label";
const REQ_RELATIONSHIPS: &str = "relationships";

/// The requirement columns a batch summary table carries.
pub const SUMMARY_REQUIREMENTS: &[&str] = &[REQ_PROJECT, REQ_MILESTONE, REQ_COMPONENT];

/// Audit one issue's facts against the resolved ticketing standard.
///
/// Why: the whole point of #7097 — the standard was enforced only by the filing
/// agent's self-report, so this reads the artifact back and states, per
/// requirement, what it found. Taking [`ResolvedTicketing`] rather than
/// hardcoded booleans is what keeps the audit and `tm issue standard` reporting
/// the same rules.
/// What: four rows in a fixed order. `project` and `milestone` are PASS/FAIL
/// only when `project_required` / `milestone_required` are set and INFO
/// otherwise; an unset milestone with a `no-milestone: <reason>` comment is
/// SKIP with the reason quoted. `component label` requires at least one of the
/// component labels [`policy_labels_configured`] yields with NO session name —
/// the `ws/<session>` workstream label is deliberately excluded, since a
/// workstream is not a component. `relationships` is always INFO.
/// Test: `a_compliant_issue_passes_every_requirement`, `a_missing_project_fails`,
/// `an_unset_milestone_with_a_reason_comment_is_a_skip`,
/// `an_unset_milestone_without_a_comment_fails`,
/// `an_unrequired_milestone_is_informational`,
/// `a_missing_component_label_fails`,
/// `relationships_are_never_a_failure`.
#[must_use]
pub fn audit_issue(facts: &IssueFacts, ticketing: &ResolvedTicketing) -> IssueAudit {
    let rows = vec![
        project_row(facts, ticketing),
        milestone_row(facts, ticketing),
        component_row(facts, ticketing),
        relationships_row(facts),
    ];
    IssueAudit {
        number: facts.number,
        rows,
    }
}

/// The `project` row.
fn project_row(facts: &IssueFacts, ticketing: &ResolvedTicketing) -> AuditRow {
    let titles: Vec<&str> = facts
        .project_items
        .iter()
        .map(|p| p.title.as_str())
        .filter(|t| !t.is_empty())
        .collect();
    let (verdict, detail) = if titles.is_empty() {
        // #7097: an unset requirement reports the absence rather than inventing
        // a rule the operator's config does not carry.
        if ticketing.project_required {
            (
                Verdict::Fail,
                "no project — `gh issue edit <N> --add-project \"<title>\"`".to_string(),
            )
        } else {
            (
                Verdict::Info,
                "no project (project_required: false)".to_string(),
            )
        }
    } else {
        (Verdict::Pass, titles.join(", "))
    };
    AuditRow {
        requirement: REQ_PROJECT,
        verdict,
        detail,
    }
}

/// The `milestone` row, including the `no-milestone:` escape hatch.
fn milestone_row(facts: &IssueFacts, ticketing: &ResolvedTicketing) -> AuditRow {
    let title = facts
        .milestone
        .as_ref()
        .map(|m| m.title.as_str())
        .filter(|t| !t.is_empty());
    let (verdict, detail) = match title {
        Some(title) => (Verdict::Pass, title.to_string()),
        None => match no_milestone_reason(&facts.comments) {
            // #7097: SKIP, never PASS — a reader must be able to tell a waived
            // milestone from a set one.
            Some(reason) => (Verdict::Skip, format!("({reason})")),
            None if ticketing.milestone_required => (
                Verdict::Fail,
                "no milestone and no `no-milestone: <reason>` comment".to_string(),
            ),
            None => (
                Verdict::Info,
                "no milestone (milestone_required: false)".to_string(),
            ),
        },
    };
    AuditRow {
        requirement: REQ_MILESTONE,
        verdict,
        detail,
    }
}

/// The reason from the first `no-milestone: <reason>` comment, if any.
///
/// Why: the standard's escape hatch is a comment, so the audit has to read
/// comment bodies rather than infer intent from an absent field.
/// What: matches the prefix case-insensitively at the start of any LINE of any
/// comment (an agent posting the note inside a longer comment still counts),
/// and returns the trimmed remainder. An empty remainder still counts as
/// recorded — the standard asks for a reason, and reporting an empty one is
/// more useful than silently treating the comment as absent.
/// Test: `an_unset_milestone_with_a_reason_comment_is_a_skip`,
/// `the_no_milestone_comment_is_matched_case_insensitively`,
/// `a_no_milestone_note_inside_a_longer_comment_counts`.
fn no_milestone_reason(comments: &[CommentRef]) -> Option<String> {
    comments.iter().find_map(|c| {
        c.body.lines().find_map(|line| {
            let line = line.trim();
            let head = line.get(..NO_MILESTONE_PREFIX.len())?;
            head.eq_ignore_ascii_case(NO_MILESTONE_PREFIX)
                .then(|| line[NO_MILESTONE_PREFIX.len()..].trim().to_string())
        })
    })
}

/// The `component label` row.
fn component_row(facts: &IssueFacts, ticketing: &ResolvedTicketing) -> AuditRow {
    // #7097: `None` for the session — `ws/<session>` is a workstream label, and
    // accepting it here would let a workstream satisfy the component rule.
    let components: Vec<String> = policy_labels_configured(ticketing, None)
        .into_iter()
        .map(|l| l.name)
        .collect();
    let present: Vec<&str> = facts
        .labels
        .iter()
        .map(|l| l.name.as_str())
        .filter(|name| components.iter().any(|c| c == name))
        .collect();
    let (verdict, detail) = if present.is_empty() {
        (
            Verdict::Fail,
            format!(
                "none of [{}] present (labels: {})",
                components.join(", "),
                if facts.labels.is_empty() {
                    "none".to_string()
                } else {
                    facts
                        .labels
                        .iter()
                        .map(|l| l.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                }
            ),
        )
    } else {
        (Verdict::Pass, present.join(", "))
    };
    AuditRow {
        requirement: REQ_COMPONENT,
        verdict,
        detail,
    }
}

/// The `relationships` row — always INFO.
///
/// Why: a standalone issue legitimately has no parent, no blocker and no
/// sub-issues, so their absence is a fact to report, never a violation.
/// Test: `relationships_are_never_a_failure`, `relationships_report_what_is_set`.
fn relationships_row(facts: &IssueFacts) -> AuditRow {
    let mut parts = Vec::with_capacity(3);
    match &facts.parent {
        Some(p) if p.number != 0 => parts.push(format!("parent #{}", p.number)),
        _ => parts.push("no parent".to_string()),
    }
    parts.push(format!("blocked-by {}", facts.blocked_by.total_count));
    parts.push(format!("sub-issues {}", facts.sub_issues.total_count));
    AuditRow {
        requirement: REQ_RELATIONSHIPS,
        verdict: Verdict::Info,
        detail: parts.join("; "),
    }
}

/// Whether an issue was created on or after `date` (`YYYY-MM-DD`).
///
/// Why: `--since` asks gh for `created:>=<date>`, and gh's search index can lag
/// or over-return. Re-checking the timestamp gh itself returned keeps the
/// reported window equal to the requested one rather than to whatever the
/// search served.
/// What: an RFC-3339 `createdAt` sorts lexicographically, so a prefix
/// comparison against the `YYYY-MM-DD` date is the whole test. An issue with no
/// `createdAt` (a caller that asked for fewer fields) is KEPT — dropping it
/// would silently shrink the audited set.
/// Test: `the_since_window_keeps_the_boundary_day`,
/// `the_since_window_keeps_an_issue_with_no_timestamp`.
#[must_use]
pub fn created_on_or_after(facts: &IssueFacts, date: &str) -> bool {
    facts.created_at.is_empty() || facts.created_at.as_str() >= date
}

/// Render one issue's audit as the per-requirement report.
///
/// Why: one line per requirement is the whole deliverable — a reader must see
/// which rule was checked, what was found, and the verdict, without reading the
/// config.
/// What: a `#<number>` header then one aligned `  <requirement>  <TAG>  <detail>`
/// line per row.
/// Test: `render_audit_prints_one_line_per_requirement`.
#[must_use]
pub fn render_audit(audit: &IssueAudit) -> String {
    let width = audit
        .rows
        .iter()
        .map(|r| r.requirement.len())
        .max()
        .unwrap_or(0);
    let mut out = format!("#{}\n", audit.number);
    for row in &audit.rows {
        out.push_str(&format!(
            "  {:width$}  {}  {}\n",
            row.requirement,
            row.verdict.tag(),
            row.detail,
        ));
    }
    out
}

/// Render a batch of audits as a summary table.
///
/// Why: `--recent`/`--since` audits many issues, and a full per-requirement
/// block for each buries the failures. The table is one row per issue with one
/// column per gating requirement, then a count.
/// What: a header row, one row per audit carrying each
/// [`SUMMARY_REQUIREMENTS`] verdict tag and the overall verdict, then the
/// audited/failing counts. Relationships are omitted — they never gate.
/// Test: `render_summary_tabulates_every_issue`,
/// `render_summary_counts_only_failures`.
#[must_use]
pub fn render_summary(audits: &[IssueAudit]) -> String {
    let mut out = String::from("issue     project   milestone  component  verdict\n");
    for audit in audits {
        let tag = |req: &str| {
            audit
                .rows
                .iter()
                .find(|r| r.requirement == req)
                .map_or("-", |r| r.verdict.tag())
        };
        out.push_str(&format!(
            "{:<9} {:<9} {:<10} {:<10} {}\n",
            format!("#{}", audit.number),
            tag(REQ_PROJECT),
            tag(REQ_MILESTONE),
            tag(REQ_COMPONENT),
            if audit.failed() { "FAIL" } else { "PASS" },
        ));
    }
    let failing = audits.iter().filter(|a| a.failed()).count();
    out.push_str(&format!(
        "\n{} issue(s) audited, {failing} with a FAIL\n",
        audits.len()
    ));
    out
}

#[cfg(test)]
#[path = "issue_audit_tests.rs"]
mod tests;
