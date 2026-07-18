//! `GET /fs` REST route over the `fs.list_dir` JSON-RPC method (#2983
//! Slice 5).
//!
//! Why: the daemon-side folder picker (`crate::fs_browse::protocol` — screen
//! 7a) is the other half of the local-development surface a REST-only
//! client needs alongside `session.*`/`task.*`. As with every route in this
//! gateway, the handler stays thin — extract the query params, call
//! `super::respond` — because the actual behaviour (path expansion/
//! canonicalization, the `NotFound`/`PermissionDenied`/`NotADirectory`/
//! `NoHome`/`Io` error taxonomy) already lives in
//! `crate::fs_browse::protocol::list_dir`; duplicating it here would fork
//! the two surfaces (see `super` module docs). No path-scoping/validation
//! logic belongs in this handler either — `fs.list_dir` intentionally
//! carries none (`crate::fs_browse` module docs: tcode is a local app run by
//! the operator, not a multi-tenant service), and this route must not
//! second-guess that decision.
//! What: [`routes`] builds a standalone `axum::Router<()>` (its own `FsState`
//! carrying just the `Arc<Router>`, `with_state`-erased before return,
//! mirroring `super::sessions::SessionsState`) mapping
//! `GET /fs?path=..&include_hidden=..` -> `fs.list_dir`. The simpler
//! `GET /fs` shape (rather than `GET /fs/list`) was chosen because no spec in
//! this repo mandates a `/list` suffix: DOC-39 §5.0 ("Today's surface")
//! explicitly says "there is no REST resource surface", and its §5.8
//! (`project.list_dir`, predating the #2983 REST gateway) proposes that
//! method as "a new method on the existing `POST /rpc` route — no new HTTP
//! route" rather than any REST shape at all.
//! `crate::serve::http::build_axum_router` merges this into the daemon's main
//! router alongside `POST /rpc`, `GET /health`, `GET /sessions/{id}/events`,
//! and the other REST resource groups — `/fs` collides with none of them.
//! Test: `tests::*`.

use std::sync::Arc;

use axum::Router as AxumRouter;
use axum::extract::{Query, State};
use axum::routing::get;
use serde::Deserialize;
use serde_json::json;

use crate::jsonrpc::Router;

use super::{RestResult, respond};

/// Shared axum state for the route in this module: just the JSON-RPC
/// router, mirroring `super::sessions::SessionsState` — the handler goes
/// through [`super::respond`] rather than touching the filesystem directly.
#[derive(Clone)]
struct FsState {
    router: Arc<Router>,
}

/// Build the `GET /fs` route group.
///
/// Why: kept separate from `crate::serve::http::build_axum_router` so this
/// resource group is unit-testable via `tower::util::ServiceExt::oneshot` on
/// its own, exactly like every other `rest::*` group.
/// What: one `GET` route, `FsState { router }`, `with_state`-erased to
/// `axum::Router<()>` so the caller can `.merge()` it alongside the other
/// resource groups into the daemon's main router.
/// Test: `tests::list_dir_returns_200_with_entries`.
pub fn routes(router: Arc<Router>) -> AxumRouter {
    AxumRouter::new()
        .route("/fs", get(list_dir))
        .with_state(FsState { router })
}

/// Query params for `GET /fs`, shaped like `fs_browse::protocol`'s private
/// `ListDirParams` — forwarded verbatim into the RPC `params` `Value`, so
/// `fs.list_dir`'s own defaulting (`path` -> `~`, `include_hidden` -> `false`)
/// applies identically either way.
#[derive(Deserialize)]
struct ListDirQuery {
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    include_hidden: Option<bool>,
}

/// `GET /fs?path=..&include_hidden=..` -> `fs.list_dir`.
///
/// Why: the REST entry point for the daemon-side folder picker.
/// What: `200` with the `DirListing` JSON on success; error mapping relies
/// entirely on `super::rpc_error_to_status`, which already distinguishes
/// `fs.list_dir`'s four caller-actionable causes: an unknown `path` ->
/// `-32002 not_found` -> `404`; a `path` that exists but is not a directory
/// (or an unresolvable `~`) -> `-32003 invalid_argument` -> `400`; an OS
/// permission refusal -> `-32001 permission_denied` -> `403`; any other OS
/// fault -> `-32603 internal` -> `500` — four distinct statuses for four
/// distinct causes, with no new mapping needed in this route (see
/// `fs_browse::protocol::to_rpc_error`'s docs for the source taxonomy).
/// Test: `tests::list_dir_returns_200_with_entries`,
/// `tests::list_dir_nonexistent_path_returns_404`,
/// `tests::list_dir_file_path_returns_400_distinct_from_404`.
async fn list_dir(State(state): State<FsState>, Query(q): Query<ListDirQuery>) -> RestResult {
    respond(
        &state.router,
        "fs.list_dir",
        json!({"path": q.path, "include_hidden": q.include_hidden}),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode};
    use serde_json::Value;
    use tower::util::ServiceExt;

    fn app() -> AxumRouter {
        let mut router = Router::new();
        crate::fs_browse::protocol::register(&mut router);
        routes(Arc::new(router))
    }

    async fn body_json(response: axum::response::Response) -> Value {
        let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    async fn get(app: &AxumRouter, uri: &str) -> axum::response::Response {
        app.clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(uri)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    /// A real directory must return `200` with its entries.
    #[tokio::test]
    async fn list_dir_returns_200_with_entries() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(tmp.path().join("alpha")).expect("mkdir");

        let resp = get(&app(), &format!("/fs?path={}", tmp.path().display())).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert_eq!(v["entries"][0]["name"], "alpha");
    }

    /// A path that does not exist must be a real `404` with a `not_found`
    /// envelope.
    #[tokio::test]
    async fn list_dir_nonexistent_path_returns_404() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let missing = tmp.path().join("no-such-dir");

        let resp = get(&app(), &format!("/fs?path={}", missing.display())).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let v = body_json(resp).await;
        assert_eq!(v["error"]["code"], -32002);
    }

    /// A path that exists but is a FILE must be a `400`, distinct from the
    /// `404` above — proving the two RPC error codes
    /// (`not_found`/`invalid_argument`) reach the client as two distinct HTTP
    /// statuses, not a single generic "bad path" response.
    #[tokio::test]
    async fn list_dir_file_path_returns_400_distinct_from_404() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let file = tmp.path().join("a.txt");
        std::fs::write(&file, "x").expect("write");

        let resp = get(&app(), &format!("/fs?path={}", file.display())).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        assert_ne!(resp.status(), StatusCode::NOT_FOUND);
        let v = body_json(resp).await;
        assert_eq!(v["error"]["code"], -32003);
    }
}
