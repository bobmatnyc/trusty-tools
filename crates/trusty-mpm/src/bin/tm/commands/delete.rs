//! `tm session delete <id> [--force]` — hard-delete a managed session RECORD (#2012).
//!
//! Why: extracted from `commands::managed` (which sits at the 500-SLOC
//! production cap) into its own file, mirroring how `commands::prune` already
//! separates the `prune-idle` verb from the rest of the managed-session CLI
//! handlers.
//! What: [`session_delete`] reads the record's persisted state, routes the
//! delete through the shared seam the `tm ls` picker uses (#7388 — an errored
//! session is stopped first), and renders the 404/409/stop-failed/success
//! outcomes for the terminal.
//! Test: HTTP path covered by `delete_route_*` in tests/session_manager_mvp.rs;
//! CLI parse by `cli_parses_session_delete`.

/// `tm session delete <id> [--force]` — hard-delete a managed session RECORD (#2012).
///
/// Why: distinct from `decommission` (stop runtime + maybe remove workspace +
/// tombstone) — this permanently drops the record from the store via the
/// existing tombstone-compaction primitive, for an operator who wants a
/// mis-provisioned or stale record gone outright rather than left as a
/// `Decommissioned` tombstone forever. Fail-closed: refuses a RUNNING session
/// unless `--force` is passed, printing an actionable message telling the
/// operator to stop it first (or force it).
/// What: delegates to [`super::picker_delete::delete_managed_then_local`], which
/// POSTs `/api/v1/sessions/managed/{id}/delete?force=<bool>` and, on a managed
/// 404, falls back to the project-session `DELETE /sessions/{id}` path — guarded
/// client-side by [`super::picker_delete::local_session_needs_force`], since that
/// legacy route has no server-side running guard of its own — so the verb
/// refuses a running session whether the id names a managed or a local session
/// (#2304). A not-found in BOTH stores prints "not found"; a running-session
/// refusal (the managed daemon's 409, or the local guard's own message) prints
/// the actionable reason and returns an `Err` so scripts see a non-zero exit;
/// success prints a confirmation naming the pre-deletion name and state.
///
/// #7388: the route is chosen from the record's persisted state
/// ([`super::picker_delete::managed_state_for_delete`]) through the same
/// [`super::picker_delete::route_delete_for_state`] seam the `tm ls` picker
/// uses, so an `errored` session is stopped and then deleted here too instead
/// of being refused with advice to go run `tm session stop`. A failed stop
/// issues no delete; a running (`active`/`provisioning`) session still needs
/// `--force`, since the delete goes out with the caller's own flag.
/// Test: HTTP path covered by `delete_route_*` in tests/session_manager_mvp.rs;
/// CLI parse by `cli_parses_session_delete`; the shared routing seam is
/// unit-tested via `classify_managed_delete_*`, and the verb's own route by
/// `verb_delete_stops_an_errored_session_before_deleting_it`,
/// `verb_delete_never_deletes_after_a_failed_stop`,
/// `verb_delete_issues_no_stop_when_the_id_is_not_a_managed_record` and
/// `picker_and_verb_route_each_state_identically` in
/// `tests_behavior_d_stop_delete_tests.rs`.
pub(crate) async fn session_delete(
    client: &reqwest::Client,
    url: &str,
    id: String,
    force: bool,
) -> anyhow::Result<()> {
    use super::picker_delete::DeleteReport;
    // #7388: learn the state before routing — without it every delete took the
    // plain branch and an errored session bounced off the daemon's 409.
    let state = super::picker_delete::managed_state_for_delete(client, url, &id).await?;
    match super::picker_delete::route_delete_for_state(client, url, &id, state.as_deref(), force)
        .await?
    {
        DeleteReport::Deleted {
            name,
            prior_state,
            local,
        } => {
            if local {
                // The legacy project-session store has no `--deleted--` state; a
                // local delete genuinely removes the record.
                println!("deleted {id} ({name}) [was {prior_state}] — removed from project store");
            } else {
                // Managed sessions are soft-deleted: marked `--deleted--`, still
                // listed. `tm sessions prune --state deleted` drops the tombstone.
                println!(
                    "deleted {id} ({name}) [was {prior_state}] — marked --deleted-- \
                     (still listed; `tm sessions prune --state deleted` to remove)"
                );
            }
            Ok(())
        }
        DeleteReport::NotFound => {
            println!("not found");
            Ok(())
        }
        DeleteReport::Refused(msg) => {
            eprintln!("error: {msg}");
            Err(anyhow::anyhow!("delete refused: {msg}"))
        }
        // #7388: reachable now that the verb takes the stop-first route for an
        // errored session. Nothing reached the delete endpoint, so the record is
        // exactly as it was — say so, and exit non-zero.
        DeleteReport::StopFailed(msg) => {
            eprintln!("error: {msg}");
            Err(anyhow::anyhow!("stop failed; {id} was not deleted: {msg}"))
        }
    }
}
