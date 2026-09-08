//! The live half of `tm issue standard` — the milestones and projects a new
//! issue can actually be filed into (#7067).
//!
//! Why: #7067 made a milestone and a project mandatory on every new issue, and
//! a rule the agent cannot satisfy is a rule it will break. The agent had no
//! read-back: it either hand-ran `gh api …/milestones` and `gh project list`
//! every filing, or guessed a title and got a `gh` error. This module is that
//! read-back, so `tm issue standard` prints the requirement and the choices in
//! one place.
//!
//! What: [`render_filing_targets`] renders the section. Both fetches go through
//! the injected [`CommandRunner`] — the same seam `tm ticket` uses, which is
//! where #1265's per-project GitHub identity is bound — so this module spawns
//! no `gh` of its own.
//!
//! # Failing closed
//!
//! A `gh` failure prints `milestones: unavailable (<error>)`, never an empty
//! list. An empty list reads as "this repo has no milestones, so none is
//! needed", which is the exact misreading that would re-create the #7067 gap;
//! the requirement lines are rendered from config and are unaffected by a
//! fetch failure.
//!
//! Test: `filing_targets_list_open_milestones_and_projects`,
//! `filing_targets_report_a_milestone_fetch_failure`,
//! `filing_targets_report_a_project_fetch_failure`,
//! `filing_targets_skip_closed_projects`.

use std::fmt::Write as _;

use anyhow::Context as _;
use serde::Deserialize;

use crate::commands::ticket::runner::CommandRunner;

/// One project row from `gh project list --format json`.
///
/// Why: decouples the rendering from gh's wire shape.
/// What: the fields the section prints, plus `closed` so a finished project is
/// never offered as a filing target.
/// Test: `filing_targets_skip_closed_projects`.
#[derive(Debug, Deserialize)]
struct GhProject {
    #[serde(default)]
    title: String,
    #[serde(default)]
    number: u64,
    #[serde(default)]
    closed: bool,
}

/// The `gh project list --format json` envelope.
#[derive(Debug, Deserialize)]
struct GhProjectList {
    #[serde(default)]
    projects: Vec<GhProject>,
}

/// Render the live filing-target section.
///
/// Why: the agent needs the exact titles it must hand to
/// `gh issue create --milestone` / `--add-project`, from the repo it is about
/// to file into — a hand-typed name is the failure this replaces.
/// What: open milestones (GitHub's list-milestones endpoint defaults to
/// `state=open`) and the owner's open projects, each rendered as a counted
/// list, or as an `unavailable (<error>)` line when `gh` failed.
/// Test: see the module doc.
pub(crate) fn render_filing_targets(runner: &dyn CommandRunner) -> String {
    let mut out = String::new();
    out.push_str("\nfiling targets — live, from gh:\n");

    match fetch_milestones(runner) {
        Ok(titles) => {
            let _ = writeln!(out, "  open milestones ({}):", titles.len());
            for title in &titles {
                let _ = writeln!(out, "    {title}");
            }
        }
        Err(e) => {
            let _ = writeln!(out, "  milestones: unavailable ({})", one_line(&e));
        }
    }

    match fetch_projects(runner) {
        Ok(projects) => {
            let _ = writeln!(out, "  open projects ({}):", projects.len());
            for (number, title) in &projects {
                let _ = writeln!(out, "    #{number}  {title}");
            }
        }
        Err(e) => {
            let _ = writeln!(out, "  projects: unavailable ({})", one_line(&e));
        }
    }

    out
}

/// Collapse an error chain to one line so a multi-line `gh` stderr cannot
/// break the section's shape.
fn one_line(err: &anyhow::Error) -> String {
    err.to_string()
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("; ")
}

/// Fetch the repository's OPEN milestone titles.
///
/// Why: the milestone an issue carries is chosen from this list, never invented.
/// What: `gh api repos/{owner}/{repo}/milestones` — gh expands the `{owner}` /
/// `{repo}` placeholders from the working directory's remote, so no separate
/// repository lookup is needed. The endpoint defaults to `state=open`.
/// Test: `filing_targets_list_open_milestones_and_projects`.
fn fetch_milestones(runner: &dyn CommandRunner) -> anyhow::Result<Vec<String>> {
    let out = runner.run(
        "gh",
        &[
            "api",
            "repos/{owner}/{repo}/milestones",
            "--paginate",
            "--jq",
            ".[].title",
        ],
    )?;
    let text = out.ok_or_stderr("gh api repos/{owner}/{repo}/milestones")?;
    Ok(text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect())
}

