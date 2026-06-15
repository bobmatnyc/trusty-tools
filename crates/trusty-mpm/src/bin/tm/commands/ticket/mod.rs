//! `tm ticket <issue#> [system]` — one-shot issue → worktree → PR → close.
//!
//! Why: the manual issue-resolution loop (validate the issue, branch off main in
//! an isolated worktree, drive an agent to implement it, post audit comments,
//! open a PR that closes the issue on merge) is repetitive and error-prone.
//! `tm ticket` packages it into a single invocation, reusing the session-manager
//! managed-spawn path (#842 driver) so the work runs in an isolated, observable
//! Claude Code session.
//! What: this module owns argument normalisation ([`parse_issue_number`]), the
//! task-prompt builder ([`build_task`]), the repo-URL resolver, and the
//! [`ticket`] dispatcher that wires backend validation → issue comments → managed
//! spawn together. Backend logic lives in `system.rs`; branch derivation in
//! `branch.rs`; the process seam in `runner.rs`.
//! Test: `parse_issue_number_*`, `build_task_*` here; backend/branch logic in the
//! sibling modules; CLI parsing in `tests.rs`.

pub(crate) mod branch;
pub(crate) mod labels;
pub(crate) mod runner;
pub(crate) mod system;

use serde::Deserialize;

use system::{GhTicketSystem, Issue, TicketSystem, TicketSystemKind, not_yet_supported};

use runner::{CommandRunner, RealCommandRunner};

/// Normalise a user-supplied issue reference into a numeric id.
///
/// Why: the spec allows `1232`, `#1232`, and `#1232` with surrounding noise; the
/// rest of the workflow needs a clean `u64`, and a bad reference should fail
/// early with a clear message rather than producing a nonsense branch.
/// What: strips an optional leading `#` and whitespace, then parses the result as
/// a `u64`; rejects zero and non-numeric input.
/// Test: `parse_issue_number_plain`, `parse_issue_number_hash`,
/// `parse_issue_number_rejects_nonnumeric`, `parse_issue_number_rejects_zero`.
pub(crate) fn parse_issue_number(raw: &str) -> anyhow::Result<u64> {
    let trimmed = raw.trim().trim_start_matches('#').trim();
    let n: u64 = trimmed.parse().map_err(|_| {
        anyhow::anyhow!(
            "invalid issue reference `{raw}` — expected a number like `1232` or `#1232`"
        )
    })?;
    if n == 0 {
        anyhow::bail!("invalid issue reference `{raw}` — issue numbers start at 1");
    }
    Ok(n)
}

/// Build the agent task prompt for the managed session.
///
/// Why: the spawned managed session needs a self-contained instruction that
/// names the issue, the exact branch to create, the base branch to create it
/// from, and the close-on-merge convention so the driver agent produces a PR
/// that closes the issue. Naming the actual base branch (not an assumed `main`)
/// keeps the instruction correct for `master`/`trunk` repos. Building it as a
/// pure function keeps the wording asserted in a test.
/// What: returns a multi-line task string embedding the issue number/title/body,
/// the derived branch name, the `base_branch` to branch from, and the
/// `Closes #<n>` + PR requirements.
/// Test: `build_task_includes_branch_and_close`,
/// `build_task_names_non_main_base_branch`.
pub(crate) fn build_task(issue: &Issue, branch: &str, base_branch: &str) -> String {
    format!(
        "Address issue #{number}: {title}\n\n\
         Issue body:\n{body}\n\n\
         Workflow requirements:\n\
         - Work on branch `{branch}` (create it off the default branch `{base_branch}`).\n\
         - Implement the change described in the issue.\n\
         - Commit with a message referencing the issue and ending in `Closes #{number}`.\n\
         - Open a pull request linking the issue so a squash-merge closes it.\n",
        number = issue.number,
        title = issue.title,
        body = if issue.body.trim().is_empty() {
            "(no body provided)"
        } else {
            issue.body.trim()
        },
        branch = branch,
        base_branch = base_branch,
    )
}

