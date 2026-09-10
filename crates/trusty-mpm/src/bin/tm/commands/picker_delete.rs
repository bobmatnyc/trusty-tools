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
//! What: [`delete_needs_force`] is the MANAGED-family running-session guard
//! (persisted-state heuristic — the daemon does the real tmux probe);
//! [`local_session_needs_force`] is the analogous guard for the LOCAL
//! (project-session) family, enforced client-side because `DELETE
//! /sessions/{id}` has no server-side running guard of its own;
//! [`classify_managed_delete`] is the family-routing seam (managed 200/404/409
//! → keep / fall back to the project-session store / surface the guard
//! refusal); [`delete_managed_then_local`] is the shared I/O helper both the
//! picker and `tm session delete` call so the deletion endpoints are never
//! duplicated; [`confirm_and_delete`] is the picker's TTY confirm-then-delete
//! driver.
//!
//! IMPORTANT — reachability: the `tm ls` picker only ever lists MANAGED
//! sessions (`list_managed_sessions` is sourced solely from
//! `SessionManager::list()`), so [`delete_managed_then_local`]'s `FallbackLocal`
//! branch is effectively unreachable from the picker (barring a delete-after-list
//! race). The branch exists for the non-interactive `tm session delete
//! <free-text-id>` verb, which accepts ANY id/name and must behave correctly
//! when that id names a project-only (local) session instead of a managed one.
//!
//! Test: `delete_needs_force_*`, `local_session_needs_force_*`,
//! `classify_managed_delete_*`, `confirm_line_*`, and the real-HTTP
//! `local_delete_*` round-trip tests in `tests_behavior_d_tests.rs`. The
//! stdin-driven half of `confirm_and_delete` is inherently side-effect-only and
//! is exercised by manual smoke tests + the e2e suite.

use std::io::Write as _;

use trusty_mpm::client::ManagedSessionSummary;
use trusty_mpm::core::session::{Session, SessionStatus};

/// Outcome of rendering a routed delete — what the caller should print/return.
///
/// Why: [`delete_managed_then_local`] serves two call sites (the picker and the
/// non-interactive subcommand) that render differently and have different exit
/// semantics; returning a small report rather than printing inside the helper
/// keeps the deletion routing in one place while letting each surface own its UX.
/// What: `Deleted` carries the pre-deletion identity and whether it came from the
/// local (project-session) store; `NotFound` means neither store had the id;
/// `Refused` carries the running-guard message — either the daemon's managed
/// 409 body, or the CLI-side local-session guard's own message (there is no
/// server-side guard for the legacy `DELETE /sessions/{id}` route).
/// Test: constructed by `delete_managed_then_local`; rendering covered by the
/// callers' behaviour.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum DeleteReport {
    /// Delete succeeded. For a managed session (`local = false`) this is a
    /// SOFT delete — the record is marked `--deleted--` and kept in the store
    /// (#2012 marker); for a project session (`local = true`, the fallthrough to
    /// `DELETE /sessions/{id}`) the record is genuinely removed.
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
    /// The stop-first leg failed, so NO delete was attempted (#7224).
    ///
    /// Carries the daemon's full status + body. Distinct from [`Self::Refused`]
    /// because nothing reached the delete endpoint at all: the session is still
    /// exactly as it was, and the operator has to deal with the stop failure
    /// before a delete can mean anything.
    StopFailed(String),
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

/// Does deleting a session in state `state` need a runtime-stop first (#7224)?
///
/// Why: an `errored` record can still have a LIVE tmux session behind it —
/// provisioning failed after the session was created, or the runtime died in a
/// way that left the pane up. [`delete_needs_force`] answers `false` for it (no
/// force word needed), the delete then goes out with `force = false`, and the
/// daemon's own tmux-liveness probe 409s. That refusal is correct and the
/// operator's only recourse used to be dropping to `tm session stop` in a
/// shell — a bounce out of the surface they were already in. Doing the stop
/// leg here is the automation of exactly that step, and nothing more: the
/// delete that follows still goes out UNFORCED, so the daemon's probe stays the
/// authority on whether the record may go.
/// What: `true` only for `errored`. A `stopped` record has no runtime to stop,
/// and `active`/`provisioning` take the force-confirm path instead.
/// Test: `delete_needs_stop_first_only_for_errored` in
/// `tests_behavior_d_stop_delete_tests.rs`.
pub(crate) fn delete_needs_stop_first(state: &str) -> bool {
    state == "errored"
}

