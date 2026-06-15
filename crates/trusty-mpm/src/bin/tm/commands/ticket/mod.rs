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
/// names the issue, the exact branch to create, and the close-on-merge
/// convention so the driver agent produces a PR that closes the issue. Building
/// it as a pure function keeps the wording asserted in a test.
/// What: returns a multi-line task string embedding the issue number/title/body,
/// the derived branch name, and the `Closes #<n>` + PR requirements.
/// Test: `build_task_includes_branch_and_close`.
pub(crate) fn build_task(issue: &Issue, branch: &str) -> String {
    format!(
        "Address issue #{number}: {title}\n\n\
         Issue body:\n{body}\n\n\
         Workflow requirements:\n\
         - Work on branch `{branch}` (create it off the latest default branch).\n\
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
    )
}

/// Resolve the GitHub repository clone URL for the current checkout via `gh`.
///
/// Why: the managed-spawn endpoint provisions an isolated workspace by cloning a
/// repo URL; for `tm ticket` that repo is the one the issue lives in (the current
/// repo). Asking `gh` keeps this independent of the local remote naming.
/// What: runs `gh repo view --json url --jq .url` and returns the trimmed URL;
/// surfaces an actionable error when `gh` cannot resolve the repo.
/// Test: covered by the live path; the parse is trivial (trim).
fn resolve_repo_url<R: CommandRunner>(runner: &R) -> anyhow::Result<String> {
    let out = runner.run("gh", &["repo", "view", "--json", "url", "--jq", ".url"])?;
    let url = out.ok_or_stderr("gh repo view")?;
    if url.is_empty() {
        anyhow::bail!(
            "could not resolve the current repository via `gh repo view` — run inside a GitHub checkout"
        );
    }
    Ok(url)
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

    // Select the backend. Only `gh` ships end-to-end; the others are stubs.
    let runner = RealCommandRunner;
    let backend = match system {
        TicketSystemKind::Gh => GhTicketSystem::new(RealCommandRunner),
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

    // 4. Resolve the repo URL and spawn the managed session that drives the work.
    let repo_url = resolve_repo_url(&runner)?;
    let task = build_task(&issue, &branch);
    spawn_managed(client, url, &repo_url, &branch, &task, runtime).await?;
    Ok(())
}

/// POST the managed-session spawn request that drives the ticket implementation.
///
/// Why: reuses the exact session-manager spawn path (`POST
/// /api/v1/sessions/managed`) that `tm session new` uses, so `tm ticket` plugs
/// into the existing isolated-workspace + runtime-adapter machinery rather than
/// re-implementing it. The provisioner clones the repo and the driver agent
/// creates `branch` off the default branch per the task instructions.
/// What: posts repo_url/ref/task/name_hint/runtime and prints the new session id,
/// state, runtime, and attach command. `ref` is the default branch (`main`); the
/// agent creates the ticket branch from there per [`build_task`].
/// Test: mirrors `session_new`; HTTP path covered by the managed MVP integration
/// test.
async fn spawn_managed(
    client: &reqwest::Client,
    url: &str,
    repo_url: &str,
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
            "ref": "main",
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
            open: true,
        };
        let task = build_task(&issue, "feat/1232-add-the-thing");
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
            open: true,
        };
        let task = build_task(&issue, "feat/7-fix-it");
        assert!(task.contains("(no body provided)"));
    }
}
