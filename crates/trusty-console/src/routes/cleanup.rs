//! Operator-driven cleanup: compact a trusty-memory palace (#6371).
//!
//! Why: the Memory tab could delete a palace outright and do nothing short of
//! that. Compaction is the non-destructive half an operator reaches for first —
//! the daemon has always been able to reclaim a palace's orphaned vectors and
//! the console had no way to ask it to.
//!
//! What: one route. `POST /api/console/memory/palaces/{id}/compact` calls
//! trusty-memory's `palace_compact` over its socket, and reports a success ONLY
//! when the answer names the palace it compacted.
//!
//! #6941: the batch index prune this module also carried
//! (`POST /api/console/search/prune-indexes`) is gone, with
//! `POST /api/console/search/deregister-unjudged` and `routes::census_guard`.
//! Console displays and the dashboard manages (DOC-73 §13), so that panel now
//! lives in the search dashboard and calls trusty-search's
//! `GET /registry/orphans` and `DELETE /indexes/{id}` directly. The removed
//! code is at commit `f954009cb`.
//!
//! Test: `compact_*` in the `tests` module below.

use std::path::{Path, PathBuf};

use axum::extract::{Path as AxumPath, State};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::routes::MEMORY_SERVICE;
use crate::routes::memory_rpc;
use crate::routes::verdict::{ActionVerdict, validate_id};
use crate::server::AppState;

// ─── trusty-memory: palace_compact over the daemon socket ───────────────────

/// Compact a palace by calling `palace_compact` on trusty-memory's socket.
///
/// Why: `palace_compact` is trusty-memory's own reclamation — it drops vector
/// index entries that have no drawer behind them, under the palace write lock
/// so a concurrent `remember` cannot have its new vector reclaimed (#6208). The
/// console must not grow a second idea of what an orphaned vector is.
/// What: one [`memory_rpc::call_tool`] exchange. The answer is a success ONLY
/// when the tool's payload names the palace it compacted — the same
/// confirmation discipline the delete routes use, because a daemon that
/// answered without naming the palace has not said it compacted this one.
/// Test: `compact_confirms_a_real_compaction`,
/// `compact_reports_an_unconfirmed_answer_as_a_failure`,
/// `compact_rejects_a_confirmation_for_another_palace`,
/// `compact_reports_a_dead_socket_as_unreachable`.
pub(crate) async fn compact_palace_on_socket(socket: &Path, id: &str) -> ActionVerdict {
    if let Err(reason) = validate_id(id) {
        return ActionVerdict::Invalid {
            id: id.to_string(),
            reason,
        };
    }

    let payload =
        match memory_rpc::call_tool(socket, "palace_compact", json!({ "palace": id }), id).await {
            Ok(payload) => payload,
            Err(verdict) => return verdict,
        };

    match payload.get("palace").and_then(Value::as_str) {
        Some(compacted) if compacted == id => ActionVerdict::Succeeded {
            id: id.to_string(),
            detail: payload,
        },
        _ => ActionVerdict::Refused {
            id: id.to_string(),
            reason: format!(
                "{MEMORY_SERVICE} answered palace_compact without confirming it compacted '{id}'"
            ),
            detail: payload,
        },
    }
}

/// `POST /api/console/memory/palaces/{id}/compact` — compact one palace (#6371).
///
/// Why: the Memory tab could delete a palace outright and do nothing short of
/// that. Compaction is the non-destructive half an operator reaches for first.
/// What: validates the id, resolves trusty-memory's socket the way the delete
/// route does — through `trusty_common::daemon_socket_path`, so both agree on
/// the path — and calls [`compact_palace_on_socket`]. Re-polls the memory
/// metrics cache on success so the reclaimed vector counts are what the UI
/// re-fetches.
/// Test: `compact_route_rejects_a_traversal_id`.
pub async fn compact_palace_handler(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
) -> Response {
    // Before resolving the daemon, for the reason #6360 records: a resolution
    // failure answered first would mask the id as the real problem and make the
    // guard untestable without a live daemon.
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

    let verdict = compact_palace_on_socket(&socket, &id).await;
    if verdict.succeeded() {
        crate::routes::deletes::refresh_metrics(
            &state,
            MEMORY_SERVICE,
            state.memory_metrics_cache(),
        )
        .await;
    }
    verdict.into_response()
}