/// What the stop-first leg's HTTP status means for the delete that follows.
///
/// Why: the fail-open risk in a stop-then-delete sequence is downgrading a
/// failed stop to "close enough" and deleting anyway. Making the status→meaning
/// mapping a pure enum is what keeps that carve-out keyed on the daemon's
/// EXPLICIT answer rather than on "an error happened".
/// What: `Stopped` = 2xx (the runtime is down); `NothingToStop` = 404, the
/// daemon's answer for a record it has no live lifecycle for — no managed
/// record, or a terminal one (`runtime_stop_core` maps both to 404), neither of
/// which is a running session; `Failed` = every other status.
/// Test: `classify_stop_first_maps_each_status` in
/// `tests_behavior_d_stop_delete_tests.rs`.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub(crate) enum StopFirstNext {
    /// 2xx — the runtime is stopped; proceed to the delete.
    Stopped,
    /// 404 — the daemon has no live session to stop; proceed to the delete,
    /// which does its own not-found routing.
    NothingToStop,
    /// Anything else — report it and delete NOTHING.
    Failed,
}

/// Map the stop-first leg's HTTP status to [`StopFirstNext`].
///
/// Test: `classify_stop_first_maps_each_status` in
/// `tests_behavior_d_stop_delete_tests.rs`.
pub(crate) fn classify_stop_first(status: reqwest::StatusCode) -> StopFirstNext {
    match status {
        s if s.is_success() => StopFirstNext::Stopped,
        reqwest::StatusCode::NOT_FOUND => StopFirstNext::NothingToStop,
        _ => StopFirstNext::Failed,
    }
}

/// The `(force, stop_first)` pair a delete of a session in `state` uses (#7224).
///
/// Why: `tm ls` has two delete surfaces — the ratatui TUI and the numbered
/// picker it falls back to when the terminal has no raw mode — and they must
/// answer "what kind of delete is this?" identically. They did not: the TUI
/// learned the stop-first leg and the numbered picker kept issuing the bare
/// delete, so the same errored row was deleted in one surface and refused in
/// the other. One function is what makes that divergence unrepresentable.
/// What: `force` from [`delete_needs_force`] (running rows demand the word
/// `force`), `stop_first` from [`delete_needs_stop_first`] (an errored row's
/// runtime is stopped before the delete goes out).
/// Test: `delete_route_flags_match_the_two_guards` in
/// `tests_behavior_d_stop_delete_tests.rs`.
pub(crate) fn delete_route_flags(state: &str) -> (bool, bool) {
    (delete_needs_force(state), delete_needs_stop_first(state))
}

/// The question both delete surfaces ask before an ERRORED row's delete (#7224).
///
/// Why: confirming an errored row runs two legs — stop the runtime, then delete
/// the record — and an operator who agrees to that in one surface must be
/// agreeing to the same thing in the other. Holding the sentence in one place
/// is what keeps the numbered picker's prompt and the TUI overlay's from
/// drifting into describing different actions.
/// Test: `both_delete_surfaces_ask_the_same_errored_question` in
/// `tests_behavior_d_stop_delete_tests.rs`, and the overlay render in
/// `confirm_overlay_asks_the_shared_errored_question`.
pub(crate) fn errored_confirm_ask(name: &str) -> String {
    format!("'{name}' is errored. Stop it and delete it? Type y, then Enter.")
}