/// The resolved repository coordinates a managed spawn needs.
///
/// Why: the managed-spawn endpoint needs both the clone URL *and* the ref to
/// branch from. Hard-coding `main` as the base ref is wrong for repos on
/// `master`/`trunk`/any other default branch — the agent would branch off a
/// non-existent or stale ref. Bundling both values keeps them resolved together
/// from the same `gh repo view` source of truth.
/// What: the clone URL and the repository's actual default branch name.
/// Test: `resolve_repo_threads_default_branch` asserts a non-`main` default
/// branch flows through; `resolve_repo_empty_url_errors` covers the bail.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RepoCoordinates {
    url: String,
    default_branch: String,
}

/// Resolve the GitHub repo clone URL and default branch for the current checkout.
///
/// Why: the managed-spawn endpoint provisions an isolated workspace by cloning a
/// repo URL and branching off a base ref; for `tm ticket` that repo is the one
/// the issue lives in (the current repo) and the base ref must be the repo's
/// *actual* default branch, not an assumed `main`. Asking `gh` keeps this
/// independent of the local remote naming and correct for `master`/`trunk` repos.
/// What: runs `gh repo view --json url,defaultBranchRef --jq ...` once per field
/// through the injected [`CommandRunner`] seam, returning a [`RepoCoordinates`];
/// surfaces an actionable error when `gh` cannot resolve the repo or returns an
/// empty URL.
/// Test: `resolve_repo_threads_default_branch`, `resolve_repo_empty_url_errors`.
fn resolve_repo<R: CommandRunner>(runner: &R) -> anyhow::Result<RepoCoordinates> {
    let url = runner
        .run("gh", &["repo", "view", "--json", "url", "--jq", ".url"])?
        .ok_or_stderr("gh repo view")?;
    if url.is_empty() {
        anyhow::bail!(
            "could not resolve the current repository via `gh repo view` — run inside a GitHub checkout"
        );
    }
    let default_branch = runner
        .run(
            "gh",
            &[
                "repo",
                "view",
                "--json",
                "defaultBranchRef",
                "--jq",
                ".defaultBranchRef.name",
            ],
        )?
        .ok_or_stderr("gh repo view")?;
    // Fall back to `main` only if gh somehow returns an empty default branch
    // (e.g. an unusual repo state); a present value is always preferred.
    let default_branch = if default_branch.is_empty() {
        "main".to_string()
    } else {
        default_branch
    };
    Ok(RepoCoordinates {
        url,
        default_branch,
    })
}

/// `tm ticket <issue#> [system]` dispatcher.
///
/// Why: the single operator entry point for the one-shot issue-resolution loop.
/// What: parses the issue reference, selects the backend (JIRA/Linear are
/// rejected with a clear stub error), validates the issue is open, posts any
/// `--note` text as issue comments, derives the ticket branch, resolves the repo
/// URL, and spawns a managed session (task = "address issue #<n> …") via the
/// daemon so the #842 driver agent implements the change and opens the PR.
/// Test: parse/build logic is unit-tested here and in the sibling modules; the
/// HTTP spawn round-trip mirrors `session_new` (covered by the managed MVP
/// integration test) and is exercised end-to-end manually.
pub(crate) async fn ticket(
    client: &reqwest::Client,
    url: &str,
    issue_ref: String,
    system: TicketSystemKind,
    notes: Vec<String>,
    runtime: trusty_mpm::runtime::RuntimeKind,
) -> anyhow::Result<()> {
    let issue_number = parse_issue_number(&issue_ref)?;

    // #1265: resolve the active project's GitHub identity once and bind it to
    // every `gh` subprocess. An absent `github:` config yields an empty binding
    // (ambient gh identity, no regression). Two independent `RealCommandRunner`s
    // (one for the backend's issue-level calls, one for repo-level `gh repo
    // view`) each carry the SAME resolved overrides.
    let gh_env = crate::gh_identity::load_gh_env()?;
    let runner = RealCommandRunner::with_env(gh_env.vars().to_vec());
    let backend = match system {
        TicketSystemKind::Gh => {
            GhTicketSystem::new(RealCommandRunner::with_env(gh_env.vars().to_vec()))
        }
        TicketSystemKind::Jira => return Err(not_yet_supported("jira")),
        TicketSystemKind::Linear => return Err(not_yet_supported("linear")),
    };

    // 1. Validate — must exist and be OPEN.
    let issue = backend.validate(issue_number)?;
    println!(
        "validated issue #{} via {}: {}",
        issue.number,
        backend.name(),
        issue.title
    );

    // 2. Notes → issue comments (audit trail) before work starts.
    for note in &notes {
        backend.comment(issue_number, note)?;
        println!("posted note as comment on #{issue_number}");
    }

    // 3. Derive the ticket branch from the issue title + labels.
    let branch = issue.branch_name();
    println!("ticket branch: {branch}");

    // 4. Resolve the repo URL + default branch, then spawn the managed session
    //    that drives the work. The base ref is the repo's *actual* default
    //    branch (so `master`/`trunk` repos branch correctly), not an assumed
    //    `main`.
    let repo = resolve_repo(&runner)?;
    let task = build_task(&issue, &branch, &repo.default_branch);
    spawn_managed(
        client,
        url,
        &repo.url,
        &repo.default_branch,
        &branch,
        &task,
        runtime,
    )
    .await?;
    Ok(())
}

