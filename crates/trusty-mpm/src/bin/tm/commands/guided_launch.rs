//! Managed-spawn POST + live provisioning progress for the guided `tm` on-ramp
//! (#1904, async rework #2605).
//!
//! Why: "launch new" in the guided default provisions a workspace (git clone /
//! `clone --bare` + worktree + agent/skill deploy) which, on a large repo,
//! outlasts the CLI's HTTP timeout — the original synchronous POST failed with
//! `operation timed out` even though the daemon kept working (#2605). This
//! module now drives the ASYNC spawn path: it POSTs `background: true`, gets a
//! job id back immediately, and POLLS `GET .../{id}/provision-status`,
//! upgrading a `trusty_progress` spinner with the live phase (and clone
//! percent) until the session is ready — then attaches. The connection is
//! never held open across the clone, so it cannot time out.
//! What: [`launch_new_session_and_attach`] (POST-background → poll → attach),
//! [`spawn_progress_message`] (initial spinner text), and
//! [`provisioning_message`] (the pure phase/detail → spinner-line formatter).
//! Test: `spawn_progress_message_first_run`, `spawn_progress_message_reuse`,
//! `provisioning_message_*` here; `launch_new_session_and_attach` is covered by
//! the `POST /api/v1/sessions/managed` + `provision-status` integration tests.

use std::time::{Duration, Instant};

use anyhow::Context as _;
use serde::Deserialize;

use super::first_run::needs_first_run_clone;

/// How long to wait between `provision-status` polls.
///
/// Why: fast enough that the spinner feels live, slow enough that a minutes-long
/// clone does not hammer the daemon with hundreds of requests.
const POLL_INTERVAL: Duration = Duration::from_millis(600);

/// Overall ceiling on how long we follow a background provision before giving
/// up on the CLI side (the daemon keeps working regardless).
///
/// Why: requirement #3 — the CLI must not wait forever, but the bound has to be
/// generous enough for a cold clone of a very large repo on a slow network.
/// Thirty minutes is far beyond any legitimate provision yet still finite; on
/// expiry the CLI prints an actionable message rather than hanging.
const PROVISION_DEADLINE: Duration = Duration::from_secs(30 * 60);

/// Ack shape for `POST /api/v1/sessions/managed` — covers BOTH the async `202`
/// (`{ id, state: "provisioning" }`) and a synchronous `201` (`{ id, name,
/// state, … }`) from an older daemon that ignores `background`.
///
/// Why: a local type avoids depending on the daemon's internal DTO from the
/// CLI binary crate; `id`/`name`/`state` are all we need to branch on.
/// Test: covered indirectly by `launch_new_session_and_attach`.
#[derive(Debug, Default, Deserialize)]
struct SpawnAck {
    #[serde(default)]
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    state: String,
}

/// Poll shape for `GET /api/v1/sessions/managed/{id}/provision-status`.
///
/// Why: mirrors `daemon::managed_routes::ProvisionStatusResponse` for the
/// fields the guided flow renders and branches on.
/// Test: covered indirectly by `launch_new_session_and_attach`; the render
/// formatting is covered by `provisioning_message_*`.
#[derive(Debug, Default, Deserialize)]
struct ProvisionStatus {
    #[serde(default)]
    state: String,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    detail: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

/// Build the spinner message shown while the managed-spawn POST is in flight
/// (issue #1904).
///
/// Why: a first-run clone triggers minutes of server-side work; the operator
/// needs a distinct message for that case versus the (much faster) worktree-
/// reuse launch, without duplicating the formatting at each call site.
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

/// Format the live spinner line from a poll snapshot's phase label + detail.
///
/// Why: keeps the "provisioning workspace: <phase> — <detail>" rendering a
/// single pure function so it is unit-testable and cannot drift between call
/// sites.
/// What: `"tm: provisioning workspace: <label>"`, appending `" — <detail>"`
/// when a fine-grained detail (e.g. clone percent) is present. Falls back to a
/// generic phrase when the daemon has not reported a label yet.
/// Test: `provisioning_message_label_only`, `provisioning_message_label_and_detail`,
/// `provisioning_message_no_label`.
fn provisioning_message(label: Option<&str>, detail: Option<&str>) -> String {
    match (label, detail) {
        (Some(l), Some(d)) => format!("tm: provisioning workspace: {l} — {d}…"),
        (Some(l), None) => format!("tm: provisioning workspace: {l}…"),
        (None, _) => "tm: provisioning workspace…".to_string(),
    }
}

/// POST a new managed session to the daemon (async) and attach once ready.
///
/// Why: "launch new" must use the daemon's protected managed-clone spawn path
/// (never write framework files into the live checkout, #1724) AND must not
/// hold one HTTP request open across a multi-minute clone (#2605). This POSTs
/// `background: true`, then polls `provision-status`, rendering live phase +
/// clone-percent progress, before attaching.
/// What: (1) shows a `trusty_progress` spinner (auto-hidden in non-TTY/plain
/// output); (2) POSTs `{ repo_url, ref: "HEAD", task: "", force_new: true,
/// background: true }`; (3) if the daemon answered synchronously (`201` with a
/// name — an older daemon ignoring `background`) attaches immediately; (4)
/// otherwise polls `GET .../{id}/provision-status` every [`POLL_INTERVAL`] up
/// to [`PROVISION_DEADLINE`], upgrading the spinner via [`provisioning_message`]
/// until `state` is `ready` (attach `name`) or `failed` (surface `error`); a
/// transient poll error is retried within the deadline. `repo_url` MUST be the
/// git working-tree root so the daemon sets `source_id` correctly.
/// Test: covered by the `POST /api/v1/sessions/managed` + `provision-status`
/// integration tests; the message formatting by `spawn_progress_message_*` and
/// `provisioning_message_*`.
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
            // #2450: the picker's EXPLICIT "launch new session" choice — force a
            // fresh session so the daemon's in-project reconnect pre-flight
            // (#1707) never adopts an unrelated live session for this project.
            "force_new": true,
            // #2605: provision asynchronously — the POST returns a job id
            // immediately and we poll for progress, so a large-repo clone can
            // never outlast the client's HTTP timeout.
            "background": true,
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
    let ack: SpawnAck = match resp.json().await {
        Ok(body) => body,
        Err(err) => {
            spinner.finish_and_clear();
            return Err(err).context("failed to parse managed spawn response");
        }
    };