/// Cap passed to `gh project list -L`. gh's own default is 30, which it applies
/// silently, so a project past position 30 would never reach the agent.
const PROJECT_LIST_LIMIT: &str = "200";

/// Fetch the repository owner's OPEN projects as `(number, title)`.
///
/// Why: `gh project list` is owner-scoped, not repo-scoped, so the owner has to
/// be resolved first; asking gh for it keeps the answer tied to the same
/// working directory the milestone call reads.
/// What: `gh repo view --json owner`, then
/// `gh project list --owner <login> -L 200 --format json`, dropping closed
/// projects. #7169: an owner-qualified list can hit a scope wall a bare
/// `gh project list` (the viewer's own projects) does not — see
/// [`fetch_projects_owner_scoped`]'s doc — so a scope-shaped failure there
/// retries with the bare form rather than reporting the section unavailable
/// on a call that a plain `gh project list` would have satisfied.
/// Test: `filing_targets_skip_closed_projects`,
/// `project_list_carries_an_explicit_limit`,
/// `filing_targets_fall_back_to_the_bare_project_list_on_a_scope_error`,
/// `a_non_scope_project_failure_does_not_fall_back`.
fn fetch_projects(runner: &dyn CommandRunner) -> anyhow::Result<Vec<(u64, String)>> {
    match fetch_projects_owner_scoped(runner) {
        Ok(projects) => Ok(projects),
        // #7169: the bare fallback also failed — surface the ORIGINAL error,
        // which names the owner-scoped call the operator actually needs
        // explained.
        Err(e) if is_scope_shaped_failure(&e) => fetch_projects_bare(runner).map_err(|_| e),
        Err(e) => Err(e),
    }
}

/// The owner-qualified `gh project list --owner <login> -L 200 --format json`
/// half of [`fetch_projects`], split out so the fallback in the caller can
/// retry independently.
fn fetch_projects_owner_scoped(runner: &dyn CommandRunner) -> anyhow::Result<Vec<(u64, String)>> {
    let owner_out = runner.run(
        "gh",
        &["repo", "view", "--json", "owner", "--jq", ".owner.login"],
    )?;
    let owner = owner_out.ok_or_stderr("gh repo view --json owner")?;
    if owner.is_empty() {
        anyhow::bail!("`gh repo view` named no owner — is this a GitHub repository?");
    }

    // #7067: without -L, gh caps the list at 30 and signals no truncation.
    let list_out = runner.run(
        "gh",
        &[
            "project",
            "list",
            "--owner",
            &owner,
            "-L",
            PROJECT_LIST_LIMIT,
            "--format",
            "json",
        ],
    )?;
    let text = list_out.ok_or_stderr("gh project list")?;
    parse_project_list(&text, &format!("`gh project list --owner {owner}`"))
}

/// The bare `gh project list -L 200 --format json` fallback — the viewer's own
/// projects, with no `--owner` qualification.
///
/// Why (#7169): `gh project list --owner <login>` queries GitHub's
/// `user(login:)`/`organization(login:)` GraphQL object, which GitHub gates on
/// the `read:project` scope even when `<login>` is the token's own account; the
/// bare form queries `viewer`, which the same token can read without that
/// scope. A token missing `read:project` therefore fails the owner-qualified
/// call while `gh project list` alone — "the same token", per #7169's report —
/// succeeds.
/// Test: `filing_targets_fall_back_to_the_bare_project_list_on_a_scope_error`.
fn fetch_projects_bare(runner: &dyn CommandRunner) -> anyhow::Result<Vec<(u64, String)>> {
    let list_out = runner.run(
        "gh",
        &[
            "project",
            "list",
            "-L",
            PROJECT_LIST_LIMIT,
            "--format",
            "json",
        ],
    )?;
    let text = list_out.ok_or_stderr("gh project list")?;
    parse_project_list(&text, "`gh project list`")
}

