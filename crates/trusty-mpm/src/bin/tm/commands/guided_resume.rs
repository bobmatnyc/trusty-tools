//! Guided-picker resume/restart flow (#1742, #2001).
//!
//! Why: resuming a managed session from the bare-`tm` picker must handle every
//! runtime state without ever exposing a raw tmux failure. A live session is
//! attached directly; a Stopped/Errored session is restarted through the daemon;
//! and a *zombie* — the daemon still marks it active/provisioning but its tmux
//! pane has vanished (e.g. after a reboot) — is now auto-reconciled: the CLI
//! stops it (resetting the record to Stopped) then restarts it via the same
//! path, so the operator does nothing (#2001). Extracted from `guided.rs` to keep
//! both files under the 500-SLOC production cap.
//!
//! What: [`plan_resume`] is the pure branch selector over [`is_zombie`] /
//! [`needs_restart`]; [`resume_guided_session`] is the I/O driver that dispatches
//! on it, delegating the two daemon round-trips to [`reconcile_zombie_stop`]
//! (POST `/runtime-stop`) and [`restart_via_daemon`] (POST `/resume`).
//!
//! Test: the pure seams (`needs_restart`, `is_zombie`, `plan_resume`) are
//! exhaustively unit-tested in `tests_behavior_c_tests.rs`; the HTTP paths are
//! exercised by the e2e suite and manual smoke tests.

use anyhow::Context as _;

use crate::formatters::banner::tmux_has_session;

/// Decide whether a guided resume must restart the session through the daemon.
///
/// Why: a stopped/errored managed session has no live tmux session; calling
/// `tmux attach-session` against it fails with "can't find session" (#1742).
/// The check is state-only: the daemon's `resume` endpoint only accepts
/// Stopped/Errored, so tmux liveness is NOT used here — see [`is_zombie`] for
/// the active-but-tmux-absent case.
/// What: returns `true` when `state` is `"stopped"` or `"errored"`. Returns
/// `false` for all other states (active, provisioning, decommissioned, …).
/// Test: `guided_resume_needs_restart_*` in `tests_behavior_c_tests.rs`.
pub(crate) fn needs_restart(state: &str) -> bool {
    matches!(state, "stopped" | "errored")
}

/// Detect a zombie session: daemon thinks it is active but the tmux session is gone.
///
/// Why: when a session's daemon state is NOT stopped/errored (i.e. active or
/// provisioning) but its tmux session has disappeared (e.g. after a machine
/// reboot before the daemon reconcile runs), the daemon's `resume` endpoint
/// would return 409 (can only resume Stopped/Errored). Rather than a dead end,
/// the CLI auto-reconciles: it stops the session (resetting the daemon record to
/// Stopped) then restarts it via the normal resume path — the operator does
/// nothing (#2001). Previously this was operator-driven (`tm session stop` then
/// `tm` again); that manual step is now automated.
/// What: returns `true` when `!needs_restart(state)` AND `!tmux_live`.
/// Test: `guided_resume_is_zombie_*` in `tests_behavior_c_tests.rs`.
pub(crate) fn is_zombie(state: &str, tmux_live: bool) -> bool {
    !tmux_live && !needs_restart(state)
}

/// The action a guided resume must take, derived purely from state + tmux liveness.
///
/// Why: extracting the branch selection from [`resume_guided_session`] into a
/// pure function makes the control flow — including the new zombie
/// auto-reconcile path (#2001) — unit-testable without a daemon, tmux, or HTTP.
/// The I/O driver simply matches on the returned variant.
/// What: four variants cover every case bare `tm` can hand a resume:
///   • [`ResumeAction::InPlace`] — bare `tm` is running INSIDE the pane of the
///     managed session named by `TM_MANAGED_SESSION_ID`, whose runtime already
///     exited (#2023 A/B); relaunch it in place via direct `exec`, never the
///     daemon kill+recreate path (#2023 C). Takes priority over everything
///     below — see [`try_inplace_relaunch`].
///   • [`ResumeAction::Attach`] — a live runtime; attach directly.
///   • [`ResumeAction::Restart`] — Stopped/Errored; POST `/resume` then attach.
///   • [`ResumeAction::ReconcileThenRestart`] — zombie (active/provisioning but
///     tmux gone); POST `/runtime-stop` to reset the record to Stopped, THEN
///     POST `/resume` then attach.
/// Test: `guided_resume_plan_*` in `tests_behavior_c_tests.rs`;
/// `plan_inplace_*` in this module.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ResumeAction {
    /// In-pane relaunch (#2023 C): see the variant doc above.
    InPlace,
    /// Runtime is live — attach directly, no daemon round-trip.
    Attach,
    /// Stopped/Errored — restart via the daemon `/resume` endpoint, then attach.
    Restart,
    /// Zombie — auto-stop to reset the record to Stopped, then restart + attach.
    ReconcileThenRestart,
}