// ─── tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use axum::http::StatusCode;
    use http_body_util::BodyExt as _;
    use tower::ServiceExt as _;

    use crate::server::build_router;

    // ── helpers ──────────────────────────────────────────────────────────────

    /// Bind a socket that answers exactly one framed request with `reply`.
    fn stub_memory_daemon(dir: &Path, reply: impl Into<String>) -> PathBuf {
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

    /// Drive the real router with trusty-search pointed at a socket nothing is
    /// bound to.
    ///
    /// Why the override (#6285): without it these tests resolve the REAL
    /// trusty-search socket, and on a machine with the daemon running a route
    /// test would prune live indexes.
    async fn post_through_router(uri: &str, body: Value) -> (StatusCode, Value) {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let router =
            build_router(AppState::new(vec![]).with_search_socket(tmp.path().join("absent.sock")));
        let req = Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .expect("request");
        let resp = router.oneshot(req).await.expect("response");
        let status = resp.status();
        let bytes = resp.into_body().collect().await.expect("body").to_bytes();
        let parsed = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, parsed)
    }

    // ── compact ──────────────────────────────────────────────────────────────

    /// Why: the success path is reachable only when the daemon names the palace
    /// it compacted, and the operator sees the reclaimed counts.
    /// Test: this is the test.
    #[tokio::test(flavor = "multi_thread")]
    async fn compact_confirms_a_real_compaction() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let socket = stub_memory_daemon(
            tmp.path(),
            tools_call_reply(
                r#"{"palace":"scratch","total_checked":120,"orphans_removed":7,"index_size_before":120,"index_size_after":113}"#,
            ),
        );

        let verdict = compact_palace_on_socket(&socket, "scratch").await;
        assert!(
            matches!(&verdict, ActionVerdict::Succeeded { id, .. } if id == "scratch"),
            "a confirmed compaction must read as success: {verdict:?}"
        );
        let response = verdict.into_response();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("body")
            .to_bytes();
        let body: Value = serde_json::from_slice(&bytes).expect("json");
        assert_eq!(body["ok"], json!(true));
        assert_eq!(
            body["detail"]["orphans_removed"],
            json!(7),
            "the operator sees what was reclaimed: {body}"
        );
    }

    /// Why: a daemon that answered something other than a compaction report has
    /// not told us it compacted anything, and reporting it as done would record
    /// a reclamation that never happened.
    /// Test: this is the test.
    #[tokio::test(flavor = "multi_thread")]
    async fn compact_reports_an_unconfirmed_answer_as_a_failure() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let socket = stub_memory_daemon(tmp.path(), tools_call_reply(r#"{"status":"noop"}"#));

        let verdict = compact_palace_on_socket(&socket, "scratch").await;
        assert!(
            matches!(&verdict, ActionVerdict::Refused { reason, .. } if reason.contains("without confirming")),
            "an unconfirmed answer must read as a failure: {verdict:?}"
        );
    }

    /// Why: a report naming a DIFFERENT palace is not a confirmation for this
    /// one — the same check the delete routes make.
    /// Test: this is the test.
    #[tokio::test(flavor = "multi_thread")]
    async fn compact_rejects_a_confirmation_for_another_palace() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let socket = stub_memory_daemon(
            tmp.path(),
            tools_call_reply(r#"{"palace":"someone-else","orphans_removed":3}"#),
        );

        let verdict = compact_palace_on_socket(&socket, "scratch").await;
        assert!(
            matches!(verdict, ActionVerdict::Refused { .. }),
            "a confirmation for another palace is not one for this one: {verdict:?}"
        );
    }

    /// Why: a daemon refusal — an unknown palace, a locked store — must carry
    /// the daemon's own message rather than a console-invented one.
    /// Test: this is the test.
    #[tokio::test(flavor = "multi_thread")]
    async fn compact_reports_a_daemon_refusal_as_a_failure() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let socket = stub_memory_daemon(
            tmp.path(),
            r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32000,"message":"palace 'scratch' is not open"}}"#,
        );

        let verdict = compact_palace_on_socket(&socket, "scratch").await;
        assert!(
            matches!(&verdict, ActionVerdict::Refused { reason, .. } if reason.contains("is not open")),
            "the refusal must carry the daemon's words: {verdict:?}"
        );
        assert_eq!(verdict.status(), StatusCode::CONFLICT);
    }

    /// Why: a socket nothing is serving must read as unreachable, not as a
    /// refusal and certainly not as a compaction.
    /// Test: this is the test.
    #[tokio::test(flavor = "multi_thread")]
    async fn compact_reports_a_dead_socket_as_unreachable() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let verdict = compact_palace_on_socket(&tmp.path().join("absent.sock"), "scratch").await;
        assert!(
            matches!(verdict, ActionVerdict::Unreachable { .. }),
            "a dead socket must read as unreachable: {verdict:?}"
        );
    }

    /// Why: an id the console will not forward must be refused before any bytes
    /// reach a daemon.
    /// Test: this is the test.
    #[tokio::test(flavor = "multi_thread")]
    async fn compact_refuses_a_bad_id_without_dialling() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let verdict = compact_palace_on_socket(&tmp.path().join("absent.sock"), "../x").await;
        assert!(
            matches!(verdict, ActionVerdict::Invalid { .. }),
            "a traversal id must be refused at the console: {verdict:?}"
        );
    }

    /// Why: the compact route must be mounted and must refuse a traversal id
    /// before it resolves or dials anything.
    /// Test: this is the test.
    #[tokio::test]
    async fn compact_route_rejects_a_traversal_id() {
        let (status, body) =
            post_through_router("/api/console/memory/palaces/..%2Fetc/compact", json!({})).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "body: {body}");
        assert_eq!(body["ok"], json!(false));
    }

    /// Why (#6360, carried into #6371): the router-wide same-origin guard must
    /// cover these routes too — a batch prune is the most destructive thing the
    /// console serves.
    /// Test: this is the test.
    #[tokio::test]
    async fn cleanup_routes_reject_a_cross_origin_caller() {
        for uri in ["/api/console/memory/palaces/scratch/compact"] {
            let router = build_router(AppState::new(vec![]));
            let req = Request::builder()
                .method("POST")
                .uri(uri)
                .header("origin", "https://evil.example")
                .header("content-type", "application/json")
                .body(Body::from(json!({}).to_string()))
                .expect("request");
            let resp = router.oneshot(req).await.expect("response");
            assert_eq!(
                resp.status(),
                StatusCode::FORBIDDEN,
                "{uri} must refuse a cross-origin cleanup"
            );
        }
    }
}
