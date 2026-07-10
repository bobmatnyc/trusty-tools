//! `tm session delete <id> [--force]` — hard-delete a managed session RECORD (#2012).
//!
//! Why: extracted from `commands::managed` (which sits at the 500-SLOC
//! production cap) into its own file, mirroring how `commands::prune` already
//! separates the `prune-idle` verb from the rest of the managed-session CLI
//! handlers.
//! What: [`session_delete`] POSTs the daemon's hard-delete endpoint and renders
//! the 404/409/success outcomes for the terminal.
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
/// Test: HTTP path covered by `delete_route_*` in tests/session_manager_mvp.rs;
/// CLI parse by `cli_parses_session_delete`; the shared routing seam is
/// unit-tested via `classify_managed_delete_*`.
pub(crate) async fn session_delete(
    client: &reqwest::Client,
    url: &str,
    id: String,
    force: bool,
) -> anyhow::Result<()> {
    use super::picker_delete::DeleteReport;
    match super::picker_delete::delete_managed_then_local(client, url, &id, force).await? {
        DeleteReport::Deleted {
            name, prior_state, ..
        } => {
            println!("deleted {id} ({name}) [was {prior_state}] — record removed from store");
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
    }
}