/// Decide, purely from `state` and `tmux_live`, how a guided resume should proceed.
///
/// Why: single source of truth for the resume branch selection so the zombie
/// reconcile, the plain restart, and the happy-path attach can never diverge
/// from what [`is_zombie`]/[`needs_restart`] classify. Pure so it is exhaustively
/// unit-tested.
/// What: zombie (checked first, since a zombie's state also fails
/// `needs_restart`) → `ReconcileThenRestart`; Stopped/Errored → `Restart`;
/// everything else (active/provisioning with a live tmux) → `Attach`.
/// Test: `guided_resume_plan_*` in `tests_behavior_c_tests.rs`.
pub(crate) fn plan_resume(state: &str, tmux_live: bool) -> ResumeAction {
    if is_zombie(state, tmux_live) {
        ResumeAction::ReconcileThenRestart
    } else if needs_restart(state) {
        ResumeAction::Restart
    } else {
        ResumeAction::Attach
    }
}

/// Restart (if needed) then attach to a managed session from the guided picker.
///
/// Why: the guided picker must handle stopped sessions gracefully (#1742). A
/// direct `tmux attach-session` against a stopped session exits with failure
/// ("can't find session"). This function routes the request correctly based on
/// session state and tmux liveness, surfaces clear actionable messages for all
/// failure modes, and never exposes a raw tmux failure to the operator.
/// What: (1) `tmux_has_session` checks liveness and [`plan_resume`] selects the
/// branch; (2) for a zombie (active/provisioning but tmux gone) it AUTO-STOPS via
/// [`reconcile_zombie_stop`] — POST `/runtime-stop` resets the daemon record to
/// Stopped — then falls through into the same restart path, so the operator does
/// nothing (#2001); (3) for stopped/errored (or a just-reconciled zombie) it warns
/// about pane kill if the tmux pane is still live, then POSTs
/// `/api/v1/sessions/managed/{id}/resume` via [`restart_via_daemon`] with a
/// 30-second timeout; (4) 404/409/422/5xx/network errors each print a distinct
/// actionable message (the 422/5xx branches surface the daemon body verbatim so
/// a removed-workspace reason reaches the operator, #2577); (5) on HTTP 200, the
/// response body is checked — if
/// `state=errored` the runtime spawn failed and the operator is directed to
/// `tm session info`; (6) falls through to `tmux_attach` only when everything is
/// confirmed ready.
/// Test: `needs_restart`, `is_zombie`, and `plan_resume` are the testable pure
/// seams; the I/O path is exercised by the e2e suite and manual smoke tests.
pub(crate) async fn resume_guided_session(
    client: &reqwest::Client,
    url: &str,
    session: &trusty_mpm::client::ManagedSessionSummary,
) -> anyhow::Result<()> {
    let tmux_live = tmux_has_session(&session.name);
    let action = plan_resume(&session.state, tmux_live);

    // Zombie auto-reconcile (#2001): the daemon still marks the session
    // active/provisioning but its tmux pane is gone (e.g. after a reboot). The
    // daemon's /resume only accepts Stopped/Errored, so first reset the record to
    // Stopped via /runtime-stop, then fall through into the SAME restart path
    // below. Only bail if the stop itself fails — that is the one remaining case
    // that needs a human.
    if action == ResumeAction::ReconcileThenRestart {
        eprintln!(
            "tm: '{}' is marked {} but its tmux pane is gone — reconciling and restarting…",
            session.name, session.state
        );
        reconcile_zombie_stop(client, url, session).await?;
    }

    // Restart path — shared by the plain Restart branch and the reconciled zombie.
    if matches!(
        action,
        ResumeAction::Restart | ResumeAction::ReconcileThenRestart
    ) {
        // Pane-kill disclosure only applies to the plain Restart branch: a zombie
        // has no live pane (that is what made it a zombie), so skip the notice there.
        if action == ResumeAction::Restart {
            if tmux_live {
                eprintln!(
                    "tm: session '{}' is {} but its tmux pane is still alive — \
                     the daemon will KILL that pane and start a fresh runtime \
                     (in-progress pane state will be lost).",
                    session.name, session.state
                );
            } else {
                eprintln!(
                    "tm: session '{}' is {} (tmux absent); restarting via daemon…",
                    session.name, session.state
                );
            }
        }
        restart_via_daemon(client, url, session).await?;
    }
    tmux_attach(&session.name)
}

