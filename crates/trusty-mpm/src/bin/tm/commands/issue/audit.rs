//! `tm issue audit` — verify a filed issue against the ticketing standard
//! (#7097).
//!
//! Why: #7067 made a milestone, a project and native relationships mandatory on
//! every new issue, and until this verb the only thing checking that was the
//! filing agent's own report. #7092 and #7093 were filed outside a `ticketing`
//! dispatch and happened to be compliant, but nothing produced that evidence —
//! a bypass or a slower fix-up would have gone undetected. This verb is the
//! read-back: it prints one line per requirement and exits 1 on a violation, so
//! a filing can be proved rather than asserted.
//!
//! What: the thin glue between the CLI and the two library halves — the pure
//! evaluation in [`trusty_mpm::core::issue_audit`] and the `gh` reads in
//! [`trusty_mpm::core::issue_audit_gh`]. [`run`] selects the single-issue or
//! windowed mode, prints, and returns `Err` when anything FAILED (which is how
//! `tm` exits 1).
//!
//! Test: `single_issue_output_is_the_per_requirement_report`,
//! `a_failing_audit_exits_nonzero`, `a_window_prints_the_summary_table`,
//! `an_empty_window_is_not_a_failure`, and `cli_parses_issue_audit_*` in
//! `tests.rs`.

use trusty_mpm::core::component_labels::ComponentLabels;
use trusty_mpm::core::gh_identity::GhEnv;
use trusty_mpm::core::issue_audit::{IssueAudit, audit_issue, render_audit, render_summary};
use trusty_mpm::core::issue_audit_gh::{AuditWindow, list_open_issues, view_issue};
use trusty_mpm::core::trusty_tools_config::ResolvedTicketing;

/// Run `tm issue audit`.
///
/// Why: one entry point for both modes so the exit-code rule — nonzero iff some
/// requirement FAILED — is decided once.
/// What: with `issue` set, reads that one issue and prints the per-requirement
/// report. With `recent` or `since` set, lists the matching OPEN issues and
/// prints the summary table. With none of the three, errors rather than
/// guessing a default window. `gh` runs in the current directory, which is what
/// selects the repository — and, since #7123, also what selects the crate
/// labels that satisfy the owning-component rule.
/// Test: see the module doc; the pure halves are unit-tested in the library.
pub(crate) fn run(
    ticketing: &ResolvedTicketing,
    gh_env: &GhEnv,
    issue: Option<u64>,
    recent: Option<usize>,
    since: Option<String>,
) -> anyhow::Result<()> {
    // #7123: the accepted component labels are the audited repository's own
    // crate labels, not just the ones the harness seeds.
    let cwd = std::env::current_dir().ok();
    let components = ComponentLabels::resolve(ticketing, cwd.as_deref());
    let audits = match (issue, recent, since) {
        (Some(number), _, _) => {
            let facts = view_issue(number, None, gh_env)?;
            vec![audit_issue(&facts, ticketing, &components)]
        }
        (None, Some(n), _) => {
            audit_window(&AuditWindow::Recent(n), ticketing, &components, gh_env)?
        }
        (None, None, Some(date)) => {
            audit_window(&AuditWindow::Since(date), ticketing, &components, gh_env)?
        }
        (None, None, None) => anyhow::bail!(
            "name an issue number, or pass --recent <n> / --since <YYYY-MM-DD> to audit a window"
        ),
    };
    print!("{}", render_report(&audits));
    exit_result(&audits)
}

/// Audit every issue in a window.
fn audit_window(
    window: &AuditWindow,
    ticketing: &ResolvedTicketing,
    components: &ComponentLabels,
    gh_env: &GhEnv,
) -> anyhow::Result<Vec<IssueAudit>> {
    Ok(list_open_issues(window, None, gh_env)?
        .iter()
        .map(|f| audit_issue(f, ticketing, components))
        .collect())
}

/// Render one audit as the per-requirement report, many as the summary table.
///
/// Why: a full four-line block per issue buries the failures once a window
/// holds thirty of them, and a one-row table hides the evidence when the
/// operator asked about a single issue. The mode picks the shape.
/// Test: `single_issue_output_is_the_per_requirement_report`,
/// `a_window_prints_the_summary_table`.
fn render_report(audits: &[IssueAudit]) -> String {
    match audits {
        [one] => render_audit(one),
        many => render_summary(many),
    }
}