/// POST the managed-session spawn request that drives the ticket implementation.
///
/// Why: reuses the exact session-manager spawn path (`POST
/// /api/v1/sessions/managed`) that `tm session new` uses, so `tm ticket` plugs
/// into the existing isolated-workspace + runtime-adapter machinery rather than
/// re-implementing it. The provisioner clones the repo and the driver agent
/// creates `branch` off the base ref per the task instructions.
/// What: posts repo_url/ref/task/name_hint/runtime and prints the new session id,
/// state, runtime, and attach command. `ref` is the repo's resolved default
/// branch (`base_ref`, e.g. `main`/`master`/`trunk`); the agent creates the
/// ticket branch from there per [`build_task`].
/// Test: mirrors `session_new`; HTTP path covered by the managed MVP integration
/// test.
#[allow(clippy::too_many_arguments)]
async fn spawn_managed(
    client: &reqwest::Client,
    url: &str,
    repo_url: &str,
    base_ref: &str,
    branch: &str,
    task: &str,
    runtime: trusty_mpm::runtime::RuntimeKind,
) -> anyhow::Result<()> {
    #[derive(Deserialize)]
    struct SpawnResp {
        id: String,
        name: String,
        state: String,
        attach_cmd: String,
        #[serde(default)]
        runtime: String,
    }
    let resp: SpawnResp = client
        .post(format!("{url}/api/v1/sessions/managed"))
        .json(&serde_json::json!({
            "repo_url": repo_url,
            "ref": base_ref,
            "task": task,
            "name_hint": branch,
            "runtime": runtime.as_str(),
        }))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    println!(
        "spawned {} ({}) [{}] runtime={}",
        resp.name, resp.id, resp.state, resp.runtime
    );
    println!("  task drives branch `{branch}` → PR (Closes the issue on merge)");
    println!("  attach: {}", resp.attach_cmd);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use runner::CommandOutput;
    use std::cell::RefCell;

    /// A scripted [`CommandRunner`] for the mod-level resolver tests.
    ///
    /// Why: lets `resolve_repo` be exercised without a live `gh` or GitHub repo,
    /// proving the default-branch (and empty-URL bail) logic in isolation.
    /// What: returns queued [`CommandOutput`]s in FIFO order.
    /// Test: drives `resolve_repo_threads_default_branch` and
    /// `resolve_repo_empty_url_errors`.
    struct FakeRunner {
        outputs: RefCell<Vec<CommandOutput>>,
    }

    impl FakeRunner {
        fn new(outputs: Vec<CommandOutput>) -> Self {
            Self {
                outputs: RefCell::new(outputs),
            }
        }
    }

    impl CommandRunner for FakeRunner {
        fn run(&self, _program: &str, _args: &[&str]) -> anyhow::Result<CommandOutput> {
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

    #[test]
    fn parse_issue_number_plain() {
        assert_eq!(parse_issue_number("1232").unwrap(), 1232);
    }

    #[test]
    fn parse_issue_number_hash() {
        assert_eq!(parse_issue_number("#1232").unwrap(), 1232);
        assert_eq!(parse_issue_number("  #42 ").unwrap(), 42);
    }

    #[test]
    fn parse_issue_number_rejects_nonnumeric() {
        assert!(parse_issue_number("abc").is_err());
        assert!(parse_issue_number("#").is_err());
        assert!(parse_issue_number("12a").is_err());
    }

    #[test]
    fn parse_issue_number_rejects_zero() {
        assert!(parse_issue_number("0").is_err());
        assert!(parse_issue_number("#0").is_err());
    }

    #[test]
    fn build_task_includes_branch_and_close() {
        let issue = Issue {
            number: 1232,
            title: "Add the thing".to_string(),
            body: "do the thing properly".to_string(),
            labels: vec!["enhancement".to_string()],
            assignees: vec![],
            open: true,
        };
        let task = build_task(&issue, "feat/1232-add-the-thing", "main");
        assert!(task.contains("issue #1232"));
        assert!(task.contains("Add the thing"));
        assert!(task.contains("do the thing properly"));
        assert!(task.contains("feat/1232-add-the-thing"));
        assert!(task.contains("Closes #1232"));
        assert!(task.contains("pull request"));
    }

    #[test]
    fn build_task_handles_empty_body() {
        let issue = Issue {
            number: 7,
            title: "Fix it".to_string(),
            body: "   ".to_string(),
            labels: vec![],
            assignees: vec![],
            open: true,
        };
        let task = build_task(&issue, "feat/7-fix-it", "main");
        assert!(task.contains("(no body provided)"));
    }

    #[test]
    fn build_task_names_non_main_base_branch() {
        // A repo on `master` must have its base branch named in the task so the
        // agent branches off the right ref, not an assumed `main`.
        let issue = Issue {
            number: 9,
            title: "Do thing".to_string(),
            body: "body".to_string(),
            labels: vec![],
            assignees: vec![],
            open: true,
        };
        let task = build_task(&issue, "feat/9-do-thing", "master");
        assert!(
            task.contains("default branch `master`"),
            "task did not name the master base branch: {task}"
        );
        assert!(
            !task.contains("`main`"),
            "task wrongly mentioned main: {task}"
        );
    }

    #[test]
    fn resolve_repo_threads_default_branch() {
        // First gh call returns the URL, second returns a non-`main` default
        // branch; both must be threaded through into the coordinates.
        let runner = FakeRunner::new(vec![
            ok_out("https://github.com/acme/widget"),
            ok_out("master"),
        ]);
        let repo = resolve_repo(&runner).expect("should resolve");
        assert_eq!(repo.url, "https://github.com/acme/widget");
        assert_eq!(repo.default_branch, "master");
    }

    #[test]
    fn resolve_repo_falls_back_to_main_on_empty_branch() {
        // An empty default-branch response falls back to `main` rather than
        // producing an empty ref.
        let runner = FakeRunner::new(vec![ok_out("https://github.com/acme/widget"), ok_out("")]);
        let repo = resolve_repo(&runner).expect("should resolve");
        assert_eq!(repo.default_branch, "main");
    }

    #[test]
    fn resolve_repo_empty_url_errors() {
        // An empty URL from `gh repo view` must bail with the actionable hint to
        // run inside a GitHub checkout.
        let runner = FakeRunner::new(vec![ok_out("")]);
        let err = resolve_repo(&runner).unwrap_err().to_string();
        assert!(
            err.contains("could not resolve the current repository"),
            "expected actionable empty-URL error, got: {err}"
        );
        assert!(
            err.contains("GitHub checkout"),
            "expected checkout hint, got: {err}"
        );
    }
}
