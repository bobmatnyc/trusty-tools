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
                "number,title,body,state,labels",
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
        // The derived branch uses the bug label → fix.
        assert_eq!(issue.branch_name(), "fix/1232-add-the-thing");
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
