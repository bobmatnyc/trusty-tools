//! Managed-spawn POST + progress spinner for the guided `tm` on-ramp (#1904).
//!
//! Why: split out of `guided.rs` (issue #610 SLOC cap) — this is the one
//! blocking network call in the guided flow, so it carries its own response
//! DTO, spinner-message formatter, and the POST-and-attach driver as a
//! self-contained unit.
//! What: [`launch_new_session_and_attach`] POSTs a new managed session and
//! attaches to it, showing a [`trusty_progress::ProgressHandle`] spinner for
//! the full duration of the blocking call. [`spawn_progress_message`] is the
//! pure helper that formats the spinner's initial message.
//! Test: `spawn_progress_message_first_run`, `spawn_progress_message_reuse` in
//! `tests_behavior_c_tests.rs`; `launch_new_session_and_attach` is covered by
//! the `POST /api/v1/sessions/managed` integration tests in
//! `tests/session_manager_mvp.rs`.

use anyhow::Context as _;
use serde::Deserialize;

use super::first_run::needs_first_run_clone;

/// Response shape for `POST /api/v1/sessions/managed` (subset we need).
///
/// Why: a local type avoids depending on the daemon's internal DTO from the
/// CLI binary crate; the fields we care about are `name` and `state`.
/// What: mirrors `daemon::managed_routes::SpawnResponse` for the two fields
/// the guided default uses.
/// Test: covered indirectly by `launch_new_session_and_attach`.
#[derive(Debug, Deserialize)]
struct SpawnManagedResponse {
    #[serde(default)]
    name: String,
    #[serde(default)]
    state: String,
}

/// Build the spinner message shown while the blocking managed-spawn POST is
/// in flight (issue #1904).
///
/// Why: a first-run clone triggers minutes of server-side work (git clone,
/// agent/skill deploy, MCP injection, tmux + runtime spawn) behind one opaque
/// HTTP call; the operator needs a distinct message for that case versus the
/// (much faster) worktree-reuse launch, without duplicating the formatting
/// logic at each call site.
/// What: returns the first-run "cloning into <path>…" message when `first_run`
/// is `Some`, else the generic "launching new session…" message.
/// Test: `spawn_progress_message_first_run`, `spawn_progress_message_reuse`.
pub(crate) fn spawn_progress_message(first_run: Option<&(String, std::path::PathBuf)>) -> String {
    match first_run {
        Some((proj, path)) => format!(
            "tm: first run for {proj} — cloning into {}, this may take a few minutes…",
            path.display()
        ),
        None => "tm: launching new session…".to_string(),
    }
}

/// POST a new managed session to the daemon and attach to it.
///
/// Why: "launch new" in the picker must use the daemon's protected managed-clone
/// spawn path — NEVER write framework files into the live checkout (#1724).
/// Issue #1904: the daemon does git clone, TASK.md write, home-trust preseed,
/// agent/skill deployment, CLAUDE.md/instructions composition, MCP injection,
/// tmux session creation, and runtime spawn — all inside this single blocking
/// POST, which can take minutes on a first-run clone. Previously the client
/// printed two static lines and then went silent for the entire duration; a
/// spinner (via the shared `trusty-progress` crate, matching the rest of the
/// trusty-* ecosystem's rustup/cargo-style progress display) now runs for the
/// full duration of the call so the operator always has visible feedback
/// instead of a silent hang.
/// What: POSTs `{"repo_url": repo_url, "ref": "HEAD", "task": ""}` to
/// `/api/v1/sessions/managed` while a [`trusty_progress::ProgressHandle`]
/// spinner runs (auto-hidden in non-TTY/plain contexts, so piped/CI output is
/// unaffected). `repo_url` MUST be the git working-tree root (not a
/// subdirectory), so the daemon finds `.git` and sets `source_id` correctly —
/// enabling future `list_project_sessions` to show this session. The spinner
/// is cleared on every exit path (success or error) before any further output
/// is printed. The daemon provisions a per-session worktree in the base clone
/// and returns the session name. Then attaches via
/// `crate::commands::tmux_attach::tmux_attach`.
/// Test: covered by the `POST /api/v1/sessions/managed` integration tests in
/// `tests/session_manager_mvp.rs`; the spinner-message formatting is covered
/// by `spawn_progress_message_*` above.
pub(crate) async fn launch_new_session_and_attach(
    client: &reqwest::Client,
    url: &str,
    repo_url: &str,
) -> anyhow::Result<()> {
    let first_run = needs_first_run_clone(repo_url);
    let progress_output = trusty_progress::Output::to_stderr();
    let spinner = trusty_progress::ProgressHandle::spinner(
        &progress_output,
        spawn_progress_message(first_run.as_ref()),
    );

    let send_result = client
        .post(format!("{url}/api/v1/sessions/managed"))
        .json(&serde_json::json!({
            "repo_url": repo_url,
            "ref": "HEAD",
            "task": "",
        }))
        .send()
        .await;
    let resp = match send_result {
        Ok(resp) => resp,
        Err(err) => {
            spinner.finish_and_clear();
            return Err(err).context("managed spawn POST failed");
        }
    };

    if !resp.status().is_success() {
        let status = resp.status();
        spinner.finish_and_clear();
        anyhow::bail!("daemon returned {status} for managed spawn");
    }
    let body: SpawnManagedResponse = match resp.json().await {
        Ok(body) => body,
        Err(err) => {
            spinner.finish_and_clear();
            return Err(err).context("failed to parse managed spawn response");
        }
    };
    if body.name.is_empty() {
        spinner.finish_and_clear();
        anyhow::bail!("daemon returned empty session name from managed spawn");
    }
    spinner.finish_and_clear();

    if first_run.is_some() {
        eprintln!("tm: clone complete — workspace ready");
    }
    eprintln!("tm: session '{}' created ({})", body.name, body.state);
    crate::commands::tmux_attach::tmux_attach(&body.name)
}
