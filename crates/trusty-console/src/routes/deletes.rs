//! Operator-driven deletion of a trusty-memory palace and a trusty-search index
//! (#6360).
//!
//! Why: the dashboard could show a palace roster and an index roster but not act
//! on either, so reclaiming one meant leaving the console for a CLI. The console
//! does NOT delete anything itself — it has no business knowing how a palace or
//! an index is laid out on disk, and a second teardown implementation is exactly
//! what CLAUDE.md's common-entry-point rule forbids. Each route calls the owning
//! daemon's existing delete operation and reports what that daemon actually did.
//!
//! What: two `DELETE` routes.
//!   - `/api/console/memory/palaces/{id}` dials `tools/call` → `palace_delete`
//!     on trusty-memory's Unix socket — the same transport
//!     [`crate::detect::MemoryConnector`] already uses, so this opens no second
//!     door to that daemon (#6286, ADR-0032).
//!   - `/api/console/search/indexes/{id}` sends `DELETE /indexes/{id}` to the
//!     live trusty-search daemon over the loopback HTTP address the poller cache
//!     already resolves, through the crate's one shared `reqwest::Client`.
//!
//! Neither route reports success it did not observe. A daemon that refuses, a
//! daemon that answers a JSON-RPC error, and a daemon that skips the work and
//! answers `removed: false` all become a NON-success verdict carrying the
//! daemon's own words — see [`ActionVerdict`]. That last case is the one worth
//! naming: `DELETE /indexes/{id}` answers `200 OK` with `removed: false` for an
//! index it never had, so status code alone would report a delete that never
//! happened as a success.
//!
//! Test: `palace_delete_*`, `index_delete_*` and `validate_id_*` in the `tests`
//! module below drive both transports against stub daemons.

use std::path::{Path, PathBuf};

use axum::extract::{Path as AxumPath, Query, State};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::routes::memory_rpc;
use crate::routes::verdict::{ActionVerdict, first_line, validate_id};
use crate::routes::{ACTION_TIMEOUT, MEMORY_SERVICE, SEARCH_SERVICE_ID};
use crate::server::AppState;

// ─── trusty-memory: palace_delete over the daemon socket ─────────────────────

/// Delete a palace by calling `palace_delete` on trusty-memory's socket.
///
/// Why: `palace_delete` is trusty-memory's own teardown — the one
/// `MemoryService::delete_palace` the HTTP surface and the MCP tool both
/// delegated to since #180. It is reached through `tools/call` rather than as a
/// bare method name because that is the envelope trusty-memory's dispatcher
/// routes it under; the folded method table does not carry it.
///
/// What: one [`memory_rpc::call_tool`] exchange, which maps an unreachable
/// daemon and a JSON-RPC `error` to their verdicts — the second is the arm a
/// non-empty palace without `force` lands in. What is left for this function is
/// the confirmation: the answer is a success ONLY when the tool's own payload
/// names the id it deleted. Anything else is `Refused`, because a daemon that
/// answered without confirming has not told us it deleted anything.
///
/// Test: `palace_delete_confirms_a_real_delete`,
/// `palace_delete_reports_a_daemon_refusal_as_a_failure`,
/// `palace_delete_reports_an_unconfirmed_answer_as_a_failure`,
/// `palace_delete_reports_a_dead_socket_as_unreachable`.
pub(crate) async fn delete_palace_on_socket(socket: &Path, id: &str, force: bool) -> ActionVerdict {
    if let Err(reason) = validate_id(id) {
        return ActionVerdict::Invalid {
            id: id.to_string(),
            reason,
        };
    }

    let payload = match memory_rpc::call_tool(
        socket,
        "palace_delete",
        json!({ "palace_id": id, "force": force }),
        id,
    )
    .await
    {
        Ok(payload) => payload,
        Err(verdict) => return verdict,
    };

    match payload.get("deleted").and_then(Value::as_str) {
        Some(deleted) if deleted == id => ActionVerdict::Succeeded {
            id: id.to_string(),
            detail: json!({ "deleted": deleted }),
        },
        _ => ActionVerdict::Refused {
            id: id.to_string(),
            reason: format!(
                "{MEMORY_SERVICE} answered palace_delete without confirming the palace was deleted"
            ),
            detail: payload,
        },
    }
}

