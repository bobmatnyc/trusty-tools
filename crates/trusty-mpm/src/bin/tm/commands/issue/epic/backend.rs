//! The `EpicBackend` seam and its `gh`/`git`-backed implementation (#8447).
//!
//! Why: `tm issue epic` needs six operations `TicketSystem` does not carry —
//! create an issue, rename it, read a body, write a body, enumerate native
//! sub-issues, attach a project — plus two `git` reads that decide whether the
//! plan document is publishable. Widening `TicketSystem` for them would push a
//! 299-SLOC trait past the cap and force the JIRA/Linear stubs to grow default
//! bodies for verbs they will never implement, so D7 puts them behind their own
//! narrow trait over the same [`CommandRunner`] seam.
//! What: [`EpicBackend`], the [`NewIssue`] creation spec, the [`ChildIssue`]
//! value type, and [`GhEpicBackend`] which drives `gh` and `git` through an
//! injected runner. Every method is fallible and every failure is propagated —
//! the ONE degradation this family permits (a project attach that the token's
//! scope refuses) is decided in `create.rs`, never swallowed here.
//! Test: `gh_backend_creates_an_issue_and_reads_its_number`,
//! `gh_backend_refuses_a_create_whose_url_carries_no_number`,
//! `gh_backend_parses_the_sub_issue_connection`,
//! `gh_backend_reports_an_absent_plan_doc_as_none` in `tests.rs`.

use serde::Deserialize;

use crate::commands::ticket::runner::CommandRunner;

/// The `git` ref a plan document must be reachable from before an epic is filed.
///
/// D4: creation refuses until the document is on the remote, so the permalink
/// the tracker carries resolves for everyone, not just the author's checkout.
pub(crate) const PUBLISH_REF: &str = "origin/main";

/// A single issue to file.
///
/// Why: `gh issue create` takes eight flags whose omission is silent (no label,
/// no milestone, no parent), so the argument set is one value the caller builds
/// once and the backend renders, rather than eight positional parameters a call
/// site can get out of order.
/// What: the title, body, label set, milestone title, and the tracker number
/// whose native sub-issue this becomes (`None` for the tracker itself).
/// Test: `gh_backend_creates_an_issue_and_reads_its_number`.
#[derive(Debug, Clone)]
pub(crate) struct NewIssue {
    /// Issue title, already in its final `[EPIC …]` form.
    pub(crate) title: String,
    /// Issue body (markdown).
    pub(crate) body: String,
    /// Every label to apply — type, `ws/<session>`, and component(s).
    pub(crate) labels: Vec<String>,
    /// Milestone title; phases take the tracker's.
    pub(crate) milestone: String,
    /// Tracker number, making this a native sub-issue in the same call.
    pub(crate) parent: Option<u64>,
}

/// One native sub-issue of a tracker.
///
/// Why: the `phases` block is rendered from live child state, so the row's
/// number, state and gate all come from the child itself.
/// What: the number, title, `OPEN`/`CLOSED` state, and body (whose `## Gate`
/// section fills the table's Gate column).
/// Test: `gh_backend_parses_the_sub_issue_connection`.
#[derive(Debug, Clone)]
pub(crate) struct ChildIssue {
    /// The child's issue number.
    pub(crate) number: u64,
    /// The child's full title, including the `[EPIC_<n> PHASE_<m>]` prefix.
    pub(crate) title: String,
    /// `OPEN` or `CLOSED`, as gh reports it.
    pub(crate) state: String,
    /// The child's body, read for its `## Gate` section.
    pub(crate) body: String,
}

/// Everything `tm issue epic` asks of GitHub and git.
///
/// Why: the verb family mutates several issues in sequence and must be provable
/// against a fake, including the partway-failure arms — a backend that fails
/// only its third call is the only way to test that an interrupted run leaves
/// no placeholder title and that a failed body write changes nothing.
/// What: two `git` reads that establish the plan document's publish state, and
/// eight `gh` operations. Contract for every implementor: a call that did not
/// do what it says returns `Err`. No method may report success on an empty
/// result — [`EpicBackend::set_body`] in particular must refuse an empty body,
/// which is the failure that wiped
/// [#8445](https://github.com/bobmatnyc/trusty-tools/issues/8445) when the
/// hand-run procedure accepted empty `awk` output.
/// Test: `GhEpicBackend` through `FakeRunner`; the orchestration through
/// `FakeBackend` — see `tests.rs`.
pub(crate) trait EpicBackend {
    /// `owner/repo` for the repository the working directory sits in.
    fn repo_slug(&self) -> anyhow::Result<String>;

