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
//! # The accepted component labels — two independent sources (#7123, #7182)
//!
//! [`trusty_mpm::core::component_labels::ComponentLabels::resolve`] derives the
//! accepted set from the audited repository's own `Cargo.toml` — no network,
//! bounded and fail-closed. #7123 shipped that alone and it is correct against
//! a checkout that actually holds the crate in question. #7182 (recurrence of
//! #7123 against `trusty-audit` in isolation) could not be reproduced against
//! this repository's HEAD — a live probe confirms `resolve` already accepts
//! `trusty-audit` — which points at the ONE thing a local Cargo.toml read can
//! never see: a stale or wrong working directory at the point `tm issue audit`
//! actually ran. [`widen_with_live_crate_labels`] closes that gap with a second,
//! independent source: the repository's OWN `gh label list`, which answers from
//! GitHub rather than the local checkout, so it is right even when the
//! filesystem read is not. A label whose description names a crate
//! (`Crate: <name>`, the convention `tm issue seed-labels` writes) widens the
//! accepted set; a `gh` failure (offline, unauthenticated) leaves the
//! Cargo.toml-derived set untouched rather than failing the audit.
//!
//! Test: `single_issue_output_is_the_per_requirement_report`,
//! `a_failing_audit_exits_nonzero`, `a_window_prints_the_summary_table`,
//! `an_empty_window_is_not_a_failure`, `widen_adds_a_crate_described_label`,
//! `widen_ignores_a_non_crate_label`, `widen_on_gh_failure_keeps_the_set`, and
//! `cli_parses_issue_audit_*` in `tests.rs`.

use trusty_mpm::core::component_labels::ComponentLabels;
use trusty_mpm::core::gh_identity::GhEnv;
use trusty_mpm::core::issue_audit::{IssueAudit, audit_issue, render_audit, render_summary};
use trusty_mpm::core::issue_audit_gh::{AuditWindow, list_open_issues, view_issue};
use trusty_mpm::core::trusty_tools_config::ResolvedTicketing;

use crate::commands::ticket::labels::gh_list_repo_labels;
use crate::commands::ticket::runner::CommandRunner;

/// Prefix a live `gh` label's description carries when it names a crate.
///
/// Why: `tm issue seed-labels` and manual seeding both write `Crate: <name>` —
/// naming it once keeps [`widen_with_live_crate_labels`] and any future reader
/// of the convention in agreement.
const CRATE_LABEL_DESCRIPTION_PREFIX: &str = "Crate:";