/// Auto-stop a zombie session so the daemon record resets to `Stopped` (#2001).
///
/// Why: the daemon's `/resume` only accepts Stopped/Errored; a zombie's record is
/// still active/provisioning, so it must be stopped first. `/runtime-stop` marks
/// the record `Stopped` synchronously (keeping the workspace), which then
/// satisfies the restart path. This is the automation of the old manual
/// `tm session stop <id>` step.
/// What: POSTs `/api/v1/sessions/managed/{id}/runtime-stop` with a 30-second
/// timeout. Network/404/other-error each print a distinct actionable message and
/// bail — a failed stop is the one case that still needs a human. Success returns
/// `Ok(())` and the caller proceeds to the restart path.
/// Test: I/O path exercised by the e2e suite; the branch selection is the pure
/// [`plan_resume`] seam.
async fn reconcile_zombie_stop(
    client: &reqwest::Client,
    url: &str,
    session: &trusty_mpm::client::ManagedSessionSummary,
) -> anyhow::Result<()> {
    let resp = match client
        .post(format!(
            "{url}/api/v1/sessions/managed/{}/runtime-stop",
            session.id
        ))
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!(
                "tm: daemon unreachable — cannot reconcile session '{}': {e}",
                session.name
            );
            eprintln!("tm: start the daemon with `tm start`, then run `tm` again.");
            anyhow::bail!("daemon unreachable; cannot reconcile zombie session: {e}");
        }
    };
    match resp.status() {
        reqwest::StatusCode::NOT_FOUND => {
            eprintln!(
                "tm: session '{}' not found on daemon; it may have been decommissioned.",
                session.name
            );
            eprintln!("tm: run `tm session ls` to see current sessions.");
            anyhow::bail!(
                "session '{}' not found on daemon during reconcile",
                session.name
            );
        }
        s if !s.is_success() => {
            eprintln!(
                "tm: daemon returned {s} stopping '{}' during reconcile — try `tm session ls`.",
                session.name
            );
            anyhow::bail!(
                "daemon error {s} reconciling zombie session '{}'",
                session.name
            );
        }
        _ => Ok(()),
    }
}