    /// The repository-root-relative path of `path`, or `None` when git does not
    /// track it.
    fn repo_relative_path(&self, path: &str) -> anyhow::Result<Option<String>>;

    /// The commit that last touched `path` on [`PUBLISH_REF`], or `None` when
    /// the document has never reached it.
    fn publish_sha(&self, path: &str) -> anyhow::Result<Option<String>>;

    /// The commit that last touched `path` on `HEAD`, or `None` when it is
    /// uncommitted locally.
    fn local_sha(&self, path: &str) -> anyhow::Result<Option<String>>;

    /// File one issue and return its number.
    fn create_issue(&self, spec: &NewIssue) -> anyhow::Result<u64>;

    /// Rename an issue in place.
    fn set_title(&self, issue: u64, title: &str) -> anyhow::Result<()>;

    /// Read an issue's body verbatim.
    fn body(&self, issue: u64) -> anyhow::Result<String>;

    /// Overwrite an issue's body.
    fn set_body(&self, issue: u64, body: &str) -> anyhow::Result<()>;

    /// Every native sub-issue of `tracker`, with bodies.
    fn children(&self, tracker: u64) -> anyhow::Result<Vec<ChildIssue>>;

    /// The tracker already carrying `outcome` as its title suffix, if any.
    fn find_tracker(&self, outcome: &str) -> anyhow::Result<Option<u64>>;

    /// Add an issue to the owner's project `number`.
    fn attach_project(&self, issue: u64, number: u64) -> anyhow::Result<()>;

    /// Post a comment on an issue.
    fn comment(&self, issue: u64, body: &str) -> anyhow::Result<()>;
}

/// `{"subIssues": {"nodes": [...]}}` — gh 2.96 renders the connection as an
/// object, never a bare array (see the `tm-epic` manual procedure).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SubIssueEnvelope {
    #[serde(default)]
    sub_issues: SubIssueConnection,
}

/// The `nodes` page of a sub-issue connection.
#[derive(Debug, Default, Deserialize)]
struct SubIssueConnection {
    #[serde(default)]
    nodes: Vec<SubIssueNode>,
}

/// One node of the sub-issue connection.
#[derive(Debug, Deserialize)]
struct SubIssueNode {
    #[serde(default)]
    number: u64,
    #[serde(default)]
    title: String,
    #[serde(default)]
    state: String,
}

/// One row of `gh issue list --json number,title`.
#[derive(Debug, Deserialize)]
struct IssueRow {
    #[serde(default)]
    number: u64,
    #[serde(default)]
    title: String,
}

/// `{"body": "..."}` — the single-field issue read.
#[derive(Debug, Deserialize)]
struct BodyOnly {
    #[serde(default)]
    body: String,
}

/// `{"nameWithOwner": "owner/repo"}`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RepoSlug {
    #[serde(default)]
    name_with_owner: String,
}

/// The `gh`/`git`-backed [`EpicBackend`].
///
/// Why: GitHub is the only backend the epic verbs ship against, and routing it
/// through [`CommandRunner`] is what binds #1265's per-project identity to
/// every call and what makes the whole family testable without a network.
/// What: renders each trait method to an argv and interprets the result. Bodies
/// travel as a `--body` argument rather than a `--body-file`: `Command::args`
/// passes argv straight to `execve`, so none of the shell quoting that forces
/// the hand-run procedure to use a file applies, and there is no temp file
/// whose cleanup could fail.
/// Test: the `gh_backend_*` tests in `tests.rs`.
pub(crate) struct GhEpicBackend<R: CommandRunner> {
    runner: R,
}