fn parse_project_list(text: &str, source: &str) -> anyhow::Result<Vec<(u64, String)>> {
    let parsed: GhProjectList =
        serde_json::from_str(text).with_context(|| format!("could not parse {source} output"))?;
    Ok(parsed
        .projects
        .into_iter()
        .filter(|p| !p.closed)
        .map(|p| (p.number, p.title))
        .collect())
}

/// Whether an error from [`fetch_projects_owner_scoped`] looks like a missing
/// `project`/`read:project` OAuth scope rather than some other failure (`gh`
/// missing, unauthenticated entirely, network outage).
///
/// Why (#7169): only a scope-shaped failure should trigger the bare-list
/// fallback — any other failure means the bare call would fail identically,
/// so retrying it would just mask the real error with a confusing second one.
/// What: a case-insensitive substring match on the phrasing GitHub's GraphQL
/// API and `gh` itself use for this class of failure.
/// Test: `filing_targets_fall_back_to_the_bare_project_list_on_a_scope_error`,
/// `a_non_scope_project_failure_does_not_fall_back`.
fn is_scope_shaped_failure(err: &anyhow::Error) -> bool {
    let text = err.to_string().to_lowercase();
    (text.contains("scope") && text.contains("project")) || text.contains("read:project")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::ticket::runner::CommandOutput;
    use std::cell::RefCell;

    /// A scripted [`CommandRunner`] returning queued outputs in order.
    struct FakeRunner {
        outputs: RefCell<Vec<anyhow::Result<CommandOutput>>>,
        calls: RefCell<Vec<Vec<String>>>,
    }

    impl FakeRunner {
        fn new(outputs: Vec<anyhow::Result<CommandOutput>>) -> Self {
            Self {
                outputs: RefCell::new(outputs),
                calls: RefCell::new(Vec::new()),
            }
        }
    }

    impl CommandRunner for FakeRunner {
        fn run(&self, program: &str, args: &[&str]) -> anyhow::Result<CommandOutput> {
            let mut call = vec![program.to_string()];
            call.extend(args.iter().map(|a| (*a).to_string()));
            self.calls.borrow_mut().push(call);
            let mut outs = self.outputs.borrow_mut();
            if outs.is_empty() {
                anyhow::bail!("FakeRunner exhausted")
            }
            outs.remove(0)
        }
    }

    fn ok_out(stdout: &str) -> anyhow::Result<CommandOutput> {
        Ok(CommandOutput {
            success: true,
            stdout: stdout.to_string(),
            stderr: String::new(),
        })
    }

    fn fail_out(stderr: &str) -> anyhow::Result<CommandOutput> {
        Ok(CommandOutput {
            success: false,
            stdout: String::new(),
            stderr: stderr.to_string(),
        })
    }

    const PROJECTS_JSON: &str = r#"{"projects":[
        {"number":3,"title":"trusty-mpm","closed":false},
        {"number":9,"title":"Retired 2025","closed":true}
    ]}"#;

    #[test]
    fn filing_targets_list_open_milestones_and_projects() {
        let runner = FakeRunner::new(vec![
            ok_out("Backlog · mpm/core\nmpm 1.4\n"),
            ok_out("bobmatnyc"),
            ok_out(PROJECTS_JSON),
        ]);
        let text = render_filing_targets(&runner);
        assert!(text.contains("open milestones (2):"), "{text}");
        assert!(text.contains("Backlog · mpm/core"), "{text}");
        assert!(text.contains("open projects (1):"), "{text}");
        assert!(text.contains("#3  trusty-mpm"), "{text}");
        // The owner reaches `gh project list`, not a hardcoded login.
        let calls = runner.calls.borrow();
        assert!(
            calls[2].contains(&"bobmatnyc".to_string()),
            "{:?}",
            calls[2]
        );
    }

    #[test]
    fn filing_targets_skip_closed_projects() {
        let runner = FakeRunner::new(vec![ok_out(""), ok_out("bobmatnyc"), ok_out(PROJECTS_JSON)]);
        let text = render_filing_targets(&runner);
        assert!(!text.contains("Retired 2025"), "{text}");
    }

    #[test]
    fn project_list_carries_an_explicit_limit() {
        // #7067: gh caps `project list` at 30 with no truncation signal, so the
        // flag and its value must both survive in argv.
        let runner = FakeRunner::new(vec![ok_out(""), ok_out("bobmatnyc"), ok_out(PROJECTS_JSON)]);
        let _ = render_filing_targets(&runner);
        let calls = runner.calls.borrow();
        let argv = &calls[2];
        let flag = argv
            .iter()
            .position(|a| a == "-L")
            .unwrap_or_else(|| panic!("`gh project list` carries -L: {argv:?}"));
        assert_eq!(
            argv.get(flag + 1).map(String::as_str),
            Some(PROJECT_LIST_LIMIT),
            "{argv:?}"
        );
        assert!(
            PROJECT_LIST_LIMIT.parse::<u32>().is_ok_and(|n| n > 30),
            "the limit must exceed gh's silent default of 30: {PROJECT_LIST_LIMIT}"
        );
    }

    #[test]
    fn filing_targets_report_a_milestone_fetch_failure() {
        // #7067: a failed fetch must never render as an empty list — an agent
        // would read that as "no milestone needed".
        let runner = FakeRunner::new(vec![
            fail_out("gh: Not Found (HTTP 404)"),
            ok_out("bobmatnyc"),
            ok_out(PROJECTS_JSON),
        ]);
        let text = render_filing_targets(&runner);
        assert!(text.contains("milestones: unavailable ("), "{text}");
        assert!(text.contains("HTTP 404"), "{text}");
        assert!(!text.contains("open milestones (0)"), "{text}");
    }

    #[test]
    fn filing_targets_report_a_project_fetch_failure() {
        let runner = FakeRunner::new(vec![
            ok_out("mpm 1.4\n"),
            fail_out("gh: your token has not been granted the required scopes: `project`"),
        ]);
        let text = render_filing_targets(&runner);
        assert!(text.contains("projects: unavailable ("), "{text}");
        assert!(text.contains("project"), "{text}");
        // The milestone half still rendered.
        assert!(text.contains("open milestones (1):"), "{text}");
    }

    /// #7169: a real `gh` scope error names `read:project` this way when a
    /// classic PAT lacks the `project` scope.
    const SCOPE_ERROR: &str = "gh: your token has not been granted the required scopes: \
        the 'read:project' scope is required";

    #[test]
    fn filing_targets_fall_back_to_the_bare_project_list_on_a_scope_error() {
        // #7169: the owner-scoped call fails on a missing `read:project`
        // scope; the bare call (same token, no `--owner`) succeeds and its
        // result must reach the report instead of "unavailable".
        let runner = FakeRunner::new(vec![
            ok_out("Backlog · mpm/core\n"),
            ok_out("bobmatnyc"),
            fail_out(SCOPE_ERROR),
            ok_out(PROJECTS_JSON),
        ]);
        let text = render_filing_targets(&runner);
        assert!(text.contains("open projects (1):"), "{text}");
        assert!(text.contains("#3  trusty-mpm"), "{text}");
        assert!(!text.contains("projects: unavailable"), "{text}");
        // The fallback call carries no `--owner`.
        let calls = runner.calls.borrow();
        let fallback = calls.last().expect("a fallback call was made");
        assert!(!fallback.contains(&"--owner".to_string()), "{fallback:?}");
    }

    #[test]
    fn a_non_scope_project_failure_does_not_fall_back() {
        // #7169: an unrelated failure (offline, unauthenticated) must not
        // trigger a second `gh` call that would just fail the same way — the
        // original error is what the operator needs to see.
        let runner = FakeRunner::new(vec![
            ok_out("Backlog · mpm/core\n"),
            ok_out("bobmatnyc"),
            fail_out("gh: not authenticated"),
        ]);
        let text = render_filing_targets(&runner);
        assert!(text.contains("projects: unavailable ("), "{text}");
        assert!(text.contains("not authenticated"), "{text}");
        assert_eq!(runner.calls.borrow().len(), 3, "no fallback call was made");
    }

    #[test]
    fn a_multi_line_gh_error_stays_one_line() {
        let runner = FakeRunner::new(vec![fail_out("first\n\nsecond"), fail_out("boom")]);
        let text = render_filing_targets(&runner);
        let line = text
            .lines()
            .find(|l| l.contains("milestones: unavailable"))
            .expect("the unavailable line renders");
        assert!(line.contains("first; second"), "{line}");
    }
}
