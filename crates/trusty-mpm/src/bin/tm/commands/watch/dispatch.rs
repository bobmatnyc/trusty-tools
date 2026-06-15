//! Per-issue dispatch for `tm watch` — build the task, then dry-run or spawn.
//!
//! Why: once an issue is matched by the routing label, watch must turn it into
//! the SAME managed-session execution `tm ticket` performs: a Claude Code session
//! spawned via `POST /api/v1/sessions/managed` whose task instructs the driver
//! agent to implement the issue and open a PR. Factoring the per-issue step out
//! of the loops keeps the safety gate (dry-run vs execute) and the task wording
//! in one tested place that both `poll` and `listen` share.
//! What: [`build_watch_task`] builds the agent prompt for a matched issue (pure,
//! tested); [`DispatchMode`] is the dry-run/execute selector; [`dispatch_issue`]
//! either prints the would-execute plan (dry-run) or POSTs the managed-spawn
//! request (execute), reusing the exact request shape `tm ticket` uses. The
//! branch ref is the repo's resolved default branch; the agent creates the ticket
//! branch from there.
//! Test: `build_watch_task_*` in the sibling `tests.rs`; the HTTP spawn mirrors
//! `tm ticket::spawn_managed` (covered by the managed MVP integration test) and
//! is exercised end-to-end manually.

use serde::Deserialize;

use trusty_mpm::runtime::RuntimeKind;

use super::github::MatchedIssue;

/// Whether `dispatch_issue` should actually spawn work or only describe it.
///
/// Why: `tm watch` drives real execution against real repos, so the default must
/// be safe — describe, do not execute — unless the operator explicitly opts in.
/// A two-variant selector makes the gate explicit and impossible to fat-finger
/// into mass execution.
/// What: `DryRun` (list what WOULD run, spawn nothing) or `Execute` (POST the
/// managed-spawn request).
/// Test: `dispatch_dry_run_spawns_nothing` (dry-run path), and the execute path
/// via the manual e2e + the shared `spawn_managed` shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DispatchMode {
    /// Describe what would run; spawn nothing (the safe default).
    DryRun,
    /// Actually POST the managed-session spawn request.
    Execute,
}

/// Build the agent task prompt for a watch-matched issue.
///
/// Why: the spawned managed session needs a self-contained instruction naming
/// the issue, the branch to create, the base branch to create it from, and the
/// close-on-merge convention — identical in spirit to `tm ticket`'s task so a
/// watched issue is resolved the same way a hand-run ticket is. Building it as a
/// pure function keeps the wording asserted in a test.
/// What: returns a multi-line task string embedding the issue number/title/body,
/// the derived `branch`, the `base_branch` to branch from, and the close-on-merge
/// (`Closes #<n>`) and pull-request requirements.
/// Test: `build_watch_task_includes_branch_and_close` and
/// `build_watch_task_handles_empty_body`.
pub(crate) fn build_watch_task(issue: &MatchedIssue, branch: &str, base_branch: &str) -> String {
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

/// Derive the per-issue working branch name for a watched issue.
///
/// Why: every matched issue needs a stable, unique branch so concurrent watch
/// dispatches do not collide; deriving it from the issue number keeps it
/// deterministic and re-runnable.
/// What: returns `tm-watch/<number>` — a namespaced branch that groups all
/// watch-driven work and never clashes across issues.
/// Test: `watch_branch_name_is_namespaced`.
pub(crate) fn watch_branch_name(issue: &MatchedIssue) -> String {
    format!("tm-watch/{}", issue.number)
}

/// Dispatch one matched issue: dry-run describes it, execute spawns it.
///
/// Why: the single per-issue action both loops call; centralising the safety
/// gate and the managed-spawn shape means dry-run can never accidentally spawn
/// and the execute path can never diverge from `tm ticket`.
/// What: in [`DispatchMode::DryRun`] prints the issue, derived branch, and target
/// repo/ref WITHOUT any HTTP call; in [`DispatchMode::Execute`] POSTs
/// `{repo_url, ref, task, name_hint, runtime}` to `/api/v1/sessions/managed` and
/// prints the new session id/state/attach command. Returns `Ok(true)` when the
/// issue was actually spawned (execute path succeeded) and `Ok(false)` for a
/// dry-run, so the caller's dedup set only records genuinely-dispatched issues.
/// Test: dry-run via `dispatch_dry_run_spawns_nothing`; execute via the manual
/// e2e (mirrors `tm ticket::spawn_managed`).
pub(crate) async fn dispatch_issue(
    client: &reqwest::Client,
    url: &str,
    repo_url: &str,
    base_ref: &str,
    issue: &MatchedIssue,
    mode: DispatchMode,
    runtime: RuntimeKind,
) -> anyhow::Result<bool> {
    let branch = watch_branch_name(issue);
    match mode {
        DispatchMode::DryRun => {
            println!(
                "[dry-run] would dispatch #{} ({}) → branch `{branch}` off `{base_ref}` of {repo_url}",
                issue.number, issue.url
            );
            println!("           title: {}", issue.title);
            Ok(false)
        }
        DispatchMode::Execute => {
            let task = build_watch_task(issue, &branch, base_ref);
            spawn_managed(client, url, repo_url, base_ref, &branch, &task, runtime).await?;
            Ok(true)
        }
    }
}

/// POST the managed-session spawn request that drives the issue implementation.
///
/// Why: reuses the exact session-manager spawn path (`POST
/// /api/v1/sessions/managed`) that `tm ticket` / `tm session new` use, so
/// `tm watch` plugs into the existing isolated-workspace + runtime-adapter
/// machinery (the P4 workspace root `~/trusty-mpm-projects/<owner>/<repo>/…`)
/// rather than re-implementing it. The provisioner clones the repo and the driver
/// agent creates `branch` off `base_ref` per the task.
/// What: posts repo_url/ref/task/name_hint/runtime and prints the new session id,
/// state, runtime, and attach command.
/// Test: mirrors `tm ticket::spawn_managed`; HTTP path covered by the managed MVP
/// integration test.
#[allow(clippy::too_many_arguments)]
async fn spawn_managed(
    client: &reqwest::Client,
    url: &str,
    repo_url: &str,
    base_ref: &str,
    branch: &str,
    task: &str,
    runtime: RuntimeKind,
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
