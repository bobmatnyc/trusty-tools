//! The `TicketSystem` backend trait and its `gh`-backed implementation.
//!
//! Why: `tm ticket` must work against multiple issue backends (GitHub now;
//! JIRA/Linear later). A trait seam — mirroring the trusty-mpm runtime-adapter
//! `RuntimeKind`/`ClaudeCodeAdapter`/`TcodeAdapter` design — lets the
//! orchestration code stay backend-agnostic while only `gh` ships end-to-end.
//! What: the [`TicketSystemKind`] selector (clap `ValueEnum`, default `gh`), the
//! [`Issue`] value type, the [`TicketSystem`] trait (validate/comment/open_pr),
//! the [`GhTicketSystem`] impl over a [`CommandRunner`], and `not-yet-supported`
//! stubs for JIRA/Linear.
//! Test: `gh_*` and `stub_*` tests in this file drive a `FakeRunner`.

use clap::ValueEnum;
use serde::Deserialize;

use super::branch::derive_branch_name;
use super::labels::{
    AssigneeTarget, RepoLabel, gh_add_label, gh_create_label, gh_list_repo_labels, gh_remove_label,
    gh_set_assignee, gh_swap_labels,
};
use super::runner::CommandRunner;

/// Which ticketing backend `tm ticket` should use.
///
/// Why: the `[system]` positional arg selects the backend; typing it as a clap
/// `ValueEnum` rejects unknown values at parse time with a "possible values"
/// hint instead of failing deep in the workflow.
/// What: `Gh` (default, fully implemented), `Jira`, and `Linear` (stubs that
/// return a clear "not yet supported" error).
/// Test: `cli_parses_ticket_*` (parse) and `stub_*` (runtime stub behavior).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub(crate) enum TicketSystemKind {
    /// GitHub issues via the `gh` CLI (default; fully supported).
    #[default]
    Gh,
    /// Atlassian JIRA (not yet supported — stub).
    Jira,
    /// Linear (not yet supported — stub).
    Linear,
}

/// A normalised issue fetched from a ticketing backend.
///
/// Why: the orchestration code needs a backend-independent view of an issue to
/// derive the branch name and PR body, regardless of whether it came from
/// GitHub, JIRA, or Linear.
/// What: the issue number, title, body, labels, and whether it is currently open.
/// Test: built from `gh` JSON in `gh_validate_parses_open_issue`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Issue {
    /// Numeric issue identifier (`1232`).
    pub(crate) number: u64,
    /// Issue title (used for the branch slug and PR title).
    pub(crate) title: String,
    /// Issue body (seeds the agent task description).
    pub(crate) body: String,
    /// Label names (drive the conventional-commit branch type).
    pub(crate) labels: Vec<String>,
    /// Current GitHub assignee logins (the read side of the assignee model;
    /// needed by `set_assignee`'s `None` clear-all rule — RFC §5.4).
    pub(crate) assignees: Vec<String>,
    /// Whether the issue is OPEN (closed issues are refused).
    pub(crate) open: bool,
}

impl Issue {
    /// Derive the ticket branch name for this issue.
    ///
    /// Why: keeps branch derivation a method on the normalised issue so every
    /// caller agrees on the same `<type>/<number>-<slug>` value.
    /// What: delegates to [`derive_branch_name`] with this issue's number,
    /// title, and labels.
    /// Test: `issue_branch_name` in this file.
    pub(crate) fn branch_name(&self) -> String {
        derive_branch_name(self.number, &self.title, &self.labels)
    }
}

/// A ticketing backend: validate an issue, comment on it, and open a PR for it.
///
/// Why: the one-shot workflow is backend-agnostic; the only backend-specific
/// bits are fetching/validating the issue, posting comments, and opening the PR.
/// What: `validate` resolves+checks the issue is open, `comment` posts an audit
/// note, `name` returns the human label. PR opening lives on the gh impl because
/// only `gh` ships now.
/// Test: `GhTicketSystem` via `FakeRunner`; stubs via `stub_*`.
pub(crate) trait TicketSystem {
    /// Human-readable backend name for messages.
    fn name(&self) -> &'static str;

    /// Resolve the issue and verify it EXISTS and is OPEN.
    ///
    /// Returns an actionable error for a missing or closed issue.
    fn validate(&self, issue_number: u64) -> anyhow::Result<Issue>;

