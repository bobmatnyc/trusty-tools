//! Interactive-picker delete action + shared managed→local delete routing (#2304).
//!
//! Why: Bob's report — "I still don't see a way to delete sessions from the CLI".
//! A non-interactive `tm session delete <id>` exists (#2012) but it is
//! managed-store-only (404s on a project-only session) and undiscoverable
//! (requires knowing a UUID). The `tm ls` picker is the discoverable surface, so
//! deletion belongs there. This module holds the pure decision seams (family
//! routing + running-session force guard) plus the small I/O driver that runs the
//! confirm prompt — kept out of `session_picker.rs` so that file stays under the
//! 500-SLOC production cap.
//!
//! What: [`delete_needs_force`] is the running-session guard (persisted-state
//! heuristic — the daemon does the real tmux probe); [`classify_managed_delete`]
//! is the family-routing seam (managed 200/404/409 → keep / fall back to the
//! project-session store / surface the guard refusal); [`delete_managed_then_local`]
//! is the shared I/O helper both the picker and `tm session delete` call so the
//! deletion endpoints are never duplicated; [`confirm_and_delete`] is the picker's
//! TTY confirm-then-delete driver.
//!
//! Test: `delete_needs_force_*`, `classify_managed_delete_*`, and
//! `confirm_line_*` in `tests_behavior_d_tests.rs`. The stdin/HTTP I/O path
//! (`confirm_and_delete`, `delete_managed_then_local`) is inherently
//! side-effect-only and is exercised by manual smoke tests + the e2e suite.

use std::io::Write as _;

use trusty_mpm::client::ManagedSessionSummary;

/// Outcome of rendering a routed delete — what the caller should print/return.
///
/// Why: [`delete_managed_then_local`] serves two call sites (the picker and the
/// non-interactive subcommand) that render differently and have different exit
/// semantics; returning a small report rather than printing inside the helper
/// keeps the deletion routing in one place while letting each surface own its UX.
/// What: `Deleted` carries the pre-deletion identity and whether it came from the
/// local (project-session) store; `NotFound` means neither store had the id;
/// `Refused` carries the daemon's running-guard message (409).
/// Test: constructed by `delete_managed_then_local`; rendering covered by the
/// callers' behaviour.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum DeleteReport {
    /// Record removed. `local` is true when it fell through to the project-session
    /// `DELETE /sessions/{id}` path (not in the managed store).
    Deleted {
        /// Best-effort display name (falls back to the id).
        name: String,
        /// Prior lifecycle state, or `?` when the local path did not report one.
        prior_state: String,
        /// True when removed via the project-session store fallback.
        local: bool,
    },
    /// The id was in neither the managed store nor the project-session store.
    NotFound,
    /// The managed running-guard (409) refused the delete; carries its message.
    Refused(String),
}

/// Next step after the managed-delete attempt, keyed on the HTTP status.
///
/// Why: folding the status→action mapping into one pure enum makes the
/// family-routing decision exhaustively unit-testable without a live daemon.
/// What: `Deleted` = 200 (record removed from the managed store); `FallbackLocal`
/// = 404 (not a managed-family session — try the project-session store);
/// `Refused` = 409 (the running-session guard fired); `Error` = any other status.
/// Test: `classify_managed_delete_*` in `tests_behavior_d_tests.rs`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ManagedDeleteNext {
    /// 200 — the managed record was deleted.
    Deleted,
    /// 404 — not in the managed store; route to the local project-session delete.
    FallbackLocal,
    /// 409 — running-session guard refused; surface the message, do NOT fall back.
    Refused,
    /// Any other status — propagate as an error.
    Error,
}

/// Decide whether hard-deleting a session in state `state` requires `--force`.
///
/// Why: the picker must never silently hard-delete a live session (#2304 item 3).
/// A `stopped`/`errored` session has no live runtime, so a plain confirmation is
/// enough; anything else (`active`/`provisioning`) is treated as running and
/// requires an explicit force-confirm, mirroring `delete_record`'s fail-closed
/// guard. This is the persisted-state heuristic that decides which prompt to show;
/// the daemon still runs the authoritative tmux-liveness probe and can 409 even
/// when this returns false, which the routing surfaces rather than auto-forcing.
/// What: returns `true` for every state except `stopped`/`errored` (via
/// [`super::guided_resume::needs_restart`], the same stopped/errored classifier
/// the resume path uses).
/// Test: `delete_needs_force_running_true`, `delete_needs_force_stopped_false`,
/// `delete_needs_force_errored_false` in `tests_behavior_d_tests.rs`.
pub(crate) fn delete_needs_force(state: &str) -> bool {
    !super::guided_resume::needs_restart(state)
}