// ─── trusty-search: DELETE /indexes/{id} on the loopback daemon ──────────────

/// Delete a search index by calling the daemon's `DELETE /indexes/{id}`.
///
/// Why: that route is trusty-search's own deregistration path — the same
/// `unregister_index` the `delete_index` MCP tool and the orphan reaper drive.
/// The console sends it through the crate's single shared `reqwest::Client` so
/// no second HTTP client exists here.
///
/// What: `DELETE {base_url}/indexes/{id}?delete_data={delete_data}`. Since
/// #4123 a bare delete deregisters and PRESERVES the on-disk corpus, so
/// `delete_data` is passed explicitly rather than left to a default either side
/// might change. The answer is read for what it says the daemon DID:
///   - a non-2xx status is [`ActionVerdict::Refused`] carrying the body;
///   - `removed: false` is `Refused` even on `200` — that is the no-op an
///     unregistered id produces, and it is the failure mode this route exists to
///     not paper over;
///   - `delete_data` requested but `data_deleted: false` is `Refused`, because
///     the registration went but the bytes stayed and reporting "deleted" would
///     record a corpus as reclaimed while every byte of it is still on disk
///     (#3049).
///
/// Test: `index_delete_confirms_a_real_delete`,
/// `index_delete_reports_a_skipped_delete_as_a_failure`,
/// `index_delete_rejects_a_confirmation_for_another_id`,
/// `index_delete_reports_a_daemon_error_status_as_a_failure`,
/// `index_delete_reports_undeleted_data_as_a_failure`,
/// `index_delete_reports_a_dead_daemon_as_unreachable`.
pub(crate) async fn delete_index_on_daemon(
    client: &reqwest::Client,
    base_url: &str,
    id: &str,
    delete_data: bool,
) -> ActionVerdict {
    if let Err(reason) = validate_id(id) {
        return ActionVerdict::Invalid {
            id: id.to_string(),
            reason,
        };
    }

    let base = crate::proxy::routes::normalize_base_url(base_url);
    // SSRF guard, identical to the proxy's: the console is loopback-only
    // (ADR-0018), so a non-local upstream in the cache is a bug and must not be
    // dialled. Reusing the proxy's predicate keeps one definition of "local".
    if !crate::proxy::routes::is_local_upstream(&base) {
        return ActionVerdict::Unreachable {
            id: id.to_string(),
            reason: format!("the resolved {SEARCH_SERVICE_ID} address '{base}' is not loopback"),
        };
    }

    // `id` is restricted to `[A-Za-z0-9._-]` by `validate_id`, so it carries no
    // path or query metacharacter and needs no escaping here.
    let url = format!(
        "{}/indexes/{id}?delete_data={delete_data}",
        base.trim_end_matches('/')
    );

    let sent = client.delete(&url).timeout(ACTION_TIMEOUT).send().await;
    let response = match sent {
        Ok(r) => r,
        Err(e) => {
            return ActionVerdict::Unreachable {
                id: id.to_string(),
                reason: format!("{SEARCH_SERVICE_ID} did not answer the delete: {e}"),
            };
        }
    };

    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    let parsed: Value = serde_json::from_str(&body).unwrap_or(Value::Null);

    if !status.is_success() {
        return ActionVerdict::Refused {
            id: id.to_string(),
            reason: format!(
                "{SEARCH_SERVICE_ID} refused the delete with HTTP {}: {}",
                status.as_u16(),
                first_line(&body)
            ),
            detail: parsed,
        };
    }

    if parsed.get("removed").and_then(Value::as_bool) != Some(true) {
        let quiesced = parsed.get("quiesced").and_then(Value::as_bool);
        return ActionVerdict::Refused {
            id: id.to_string(),
            reason: format!(
                "{SEARCH_SERVICE_ID} skipped the delete: no registration for '{id}' was removed{}",
                match quiesced {
                    Some(false) =>
                        " (in-flight writers never quiesced, so the teardown was abandoned)",
                    _ => "",
                }
            ),
            detail: parsed,
        };
    }

    // #6360: the daemon echoes the id it acted on. A `removed: true` naming a
    // DIFFERENT index is not a confirmation for the one that was asked for —
    // the same confirmation check `delete_palace_on_socket` makes. An answer
    // carrying no `id` at all is accepted, because that field is the daemon's
    // echo of the request rather than a fact it discovered.
    match parsed.get("id").and_then(Value::as_str) {
        Some(echoed) if echoed != id => {
            return ActionVerdict::Refused {
                id: id.to_string(),
                reason: format!(
                    "{SEARCH_SERVICE_ID} confirmed a delete for '{echoed}', not for '{id}'"
                ),
                detail: parsed,
            };
        }
        _ => {}
    }

    if delete_data && parsed.get("data_deleted").and_then(Value::as_bool) != Some(true) {
        return ActionVerdict::Refused {
            id: id.to_string(),
            reason: format!(
                "{SEARCH_SERVICE_ID} deregistered '{id}' but did not delete its on-disk data"
            ),
            detail: parsed,
        };
    }

    ActionVerdict::Succeeded {
        id: id.to_string(),
        detail: parsed,
    }
}