/// POST `/resume` for a Stopped/Errored (or just-reconciled) session and validate.
///
/// Why: the restart round-trip is shared by the plain stopped/errored path and
/// the zombie auto-reconcile path (#2001); a single helper keeps their error
/// handling identical. Extracting it also keeps [`resume_guided_session`] small.
/// What: POSTs `/api/v1/sessions/managed/{id}/resume` with a 30-second timeout so
/// a hung daemon cannot freeze the CLI. 404/409/422/5xx/other/network errors each
/// print a distinct actionable message and bail; the 422/5xx/other branches now
/// surface the daemon's response body verbatim (#2577) so an operator sees the
/// concrete reason (e.g. "workspace directory … no longer exists") instead of a
/// bare status code. On HTTP 200 the body is parsed
/// and, if `state=errored`, the async runtime spawn failed → bail directing the
/// operator to `tm session info`. Success prints "restarted — attaching…" and
/// returns `Ok(())`; the caller then attaches.
/// Test: I/O path exercised by the e2e suite; branch selection is [`plan_resume`].
async fn restart_via_daemon(
    client: &reqwest::Client,
    url: &str,
    session: &trusty_mpm::client::ManagedSessionSummary,
) -> anyhow::Result<()> {
    // POST with a 30-second timeout — a hung daemon must not freeze the CLI.
    let resp = match client
        .post(format!(
            "{url}/api/v1/sessions/managed/{}/resume",
            session.id
        ))
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!(
                "tm: daemon unreachable — cannot restart session '{}': {e}",
                session.name
            );
            eprintln!("tm: start the daemon with `tm start`, then run `tm` again.");
            anyhow::bail!("daemon unreachable; cannot restart stopped session: {e}");
        }
    };

    match resp.status() {
        reqwest::StatusCode::NOT_FOUND => {
            eprintln!(
                "tm: session '{}' not found on daemon; it may have been decommissioned.",
                session.name
            );
            eprintln!(
                "tm: run `tm session ls` to see current sessions, \
                 or press [Enter] to launch a new one."
            );
            anyhow::bail!("session '{}' not found on daemon", session.name);
        }
        reqwest::StatusCode::CONFLICT => {
            let msg = resp.text().await.unwrap_or_default();
            let detail = truncate_for_display(msg.trim());
            eprintln!("tm: cannot restart session '{}': {}", session.name, detail);
            eprintln!("tm: run `tm session ls` to see the current state.");
            anyhow::bail!("cannot restart session '{}': {detail}", session.name);
        }
        // 422 (#2577): the session cannot be resumed because of its on-disk state
        // — its workspace directory was removed, or its recorded pane vanished.
        // The daemon body carries the concrete, actionable reason (the vanished
        // path/pane); surface it verbatim rather than a bare status. Previously
        // these mapped to 500 and the operator saw only "daemon returned an
        // internal error (500)" with no clue it was a removed worktree.
        //
        // The `x-trusty-resume-reason` header (#2577 review) disambiguates WHICH
        // on-disk precondition fired so the printed remedy names a REAL, SAFE
        // `tm session` verb: reading the body text alone cannot reliably tell
        // "workspace gone" (safe to delete the record) from "sibling window
        // still alive" (decommission would kill that live window too) without
        // fragile substring matching — the exact anti-pattern the daemon-side
        // typed `ResumeManagedError` was introduced to eliminate for 404/409.
        reqwest::StatusCode::UNPROCESSABLE_ENTITY => {
            let reason = resp
                .headers()
                .get("x-trusty-resume-reason")
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned);
            let msg = resp.text().await.unwrap_or_default();
            let detail = truncate_for_display(msg.trim());
            eprintln!("tm: cannot restart session '{}': {}", session.name, detail);
            eprintln!(
                "{}",
                unresumable_remedy_line(&session.id, reason.as_deref())
            );
            anyhow::bail!("cannot restart session '{}': {detail}", session.name);
        }
        // 5xx means the daemon IS running but had a genuine internal error —
        // "start the daemon" is wrong advice; surface the body (if any) so the
        // operator has something concrete to act on, and direct them to inspect
        // state. Threading the body through (#2577) mirrors the #2485 precedent:
        // the daemon already puts a message in the 500 body — don't discard it.
        s if s.is_server_error() => {
            let msg = resp.text().await.unwrap_or_default();
            let detail = truncate_for_display(msg.trim());
            if detail.is_empty() {
                eprintln!(
                    "tm: daemon returned an internal error ({s}) restarting '{}' — \
                     try `tm session ls`.",
                    session.name
                );
            } else {
                eprintln!(
                    "tm: daemon returned an internal error ({s}) restarting '{}': {detail} — \
                     try `tm session ls`.",
                    session.name
                );
            }
            anyhow::bail!(
                "daemon internal error {s} restarting session '{}': {detail}",
                session.name
            );
        }
        s if !s.is_success() => {
            let msg = resp.text().await.unwrap_or_default();
            let detail = truncate_for_display(msg.trim());
            if detail.is_empty() {
                eprintln!(
                    "tm: daemon returned {s} restarting '{}' — try `tm session ls`.",
                    session.name
                );
            } else {
                eprintln!(
                    "tm: daemon returned {s} restarting '{}': {detail} — try `tm session ls`.",
                    session.name
                );
            }
            anyhow::bail!(
                "daemon error {s} restarting session '{}': {detail}",
                session.name
            );
        }
        _ => {
            // The daemon returned 200 but the runtime spawn may still have failed:
            // it creates the tmux session synchronously, then spawns claude/tcode
            // asynchronously and marks the session errored if that fails.
            // Deserialize the body and check the final state.
            let body: trusty_mpm::client::ManagedSessionSummary = resp
                .json()
                .await
                .context("failed to parse daemon resume response")?;
            if body.state == "errored" {
                eprintln!(
                    "tm: session '{}' restarted but the runtime failed to start.",
                    session.name
                );
                eprintln!("tm: check `tm session info {}` for details.", session.id);
                anyhow::bail!(
                    "session '{}' restarted but runtime failed to start (state=errored)",
                    session.name
                );
            }
            eprintln!("tm: session restarted — attaching…");
            Ok(())
        }
    }
}

