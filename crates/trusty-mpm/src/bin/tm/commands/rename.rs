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
//! 400/404/success outcomes — printing the DAEMON-confirmed name from the
//! response body, which may be an auto-suffixed variant of the requested one
//! when it collided (#3692; see [`rename_success_message`]). The in-session form cross-checks the env var
//! against this process's own tmux pane before trusting it (issue #3600, see
//! [`confirm_rename_target_pane`]) — `$TM_MANAGED_SESSION_ID` is published
//! SESSION-scoped (`tmux set-environment -t <session>`), so a sibling pane can
//! inherit an identical value and would otherwise rename a session it does not
//! own.
//! Test: CLI parse by `cli_parses_sessions_rename_*`; HTTP path by
//! `rename_route_*` in `tests/session_lifecycle.rs`; the pane cross-check by
//! `rename_tests.rs`.

use crate::commands::pane_identity::EnvSessionIdentity;

/// Environment variable a managed session exports carrying its own managed id,
/// used to resolve "the session I'm currently in" for the in-session rename form.
const CURRENT_SESSION_ENV: &str = "TM_MANAGED_SESSION_ID";

/// Cross-check the in-session rename target's own `pane_id` against this
/// process's CURRENT tmux pane before trusting `session_id` (issue #3600).
///
/// Why: `TM_MANAGED_SESSION_ID` is tmux SESSION-scoped (`tmux
/// set-environment -t <session>`), not pane-scoped — a sibling pane opened
/// after a runtime-exit heal inherits the SAME env var and, without this
/// check, would rename a session it does not own — possibly another user's
/// live session. `guided.rs`'s nested-session guard already fixed the
/// identical hijack for its own call path; this reuses the shared
/// [`crate::commands::pane_identity::EnvSessionIdentity`] cross-check rather
/// than trusting the bare env var a second time.
/// What: `Ok(())` only when both pane ids are known and equal
/// ([`EnvSessionIdentity::Confirmed`]). `Err(<message>)` naming `session_id`
/// and both pane ids for a genuine mismatch, or explaining that identity
/// could not be verified at all (not inside tmux, a tmux query failure, or
/// `session_id` unknown to the daemon) — refuses in BOTH cases, since
/// silently renaming the wrong session is far worse than an error the
/// operator can retry with an explicit target.
/// Test: `confirm_rename_target_pane_ok_when_confirmed`,
/// `confirm_rename_target_pane_refuses_on_mismatch`,
/// `confirm_rename_target_pane_refuses_when_unavailable` (`rename_tests.rs`).
fn confirm_rename_target_pane(
    session_id: &str,
    current_pane_id: Option<&str>,
    record_pane_id: Option<&str>,
) -> Result<(), String> {
    match EnvSessionIdentity::evaluate(current_pane_id, record_pane_id) {
        EnvSessionIdentity::Confirmed => Ok(()),
        EnvSessionIdentity::Mismatch {
            current_pane_id,
            record_pane_id,
        } => Err(format!(
            "refusing to rename: ${CURRENT_SESSION_ENV}={session_id} does not belong to this \
             pane (this pane={current_pane_id}, session's own pane={record_pane_id}) — tmux's \
             session-scoped environment can leak a sibling pane's session id into a different \
             pane; pass an explicit target instead: \
             `tm sessions rename <id-or-name> <new-name>`"
        )),
        EnvSessionIdentity::Unavailable => Err(format!(
            "refusing to rename: could not verify this pane owns session {session_id} (not \
             inside tmux, the pane lookup failed, or the session is unknown to the daemon) — \
             pass an explicit target instead: `tm sessions rename <id-or-name> <new-name>`"
        )),
    }
}

/// Resolve the in-session rename target: fetch `session_id`'s own record and
/// cross-check its `pane_id` against `current_pane_id` (issue #3600).
///
/// Why: separates the daemon round trip from the pure decision in
/// [`confirm_rename_target_pane`] so each is independently testable — the
/// fetch via a mock daemon, the decision via plain unit tests. Returning the
/// resolved [`ManagedSessionSummary`] on success also lets [`session_rename`]
/// skip a second `resolve_managed_match` round trip for the id it already has.
/// What: `Ok(record)` only when the record is found AND
/// [`confirm_rename_target_pane`] confirms pane identity; `Err(<message>)`
/// otherwise (record not found collapses into the same "unavailable" refusal
/// as an unresolvable pane — both mean identity cannot be proven).
/// Test: `resolve_in_session_rename_target_confirmed`,
/// `resolve_in_session_rename_target_refuses_on_pane_mismatch`,
/// `resolve_in_session_rename_target_refuses_when_record_not_found`
/// (`rename_tests.rs`).
async fn resolve_in_session_rename_target(
    client: &reqwest::Client,
    url: &str,
    session_id: &str,
    current_pane_id: Option<&str>,
) -> Result<trusty_mpm::client::ManagedSessionSummary, String> {
    let record =
        crate::commands::managed_route::resolve_managed_summary(client, url, session_id).await;
    let record_pane_id = record.as_ref().and_then(|r| r.pane_id.as_deref());
    confirm_rename_target_pane(session_id, current_pane_id, record_pane_id)?;
    // `confirm_rename_target_pane` only returns `Ok` when `record_pane_id` was
    // `Some` (matched against a `Some` current_pane_id), which is only
    // possible when `record` itself is `Some` — this `expect` documents that
    // invariant rather than silently unwrapping a real failure mode.
    Ok(record.expect("confirmed pane identity implies a resolved record"))
}

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
/// operator is not inside a managed session), cross-checked against this
/// process's own tmux pane via [`resolve_in_session_rename_target`] (issue
/// #3600) before being trusted. PATCHes `/api/v1/sessions/managed/{id}` with
/// `{ "name": <new> }`; a 404 bails, a 409 (name collision) or 400 (invalid
/// name) prints the daemon's actionable message and returns `Err` (non-zero
/// exit), and success prints a confirmation.
/// Test: `cli_parses_sessions_rename_two_args`,
/// `cli_parses_sessions_rename_in_session`; HTTP path by `rename_route_*`;
/// the pane cross-check by `rename_tests.rs`.
pub(crate) async fn session_rename(
    client: &reqwest::Client,
    url: &str,
    arg1: String,
    arg2: Option<String>,
) -> anyhow::Result<()> {
    let (target, id) = match arg2 {
        Some(new_name) => {
            // Explicit target: no ambiguity from the env var, resolve normally.
            let target = arg1;
            let id = crate::commands::managed_route::resolve_managed_match(client, url, &target)
                .await
                .ok_or_else(|| anyhow::anyhow!("managed session '{target}' not found"))?;
            return finish_rename(client, url, &target, &id, new_name).await;
        }
        None => {
            // In-session form: resolve the current session from the env var the
            // managed launcher exports into every session, then refuse unless
            // this pane is provably that session's own pane (#3600).
            let current = std::env::var(CURRENT_SESSION_ENV).map_err(|_| {
                anyhow::anyhow!(
                    "not inside a managed session (${CURRENT_SESSION_ENV} is unset) — \
                     pass an explicit target: `tm sessions rename <id-or-name> <new-name>`"
                )
            })?;
            let current_pane_id = super::tmux_attach::current_tmux_pane_id();
            let record =
                resolve_in_session_rename_target(client, url, &current, current_pane_id.as_deref())
                    .await
                    .map_err(anyhow::Error::msg)?;
            (current, record.id)
        }
    };
    finish_rename(client, url, &target, &id, arg1).await
}