// ─── handlers ────────────────────────────────────────────────────────────────

/// Query parameters for the palace-delete route.
#[derive(Debug, Default, Deserialize)]
pub struct PalaceDeleteParams {
    /// Delete the palace even though it still holds drawers.
    ///
    /// Absent ⇒ `false`, which is what makes a non-empty palace a visible
    /// refusal rather than a silent teardown.
    #[serde(default)]
    force: bool,
}

/// `DELETE /api/console/memory/palaces/{id}` — delete a palace (#6360).
///
/// Why: the dashboard's palace roster needs an action, and the console must not
/// grow its own teardown to provide one.
/// What: validates the id, then resolves trusty-memory's socket the way
/// [`crate::detect::MemoryConnector`] does — through
/// `trusty_common::daemon_socket_path`, so both agree on the path — then calls
/// [`delete_palace_on_socket`]. On success it re-polls trusty-memory's
/// `console_metrics` immediately so the roster the UI re-fetches reflects the
/// delete instead of a cache written up to a poll interval ago.
/// Test: `palace_route_rejects_a_traversal_id`.
pub async fn delete_palace_handler(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Query(params): Query<PalaceDeleteParams>,
) -> Response {
    // #6360: BEFORE resolving the daemon. Resolution can fail on its own — an
    // unresolvable data directory, a daemon that is not running — and a
    // resolution failure answered first would mask the id as the real problem
    // and make the guard untestable without a live daemon.
    if let Err(reason) = validate_id(&id) {
        return ActionVerdict::Invalid { id, reason }.into_response();
    }

    let socket: PathBuf = match trusty_common::daemon_socket_path(MEMORY_SERVICE) {
        Ok(p) => p,
        Err(e) => {
            return ActionVerdict::Unreachable {
                id,
                reason: format!("could not resolve the {MEMORY_SERVICE} socket path: {e:#}"),
            }
            .into_response();
        }
    };

    let verdict = delete_palace_on_socket(&socket, &id, params.force).await;
    if matches!(verdict, ActionVerdict::Succeeded { .. }) {
        refresh_metrics(&state, MEMORY_SERVICE, state.memory_metrics_cache()).await;
    }
    verdict.into_response()
}

/// Query parameters for the index-delete route.
#[derive(Debug, Default, Deserialize)]
pub struct IndexDeleteParams {
    /// Destroy the index's on-disk corpus as well as its registration.
    ///
    /// Absent ⇒ `false`, matching trusty-search's own contract since #4123: a
    /// bare delete deregisters and preserves the data.
    #[serde(default)]
    delete_data: bool,
}