    /// Post a comment on the issue for the audit trail.
    fn comment(&self, issue_number: u64, body: &str) -> anyhow::Result<()>;

    // --- NEW for #1246: label/assignee write side -----------------------------
    //
    // Each ships with a default body that ERRORS so the `Jira`/`Linear` stubs
    // keep compiling unchanged; `GhTicketSystem` overrides all of them. A future
    // `tm issue` run against a non-`gh` backend fails loudly and legibly rather
    // than silently. (RFC §3.3.)

    /// List labels that already exist in the repo (for idempotent seeding).
    fn list_repo_labels(&self) -> anyhow::Result<Vec<RepoLabel>> {
        anyhow::bail!("list_repo_labels not supported for this ticket system")
    }

    /// Create a label in the repo (create-missing half of seeding).
    fn create_label(&self, _label: &RepoLabel) -> anyhow::Result<()> {
        anyhow::bail!("create_label not supported for this ticket system")
    }

    /// Add one label to an issue (two-call transition fallback primitive).
    fn add_label(&self, _issue: u64, _label: &str) -> anyhow::Result<()> {
        anyhow::bail!("add_label not supported for this ticket system")
    }

    /// Remove one label from an issue (`repair`/fallback primitive).
    fn remove_label(&self, _issue: u64, _label: &str) -> anyhow::Result<()> {
        anyhow::bail!("remove_label not supported for this ticket system")
    }

    /// Atomic single-call label swap (the transition default — RFC §5.2).
    fn swap_labels(&self, _issue: u64, _add: &str, _remove: &str) -> anyhow::Result<()> {
        anyhow::bail!("swap_labels not supported for this ticket system")
    }

    /// Apply an assignee rule to an issue; `current` is the issue's existing
    /// assignee set (needed for the `None` clear-all rule — RFC §5.4).
    fn set_assignee(
        &self,
        _issue: u64,
        _who: &AssigneeTarget,
        _current: &[String],
    ) -> anyhow::Result<()> {
        anyhow::bail!("set_assignee not supported for this ticket system")
    }
}

/// GitHub-backed [`TicketSystem`] driving the `gh` CLI.
///
/// Why: GitHub is the default and only fully-supported backend; wrapping `gh`
/// behind a [`CommandRunner`] keeps it unit-testable.
/// What: `validate` runs `gh issue view <n> --json ...` and parses the JSON;
/// `comment` runs `gh issue comment <n> --body <text>`.
/// Test: `gh_validate_parses_open_issue`, `gh_validate_rejects_closed`,
/// `gh_validate_rejects_missing`, `gh_comment_posts`.
pub(crate) struct GhTicketSystem<R: CommandRunner> {
    runner: R,
}

impl<R: CommandRunner> GhTicketSystem<R> {
    /// Construct a gh-backed system over the given runner.
    ///
    /// Why: dependency injection of the runner is what makes the type testable.
    /// What: stores the runner for later `gh` invocations.
    /// Test: used by every `gh_*` test.
    pub(crate) fn new(runner: R) -> Self {
        Self { runner }
    }
}

/// Shape of the `gh issue view --json number,title,body,state,labels` payload.
///
/// Why: decouples our [`Issue`] from gh's wire shape so a gh field rename only
/// touches this struct.
/// What: deserializes the subset of fields we request.
/// Test: parsed in `gh_validate_parses_open_issue`.
#[derive(Debug, Deserialize)]
struct GhIssueJson {
    number: u64,
    #[serde(default)]
    title: String,
    #[serde(default)]
    body: String,
    /// gh reports `OPEN` / `CLOSED`.
    #[serde(default)]
    state: String,
    #[serde(default)]
    labels: Vec<GhLabel>,
    #[serde(default)]
    assignees: Vec<GhAssignee>,
}

/// A single assignee object in the gh issue JSON.
///
/// Why: gh returns assignees as objects with a `login` field; the `None`
/// clear-all assignee rule (RFC §5.4) needs those logins.
/// What: captures just the `login`.
/// Test: parsed in `gh_validate_parses_assignees`.
#[derive(Debug, Deserialize)]
struct GhAssignee {
    #[serde(default)]
    login: String,
}

/// A single label object in the gh issue JSON.
///
/// Why: gh returns labels as objects with a `name` field, not bare strings.
/// What: captures just the `name`.
/// Test: parsed alongside `GhIssueJson`.
#[derive(Debug, Deserialize)]
struct GhLabel {
    #[serde(default)]
    name: String,
}