/// Issue the PATCH and render the 404/409/400/success outcomes — shared by
/// both `session_rename` branches (explicit target and the in-session,
/// pane-cross-checked form). Thin printing shell over [`do_rename_request`]
/// (the testable part).
async fn finish_rename(
    client: &reqwest::Client,
    url: &str,
    target: &str,
    id: &str,
    new_name: String,
) -> anyhow::Result<()> {
    let msg = do_rename_request(client, url, target, id, new_name).await?;
    println!("{msg}");
    Ok(())
}

/// Issue the rename PATCH and build the success message from the DAEMON'S
/// confirmed name, not the locally-requested one (issue #3692 review HIGH-1).
///
/// Why: since #3692, `SessionManager::rename` never rejects a collision — it
/// auto-suffixes, so the name the daemon actually applied (in the response
/// body's session summary) can differ from the one the operator asked for.
/// Printing the requested name would tell the operator a rename happened that
/// didn't ("renamed → tm-x" when the session is really `tm-x-2`), poisoning
/// their next `tmux attach`/resume-by-name.
/// What: PATCHes `{ "name": … }`; 404 bails; 400 (and the legacy 409, kept for
/// older daemons) surfaces the daemon's message on stderr and errors. On
/// success, parses the response body's `name` field and returns
/// [`rename_success_message`] built from it — falling back to the requested
/// name only when the body carries none (an older daemon). Returns the
/// message rather than printing so tests can assert it.
/// Test: `do_rename_request_reports_server_confirmed_suffixed_name`,
/// `do_rename_request_reports_plain_success` (`rename_tests.rs`).
async fn do_rename_request(
    client: &reqwest::Client,
    url: &str,
    target: &str,
    id: &str,
    new_name: String,
) -> anyhow::Result<String> {
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
            // Invalid name (400) — or a collision (409) from a pre-#3692
            // daemon that still rejects instead of auto-suffixing: surface the
            // daemon's actionable message on stderr and fail (non-zero exit).
            let msg = resp.text().await.unwrap_or_default();
            eprintln!("error: {msg}");
            Err(anyhow::anyhow!("rename rejected: {msg}"))
        }
        _ => {
            let resp = resp.error_for_status()?;
            // The daemon's summary carries the name it ACTUALLY applied,
            // which may be an auto-suffixed variant of what we sent (#3692).
            let confirmed = resp
                .json::<serde_json::Value>()
                .await
                .ok()
                .and_then(|v| v.get("name").and_then(|n| n.as_str()).map(str::to_string));
            Ok(rename_success_message(id, &new_name, confirmed.as_deref()))
        }
    }
}

/// Build the rename success line from the server-confirmed name (issue #3692
/// review HIGH-1) — pure, so the collided-and-suffixed wording is unit-tested.
///
/// Why: see [`do_rename_request`] — the printed name must be the one the
/// daemon applied, and a silent suffix would leave the operator believing the
/// requested name stuck.
/// What: `confirmed = None` (older daemon, no parseable body) falls back to
/// the requested name. When the confirmed name differs from the requested
/// one, the message says so explicitly ("was taken; auto-suffixed").
/// Test: `rename_success_message_plain`,
/// `rename_success_message_notes_suffix_on_collision`,
/// `rename_success_message_falls_back_without_body` (`rename_tests.rs`).
fn rename_success_message(id: &str, requested: &str, confirmed: Option<&str>) -> String {
    match confirmed {
        Some(actual) if actual != requested => format!(
            "renamed {id} -> {actual} (requested name '{requested}' was taken; auto-suffixed)"
        ),
        Some(actual) => format!("renamed {id} -> {actual}"),
        None => format!("renamed {id} -> {requested}"),
    }
}

#[cfg(test)]
#[path = "rename_tests.rs"]
mod tests;