/// `DELETE /api/console/search/indexes/{id}` — delete a search index (#6360).
///
/// Why: the counterpart to [`delete_palace_handler`] for the Search tab's index
/// roster.
/// What: validates the id, then resolves trusty-search's loopback base URL from
/// the health-poll cache —
/// the same resolution the reverse proxy uses, so there is one answer to "where
/// is trusty-search" — then calls [`delete_index_on_daemon`] with the crate's
/// shared HTTP client. Refreshes the search metrics cache after a confirmed
/// delete.
/// Test: `index_route_rejects_a_bad_id`,
/// `index_route_reports_an_unresolved_daemon_as_unreachable`.
pub async fn delete_index_handler(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Query(params): Query<IndexDeleteParams>,
) -> Response {
    // #6360: BEFORE the cache lookup. Resolving first answered `503` for a
    // malformed id whenever trusty-search happened to be unresolved, which hid
    // whether the guard ran at all — `index_route_rejects_a_bad_id` could pass
    // against an empty `AppState` without validation ever executing.
    if let Err(reason) = validate_id(&id) {
        return ActionVerdict::Invalid { id, reason }.into_response();
    }

    let base_url = match state.poller_cache().snapshot().await {
        Some(snap) => snap.url_map().get(SEARCH_SERVICE_ID).cloned(),
        None => None,
    };
    let Some(base_url) = base_url else {
        return ActionVerdict::Unreachable {
            id,
            reason: format!(
                "{SEARCH_SERVICE_ID} is not reachable: the console has no live address for it"
            ),
        }
        .into_response();
    };

    let client = state.http_client();
    let verdict = delete_index_on_daemon(&client, &base_url, &id, params.delete_data).await;
    if matches!(verdict, ActionVerdict::Succeeded { .. }) {
        refresh_metrics(&state, SEARCH_SERVICE_ID, state.search_metrics_cache()).await;
    }
    verdict.into_response()
}

/// Re-poll one service's `console_metrics` and overwrite its cache.
///
/// Why: the roster the dashboard renders comes from a cache the background
/// poller refreshes every `--poll-interval` seconds (15 by default). Without
/// this, a UI that correctly re-fetches after a delete would still be shown the
/// pre-delete roster for up to that long, and the operator would read a
/// successful delete as a failed one. Re-polling here means the re-fetch is
/// answered from the daemon's current state.
/// What: looks the service's MCP handle up and runs one poll cycle. A failed
/// poll is logged and left alone — the cache keeps its previous value, and the
/// next background tick retries. The delete already succeeded, so a stale roster
/// must never turn it into a reported failure.
/// Test: `palace_route_rejects_a_traversal_id` reaches this module without
/// reaching this function; the refresh itself needs a live daemon and is covered
/// by the #6360 smoke run.
pub(crate) async fn refresh_metrics(
    state: &AppState,
    service_id: &str,
    cache: &crate::metrics_poller::MetricsCache,
) {
    let Some(handle) = state.mcp_handles().get(service_id).cloned() else {
        tracing::warn!("delete: no MCP handle registered for {service_id}; roster not refreshed");
        return;
    };
    crate::metrics_poller::poll_once(&handle, cache).await;
}