/// Cap an echoed daemon error body at a sane display length.
///
/// Why: a daemon error body is normally a short, hand-written message, but
/// nothing on the wire enforces that — a future error path (or a proxy
/// mangling the response) could hand the CLI an unbounded string. Printing it
/// straight to the terminal without a cap risks flooding the operator's
/// scrollback for what should be a one-line diagnostic.
/// What: returns `s` unchanged when it fits within 2000 chars; otherwise
/// truncates to the first 2000 chars (on a `char` boundary) and appends a
/// `… (truncated)` marker.
/// Test: `truncate_for_display_leaves_short_bodies_unchanged`,
/// `truncate_for_display_caps_long_bodies` in `tests_behavior_e_tests.rs`.
pub(crate) fn truncate_for_display(s: &str) -> String {
    const MAX_CHARS: usize = 2000;
    if s.chars().count() <= MAX_CHARS {
        return s.to_string();
    }
    let truncated: String = s.chars().take(MAX_CHARS).collect();
    format!("{truncated}… (truncated)")
}

/// Build the operator-facing remedy line for a 422 "session not resumable"
/// response, selected by the daemon's `x-trusty-resume-reason` header.
///
/// Why (#2577 review): a prior draft pointed EVERY 422 at the same generic
/// "delete it" hint, including `tm session rm <id>` — a subcommand that DOES
/// NOT EXIST (`tm session` only defines `delete`/`decommission`; running it
/// fails with "unrecognized subcommand 'rm'"). Worse, that draft conflated
/// `WorkspaceMissing` (the workspace is genuinely gone; `delete --force` is
/// safe — store-only, never touches tmux) with `PaneGone` (the tmux SESSION is
/// still alive via a sibling window; `decommission` KILLS THE WHOLE SESSION,
/// destroying that live sibling too) under one framing. Steering an operator
/// through the pane-gone case toward teardown risked destroying work in the
/// surviving window. This function is a pure, table-driven seam so its LITERAL
/// text can be pinned against `Cli::try_parse_from` in a unit test — the exact
/// bug class that shipped the nonexistent `rm` verb.
/// What: `Some("pane_gone")` → an inspect-before-acting remedy naming
/// `tmux list-panes` / `tm session info` and warning that `decommission` kills
/// the whole session; `Some("workspace_missing")` → `tm session delete <id>
/// --force` (safe: store-only, and there is no workspace left to protect);
/// anything else (an unrecognized or absent header — e.g. an older daemon that
/// predates #2577) → a conservative pointer naming no destructive verb at all.
/// Test: `unresumable_remedy_line_cites_real_subcommands` in
/// `tests_behavior_e_tests.rs`.
pub(crate) fn unresumable_remedy_line(id: &str, reason: Option<&str>) -> String {
    match reason {
        Some("pane_gone") => format!(
            "tm: this session's tmux window is still alive (a sibling window is keeping it up) \
             but its recorded pane is gone — inspect with `tmux list-panes` or `tm session info {id}` \
             before acting; `tm session decommission {id}` kills the WHOLE tmux session, including \
             that sibling window, so verify first."
        ),
        Some("workspace_missing") => format!(
            "tm: its workspace is gone for good — `tm session delete {id} --force` removes the \
             stale record (store-only, never touches tmux), or run `tm session info {id}` to \
             review first."
        ),
        _ => format!(
            "tm: this session is not resumable as-is — run `tm session ls` or `tm session info {id}` \
             to review."
        ),
    }
}

/// Attach (or switch) the current terminal into tmux session `name`.
///
/// Why: resuming a session means handing the terminal over to tmux; when the
/// operator is already inside a tmux client, a plain `attach-session` is
/// refused by tmux (nesting guard), so the actual argv choice is delegated to
/// the shared [`crate::commands::tmux_attach::tmux_attach`] helper (#1873).
/// What: thin re-export so existing call sites don't need to change their import
/// path.
/// Test: `attach_argv_inside_tmux_uses_switch_client`,
/// `attach_argv_outside_tmux_uses_attach_session` in `tmux_attach.rs` cover
/// the argv decision; exercised indirectly here by the picker flow.
fn tmux_attach(name: &str) -> anyhow::Result<()> {
    crate::commands::tmux_attach::tmux_attach(name)
}