impl<R: CommandRunner> TicketSystem for GhTicketSystem<R> {
    fn name(&self) -> &'static str {
        "gh"
    }

    fn validate(&self, issue_number: u64) -> anyhow::Result<Issue> {
        let number = issue_number.to_string();
        let out = self.runner.run(
            "gh",
            &[
                "issue",
                "view",
                &number,
                "--json",
                "number,title,body,state,labels,assignees",
            ],
        )?;
        if !out.success {
            let detail = out.stderr.trim();
            anyhow::bail!(
                "issue #{issue_number} could not be resolved via `gh`{} \
                 — check the number and that `gh auth status` is logged in",
                if detail.is_empty() {
                    String::new()
                } else {
                    format!(" ({detail})")
                }
            );
        }
        let parsed: GhIssueJson = serde_json::from_str(out.stdout.trim()).map_err(|e| {
            anyhow::anyhow!("failed to parse `gh issue view` JSON for #{issue_number}: {e}")
        })?;
        let open = parsed.state.eq_ignore_ascii_case("OPEN");
        if !open {
            anyhow::bail!(
                "issue #{issue_number} is {} — refusing to start work on a non-open issue",
                if parsed.state.is_empty() {
                    "not open".to_string()
                } else {
                    parsed.state.to_lowercase()
                }
            );
        }
        Ok(Issue {
            number: parsed.number,
            title: parsed.title,
            body: parsed.body,
            labels: parsed.labels.into_iter().map(|l| l.name).collect(),
            assignees: parsed.assignees.into_iter().map(|a| a.login).collect(),
            open,
        })
    }

    fn comment(&self, issue_number: u64, body: &str) -> anyhow::Result<()> {
        let number = issue_number.to_string();
        let out = self
            .runner
            .run("gh", &["issue", "comment", &number, "--body", body])?;
        out.ok_or_stderr("gh issue comment")?;
        Ok(())
    }

    // --- #1246 gh-backed overrides: thin delegation to the `labels` helpers ----

    fn list_repo_labels(&self) -> anyhow::Result<Vec<RepoLabel>> {
        gh_list_repo_labels(&self.runner)
    }

    fn create_label(&self, label: &RepoLabel) -> anyhow::Result<()> {
        gh_create_label(&self.runner, label)
    }

    fn add_label(&self, issue: u64, label: &str) -> anyhow::Result<()> {
        gh_add_label(&self.runner, issue, label)
    }

    fn remove_label(&self, issue: u64, label: &str) -> anyhow::Result<()> {
        gh_remove_label(&self.runner, issue, label)
    }

    fn swap_labels(&self, issue: u64, add: &str, remove: &str) -> anyhow::Result<()> {
        gh_swap_labels(&self.runner, issue, add, remove)
    }

    fn set_assignee(
        &self,
        issue: u64,
        who: &AssigneeTarget,
        current: &[String],
    ) -> anyhow::Result<()> {
        gh_set_assignee(&self.runner, issue, who, current)
    }
}

/// Build the "not yet supported" error for a stubbed backend.
///
/// Why: JIRA and Linear are designed-for but unimplemented; a single shared
/// constructor keeps the wording consistent and asserted in one place.
/// What: returns an `anyhow` error naming the backend and pointing at `gh`.
/// Test: `stub_jira_not_supported`, `stub_linear_not_supported`.
pub(crate) fn not_yet_supported(system: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "ticket system `{system}` is not yet supported — only `gh` (GitHub) is \
         implemented today; JIRA and Linear are planned"
    )
}

#[cfg(test)]
mod tests {
    use super::super::runner::CommandOutput;
    use super::*;
    use std::cell::RefCell;

    /// A scripted [`CommandRunner`] for unit tests.
    ///
    /// Why: drives the gh-backed system without spawning real processes.
    /// What: returns queued [`CommandOutput`]s in order and records the
    /// `(program, args)` of every call for assertion.
    /// Test: used by all `gh_*` tests in this module.
    struct FakeRunner {
        outputs: RefCell<Vec<CommandOutput>>,
        calls: RefCell<Vec<(String, Vec<String>)>>,
    }

    impl FakeRunner {
        fn new(outputs: Vec<CommandOutput>) -> Self {
            Self {
                outputs: RefCell::new(outputs),
                calls: RefCell::new(Vec::new()),
            }
        }
    }