/// The exit-code rule: `Err` iff some requirement FAILED.
///
/// Why: a script gates on this, so it must read only [`IssueAudit::failed`] —
/// a SKIP is a satisfied requirement and an INFO was never one.
/// Test: `a_failing_audit_exits_nonzero`, `an_empty_window_is_not_a_failure`.
fn exit_result(audits: &[IssueAudit]) -> anyhow::Result<()> {
    let failing: Vec<String> = audits
        .iter()
        .filter(|a| a.failed())
        .map(|a| format!("#{}", a.number))
        .collect();
    if failing.is_empty() {
        return Ok(());
    }
    anyhow::bail!(
        "{} issue(s) violate the ticketing standard: {}",
        failing.len(),
        failing.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use trusty_mpm::core::issue_audit::IssueFacts;
    use trusty_mpm::core::trusty_tools_config::{TrustyToolsConfig, resolve_ticketing};

    fn standard() -> ResolvedTicketing {
        resolve_ticketing(&TrustyToolsConfig::default()).expect("the defaults resolve")
    }

    fn facts(json: &str) -> IssueFacts {
        serde_json::from_str(json).expect("the fixture parses")
    }

    /// The accepted component labels, named literally (#7123) — these cases
    /// exercise rendering and the exit rule, not the derivation, which
    /// `ComponentLabels`'s own tests cover.
    fn components() -> ComponentLabels {
        ComponentLabels::from_names([String::from("trusty-mpm")])
    }

    const COMPLIANT: &str = r#"{
      "number": 7093,
      "milestone": {"title": "Backlog · mpm/core"},
      "projectItems": [{"title": "trusty-mpm"}],
      "labels": [{"name": "trusty-mpm"}],
      "comments": [],
      "state": "OPEN",
      "createdAt": "2026-09-08T00:30:20Z",
      "parent": null,
      "blockedBy": {"nodes": [], "totalCount": 0},
      "subIssues": {"nodes": [], "totalCount": 0}
    }"#;

    const NO_PROJECT: &str = r#"{
      "number": 7104,
      "milestone": {"title": "Backlog · mpm/core"},
      "projectItems": [],
      "labels": [{"name": "enhancement"}],
      "comments": [],
      "state": "OPEN",
      "createdAt": "2026-09-08T02:51:28Z",
      "parent": null,
      "blockedBy": {"nodes": [], "totalCount": 0},
      "subIssues": {"nodes": [], "totalCount": 0}
    }"#;

    #[test]
    fn single_issue_output_is_the_per_requirement_report() {
        let audits = vec![audit_issue(&facts(COMPLIANT), &standard(), &components())];
        let text = render_report(&audits);
        assert!(text.starts_with("#7093\n"), "{text}");
        assert!(text.contains("milestone"), "{text}");
        assert!(!text.contains("verdict"), "not the table: {text}");
    }

    #[test]
    fn a_window_prints_the_summary_table() {
        let audits = vec![
            audit_issue(&facts(COMPLIANT), &standard(), &components()),
            audit_issue(&facts(NO_PROJECT), &standard(), &components()),
        ];
        let text = render_report(&audits);
        assert!(text.contains("verdict"), "the table header renders: {text}");
        assert!(text.contains("#7104"), "{text}");
        assert!(text.contains("2 issue(s) audited, 1 with a FAIL"), "{text}");
    }

    #[test]
    fn a_failing_audit_exits_nonzero() {
        let audits = vec![audit_issue(&facts(NO_PROJECT), &standard(), &components())];
        let err = exit_result(&audits).expect_err("a violation must exit 1");
        assert!(err.to_string().contains("#7104"), "{err}");
    }

    #[test]
    fn a_passing_audit_exits_zero() {
        let audits = vec![audit_issue(&facts(COMPLIANT), &standard(), &components())];
        assert!(exit_result(&audits).is_ok());
    }

    #[test]
    fn an_empty_window_is_not_a_failure() {
        // A window with no issues has nothing wrong with it. Exiting 1 there
        // would make a quiet week look like a violation.
        assert!(exit_result(&[]).is_ok());
        assert!(render_report(&[]).contains("0 issue(s) audited"));
    }
}