/// Map a managed-delete HTTP status to the next routing step.
///
/// Why: pure seam for the family routing so 200/404/409/other are testable
/// without HTTP. A 404 specifically means the selected entry is NOT a managed
/// record — the picker can still list a project-only session, which is deleted
/// through the project-session store — so 404 routes to the local fallback rather
/// than reporting failure.
/// What: 200→`Deleted`, 404→`FallbackLocal`, 409→`Refused`, else→`Error`.
/// Test: `classify_managed_delete_ok`, `classify_managed_delete_not_found`,
/// `classify_managed_delete_conflict`, `classify_managed_delete_other` in
/// `tests_behavior_d_tests.rs`.
pub(crate) fn classify_managed_delete(status: reqwest::StatusCode) -> ManagedDeleteNext {
    match status {
        reqwest::StatusCode::OK => ManagedDeleteNext::Deleted,
        reqwest::StatusCode::NOT_FOUND => ManagedDeleteNext::FallbackLocal,
        reqwest::StatusCode::CONFLICT => ManagedDeleteNext::Refused,
        _ => ManagedDeleteNext::Error,
    }
}

/// Interpret a confirmation line for a non-force (safe) delete.
///
/// Why: a single classifier keeps the accepted spellings identical across the
/// picker's prompts and any future call site, and makes the accept/reject rule
/// unit-testable without stdin.
/// What: returns `true` for `y`/`yes` (case-insensitive, trimmed); everything
/// else (including empty — the safe default) is `false`.
/// Test: `confirm_line_yes_variants`, `confirm_line_default_rejects` in
/// `tests_behavior_d_tests.rs`.
pub(crate) fn confirm_is_yes(line: &str) -> bool {
    let t = line.trim();
    t.eq_ignore_ascii_case("y") || t.eq_ignore_ascii_case("yes")
}

/// Interpret a confirmation line for a force (running-session) delete.
///
/// Why: deleting a running session is destructive, so a bare `y` must not be
/// enough — the operator types the word `force` explicitly, mirroring the
/// `--force` flag on the non-interactive verb (#2304 item 3).
/// What: returns `true` only for the exact word `force` (case-insensitive,
/// trimmed); everything else is `false`.
/// Test: `confirm_line_force_accepts`, `confirm_line_force_rejects_yes` in
/// `tests_behavior_d_tests.rs`.
pub(crate) fn confirm_is_force(line: &str) -> bool {
    line.trim().eq_ignore_ascii_case("force")
}

/// Try the managed hard-delete, falling back to the project-session delete on 404.
///
/// Why: the picker and `tm session delete` must route a delete to the correct
/// store WITHOUT re-implementing either deletion endpoint. Both call this one
/// helper: it POSTs the managed hard-delete first (all `tm ls` entries come from
/// the managed store, so this is the common path) and, only when the record is
/// absent there (404), issues the project-session `DELETE /sessions/{id}` — the
/// exact path `tm session stop` uses for a local session.
/// What: POSTs `/api/v1/sessions/managed/{id}/delete?force=<force>`, classifies
/// the status via [`classify_managed_delete`], and returns a [`DeleteReport`]. A
/// 409 returns `Refused` (never auto-escalates to force); a 200 parses the
/// flattened pre-deletion summary; a 404 issues the local DELETE and reports
/// `Deleted { local: true }` or `NotFound`. Non-success statuses on either leg
/// propagate as an `Err`.
/// Test: side-effect-only HTTP driver — the status→route decision is unit-tested
/// via [`classify_managed_delete`]; the round-trip is covered by manual smoke
/// tests and the session-manager integration suite.
pub(crate) async fn delete_managed_then_local(
    client: &reqwest::Client,
    url: &str,
    id: &str,
    force: bool,
) -> anyhow::Result<DeleteReport> {
    let resp = client
        .post(format!("{url}/api/v1/sessions/managed/{id}/delete"))
        .query(&[("force", force.to_string())])
        .send()
        .await?;
    match classify_managed_delete(resp.status()) {
        ManagedDeleteNext::Deleted => {
            let body: serde_json::Value = resp.json().await?;
            let name = body
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or(id)
                .to_string();
            let prior_state = body
                .get("state")
                .and_then(|v| v.as_str())
                .unwrap_or("?")
                .to_string();
            Ok(DeleteReport::Deleted {
                name,
                prior_state,
                local: false,
            })
        }
        ManagedDeleteNext::Refused => {
            Ok(DeleteReport::Refused(resp.text().await.unwrap_or_default()))
        }
        ManagedDeleteNext::FallbackLocal => delete_local(client, url, id).await,
        ManagedDeleteNext::Error => {
            // Surface a real daemon 5xx rather than masking it as "not found".
            resp.error_for_status()?;
            Ok(DeleteReport::NotFound)
        }
    }
}