/// Issue a confirmed delete by the route `stop_first` selects (#7224).
///
/// Why: both delete surfaces had this two-branch choice written out inline, and
/// only one of them had both branches. Naming the route once removes the branch
/// a surface can forget to write.
/// What: `stop_first` sends the delete through [`stop_then_delete`]; otherwise
/// straight to [`delete_managed_then_local`]. `force` is threaded unchanged
/// either way — the stop leg never escalates it.
/// Test: the `picker_delete_*` and `stop_then_delete_*` round trips in
/// `tests_behavior_d_stop_delete_tests.rs`.
pub(crate) async fn route_delete(
    client: &reqwest::Client,
    url: &str,
    id: &str,
    force: bool,
    stop_first: bool,
) -> anyhow::Result<DeleteReport> {
    if stop_first {
        stop_then_delete(client, url, id, force).await
    } else {
        delete_managed_then_local(client, url, id, force).await
    }
}

/// Map a managed-delete HTTP status to the next routing step.
///
/// Why: pure seam for the family routing so 200/404/409/other are testable
/// without HTTP. A 404 specifically means the id is NOT a managed record. The
/// `tm ls` picker never surfaces this branch in practice (every entry it offers
/// IS a managed record — see the module doc); it exists for the non-interactive
/// `tm session delete <id>`, which accepts free-text ids that may name a
/// project-only (local) session instead, so 404 routes to the local fallback
/// rather than reporting failure.
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

/// Decide whether hard-deleting a LOCAL (project) session requires `--force`.
///
/// Why: `DELETE /sessions/{id}` (`remove_session` in `daemon/api.rs`)
/// unconditionally kills the tmux host for ANY status — unlike the managed
/// family, there is NO server-side running guard on this route (this was the
/// CRITICAL gap: a bare `tm session delete <local-id>` used to 404 harmlessly
/// before this module existed, and after this module's first cut it silently
/// force-killed a live local session). The CLI must therefore enforce the same
/// fail-closed contract client-side, by probing the session's status before
/// ever issuing the DELETE.
/// What: returns `true` for every [`SessionStatus`] except [`SessionStatus::Stopped`]
/// — i.e. `Starting`/`Active`/`AwaitingApproval`/`Detached`/`Paused` all mean a
/// live process still backs the session and require an explicit `--force`
/// (or the picker's force-confirm) to delete.
/// Test: `local_session_needs_force_stopped_false`,
/// `local_session_needs_force_active_and_others_true` in
/// `tests_behavior_d_tests.rs`.
pub(crate) fn local_session_needs_force(status: SessionStatus) -> bool {
    !matches!(status, SessionStatus::Stopped)
}

/// Try the managed hard-delete, falling back to the project-session delete on 404.
///
/// Why: the picker and `tm session delete` must route a delete to the correct
/// store WITHOUT re-implementing either deletion endpoint. Both call this one
/// helper: it POSTs the managed hard-delete first (every `tm ls` picker entry IS
/// a managed record, so this is the common — in practice only — path there) and,
/// only when the record is absent there (404 — reached in practice by the
/// non-interactive `tm session delete <free-text-id>` when the id names a
/// project-only session), issues the guarded local delete.
/// What: POSTs `/api/v1/sessions/managed/{id}/delete?force=<force>`, classifies
/// the status via [`classify_managed_delete`], and returns a [`DeleteReport`]. A
/// 409 returns `Refused` (never auto-escalates to force); a 200 parses the
/// flattened pre-deletion summary; a 404 delegates to [`delete_local`] (which
/// applies its OWN running guard) with the same `force` flag threaded through —
/// so a picker force-confirm (`force = true`) still bypasses the local guard,
/// while a plain non-interactive call without `--force` does not. Non-success
/// statuses on the managed leg propagate as an `Err`.
/// Test: side-effect-only HTTP driver — the status→route decision is unit-tested
/// via [`classify_managed_delete`]; the local guard's real-HTTP round trip is
/// covered by `local_delete_*` in `tests_behavior_d_tests.rs`.
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
        ManagedDeleteNext::FallbackLocal => delete_local(client, url, id, force).await,
        ManagedDeleteNext::Error => {
            // `resp` is guaranteed non-2xx here (every 2xx/404/409 status is
            // handled by the arms above), so build the error directly instead
            // of relying on `error_for_status()` — using it here would make the
            // success path that follows unreachable dead code.
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("managed delete failed with HTTP {status}: {body}")
        }
    }
}