/// Run `tm issue audit`.
///
/// Why: one entry point for both modes so the exit-code rule — nonzero iff some
/// requirement FAILED — is decided once.
/// What: with `issue` set, reads that one issue and prints the per-requirement
/// report. With `recent` or `since` set, lists the matching OPEN issues and
/// prints the summary table. With none of the three, errors rather than
/// guessing a default window. `gh` runs in the current directory, which is what
/// selects the repository — and, since #7123, also what selects the crate
/// labels that satisfy the owning-component rule (widened per the module doc).
/// Test: see the module doc; the pure halves are unit-tested in the library.
pub(crate) fn run(
    ticketing: &ResolvedTicketing,
    gh_env: &GhEnv,
    runner: &dyn CommandRunner,
    issue: Option<u64>,
    recent: Option<usize>,
    since: Option<String>,
) -> anyhow::Result<()> {
    // #7123: the accepted component labels are the audited repository's own
    // crate labels, not just the ones the harness seeds.
    let cwd = std::env::current_dir().ok();
    let components = ComponentLabels::resolve(ticketing, cwd.as_deref());
    // #7182: widen from the live `gh` label set, which is right even when the
    // Cargo.toml read above was not (see the module doc).
    let components = widen_with_live_crate_labels(components, runner);
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

/// Widen `components` with every live `gh` label whose description names a
/// crate (`Crate: <name>`).
///
/// Why: see the module doc's "two independent sources" section (#7182) — a
/// stale or wrong working directory defeats the Cargo.toml-derived set with no
/// visible error, while `gh label list` answers from GitHub regardless of what
/// the local checkout holds.
/// What: `gh_list_repo_labels` on success unions every matching label's `name`
/// into `components`; a `gh` failure (offline, unauthenticated, a truncated
/// page) returns `components` unchanged — an ADDITIONAL source failing closed,
/// never turning an audit itself into a hard error.
/// Test: `widen_adds_a_crate_described_label`, `widen_ignores_a_non_crate_label`,
/// `widen_on_gh_failure_keeps_the_set`.
fn widen_with_live_crate_labels(
    components: ComponentLabels,
    runner: &dyn CommandRunner,
) -> ComponentLabels {
    let Ok(labels) = gh_list_repo_labels(runner) else {
        return components;
    };
    let extra = labels
        .into_iter()
        .filter(|l| {
            l.description
                .trim_start()
                .starts_with(CRATE_LABEL_DESCRIPTION_PREFIX)
        })
        .map(|l| l.name);
    ComponentLabels::from_names(components.names().iter().cloned().chain(extra))
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
    use std::cell::RefCell;

    use super::*;
    use crate::commands::ticket::runner::CommandOutput;
    use trusty_mpm::core::issue_audit::IssueFacts;
    use trusty_mpm::core::trusty_tools_config::{TrustyToolsConfig, resolve_ticketing};

    fn standard() -> ResolvedTicketing {
        resolve_ticketing(&TrustyToolsConfig::default()).expect("the defaults resolve")
    }

    /// A scripted [`CommandRunner`] returning one queued `gh label list`
    /// result.
    struct FakeRunner(RefCell<Option<anyhow::Result<CommandOutput>>>);

    impl CommandRunner for FakeRunner {
        fn run(&self, _program: &str, _args: &[&str]) -> anyhow::Result<CommandOutput> {
            self.0
                .borrow_mut()
                .take()
                .unwrap_or_else(|| anyhow::bail!("FakeRunner called more than once"))
        }
    }

    fn ok_out(stdout: &str) -> FakeRunner {
        FakeRunner(RefCell::new(Some(Ok(CommandOutput {
            success: true,
            stdout: stdout.to_string(),
            stderr: String::new(),
        }))))
    }

    fn fail_out() -> FakeRunner {
        FakeRunner(RefCell::new(Some(Ok(CommandOutput {
            success: false,
            stdout: String::new(),
            stderr: "gh: not authenticated".to_string(),
        }))))
    }

    #[test]
    fn widen_adds_a_crate_described_label() {
        // #7182: `trusty-audit` in isolation — the exact shape the recurrence
        // was filed against, sourced from GitHub rather than the checkout.
        let runner = ok_out(
            r#"[{"name": "trusty-audit", "color": "", "description": "Crate: trusty-audit"}]"#,
        );
        let widened = widen_with_live_crate_labels(
            ComponentLabels::from_names(["trusty-mpm".into()]),
            &runner,
        );
        assert!(
            widened.accepts("trusty-audit"),
            "set was {:?}",
            widened.names()
        );
        assert!(widened.accepts("trusty-mpm"), "the original set is kept");
    }

    #[test]
    fn widen_ignores_a_non_crate_label() {
        let runner = ok_out(
            r#"[{"name": "enhancement", "color": "", "description": "New feature or request"}]"#,
        );
        let widened = widen_with_live_crate_labels(
            ComponentLabels::from_names(["trusty-mpm".into()]),
            &runner,
        );
        assert!(
            !widened.accepts("enhancement"),
            "set was {:?}",
            widened.names()
        );
    }

    #[test]
    fn widen_on_gh_failure_keeps_the_set() {
        let runner = fail_out();
        let original = ComponentLabels::from_names(["trusty-mpm".into()]);
        let widened = widen_with_live_crate_labels(original.clone(), &runner);
        assert_eq!(widened, original, "a gh failure must not change the set");
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