/// Delete a project-session record via `DELETE /sessions/{id}` (the local path).
///
/// Why: a session the managed store does not know is a project-session record;
/// this is the same full-removal path `tm session stop` uses for a local session,
/// reused rather than duplicated.
/// What: issues the DELETE, maps 404→`NotFound`, propagates other non-success
/// statuses, and reports `Deleted { local: true }` on success (the local endpoint
/// returns no body, so name falls back to the id and prior state is `?`).
/// Test: side-effect-only HTTP; reached only from the 404 branch above.
async fn delete_local(
    client: &reqwest::Client,
    url: &str,
    id: &str,
) -> anyhow::Result<DeleteReport> {
    let resp = client.delete(format!("{url}/sessions/{id}")).send().await?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(DeleteReport::NotFound);
    }
    resp.error_for_status()?;
    Ok(DeleteReport::Deleted {
        name: id.to_string(),
        prior_state: "?".to_string(),
        local: true,
    })
}

/// Picker action: confirm, then delete the selected session (#2304).
///
/// Why: the interactive delete must be explicit and, for a running session,
/// force-confirmed — never a silent destructive default. This is the TTY driver
/// that renders the confirm prompt and calls the shared routing helper.
/// What: computes [`delete_needs_force`] from the session state; for a running
/// session it prints a force warning and requires the operator to type `force`
/// ([`confirm_is_force`]); otherwise it requires `y`/`yes` ([`confirm_is_yes`]).
/// On confirm it calls [`delete_managed_then_local`] with the matching `force`
/// flag and renders the [`DeleteReport`]. Returns `Ok(true)` only when a record
/// was actually removed (so the caller knows the list changed); a cancel, an EOF,
/// a 409 refusal, or a not-found all return `Ok(false)`.
/// Test: stdin/HTTP path is side-effect-only (manual smoke + e2e); the pure
/// confirm/route/guard seams it composes are unit-tested (see module doc).
pub(crate) async fn confirm_and_delete(
    client: &reqwest::Client,
    url: &str,
    session: &ManagedSessionSummary,
) -> anyhow::Result<bool> {
    let force = delete_needs_force(&session.state);
    if force {
        eprintln!(
            "tm: '{}' is {} (running) — deleting will FORCE-remove the record.",
            session.name, session.state
        );
        eprint!("tm: type 'force' to confirm, or anything else to cancel > ");
    } else {
        eprintln!(
            "tm: delete '{}' ({})? this permanently removes the session record.",
            session.name, session.state
        );
        eprint!("tm: [y/N] > ");
    }
    // Flush the prompt: it goes to stderr (unbuffered on most platforms) but be
    // explicit so the operator always sees it before the read blocks.
    let _ = std::io::stderr().flush();

    let mut line = String::new();
    let n = std::io::stdin().read_line(&mut line)?;
    if n == 0 {
        // EOF (Ctrl-D) — treat as cancel, never as confirm.
        eprintln!("tm: cancelled.");
        return Ok(false);
    }
    let confirmed = if force {
        confirm_is_force(&line)
    } else {
        confirm_is_yes(&line)
    };
    if !confirmed {
        eprintln!("tm: cancelled — '{}' was not deleted.", session.name);
        return Ok(false);
    }

    match delete_managed_then_local(client, url, &session.id, force).await? {
        DeleteReport::Deleted {
            name,
            prior_state,
            local,
        } => {
            let store = if local {
                "project store"
            } else {
                "managed store"
            };
            eprintln!("tm: deleted '{name}' [was {prior_state}] — removed from the {store}.");
            Ok(true)
        }
        DeleteReport::Refused(msg) => {
            eprintln!("tm: delete refused: {msg}");
            Ok(false)
        }
        DeleteReport::NotFound => {
            eprintln!("tm: '{}' not found — already gone.", session.name);
            Ok(false)
        }
    }
}
