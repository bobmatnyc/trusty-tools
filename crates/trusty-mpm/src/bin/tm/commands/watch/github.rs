//! GitHub issue discovery for `tm watch` — list + label-filter via `gh`.
//!
//! Why: `tm watch` routes work by LABEL (no bot account exists, so we do NOT
//! filter by assignee). It must enumerate a repo's issues carrying the routing
//! label and turn them into a small, backend-independent value the dispatch and
//! listen loops can act on. Hiding the `gh issue list` call behind a trait seam
//! ([`IssueLister`]) lets the loops be unit-tested with a scripted fake instead
//! of hitting GitHub over the network.
//! What: [`MatchedIssue`] is the normalised issue view; [`IssueState`] selects
//! open/all; [`IssueLister`] is the one-method seam; [`GhIssueLister`] is the
//! `gh`-backed production impl over the shared [`CommandRunner`]; [`parse_issues`]
//! turns `gh issue list --json …` output into `MatchedIssue`s and
//! [`filter_by_label`] applies the routing-label predicate defensively (gh's
//! `--label` already filters server-side, but we re-check so the predicate is
//! tested and a fake/loose backend cannot leak unmatched issues).
//! Test: `parse_issues_*`, `filter_by_label_*`, `gh_list_*` in the sibling
//! `tests.rs`.

use serde::Deserialize;

use crate::commands::ticket::runner::CommandRunner;

/// Which issues `tm watch` should consider.
///
/// Why: a one-shot `poll` against a busy board may want only `open` issues
/// (the common, safe case) but an operator backfilling may want `all`. Typing
/// it as a clap `ValueEnum` rejects bad values at parse time.
/// What: `Open` (default — the routable working set) or `All` (open + closed).
/// Maps to the `gh issue list --state` value via [`IssueState::as_gh`].
/// Test: `issue_state_default_is_open`, `issue_state_as_gh`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, clap::ValueEnum)]
pub(crate) enum IssueState {
    /// Only open issues (default — the routable working set).
    #[default]
    Open,
    /// Both open and closed issues.
    All,
}

impl IssueState {
    /// Return the `gh issue list --state` value for this selection.
    ///
    /// Why: `gh` expects the lowercase string; keeping the mapping in one place
    /// stops the CLI and the gh invocation from drifting.
    /// What: `"open"` or `"all"`.
    /// Test: `issue_state_as_gh`.
    pub(crate) fn as_gh(self) -> &'static str {
        match self {
            IssueState::Open => "open",
            IssueState::All => "all",
        }
    }
}

/// A normalised issue matched by the routing label.
///
/// Why: the dispatch and listen loops need a backend-independent, hashable-by-
/// number view of an issue — number (the dedup key), title/body (seed the agent
/// task), labels (for the defensive re-filter), and url (operator-facing log).
/// What: the issue number, title, body, label names, and html url.
/// Test: built from `gh` JSON in `parse_issues_basic`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MatchedIssue {
    /// Numeric issue identifier (the dedup key for listen mode).
    pub(crate) number: u64,
    /// Issue title (used for the spawned session name hint + task).
    pub(crate) title: String,
    /// Issue body (seeds the agent task description).
    pub(crate) body: String,
    /// Label names carried by the issue (drives the defensive re-filter).
    pub(crate) labels: Vec<String>,
    /// Web URL for the issue (operator-facing log line).
    pub(crate) url: String,
}

/// A seam for listing a repo's issues filtered by label.
///
/// Why: decouples the watch loops from a live `gh` + GitHub so the discovery →
/// filter → dispatch flow is unit-testable with a scripted fake, mirroring the
/// `TicketSystem`/`CommandRunner` seams `tm ticket` already uses.
/// What: one `list` method taking the `owner/repo`, routing label, and state,
/// returning the matched issues.
/// Test: `GhIssueLister` via `FakeRunner`; the loops via a `FakeLister`.
pub(crate) trait IssueLister {
    /// List `repo`'s issues carrying `label` in `state`.
    fn list(&self, repo: &str, label: &str, state: IssueState)
    -> anyhow::Result<Vec<MatchedIssue>>;
}

