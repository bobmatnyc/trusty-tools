//! Settle one registration trusty-search could not check (#6423).
//!
//! Why: #6371 made the census's `indeterminate` rows visible and deliberately
//! unselectable — a root the daemon declined to judge is not a root it called
//! gone, and offering the list for deletion is how an unplugged volume loses its
//! whole index roster. That rule holds. What it left with no exit is the class
//! that can never become valid again: six registrations under a retired
//! `.base/.worktrees/` tree whose parent is also gone, which the daemon reports
//! as "may become valid again" forever because the heuristic cannot know the
//! topology was retired. The operator could read those rows and do nothing else
//! with them.
//!
//! What: `POST /api/console/search/deregister-unjudged` settles exactly ONE
//! reviewed row. It is a per-row action on purpose — the batch prune still reads
//! `orphans` alone and cannot reach these rows, so nothing sweeps them in.
//! Deregistration goes through [`delete_index_on_socket`], the same
//! `search.index.delete` every other console delete uses; this route adds no
//! deletion of its own.
//!
//! Two things it refuses rather than guesses:
//!
//! - **The row must still be one the daemon cannot check.** The review is
//!   human-paced, and an id is derived from its root path, so the answer can
//!   change underneath it. [`OrphanGuard::unjudged_root`] re-reads the census
//!   immediately before the delete, and the request carries the path the
//!   operator actually read so a root that moved refuses instead of deleting.
//! - **The data is never destroyed.** `delete_data` is passed explicitly
//!   `false`. The root is not on disk, so there is nothing beside it to reclaim,
//!   and an explicit argument cannot be moved by a change to the daemon's own
//!   default.
//!
//! Test: the `deregister_*` tests below, plus
//! `deregister_route_rejects_a_traversal_id` for the pre-dial guard.

use std::path::{Path, PathBuf};

use axum::extract::State;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::json;

use crate::routes::SEARCH_SERVICE_ID;
use crate::routes::census_guard::OrphanGuard;
use crate::routes::deletes::delete_index_on_socket;
use crate::routes::verdict::{ActionVerdict, validate_id};
use crate::server::AppState;

/// The body `POST /api/console/search/deregister-unjudged` takes.
///
/// Why `root_path` is required rather than optional: it is the operator's half
/// of the agreement. The console shows a path in the review panel, and this is
/// the assertion that the path shown is the path being settled. Without it the
/// request would carry only an id, and an id outlives the root it was derived
/// from.
#[derive(Debug, Deserialize)]
pub struct DeregisterUnjudgedRequest {
    /// The registration id, from the census's `indeterminate` list.
    id: String,
    /// The `root_path` the operator reviewed for that id.
    root_path: String,
}

/// Deregister one reviewed row, or say why it did not happen.
///
/// Why: the whole action lives here so the refusals are testable without a
/// router — and they are most of the behaviour. A reviewed row that moved, a
/// census that could not be re-read, and a daemon that skipped the delete all
/// have to read as failures, because a deregistration reported as done that did
/// not happen leaves the operator believing the registration is gone.
/// What: re-censuses through [`OrphanGuard`], requires the daemon to still
/// decline judging this id AND to report the reviewed path, then delegates to
/// [`delete_index_on_socket`] with `delete_data: false` and the root pinned.
/// Test: `deregister_settles_a_row_the_daemon_still_cannot_check`,
/// `deregister_refuses_a_row_whose_root_moved_since_the_review`,
/// `deregister_refuses_a_row_the_census_now_calls_stale`,
/// `deregister_reports_a_dead_daemon_as_unreachable`.
pub(crate) async fn deregister_unjudged_on_socket(
    socket: &Path,
    id: &str,
    reviewed_root: &str,
) -> ActionVerdict {
    if let Err(reason) = validate_id(id) {
        return ActionVerdict::Invalid {
            id: id.to_string(),
            reason,
        };
    }

    let current_root = match OrphanGuard::new(socket).unjudged_root(id).await {
        Ok(root) => root,
        Err(reason) => {
            return ActionVerdict::Refused {
                id: id.to_string(),
                reason,
                detail: json!({ "reviewed_root_path": reviewed_root }),
            };
        }
    };

    if current_root != reviewed_root {
        return ActionVerdict::Refused {
            id: id.to_string(),
            reason: format!(
                "not deregistered: '{id}' now names {current_root}, not the {reviewed_root} \
                 that was reviewed"
            ),
            detail: json!({
                "reviewed_root_path": reviewed_root,
                "current_root_path": current_root,
            }),
        };
    }

    // #6423: `delete_data` is false EXPLICITLY. A root that is not on disk has
    // no data beside it to destroy, and an explicit argument cannot be moved by
    // a change to the daemon's default.
    delete_index_on_socket(socket, id, false, Some(&current_root)).await
}