impl<R: CommandRunner> GhEpicBackend<R> {
    /// Construct a backend over the given runner.
    pub(crate) fn new(runner: R) -> Self {
        Self { runner }
    }

    /// Run a `git` read whose empty stdout is a meaningful "not found".
    ///
    /// Why: three of this backend's reads (`ls-files`, the two `log` lookups)
    /// exit 0 with empty output when nothing matches, so "empty" has to be
    /// mapped to `None` exactly once rather than at each call site.
    /// Test: `gh_backend_reports_an_absent_plan_doc_as_none`.
    fn git_first_line(&self, args: &[&str]) -> anyhow::Result<Option<String>> {
        let out = self.runner.run("git", args)?;
        let text = out.ok_or_stderr("git")?;
        Ok(text
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .map(str::to_string))
    }
}

impl<R: CommandRunner> EpicBackend for GhEpicBackend<R> {
    fn repo_slug(&self) -> anyhow::Result<String> {
        let out = self
            .runner
            .run("gh", &["repo", "view", "--json", "nameWithOwner"])?;
        let text = out.ok_or_stderr("gh repo view")?;
        let parsed: RepoSlug = serde_json::from_str(&text)
            .map_err(|e| anyhow::anyhow!("failed to parse `gh repo view` JSON: {e}"))?;
        if parsed.name_with_owner.is_empty() {
            anyhow::bail!("`gh repo view` returned no nameWithOwner — is this a GitHub checkout?");
        }
        Ok(parsed.name_with_owner)
    }

    fn repo_relative_path(&self, path: &str) -> anyhow::Result<Option<String>> {
        self.git_first_line(&["ls-files", "--full-name", "--", path])
    }

    fn publish_sha(&self, path: &str) -> anyhow::Result<Option<String>> {
        self.git_first_line(&["log", "-1", "--format=%H", PUBLISH_REF, "--", path])
    }

    fn local_sha(&self, path: &str) -> anyhow::Result<Option<String>> {
        self.git_first_line(&["log", "-1", "--format=%H", "HEAD", "--", path])
    }

    fn create_issue(&self, spec: &NewIssue) -> anyhow::Result<u64> {
        let number = spec.parent.map(|p| p.to_string());
        let mut args: Vec<&str> = vec![
            "issue",
            "create",
            "--title",
            &spec.title,
            "--body",
            &spec.body,
            "--milestone",
            &spec.milestone,
        ];
        for label in &spec.labels {
            args.push("--label");
            args.push(label);
        }
        // #8447: `--parent` creates the native sub-issue link in the SAME call
        // as the child, so there is no window where a created child is unlinked.
        if let Some(parent) = number.as_deref() {
            args.push("--parent");
            args.push(parent);
        }
        let out = self.runner.run("gh", &args)?;
        let text = out.ok_or_stderr("gh issue create")?;
        issue_number_from_url(&text)
    }

    fn set_title(&self, issue: u64, title: &str) -> anyhow::Result<()> {
        let n = issue.to_string();
        self.runner
            .run("gh", &["issue", "edit", &n, "--title", title])?
            .ok_or_stderr("gh issue edit --title")?;
        Ok(())
    }

    fn body(&self, issue: u64) -> anyhow::Result<String> {
        let n = issue.to_string();
        let out = self
            .runner
            .run("gh", &["issue", "view", &n, "--json", "body"])?;
        let text = out.ok_or_stderr("gh issue view --json body")?;
        let parsed: BodyOnly = serde_json::from_str(&text).map_err(|e| {
            anyhow::anyhow!("failed to parse `gh issue view` body for #{issue}: {e}")
        })?;
        Ok(parsed.body)
    }

    fn set_body(&self, issue: u64, body: &str) -> anyhow::Result<()> {
        // #8447: the empty-body guard the hand-run procedure learned the hard
        // way — `gh issue edit --body-file` accepts an empty file and wipes the
        // issue. Refuse here too, so no caller can reintroduce it.
        if body.trim().is_empty() {
            anyhow::bail!("refusing to write an empty body to #{issue}");
        }
        let n = issue.to_string();
        self.runner
            .run("gh", &["issue", "edit", &n, "--body", body])?
            .ok_or_stderr("gh issue edit --body")?;
        Ok(())
    }