    impl CommandRunner for FakeRunner {
        fn run(&self, program: &str, args: &[&str]) -> anyhow::Result<CommandOutput> {
            self.calls.borrow_mut().push((
                program.to_string(),
                args.iter().map(|a| a.to_string()).collect(),
            ));
            let mut outs = self.outputs.borrow_mut();
            if outs.is_empty() {
                anyhow::bail!("FakeRunner exhausted")
            }
            Ok(outs.remove(0))
        }
    }

    fn ok_out(stdout: &str) -> CommandOutput {
        CommandOutput {
            success: true,
            stdout: stdout.to_string(),
            stderr: String::new(),
        }
    }

    fn fail_out(stderr: &str) -> CommandOutput {
        CommandOutput {
            success: false,
            stdout: String::new(),
            stderr: stderr.to_string(),
        }
    }

    #[test]
    fn ticket_system_kind_default_is_gh() {
        assert_eq!(TicketSystemKind::default(), TicketSystemKind::Gh);
    }

    #[test]
    fn issue_branch_name() {
        let issue = Issue {
            number: 1232,
            title: "Add the thing".to_string(),
            body: String::new(),
            labels: vec!["enhancement".to_string()],
            assignees: vec![],
            open: true,
        };
        assert_eq!(issue.branch_name(), "feat/1232-add-the-thing");
    }

    #[test]
    fn gh_validate_parses_open_issue() {
        let json = r#"{"number":1232,"title":"Add the thing","body":"do it","state":"OPEN","labels":[{"name":"bug"}]}"#;
        let sys = GhTicketSystem::new(FakeRunner::new(vec![ok_out(json)]));
        let issue = sys.validate(1232).expect("should validate");
        assert_eq!(issue.number, 1232);
        assert_eq!(issue.title, "Add the thing");
        assert_eq!(issue.body, "do it");
        assert_eq!(issue.labels, vec!["bug".to_string()]);
        assert!(issue.open);
        // Missing `assignees` defaults to empty.
        assert!(issue.assignees.is_empty());
        // The derived branch uses the bug label → fix.
        assert_eq!(issue.branch_name(), "fix/1232-add-the-thing");
    }

    #[test]
    fn gh_validate_parses_assignees() {
        let json = r#"{"number":7,"title":"t","body":"","state":"OPEN","labels":[],"assignees":[{"login":"alice"},{"login":"bob"}]}"#;
        let sys = GhTicketSystem::new(FakeRunner::new(vec![ok_out(json)]));
        let issue = sys.validate(7).expect("should validate");
        assert_eq!(
            issue.assignees,
            vec!["alice".to_string(), "bob".to_string()]
        );
    }

    #[test]
    fn gh_validate_rejects_closed() {
        let json = r#"{"number":5,"title":"old","body":"","state":"CLOSED","labels":[]}"#;
        let sys = GhTicketSystem::new(FakeRunner::new(vec![ok_out(json)]));
        let err = sys.validate(5).unwrap_err().to_string();
        assert!(
            err.contains("closed"),
            "expected closed message, got: {err}"
        );
    }

    #[test]
    fn gh_validate_rejects_missing() {
        let sys = GhTicketSystem::new(FakeRunner::new(vec![fail_out(
            "GraphQL: Could not resolve to an Issue",
        )]));
        let err = sys.validate(99999).unwrap_err().to_string();
        assert!(
            err.contains("could not be resolved"),
            "expected resolve error, got: {err}"
        );
    }

    #[test]
    fn gh_comment_posts() {
        let sys = GhTicketSystem::new(FakeRunner::new(vec![ok_out("")]));
        sys.comment(1232, "starting work").expect("should comment");
    }

    #[test]
    fn command_output_ok_returns_stdout() {
        let out = ok_out("  hello  ");
        assert_eq!(out.ok_or_stderr("gh").unwrap(), "hello");
    }

    #[test]
    fn command_output_err_includes_stderr() {
        let out = fail_out("boom");
        let err = out.ok_or_stderr("gh").unwrap_err().to_string();
        assert!(err.contains("boom"), "got: {err}");
    }

    #[test]
    fn stub_jira_not_supported() {
        let err = not_yet_supported("jira").to_string();
        assert!(err.contains("jira"));
        assert!(err.contains("not yet supported"));
    }

    #[test]
    fn stub_linear_not_supported() {
        let err = not_yet_supported("linear").to_string();
        assert!(err.contains("linear"));
        assert!(err.contains("not yet supported"));
    }
}