/// Stop a session's runtime, then delete its record — one operator action (#7224).
///
/// Why: the `tm ls` surface must not answer a delete with instructions for a
/// different surface. An `errored` record whose tmux session is still up gets
/// refused by the daemon's liveness probe, and the refusal's own advice is
/// "stop it first" — a step the caller can perform. This performs it.
/// What: POSTs `/api/v1/sessions/managed/{id}/runtime-stop`, classifies the
/// status through [`classify_stop_first`], and only then calls
/// [`delete_managed_then_local`] with the SAME `force` flag the caller passed —
/// never an escalated one, so the daemon's tmux probe still decides whether the
/// record may go. A `Failed` status returns [`DeleteReport::StopFailed`] with
/// the status and body, and NO delete request is issued: there is no branch in
/// which a stop the daemon rejected is downgraded into a delete. A transport
/// error on the stop leg propagates as `Err` for the same reason.
/// Test: `stop_then_delete_deletes_after_a_successful_stop`,
/// `stop_then_delete_treats_not_found_as_nothing_to_stop`,
/// `stop_then_delete_never_deletes_after_a_failed_stop` in
/// `tests_behavior_d_stop_delete_tests.rs`.
pub(crate) async fn stop_then_delete(
    client: &reqwest::Client,
    url: &str,
    id: &str,
    force: bool,
) -> anyhow::Result<DeleteReport> {
    let resp = client
        .post(format!("{url}/api/v1/sessions/managed/{id}/runtime-stop"))
        .send()
        .await?;
    let status = resp.status();
    if classify_stop_first(status) == StopFirstNext::Failed {
        let body = resp.text().await.unwrap_or_default();
        return Ok(DeleteReport::StopFailed(format!(
            "stop returned HTTP {status}: {}",
            body.trim()
        )));
    }
    delete_managed_then_local(client, url, id, force).await
}

/// Delete a project-session record via `DELETE /sessions/{id}` (the local path),
/// refusing a still-running session unless `force` is set (#2304 CRITICAL fix).
///
/// Why: a session the managed store does not know is a project-session record —
/// reached in practice only via the non-interactive `tm session delete
/// <free-text-id>` (see the module doc: the picker never lists local sessions).
/// `DELETE /sessions/{id}` itself has no server-side running guard (unlike the
/// managed family's 409), so this function is the ONLY place that stands between
/// an operator and force-killing a live local session by accident — it must
/// check liveness BEFORE issuing the DELETE, not rely on the daemon to refuse.
/// What: GETs `/sessions/{id}` first (a 404 there is `NotFound` — nothing to
/// delete, no guard to apply). When [`local_session_needs_force`] finds the
/// fetched status is not `Stopped` and `force` is `false`, returns `Refused`
/// WITHOUT ever issuing the DELETE. Otherwise (force is `true`, or the session
/// is already `Stopped`) issues `DELETE /sessions/{id}` — the same full-removal
/// path `tm session stop` uses for a local session, reused rather than
/// duplicated — and reports `Deleted { local: true }` (name falls back to the
/// id; the local endpoint returns no body to read a display name from).
/// Test: `local_delete_refuses_running_session_without_force`,
/// `local_delete_allows_stopped_session_without_force`,
/// `local_delete_force_bypasses_guard_on_running_session` (real-HTTP round trip
/// against a loopback daemon) in `tests_behavior_d_tests.rs`.
async fn delete_local(
    client: &reqwest::Client,
    url: &str,
    id: &str,
    force: bool,
) -> anyhow::Result<DeleteReport> {
    let get_resp = client.get(format!("{url}/sessions/{id}")).send().await?;
    if get_resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(DeleteReport::NotFound);
    }
    let session: Session = get_resp.error_for_status()?.json().await?;
    let prior_state = format!("{:?}", session.status).to_lowercase();
    if !force && local_session_needs_force(session.status) {
        return Ok(DeleteReport::Refused(format!(
            "session is {prior_state} — stop it first with `tm session stop {id}`, \
             or pass --force to delete anyway"
        )));
    }

    let resp = client.delete(format!("{url}/sessions/{id}")).send().await?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(DeleteReport::NotFound);
    }
    resp.error_for_status()?;
    Ok(DeleteReport::Deleted {
        name: id.to_string(),
        prior_state,
        local: true,
    })
}

