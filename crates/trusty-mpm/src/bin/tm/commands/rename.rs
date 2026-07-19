//! `tm sessions rename` — rename a managed session (from the list or in-session).
//!
//! Why: extracted from `commands::managed`/`commands::session` (both near the
//! 500-SLOC production cap) into its own file, mirroring how `commands::delete`
//! separates the `delete` verb. A managed session's identity name is its tmux
//! name, so a rename mutates the record AND the live tmux session — both owned
//! by the daemon, so this issues a daemon PATCH rather than touching state
//! directly.
//! What: [`session_rename`] resolves the target (an explicit id/name, or the
//! CURRENT session from `$TM_MANAGED_SESSION_ID` when only a new name is given),
//! PATCHes `/api/v1/sessions/managed/{id}` with `{ "name": … }`, and renders the
//! 400/404/409/success outcomes.
//! Test: CLI parse by `cli_parses_sessions_rename_*`; HTTP path by
//! `rename_route_*` in `tests/session_lifecycle.rs`.

/// Environment variable a managed session exports carrying its own managed id,
/// used to resolve "the session I'm currently in" for the in-session rename form.
const CURRENT_SESSION_ENV: &str = "TM_MANAGED_SESSION_ID";

/// `tm sessions rename <id-or-name> <new-name>` / `tm sessions rename <new-name>`.
///
/// Why: operators rename a session either by naming it explicitly from the
/// master list, or — from WITHIN a session — by giving just the new name, with
/// the current session resolved from `$TM_MANAGED_SESSION_ID` (the id every
/// managed session exports into its own tmux/runtime environment).
/// What: when `arg2` is `Some`, `arg1` is the target (id or friendly name,
/// resolved to a managed id via the shared resolver) and `arg2` is the new
/// name; when `arg2` is `None`, `arg1` is the new name and the target is the
/// current session read from `$TM_MANAGED_SESSION_ID` (an error if unset — the
/// operator is not inside a managed session). PATCHes
/// `/api/v1/sessions/managed/{id}` with `{ "name": <new> }`; a 404 bails, a 409
/// (name collision) or 400 (invalid name) prints the daemon's actionable
/// message and returns `Err` (non-zero exit), and success prints a confirmation.
/// Test: `cli_parses_sessions_rename_two_args`,
/// `cli_parses_sessions_rename_in_session`; HTTP path by `rename_route_*`.
pub(crate) async fn session_rename(
    client: &reqwest::Client,
    url: &str,
    arg1: String,
    arg2: Option<String>,
) -> anyhow::Result<()> {
    let (target, new_name) = match arg2 {
        Some(new_name) => (arg1, new_name),
        None => {
            // In-session form: resolve the current session from the env var the
            // managed launcher exports into every session.
            let current = std::env::var(CURRENT_SESSION_ENV).map_err(|_| {
                anyhow::anyhow!(
                    "not inside a managed session (${CURRENT_SESSION_ENV} is unset) — \
                     pass an explicit target: `tm sessions rename <id-or-name> <new-name>`"
                )
            })?;
            (current, arg1)
        }
    };

    // Resolve the target (id or friendly name) to a canonical managed id via the
    // same resolver `stop`/`resume` use, so a friendly name works everywhere.
    let id = crate::commands::managed_route::resolve_managed_match(client, url, &target)
        .await
        .ok_or_else(|| anyhow::anyhow!("managed session '{target}' not found"))?;

    let resp = client
        .patch(format!("{url}/api/v1/sessions/managed/{id}"))
        .json(&serde_json::json!({ "name": new_name }))
        .send()
        .await?;
    match resp.status() {
        reqwest::StatusCode::NOT_FOUND => {
            anyhow::bail!("managed session '{target}' not found");
        }
        reqwest::StatusCode::CONFLICT | reqwest::StatusCode::BAD_REQUEST => {
            // Name collision (409) or invalid name (400): surface the daemon's
            // actionable message on stderr and fail (non-zero exit).
            let msg = resp.text().await.unwrap_or_default();
            eprintln!("error: {msg}");
            Err(anyhow::anyhow!("rename rejected: {msg}"))
        }
        _ => {
            resp.error_for_status()?;
            println!("renamed {id} -> {new_name}");
            Ok(())
        }
    }
}