    // Synchronous completion (older daemon that ignored `background`): the ack
    // already carries the final name/state — attach without polling.
    if ack.state != "provisioning" && !ack.name.is_empty() {
        spinner.finish_and_clear();
        if first_run.is_some() {
            eprintln!("tm: clone complete — workspace ready");
        }
        eprintln!("tm: session '{}' created ({})", ack.name, ack.state);
        return crate::commands::tmux_attach::tmux_attach(&ack.name);
    }

    if ack.id.is_empty() {
        spinner.finish_and_clear();
        anyhow::bail!("daemon accepted the spawn but returned no job id to poll");
    }

    // Async path: follow the background provision to completion.
    let name = match poll_until_ready(client, url, &ack.id, &spinner).await {
        Ok(name) => name,
        Err(err) => {
            spinner.finish_and_clear();
            return Err(err);
        }
    };
    spinner.finish_and_clear();

    if first_run.is_some() {
        eprintln!("tm: clone complete — workspace ready");
    }
    eprintln!("tm: session '{name}' created (active)");
    crate::commands::tmux_attach::tmux_attach(&name)
}

/// Poll `provision-status` until the background job is ready, rendering live
/// progress into `spinner`; returns the ready session's tmux name.
///
/// Why: the whole async fix — the CLI follows the daemon's background provision
/// via short, bounded poll requests instead of one long-held connection.
/// What: loops every [`POLL_INTERVAL`] until [`PROVISION_DEADLINE`]. On each
/// poll: `ready` → returns the `name` (falling back to `session_id`); `failed`
/// → returns an error carrying the daemon's `error`; `provisioning` → updates
/// the spinner via [`provisioning_message`]; a transient request/parse error is
/// tolerated and retried until the deadline. On deadline expiry returns an
/// actionable error (the session may still be provisioning server-side).
/// Test: covered by the `provision-status` integration tests.
async fn poll_until_ready(
    client: &reqwest::Client,
    url: &str,
    id: &str,
    spinner: &trusty_progress::ProgressHandle,
) -> anyhow::Result<String> {
    let status_url = format!("{url}/api/v1/sessions/managed/{id}/provision-status");
    let start = Instant::now();

    loop {
        if start.elapsed() > PROVISION_DEADLINE {
            anyhow::bail!(
                "provisioning did not finish within {} minutes; it may still be running on the \
                 daemon — check `tm sessions ls`",
                PROVISION_DEADLINE.as_secs() / 60
            );
        }

        match client.get(&status_url).send().await {
            Ok(resp) if resp.status().is_success() => {
                let status: ProvisionStatus = resp.json().await.unwrap_or_default();
                match status.state.as_str() {
                    "ready" => {
                        return status
                            .name
                            .or(status.session_id)
                            .filter(|s| !s.is_empty())
                            .context("daemon reported ready but returned no session name");
                    }
                    "failed" => {
                        anyhow::bail!(
                            "provisioning failed: {}",
                            status.error.as_deref().unwrap_or("unknown error")
                        );
                    }
                    _ => {
                        spinner.set_message(provisioning_message(
                            status.label.as_deref(),
                            status.detail.as_deref(),
                        ));
                    }
                }
            }
            // A transient poll failure (daemon briefly busy, mid-restart) is not
            // fatal — keep polling until the deadline.
            _ => {}
        }

        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spawn_progress_message_first_run() {
        let msg = spawn_progress_message(Some(&(
            "acme/widgets".to_string(),
            std::path::PathBuf::from("/tmp/ws"),
        )));
        assert!(msg.contains("first run for acme/widgets"));
        assert!(msg.contains("/tmp/ws"));
    }

    #[test]
    fn spawn_progress_message_reuse() {
        let msg = spawn_progress_message(None);
        assert_eq!(msg, "tm: launching new session…");
    }

    #[test]
    fn provisioning_message_label_only() {
        assert_eq!(
            provisioning_message(Some("Cloning repository"), None),
            "tm: provisioning workspace: Cloning repository…"
        );
    }

    #[test]
    fn provisioning_message_label_and_detail() {
        assert_eq!(
            provisioning_message(Some("Cloning repository"), Some("Receiving objects: 63%")),
            "tm: provisioning workspace: Cloning repository — Receiving objects: 63%…"
        );
    }

    #[test]
    fn provisioning_message_no_label() {
        assert_eq!(
            provisioning_message(None, Some("ignored")),
            "tm: provisioning workspace…"
        );
    }
}