/// Picker action: confirm, then delete the selected session (#2304).
///
/// Why: the interactive delete must be explicit and, for a running session,
/// force-confirmed — never a silent destructive default. This is the TTY driver
/// that renders the confirm prompt and calls the shared routing helper.
/// What: computes the route with [`delete_route_flags`]; for a running session
/// it prints a force warning and requires the operator to type `force`
/// ([`confirm_is_force`]); an errored session is asked the shared
/// [`errored_confirm_ask`] question, which names the stop leg as well; every
/// other state is asked plainly. All three then require the same `y`/`yes`
/// ([`confirm_is_yes`]) except the force case. On confirm it hands off to
/// [`delete_confirmed`], which routes and renders. Returns `Ok(true)` only when
/// a record was actually removed (so the caller knows the list changed); a
/// cancel, an EOF, a 409 refusal, or a not-found all return `Ok(false)`.
/// Test: stdin/HTTP path is side-effect-only (manual smoke + e2e); the pure
/// confirm/route/guard seams it composes are unit-tested (see module doc).
pub(crate) async fn confirm_and_delete(
    client: &reqwest::Client,
    url: &str,
    session: &ManagedSessionSummary,
) -> anyhow::Result<bool> {
    let (force, stop_first) = delete_route_flags(&session.state);
    if force {
        eprintln!(
            "tm: '{}' is {} (running) — deleting will FORCE-remove the record.",
            session.name, session.state
        );
        eprint!("tm: type 'force' to confirm, or anything else to cancel > ");
    } else if stop_first {
        // #7224: an errored row's confirm covers BOTH legs, so say so before
        // the operator agrees rather than after.
        eprintln!("tm: {}", errored_confirm_ask(&session.name));
        eprint!("tm: [y/N] > ");
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

    delete_confirmed(client, url, session).await
}

/// Route an already-confirmed delete and report what the daemon did (#7224).
///
/// Why: the confirm half of [`confirm_and_delete`] reads stdin, so while the
/// routing lived inside it the numbered picker's delete could not be proven
/// against a stub daemon the way the TUI's can. Splitting the two is what let
/// the missing stop-first leg be caught: the ROUTING is now decidable without a
/// terminal.
/// What: recomputes the delete's route from `session.state`, issues it, and
/// prints the [`DeleteReport`]. Returns `Ok(true)` only when a record was
/// actually removed, so the caller knows the list changed.
/// Test: `picker_delete_stops_an_errored_session_before_deleting_it`,
/// `picker_delete_treats_a_not_found_stop_as_nothing_to_stop`,
/// `picker_delete_never_deletes_after_a_failed_stop`,
/// `picker_delete_issues_no_stop_for_a_non_errored_row` in
/// `tests_behavior_d_stop_delete_tests.rs`.
pub(crate) async fn delete_confirmed(
    client: &reqwest::Client,
    url: &str,
    session: &ManagedSessionSummary,
) -> anyhow::Result<bool> {
    let (force, stop_first) = delete_route_flags(&session.state);
    match route_delete(client, url, &session.id, force, stop_first).await? {
        DeleteReport::Deleted {
            name,
            prior_state,
            local,
        } => {
            if local {
                eprintln!(
                    "tm: deleted '{name}' [was {prior_state}] — removed from the project store."
                );
            } else {
                // Managed sessions are soft-deleted (#2012 marker): marked
                // `--deleted--` and kept in the list, not dropped.
                eprintln!(
                    "tm: '{name}' [was {prior_state}] marked --deleted-- \
                     (still listed; `tm sessions prune --state deleted` to remove)."
                );
            }
            Ok(true)
        }
        DeleteReport::Refused(msg) => {
            eprintln!("tm: delete refused: {msg}");
            Ok(false)
        }
        DeleteReport::StopFailed(msg) => {
            eprintln!(
                "tm: stop failed — '{}' was NOT deleted: {msg}",
                session.name
            );
            Ok(false)
        }
        DeleteReport::NotFound => {
            eprintln!("tm: '{}' not found — already gone.", session.name);
            Ok(false)
        }
    }
}
