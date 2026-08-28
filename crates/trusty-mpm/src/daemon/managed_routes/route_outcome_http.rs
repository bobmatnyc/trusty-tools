//! The axum half of the transport-neutral [`RouteOutcome`] (#6288 slice 4).
//!
//! Why: the route bodies this slice shares between axum and the Unix socket
//! return a [`RouteOutcome`], which knows nothing about HTTP. Something still
//! has to turn one into a `Response` for the HTTP surface, and that something
//! must not live under `daemon::rpc` — the acceptance bar for slice 4 is that
//! no HTTP type appears in the RPC modules, so the axum dependency is pinned
//! here instead, next to the handlers that need it.
//!
//! What: one `IntoResponse` impl. A `Json` body becomes an
//! `application/json` response at the outcome's status; a `Text` body becomes a
//! bare string at that status — byte for byte the two shapes the pre-slice
//! handlers produced with `Json(..).into_response()` and
//! `(StatusCode::X, msg).into_response()`.
//!
//! Test: `route_outcome_http_tests` below; every pre-existing HTTP handler test
//! is the real regression net, since each now reaches axum through this impl.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use crate::daemon::rpc::managed::outcome::{RouteBody, RouteOutcome};

impl IntoResponse for RouteOutcome {
    fn into_response(self) -> Response {
        // A status this daemon does not produce would be a programmer error, so
        // it degrades to 500 rather than panicking inside a live request.
        let status = StatusCode::from_u16(self.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        match self.body {
            RouteBody::Json(v) => (status, axum::Json(v)).into_response(),
            RouteBody::Text(s) => (status, s).into_response(),
        }
    }
}

/// Read an already-built axum `Response` back into a [`RouteOutcome`] (#6288).
///
/// Why: three refusals in this slice's scope are produced by error types that
/// predate it and implement `IntoResponse` themselves — `DaemonError` and the
/// deliverable-scope errors. Re-deriving their status and wording in a neutral
/// form would create the second implementation this slice exists to prevent.
/// Reading the response back preserves both verbatim, so the socket reports
/// exactly what HTTP reports.
/// What: takes the status, and the body as JSON when it parses as JSON and as
/// text otherwise. A body that cannot be read at all becomes a 500 naming the
/// read failure.
/// Test: `a_json_error_response_round_trips_into_an_outcome`.
pub(crate) async fn outcome_from_response(resp: Response) -> RouteOutcome {
    let status = resp.status().as_u16();
    let bytes = match axum::body::to_bytes(resp.into_body(), usize::MAX).await {
        Ok(b) => b,
        Err(e) => return RouteOutcome::text(500, format!("read response body: {e}")),
    };
    match serde_json::from_slice::<serde_json::Value>(&bytes) {
        Ok(v) => RouteOutcome::json(status, &v),
        Err(_) => RouteOutcome::text(status, String::from_utf8_lossy(&bytes).into_owned()),
    }
}

#[cfg(test)]
mod route_outcome_http_tests {
    use super::*;

    #[tokio::test]
    async fn a_json_outcome_becomes_a_json_response_at_its_status() {
        let resp = RouteOutcome::created(&serde_json::json!({"id": "s1"})).into_response();
        assert_eq!(resp.status(), StatusCode::CREATED);
        assert_eq!(
            resp.headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok()),
            Some("application/json")
        );
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body");
        assert_eq!(&bytes[..], br#"{"id":"s1"}"#);
    }

    #[tokio::test]
    async fn a_text_outcome_becomes_a_bare_string_at_its_status() {
        let resp = RouteOutcome::text(404, "session zzz not found").into_response();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body");
        assert_eq!(&bytes[..], b"session zzz not found");
    }

    #[test]
    fn an_impossible_status_degrades_to_500_rather_than_panicking() {
        let resp = RouteOutcome::text(9999, "nonsense").into_response();
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn a_json_error_response_round_trips_into_an_outcome() {
        let resp = (
            StatusCode::NOT_FOUND,
            axum::Json(serde_json::json!({"error": "no project"})),
        )
            .into_response();
        let outcome = outcome_from_response(resp).await;
        assert_eq!(outcome.status, 404);
        assert_eq!(
            outcome.body,
            RouteBody::Json(serde_json::json!({"error": "no project"}))
        );
    }

    #[tokio::test]
    async fn a_text_error_response_round_trips_as_text() {
        let resp = (StatusCode::CONFLICT, "already adopted").into_response();
        let outcome = outcome_from_response(resp).await;
        assert_eq!(outcome.status, 409);
        assert_eq!(outcome.body, RouteBody::Text("already adopted".to_owned()));
    }
}