/// GitHub-backed [`IssueLister`] driving the `gh` CLI.
///
/// Why: GitHub is the only supported board today; wrapping `gh issue list`
/// behind a [`CommandRunner`] keeps the discovery path unit-testable and reuses
/// the exact process seam `tm ticket` uses.
/// What: `list` runs `gh issue list -R <repo> --label <l> --state <s> --json
/// number,title,body,labels,assignees,url`, parses the JSON, and applies the
/// defensive label re-filter.
/// Test: `gh_list_parses_and_filters`, `gh_list_surfaces_gh_failure`.
pub(crate) struct GhIssueLister<R: CommandRunner> {
    runner: R,
}

impl<R: CommandRunner> GhIssueLister<R> {
    /// Construct a gh-backed lister over the given runner.
    ///
    /// Why: dependency injection of the runner is what makes the type testable.
    /// What: stores the runner for later `gh` invocations.
    /// Test: used by every `gh_list_*` test.
    pub(crate) fn new(runner: R) -> Self {
        Self { runner }
    }
}

impl<R: CommandRunner> IssueLister for GhIssueLister<R> {
    fn list(
        &self,
        repo: &str,
        label: &str,
        state: IssueState,
    ) -> anyhow::Result<Vec<MatchedIssue>> {
        let out = self.runner.run(
            "gh",
            &[
                "issue",
                "list",
                "-R",
                repo,
                "--label",
                label,
                "--state",
                state.as_gh(),
                "--json",
                "number,title,body,labels,assignees,url",
            ],
        )?;
        if !out.success {
            let detail = out.stderr.trim();
            anyhow::bail!(
                "`gh issue list -R {repo}` failed{} — check the repo, the `gh` \
                 install, and that `gh auth status` is logged in",
                if detail.is_empty() {
                    String::new()
                } else {
                    format!(" ({detail})")
                }
            );
        }
        let issues = parse_issues(out.stdout.trim())?;
        Ok(filter_by_label(issues, label))
    }
}

/// Shape of a single `gh issue list --json …` array element.
///
/// Why: decouples our [`MatchedIssue`] from gh's wire shape so a field rename
/// only touches this struct.
/// What: deserialises the subset of fields we request.
/// Test: parsed in `parse_issues_basic`.
#[derive(Debug, Deserialize)]
struct GhIssueJson {
    number: u64,
    #[serde(default)]
    title: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    labels: Vec<GhLabel>,
    #[serde(default)]
    url: String,
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

/// Parse `gh issue list --json …` output into [`MatchedIssue`]s.
///
/// Why: keeping the JSON→value mapping a pure function makes every parse branch
/// (empty array, missing optional fields) deterministically unit-testable.
/// What: deserialises the gh array, flattening each label object to its name.
/// An empty input string is treated as an empty array (gh prints nothing when
/// there are no matches under some configurations).
/// Test: `parse_issues_basic`, `parse_issues_empty`.
pub(crate) fn parse_issues(stdout: &str) -> anyhow::Result<Vec<MatchedIssue>> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    let parsed: Vec<GhIssueJson> = serde_json::from_str(trimmed)
        .map_err(|e| anyhow::anyhow!("failed to parse `gh issue list` JSON: {e}"))?;
    Ok(parsed
        .into_iter()
        .map(|i| MatchedIssue {
            number: i.number,
            title: i.title,
            body: i.body,
            labels: i.labels.into_iter().map(|l| l.name).collect(),
            url: i.url,
        })
        .collect())
}

/// Keep only issues that actually carry `label` (defensive re-filter).
///
/// Why: `gh issue list --label` filters server-side, but the routing contract is
/// "only issues carrying the label are picked up". Re-applying the predicate
/// here makes that contract a tested invariant and means a fake/loose backend
/// (or a future HTTP path) can never leak an unmatched issue into execution.
/// The match is case-insensitive because GitHub label names are not.
/// What: returns the issues whose `labels` contain `label` (ASCII-case-insensitive).
/// Test: `filter_by_label_keeps_only_matching`, `filter_by_label_case_insensitive`.
pub(crate) fn filter_by_label(issues: Vec<MatchedIssue>, label: &str) -> Vec<MatchedIssue> {
    issues
        .into_iter()
        .filter(|i| i.labels.iter().any(|l| l.eq_ignore_ascii_case(label)))
        .collect()
}