// ─── tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routes::verdict::MAX_ID_LEN;
    use axum::body::Body;
    use axum::http::Request;
    use axum::http::StatusCode;
    use http_body_util::BodyExt as _;
    use tower::ServiceExt as _;

    use crate::server::build_router;

    // ── helpers ──────────────────────────────────────────────────────────────

    /// Bind a socket that answers exactly one framed request with `reply`.
    ///
    /// Mirrors the stub the memory connector's own tests use, so both exercise
    /// the same framing the daemon speaks.
    fn stub_memory_daemon(dir: &std::path::Path, reply: impl Into<String>) -> PathBuf {
        let socket = dir.join("sockets").join("memory.sock");
        let reply = reply.into();
        let listener = trusty_common::uds::bind_hardened(&socket).expect("bind");
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
            let Ok((mut conn, _)) = listener.accept().await else {
                return;
            };
            let mut sink = Vec::new();
            let _ = conn.read_to_end(&mut sink).await;
            let _ = conn.write_all(reply.as_bytes()).await;
            let _ = conn.write_all(b"\n").await;
            let _ = conn.flush().await;
        });
        socket
    }

    /// Wrap a tool payload in the `tools/call` envelope trusty-memory answers.
    fn tools_call_reply(payload: &str) -> String {
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": { "content": [{ "type": "text", "text": payload }] },
        })
        .to_string()
    }

    /// Start a stub trusty-search on loopback that answers every `DELETE
    /// /indexes/{id}` with `status` and `body`. Returns its base URL.
    async fn stub_search_daemon(status: StatusCode, body: Value) -> String {
        let app = axum::Router::new().route(
            "/indexes/{id}",
            axum::routing::delete(move || {
                let body = body.clone();
                async move { (status, axum::Json(body)) }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        format!("http://{addr}")
    }

    /// The client the routes actually use.
    ///
    /// Built through `AppState` rather than by a second builder here: the
    /// redirect assertion in `index_delete_does_not_follow_a_redirect` is only
    /// worth anything if it exercises the production policy.
    fn client() -> reqwest::Client {
        (*AppState::new(vec![]).http_client()).clone()
    }

    /// Assert a verdict is not a success and say what it carried when it is.
    fn assert_failure(verdict: &ActionVerdict, must_mention: &str) {
        assert!(
            !matches!(verdict, ActionVerdict::Succeeded { .. }),
            "expected a failure verdict, got {verdict:?}"
        );
        let text = format!("{verdict:?}");
        assert!(
            text.contains(must_mention),
            "the failure must carry the daemon's own words ({must_mention:?}): {text}"
        );
    }

    // ── id validation ────────────────────────────────────────────────────────

    /// Why: the ids real palaces and indexes carry must pass unchanged, or the
    /// guard is a denial of the feature rather than a guard.
    /// Test: this is the test.
    #[test]
    fn validate_id_accepts_ordinary_ids() {
        for id in ["trusty-tools", "my_palace", "index.v2", "A1", "a-b_c.d-9"] {
            assert!(validate_id(id).is_ok(), "{id} must be accepted");
        }
    }

    /// Why (#6360, acceptance 5): an id is appended to a trusty-search URL path.
    /// `..` there could walk the request onto a different daemon route.
    /// Test: this is the test.
    #[test]
    fn validate_id_rejects_traversal() {
        for id in ["..", "../etc", "a/../b", "....//"] {
            assert!(validate_id(id).is_err(), "{id:?} must be rejected");
        }
    }

    /// Why: separators, query metacharacters, whitespace and control bytes all
    /// change what URL the console actually requests.
    /// Test: this is the test.
    #[test]
    fn validate_id_rejects_separators_and_control_bytes() {
        for id in [
            "",
            "a/b",
            "a\\b",
            "a?b",
            "a#b",
            "a b",
            "a\nb",
            "a\0b",
            "a;rm -rf /",
            "a%2fb",
        ] {
            assert!(validate_id(id).is_err(), "{id:?} must be rejected");
        }
        let too_long = "a".repeat(MAX_ID_LEN + 1);
        assert!(
            validate_id(&too_long).is_err(),
            "an oversized id is refused"
        );
    }

    /// Why: each verdict must reach the operator as a distinguishable HTTP
    /// status; collapsing them would make a refusal and an outage look alike.
    /// Test: this is the test.
    #[test]
    fn verdict_status_codes_separate_the_four_arms() {
        let id = "x".to_string();
        assert_eq!(
            ActionVerdict::Succeeded {
                id: id.clone(),
                detail: Value::Null
            }
            .status(),
            StatusCode::OK
        );
        assert_eq!(
            ActionVerdict::Refused {
                id: id.clone(),
                reason: String::new(),
                detail: Value::Null
            }
            .status(),
            StatusCode::CONFLICT
        );
        assert_eq!(
            ActionVerdict::Unreachable {
                id: id.clone(),
                reason: String::new()
            }
            .status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            ActionVerdict::Invalid {
                id,
                reason: String::new()
            }
            .status(),
            StatusCode::BAD_REQUEST
        );
    }

    // ── palace delete ────────────────────────────────────────────────────────

    /// Why: the success path has to be reachable, and it is only reachable when
    /// the daemon confirms the exact id — which is what the stub answers.
    /// Test: this is the test.
    #[tokio::test(flavor = "multi_thread")]
    async fn palace_delete_confirms_a_real_delete() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let socket = stub_memory_daemon(tmp.path(), tools_call_reply(r#"{"deleted":"scratch"}"#));

        let verdict = delete_palace_on_socket(&socket, "scratch", true).await;
        assert!(
            matches!(&verdict, ActionVerdict::Succeeded { id, .. } if id == "scratch"),
            "a confirmed delete must read as success: {verdict:?}"
        );
    }

    /// Why (#6360, acceptance 2): a daemon refusal — the arm a non-empty palace
    /// without `force` lands in — must reach the operator as a failure carrying
    /// the daemon's own message, never as "deleted".
    /// Test: this is the test.
    #[tokio::test(flavor = "multi_thread")]
    async fn palace_delete_reports_a_daemon_refusal_as_a_failure() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let socket = stub_memory_daemon(
            tmp.path(),
            r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32000,"message":"Palace 'scratch' still has 4 drawers; pass force=true"}}"#,
        );

        let verdict = delete_palace_on_socket(&socket, "scratch", false).await;
        assert_failure(&verdict, "still has 4 drawers");
    }

    /// Why (#6360, acceptance 2): a daemon that answers something OTHER than a
    /// delete confirmation has not told us it deleted anything. Treating any
    /// non-error result as success is how a no-op renders as "deleted".
    /// Test: this is the test.
    #[tokio::test(flavor = "multi_thread")]
    async fn palace_delete_reports_an_unconfirmed_answer_as_a_failure() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let socket = stub_memory_daemon(
            tmp.path(),
            tools_call_reply(r#"{"status":"noop","reason":"not loaded"}"#),
        );

        let verdict = delete_palace_on_socket(&socket, "scratch", true).await;
        assert_failure(&verdict, "without confirming");
    }

    /// Why: a confirmation naming a DIFFERENT palace is not a confirmation for
    /// the one that was asked for.
    /// Test: this is the test.
    #[tokio::test(flavor = "multi_thread")]
    async fn palace_delete_rejects_a_confirmation_for_another_id() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let socket = stub_memory_daemon(
            tmp.path(),
            tools_call_reply(r#"{"deleted":"someone-else"}"#),
        );

        let verdict = delete_palace_on_socket(&socket, "scratch", true).await;
        assert_failure(&verdict, "without confirming");
    }

    /// Why: a socket nothing is serving must read as unreachable, not as a
    /// refusal and certainly not as a delete.
    /// Test: this is the test.
    #[tokio::test(flavor = "multi_thread")]
    async fn palace_delete_reports_a_dead_socket_as_unreachable() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let verdict =
            delete_palace_on_socket(&tmp.path().join("absent.sock"), "scratch", false).await;
        assert!(
            matches!(verdict, ActionVerdict::Unreachable { .. }),
            "a dead socket must read as unreachable: {verdict:?}"
        );
    }

    /// Why: an id the console will not forward must be refused before any bytes
    /// reach a daemon.
    /// Test: this is the test.
    #[tokio::test(flavor = "multi_thread")]
    async fn palace_delete_refuses_a_bad_id_without_dialling() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let verdict = delete_palace_on_socket(&tmp.path().join("absent.sock"), "../x", false).await;
        assert!(
            matches!(verdict, ActionVerdict::Invalid { .. }),
            "a traversal id must be refused at the console: {verdict:?}"
        );
    }

    // ── index delete ─────────────────────────────────────────────────────────

    /// Why: the success path must be reachable when the daemon reports it
    /// actually removed the registration.
    /// Test: this is the test.
    #[tokio::test(flavor = "multi_thread")]
    async fn index_delete_confirms_a_real_delete() {
        let base = stub_search_daemon(
            StatusCode::OK,
            json!({"id":"scratch","removed":true,"data_deleted":false,"quiesced":true}),
        )
        .await;
        let verdict = delete_index_on_daemon(&client(), &base, "scratch", false).await;
        assert!(
            matches!(&verdict, ActionVerdict::Succeeded { id, .. } if id == "scratch"),
            "a confirmed delete must read as success: {verdict:?}"
        );
    }

    /// Why (#6360, acceptance 2): `DELETE /indexes/{id}` answers `200 OK` with
    /// `removed: false` for an id it never had. Status code alone would render
    /// that skipped delete as a success — this is the exact recorded no-op the
    /// route must not paper over.
    /// Test: this is the test.
    #[tokio::test(flavor = "multi_thread")]
    async fn index_delete_reports_a_skipped_delete_as_a_failure() {
        let base = stub_search_daemon(
            StatusCode::OK,
            json!({"id":"scratch","removed":false,"data_deleted":false,"quiesced":true}),
        )
        .await;
        let verdict = delete_index_on_daemon(&client(), &base, "scratch", false).await;
        assert_failure(&verdict, "skipped the delete");

        // The rendered body must say `ok: false` — the field the UI reads.
        let response = verdict.into_response();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("body")
            .to_bytes();
        let body: Value = serde_json::from_slice(&bytes).expect("json");
        assert_eq!(
            body["ok"],
            json!(false),
            "a no-op must never render ok:true"
        );
    }

    /// Why (#6360): a `200` whose body carries no `removed` field at all is the
    /// same class of answer as `removed: false` — the daemon did not say it
    /// removed anything. Reading a missing field as success is how a protocol
    /// change or a proxy that swallowed the body turns into a phantom delete.
    /// Test: this is the test.
    #[tokio::test(flavor = "multi_thread")]
    async fn index_delete_reports_an_empty_body_as_a_failure() {
        let base = stub_search_daemon(StatusCode::OK, json!({})).await;
        let verdict = delete_index_on_daemon(&client(), &base, "scratch", false).await;
        assert_failure(&verdict, "skipped the delete");
    }

    /// Why: a `removed: true` naming a DIFFERENT index is not a confirmation
    /// for the one that was asked for.
    /// Test: this is the test.
    #[tokio::test(flavor = "multi_thread")]
    async fn index_delete_rejects_a_confirmation_for_another_id() {
        let base = stub_search_daemon(
            StatusCode::OK,
            json!({"id":"someone-else","removed":true,"data_deleted":false,"quiesced":true}),
        )
        .await;
        let verdict = delete_index_on_daemon(&client(), &base, "scratch", false).await;
        assert_failure(&verdict, "not for 'scratch'");
    }

    /// Why: an abandoned teardown (`quiesced: false` beside `removed: false`)
    /// is a distinct condition the operator can act on — retry it — so the
    /// message must say so rather than reporting a bare skip.
    /// Test: this is the test.
    #[tokio::test(flavor = "multi_thread")]
    async fn index_delete_names_an_abandoned_teardown() {
        let base = stub_search_daemon(
            StatusCode::OK,
            json!({"id":"scratch","removed":false,"data_deleted":false,"quiesced":false}),
        )
        .await;
        let verdict = delete_index_on_daemon(&client(), &base, "scratch", false).await;
        assert_failure(&verdict, "never quiesced");
    }

    /// Why (#6360, acceptance 2): a 4xx/5xx from the daemon must carry the
    /// daemon's own body, not a console-invented message.
    /// Test: this is the test.
    #[tokio::test(flavor = "multi_thread")]
    async fn index_delete_reports_a_daemon_error_status_as_a_failure() {
        let base = stub_search_daemon(
            StatusCode::BAD_REQUEST,
            json!({"error":"delete_data must be a boolean"}),
        )
        .await;
        let verdict = delete_index_on_daemon(&client(), &base, "scratch", false).await;
        assert_failure(&verdict, "delete_data must be a boolean");
    }

    /// Why (#3049, carried into #6360): `data_deleted: false` after the caller
    /// ASKED for the data means the registration went and every byte stayed.
    /// Reporting that as a success records a corpus as reclaimed while it is
    /// still on disk.
    /// Test: this is the test.
    #[tokio::test(flavor = "multi_thread")]
    async fn index_delete_reports_undeleted_data_as_a_failure() {
        let base = stub_search_daemon(
            StatusCode::OK,
            json!({"id":"scratch","removed":true,"data_deleted":false,"quiesced":true}),
        )
        .await;
        let verdict = delete_index_on_daemon(&client(), &base, "scratch", true).await;
        assert_failure(&verdict, "did not delete its on-disk data");
    }

    /// Why: nothing listening must read as unreachable, distinct from a refusal.
    /// Test: this is the test.
    #[tokio::test(flavor = "multi_thread")]
    async fn index_delete_reports_a_dead_daemon_as_unreachable() {
        // Bind and immediately drop, so the port is almost certainly free.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        drop(listener);

        let verdict =
            delete_index_on_daemon(&client(), &format!("http://{addr}"), "scratch", false).await;
        assert!(
            matches!(verdict, ActionVerdict::Unreachable { .. }),
            "a dead daemon must read as unreachable: {verdict:?}"
        );
    }

    /// Why (#6360): every loopback check in this crate validates the URL it was
    /// handed and nothing else. A followed 3xx would re-issue the DELETE, method
    /// and all, at whatever host the `Location` names — past the guard that just
    /// ran. The stub redirects to a public address; if the policy ever went back
    /// to `limited(10)`, this would leave loopback.
    /// Test: this is the test.
    #[tokio::test(flavor = "multi_thread")]
    async fn index_delete_does_not_follow_a_redirect() {
        let app = axum::Router::new().route(
            "/indexes/{id}",
            axum::routing::delete(|| async {
                (
                    StatusCode::TEMPORARY_REDIRECT,
                    [(
                        axum::http::header::LOCATION,
                        "http://169.254.169.254/indexes/scratch",
                    )],
                )
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let verdict =
            delete_index_on_daemon(&client(), &format!("http://{addr}"), "scratch", false).await;
        assert_failure(&verdict, "HTTP 307");
    }

    /// Why (ADR-0018): a non-loopback upstream in the cache is a bug or a
    /// compromise; the delete must not be sent to it.
    /// Test: this is the test.
    #[tokio::test(flavor = "multi_thread")]
    async fn index_delete_refuses_a_non_loopback_upstream() {
        let verdict =
            delete_index_on_daemon(&client(), "http://10.0.0.9:7878", "scratch", false).await;
        assert!(
            matches!(&verdict, ActionVerdict::Unreachable { reason, .. } if reason.contains("not loopback")),
            "a remote upstream must be refused: {verdict:?}"
        );
    }

    // ── route wiring ─────────────────────────────────────────────────────────

    async fn delete_through_router(uri: &str) -> (StatusCode, Value) {
        let router = build_router(AppState::new(vec![]));
        let req = Request::builder()
            .method("DELETE")
            .uri(uri)
            .body(Body::empty())
            .expect("request");
        let resp = router.oneshot(req).await.expect("response");
        let status = resp.status();
        let bytes = resp.into_body().collect().await.expect("body").to_bytes();
        let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, body)
    }

    /// Why: the palace route must be mounted and must refuse a traversal id
    /// before it resolves or dials anything.
    /// Test: this is the test.
    #[tokio::test]
    async fn palace_route_rejects_a_traversal_id() {
        let (status, body) = delete_through_router("/api/console/memory/palaces/..%2Fetc").await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "body: {body}");
        assert_eq!(body["ok"], json!(false));
    }

    /// Why: the index route must be mounted, and with no poller snapshot yet it
    /// must say trusty-search is unreachable rather than 500 or claim a delete.
    /// Test: this is the test.
    #[tokio::test]
    async fn index_route_reports_an_unresolved_daemon_as_unreachable() {
        let (status, body) = delete_through_router("/api/console/search/indexes/scratch").await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "body: {body}");
        assert_eq!(body["ok"], json!(false));
        assert!(
            body["error"]
                .as_str()
                .unwrap_or_default()
                .contains("trusty-search"),
            "the error must name the daemon: {body}"
        );
    }

    /// Why: the index route rejects a bad id AHEAD of daemon resolution, so the
    /// guard fires with no daemon running. Accepting `503` here too would have
    /// let this pass against an empty `AppState` without validation ever
    /// executing — exactly the hole that made the assertion meaningless.
    /// Test: this is the test.
    #[tokio::test]
    async fn index_route_rejects_a_bad_id() {
        let (status, body) = delete_through_router("/api/console/search/indexes/a%2Fb").await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "validation must run before daemon resolution, so this cannot be 503: {body}"
        );
        assert_eq!(body["ok"], json!(false));
        assert!(
            body["error"].as_str().unwrap_or_default().contains('/'),
            "the error must name the offending character: {body}"
        );
    }

    /// Why (#6360, acceptance 5): the router-wide same-origin guard must cover
    /// these DELETEs — they are the most destructive routes the console serves.
    /// Test: this is the test.
    #[tokio::test]
    async fn delete_routes_reject_a_cross_origin_caller() {
        for uri in [
            "/api/console/memory/palaces/scratch",
            "/api/console/search/indexes/scratch",
        ] {
            let router = build_router(AppState::new(vec![]));
            let req = Request::builder()
                .method("DELETE")
                .uri(uri)
                .header("origin", "https://evil.example")
                .body(Body::empty())
                .expect("request");
            let resp = router.oneshot(req).await.expect("response");
            assert_eq!(
                resp.status(),
                StatusCode::FORBIDDEN,
                "{uri} must refuse a cross-origin delete"
            );
        }
    }
}