/// `POST /api/console/search/deregister-unjudged` — settle one reviewed row.
///
/// Why: the counterpart to the batch prune for the rows the batch may not touch.
/// One id per request, because the review this acts on is per-row.
/// What: validates the id and the reviewed path BEFORE dialling anything, then
/// resolves trusty-search's socket the way every other console action does
/// (#6285) and runs [`deregister_unjudged_on_socket`]. Refreshes the search
/// metrics cache on success so the roster the UI re-fetches reflects the change.
/// Test: `deregister_route_rejects_a_traversal_id`,
/// `deregister_route_rejects_an_empty_reviewed_path`,
/// `deregister_route_reports_an_unreachable_daemon`.
pub async fn deregister_unjudged_handler(
    State(state): State<AppState>,
    axum::Json(req): axum::Json<DeregisterUnjudgedRequest>,
) -> Response {
    // Before resolving the daemon, for the reason #6360 records: a resolution
    // failure answered first would mask the id as the real problem.
    if let Err(reason) = validate_id(&req.id) {
        return ActionVerdict::Invalid { id: req.id, reason }.into_response();
    }
    if req.root_path.trim().is_empty() {
        return ActionVerdict::Invalid {
            id: req.id,
            reason: "the request named no reviewed root path, so there is nothing to \
                     check the registration against"
                .to_string(),
        }
        .into_response();
    }

    let socket: PathBuf = match state.search_socket_path() {
        Ok(p) => p,
        Err(reason) => {
            return ActionVerdict::Unreachable {
                id: req.id,
                reason: format!("could not resolve the {SEARCH_SERVICE_ID} socket path: {reason}"),
            }
            .into_response();
        }
    };

    let verdict = deregister_unjudged_on_socket(&socket, &req.id, &req.root_path).await;
    if verdict.succeeded() {
        crate::routes::deletes::refresh_metrics(
            &state,
            SEARCH_SERVICE_ID,
            state.search_metrics_cache(),
        )
        .await;
    }
    verdict.into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt as _;
    use serde_json::Value;
    use tower::ServiceExt as _;

    use crate::server::build_router;

    /// A stub trusty-search whose census reports one unjudged row and one stale
    /// row, and whose delete removes anything it is asked to remove.
    ///
    /// Why the delete is unconditionally successful: these tests are about the
    /// guard in front of it. The delete's own verdicts already have coverage in
    /// `deletes` and in the prune batch.
    fn stub_search_socket_with(dir: &Path, unjudged_root: &str) -> PathBuf {
        let unjudged_root = unjudged_root.to_string();
        crate::routes::deletes::tests::stub_search_socket(dir, move |request: &Value| {
            let result = if request["method"] == json!("search.registry.orphans") {
                json!({
                    "orphans": [{ "id": "wiped", "root_path": "/gone/wiped" }],
                    "indeterminate": [{
                        "id": "retired",
                        "root_path": unjudged_root,
                        "reason": "the root is missing and so is its parent directory",
                        "colocated": false,
                        "repo_identity": null,
                    }],
                    "live_count": 0,
                    "total": 2,
                })
            } else {
                json!({
                    "id": request["params"]["index_id"].clone(),
                    "removed": true,
                    "data_deleted": request["params"]["delete_data"].clone(),
                    "quiesced": true,
                })
            };
            json!({ "jsonrpc": "2.0", "id": 1, "result": result })
        })
    }

    /// Why (#6423 closure condition 1): the action the issue asks for. A row the
    /// daemon still cannot check, reviewed at the path it reports, is settled.
    /// Test: this is the test.
    #[tokio::test(flavor = "multi_thread")]
    async fn deregister_settles_a_row_the_daemon_still_cannot_check() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let socket = stub_search_socket_with(tmp.path(), "/retired/.base/.worktrees/x");

        let verdict =
            deregister_unjudged_on_socket(&socket, "retired", "/retired/.base/.worktrees/x").await;
        assert!(verdict.succeeded(), "{verdict:?}");
    }

    /// Why (#6423): the reviewed path is the operator's assertion about what
    /// they are settling. A root that changed between the review and the action
    /// means they are looking at a different registration than the one they
    /// would remove.
    /// Test: this is the test.
    #[tokio::test(flavor = "multi_thread")]
    async fn deregister_refuses_a_row_whose_root_moved_since_the_review() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let socket = stub_search_socket_with(tmp.path(), "/retired/now-somewhere-else");

        let verdict =
            deregister_unjudged_on_socket(&socket, "retired", "/retired/.base/.worktrees/x").await;
        assert!(!verdict.succeeded(), "a moved root must refuse");
        assert!(
            verdict.reason().contains("now names")
                && verdict.reason().contains("/retired/.base/.worktrees/x"),
            "the refusal must name both paths: {}",
            verdict.reason()
        );
    }

    /// Why (#6423, fail-closed): the review path may only touch the census's
    /// `indeterminate` list. An id that became a plain stale candidate goes
    /// through the batch prune, which has its own confirm step.
    /// Test: this is the test.
    #[tokio::test(flavor = "multi_thread")]
    async fn deregister_refuses_a_row_the_census_now_calls_stale() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let socket = stub_search_socket_with(tmp.path(), "/retired/x");

        let verdict = deregister_unjudged_on_socket(&socket, "wiped", "/gone/wiped").await;
        assert!(!verdict.succeeded(), "a stale row must refuse this route");
        assert!(
            verdict.reason().contains("stale registration"),
            "the refusal must say where the row went: {}",
            verdict.reason()
        );
    }

    /// Why (#6423): a failed deregistration is reported as failed. A daemon that
    /// never answered has not removed anything, and reading that as done leaves
    /// the operator believing a registration is gone.
    /// Test: this is the test.
    #[tokio::test(flavor = "multi_thread")]
    async fn deregister_reports_a_dead_daemon_as_unreachable() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let verdict =
            deregister_unjudged_on_socket(&tmp.path().join("absent.sock"), "retired", "/retired/x")
                .await;
        assert!(!verdict.succeeded(), "a dead daemon must not read as done");
        assert!(
            verdict.reason().contains("not deregistered"),
            "the refusal must say the deregistration did not happen: {}",
            verdict.reason()
        );
    }

    /// Drive the real router with trusty-search pointed at a socket nothing is
    /// bound to, so a route test can never reach a live daemon (#6285).
    async fn post_through_router(body: Value) -> (StatusCode, Value) {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let router =
            build_router(AppState::new(vec![]).with_search_socket(tmp.path().join("absent.sock")));
        let req = Request::builder()
            .method("POST")
            .uri("/api/console/search/deregister-unjudged")
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .expect("request");
        let resp = router.oneshot(req).await.expect("response");
        let status = resp.status();
        let bytes = resp.into_body().collect().await.expect("body").to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    /// Why: the id allowlist runs before anything is dialled, so a traversal id
    /// is a `400` from the console rather than a path handed to a daemon.
    /// Test: this is the test.
    #[tokio::test(flavor = "multi_thread")]
    async fn deregister_route_rejects_a_traversal_id() {
        let (status, body) =
            post_through_router(json!({ "id": "../etc", "root_path": "/gone" })).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["ok"], json!(false));
    }

    /// Why (#6423): without the reviewed path there is nothing to check the
    /// registration against, so the request is malformed rather than merely
    /// unlucky.
    /// Test: this is the test.
    #[tokio::test(flavor = "multi_thread")]
    async fn deregister_route_rejects_an_empty_reviewed_path() {
        let (status, body) =
            post_through_router(json!({ "id": "retired", "root_path": "   " })).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(
            body["error"].as_str().unwrap_or_default().contains("root"),
            "the refusal must name the missing field: {body}"
        );
    }
}