    fn children(&self, tracker: u64) -> anyhow::Result<Vec<ChildIssue>> {
        let n = tracker.to_string();
        let out = self
            .runner
            .run("gh", &["issue", "view", &n, "--json", "subIssues"])?;
        let text = out.ok_or_stderr("gh issue view --json subIssues")?;
        let parsed: SubIssueEnvelope = serde_json::from_str(&text).map_err(|e| {
            anyhow::anyhow!("failed to parse the sub-issue list for #{tracker}: {e}")
        })?;
        let mut children = Vec::new();
        for node in parsed.sub_issues.nodes {
            // Fail-closed: a child whose body cannot be read would render a
            // blank Gate column, and the Gate is what justifies the pattern.
            let body = self.body(node.number)?;
            children.push(ChildIssue {
                number: node.number,
                title: node.title,
                state: node.state,
                body,
            });
        }
        Ok(children)
    }

    fn find_tracker(&self, outcome: &str) -> anyhow::Result<Option<u64>> {
        let search = format!("{outcome} in:title");
        let out = self.runner.run(
            "gh",
            &[
                "issue",
                "list",
                "--state",
                "all",
                "--limit",
                "50",
                "--search",
                &search,
                "--json",
                "number,title",
            ],
        )?;
        let text = out.ok_or_stderr("gh issue list --search")?;
        let rows: Vec<IssueRow> = serde_json::from_str(&text)
            .map_err(|e| anyhow::anyhow!("failed to parse `gh issue list` JSON: {e}"))?;
        let matches: Vec<u64> = rows
            .iter()
            .filter(|r| super::render::is_tracker_title(&r.title, r.number, outcome))
            .map(|r| r.number)
            .collect();
        match matches.as_slice() {
            [] => Ok(None),
            [one] => Ok(Some(*one)),
            many => anyhow::bail!(
                "{} issues already carry this outcome as a tracker title ({}) — \
                 pass `--tracker <number>` to say which one to resume",
                many.len(),
                many.iter()
                    .map(|n| format!("#{n}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }

    fn attach_project(&self, issue: u64, number: u64) -> anyhow::Result<()> {
        // #7952: `gh issue edit --add-project "<title>"` exits 0 and attaches
        // nothing when the title does not resolve in the scope gh derives from
        // the repository. The owner-and-number form names exactly one project.
        let slug = self.repo_slug()?;
        let owner = slug.split('/').next().unwrap_or(&slug).to_string();
        let project = number.to_string();
        let url = format!("https://github.com/{slug}/issues/{issue}");
        self.runner
            .run(
                "gh",
                &[
                    "project", "item-add", &project, "--owner", &owner, "--url", &url,
                ],
            )?
            .ok_or_stderr("gh project item-add")?;
        Ok(())
    }

    fn comment(&self, issue: u64, body: &str) -> anyhow::Result<()> {
        let n = issue.to_string();
        self.runner
            .run("gh", &["issue", "comment", &n, "--body", body])?
            .ok_or_stderr("gh issue comment")?;
        Ok(())
    }
}

/// Read an issue number out of the URL `gh issue create` prints.
///
/// Why: the number is the input to every later step — the retitle, the phase
/// titles, the `--parent` link. An unparsable URL means the create did
/// something other than what we asked, so it is an error rather than a zero.
/// What: takes the last `/`-separated segment of the last non-empty output line
/// and parses it as a number.
/// Test: `gh_backend_creates_an_issue_and_reads_its_number`,
/// `gh_backend_refuses_a_create_whose_url_carries_no_number`.
pub(crate) fn issue_number_from_url(text: &str) -> anyhow::Result<u64> {
    let line = text
        .lines()
        .map(str::trim)
        .rfind(|l| !l.is_empty())
        .unwrap_or("");
    line.rsplit('/')
        .next()
        .and_then(|n| n.parse::<u64>().ok())
        .ok_or_else(|| {
            anyhow::anyhow!("`gh issue create` printed no issue URL — got {line:?}; nothing filed")
        })
}
