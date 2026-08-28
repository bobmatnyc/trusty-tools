//! Operator-driven cleanup: prune stale index registrations, compact a palace
//! (#6371).
//!
//! Why: #6360 gave the dashboard one delete per row, which is the wrong shape
//! for the leak these routes exist for. A host accumulates dozens of index
//! registrations whose root was wiped (#4255) — 60 of them on the owner's
//! machine, each keeping `warm_boot_degraded` true — and clearing them one row
//! at a time is why they were still there. On the memory side the daemon has
//! always been able to reclaim a palace's orphaned vectors and the dashboard
//! had no way to ask it to.
//!
//! What: two routes.
//!   - `POST /api/console/search/prune-indexes` takes the ids an operator
//!     CONFIRMED and deletes them one at a time through
//!     [`crate::routes::deletes::delete_index_on_daemon`] — the same
//!     `DELETE /indexes/{id}` a single-row delete uses, which since #6365
//!     also removes a registration the warm-boot allowlist excluded. This
//!     route discovers nothing and selects nothing: the candidate list comes
//!     from the daemon's own `GET /registry/orphans` census, which the UI
//!     reaches through the console's reverse proxy so there is no second answer
//!     to "which registrations are stale".
//!   - `POST /api/console/memory/palaces/{id}/compact` calls trusty-memory's
//!     `palace_compact` over its socket.
//!
//! Neither route reports work it did not observe. The prune answers one row per
//! id with that id's own outcome, so a batch where three ids succeeded and one
//! was refused reads as exactly that — never as "cleaned". A batch that runs
//! past its budget reports the ids it never attempted as not attempted, rather
//! than leaving a caller to infer it from a missing row.
//!
//! Test: `prune_*` and `compact_*` in the `tests` module below.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use axum::extract::{Path as AxumPath, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::routes::deletes::delete_index_on_daemon;
use crate::routes::memory_rpc;
use crate::routes::verdict::{ActionVerdict, validate_id};
use crate::routes::{MEMORY_SERVICE, SEARCH_SERVICE_ID};
use crate::server::AppState;

/// Most registrations one prune request may delete.
///
/// Why: each id is a separate round trip to the daemon, and an unbounded list
/// would let one request hold a console worker for an unbounded time. A host
/// with more stale registrations than this prunes them in two passes, which the
/// census makes obvious because the leftovers are still listed.
const MAX_PRUNE_BATCH: usize = 100;

/// How long a whole prune batch may run before it stops attempting ids.
///
/// Why: the per-id timeout bounds one delete, not a hundred of them. Without a
/// batch budget a request could sit for the product of the two, and the
/// operator would have no answer at all rather than a partial one they can act
/// on. Ids past the budget are REPORTED, not silently dropped.
const PRUNE_BUDGET: Duration = Duration::from_secs(120);

// ─── trusty-search: prune a confirmed batch of registrations ────────────────

/// What one prune batch did, per id.
///
/// Why: a batch has no single outcome. `removed` and `failed` are counts a UI
/// can headline, and `rows` is what it must actually render — an operator whose
/// batch half-worked needs to know WHICH half.
/// Test: `prune_reports_per_item_outcomes_for_a_partial_batch`.
#[derive(Debug)]
pub(crate) struct PruneOutcome {
    /// One row per requested id, in request order.
    pub(crate) rows: Vec<Value>,
    /// How many ids the daemon confirmed it removed.
    pub(crate) removed: usize,
    /// How many ids did not get removed, for any reason.
    pub(crate) failed: usize,
}

impl PruneOutcome {
    /// The body this outcome answers with.
    ///
    /// `ok` is true only when every requested id was removed — a batch with one
    /// failure is not a successful cleanup, and a UI that reads `ok` must not be
    /// told otherwise.
    fn body(&self) -> Value {
        json!({
            "ok": self.failed == 0,
            "removed": self.removed,
            "failed": self.failed,
            "results": self.rows,
        })
    }

    /// `200` for a clean batch, `409` when any id was not removed.
    ///
    /// Mirrors the single-delete contract: the daemon's state prevented the
    /// work and the operator can act on it.
    fn status(&self) -> StatusCode {
        if self.failed == 0 {
            StatusCode::OK
        } else {
            StatusCode::CONFLICT
        }
    }
}

/// Delete each id in turn and record what the daemon said about it.
///
/// Why: this is the whole prune. It holds no idea of what "stale" means — the
/// daemon's census decides that and the operator confirms it — and it adds no
/// deletion path, calling the same [`delete_index_on_daemon`] a single-row
/// delete uses. Its one job is to not lose a per-id outcome.
/// What: walks `ids` in order. Past `deadline` an id is not attempted and says
/// so. Every id produces exactly one row carrying `ok` and, when it failed, the
/// daemon's own message.
/// Test: `prune_reports_per_item_outcomes_for_a_partial_batch`,
/// `prune_reports_an_expired_budget_as_unattempted`,
/// `prune_of_a_clean_batch_reports_every_id_removed`.
pub(crate) async fn prune_indexes_on_daemon(
    client: &reqwest::Client,
    base_url: &str,
    ids: &[String],
    delete_data: bool,
    deadline: Instant,
) -> PruneOutcome {
    let mut outcome = PruneOutcome {
        rows: Vec::with_capacity(ids.len()),
        removed: 0,
        failed: 0,
    };

    for id in ids {
        if Instant::now() >= deadline {
            outcome.failed += 1;
            outcome.rows.push(json!({
                "id": id,
                "ok": false,
                "error": format!(
                    "not attempted: the prune batch exceeded its {}s budget",
                    PRUNE_BUDGET.as_secs()
                ),
            }));
            continue;
        }

        let verdict = delete_index_on_daemon(client, base_url, id, delete_data).await;
        if verdict.succeeded() {
            outcome.removed += 1;
            outcome.rows.push(json!({ "id": verdict.id(), "ok": true }));
        } else {
            outcome.failed += 1;
            outcome.rows.push(json!({
                "id": verdict.id(),
                "ok": false,
                "error": verdict.reason(),
            }));
        }
    }

    outcome
}

/// The body `POST /api/console/search/prune-indexes` takes.
#[derive(Debug, Default, Deserialize)]
pub struct PruneRequest {
    /// The registration ids the operator confirmed, from the daemon's census.
    #[serde(default)]
    ids: Vec<String>,
    /// Destroy each index's on-disk corpus as well as its registration.
    ///
    /// Absent ⇒ `false`, matching trusty-search's own contract since #4123: a
    /// bare delete deregisters and preserves the data. A stale registration's
    /// data is usually worth reclaiming too, but that is the operator's call to
    /// make in the confirm step, not this route's default.
    #[serde(default)]
    delete_data: bool,
}

/// `POST /api/console/search/prune-indexes` — remove confirmed registrations.
///
/// The path is `prune-indexes` rather than `indexes/prune` on purpose: a static
/// `prune` segment beside `indexes/{id}` would shadow an index literally named
/// `prune` and leave it undeletable from the console.
///
/// Why: the counterpart to the per-row delete, for the case the per-row delete
/// is unusable in — dozens of dead registrations at once.
/// What: refuses an empty or oversized list and any id outside the console's id
/// allowlist BEFORE dialling anything, so a malformed batch never partially
/// executes. Then resolves trusty-search's loopback address from the same
/// poller cache the single delete uses and prunes under [`PRUNE_BUDGET`].
/// Refreshes the search metrics cache when anything was removed, so the roster
/// the UI re-fetches reflects the prune.
/// Test: `prune_route_rejects_an_empty_batch`,
/// `prune_route_rejects_a_bad_id_without_dialling`,
/// `prune_route_rejects_an_oversized_batch`,
/// `prune_route_reports_an_unresolved_daemon_as_unreachable`.
pub async fn prune_indexes_handler(
    State(state): State<AppState>,
    axum::Json(req): axum::Json<PruneRequest>,
) -> Response {
    if req.ids.is_empty() {
        return bad_request("the prune request named no registration ids");
    }
    if req.ids.len() > MAX_PRUNE_BATCH {
        return bad_request(&format!(
            "the prune request named {} ids; at most {MAX_PRUNE_BATCH} may be pruned at once",
            req.ids.len()
        ));
    }
    // #6371: validate EVERY id before dialling. A batch that fails halfway
    // through on a malformed id has already deleted the ids ahead of it, and
    // the operator confirmed a list, not a prefix of one.
    for id in &req.ids {
        if let Err(reason) = validate_id(id) {
            return bad_request(&format!(
                "id {id:?} is not one this console will forward: {reason}"
            ));
        }
    }

    let base_url = match state.poller_cache().snapshot().await {
        Some(snap) => snap.url_map().get(SEARCH_SERVICE_ID).cloned(),
        None => None,
    };
    let Some(base_url) = base_url else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            axum::Json(json!({
                "ok": false,
                "error": format!(
                    "{SEARCH_SERVICE_ID} is not reachable: the console has no live address for it"
                ),
            })),
        )
            .into_response();
    };

    let client = state.http_client();
    let outcome = prune_indexes_on_daemon(
        &client,
        &base_url,
        &req.ids,
        req.delete_data,
        Instant::now() + PRUNE_BUDGET,
    )
    .await;

    if outcome.removed > 0 {
        crate::routes::deletes::refresh_metrics(
            &state,
            SEARCH_SERVICE_ID,
            state.search_metrics_cache(),
        )
        .await;
    }
    (outcome.status(), axum::Json(outcome.body())).into_response()
}

/// A `400` carrying `ok: false` and a reason, in the shape the UI reads.
fn bad_request(reason: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        axum::Json(json!({ "ok": false, "error": reason })),
    )
        .into_response()
}

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

    /// Start a stub trusty-search whose `DELETE /indexes/{id}` answer depends on
    /// the id: `doomed-*` is removed, anything else is a skipped no-op.
    ///
    /// Why per-id: a batch's whole contract is that one id's failure does not
    /// change another id's row, and a stub that answers every id identically
    /// cannot show that.
    async fn stub_search_daemon_removing_only_doomed() -> String {
        let app = axum::Router::new().route(
            "/indexes/{id}",
            axum::routing::delete(
                |axum::extract::Path(id): axum::extract::Path<String>| async move {
                    let removed = id.starts_with("doomed-");
                    let body = json!({
                        "id": id,
                        "removed": removed,
                        "data_deleted": false,
                        "quiesced": true,
                    });
                    (StatusCode::OK, axum::Json(body))
                },
            ),
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
    fn client() -> reqwest::Client {
        (*AppState::new(vec![]).http_client()).clone()
    }

    fn ids(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    async fn post_through_router(uri: &str, body: Value) -> (StatusCode, Value) {
        let router = build_router(AppState::new(vec![]));
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

    // ── prune: per-item outcomes ─────────────────────────────────────────────

    /// Why (#6371): the failure this route exists to not have — a batch where
    /// one delete was skipped reporting the whole batch as cleaned. Each id must
    /// carry its OWN outcome, and the one that failed must carry the daemon's
    /// own words.
    /// Test: this is the test.
    #[tokio::test(flavor = "multi_thread")]
    async fn prune_reports_per_item_outcomes_for_a_partial_batch() {
        let base = stub_search_daemon_removing_only_doomed().await;
        let outcome = prune_indexes_on_daemon(
            &client(),
            &base,
            &ids(&["doomed-a", "survivor", "doomed-b"]),
            false,
            Instant::now() + PRUNE_BUDGET,
        )
        .await;

        assert_eq!(outcome.removed, 2, "{outcome:?}");
        assert_eq!(outcome.failed, 1, "{outcome:?}");

        let body = outcome.body();
        assert_eq!(
            body["ok"],
            json!(false),
            "a batch with one failure is not a successful cleanup: {body}"
        );
        let rows = body["results"].as_array().expect("rows");
        assert_eq!(rows.len(), 3, "one row per requested id: {body}");
        assert_eq!(rows[0]["id"], json!("doomed-a"));
        assert_eq!(rows[0]["ok"], json!(true));
        assert_eq!(rows[1]["id"], json!("survivor"));
        assert_eq!(rows[1]["ok"], json!(false));
        assert!(
            rows[1]["error"]
                .as_str()
                .unwrap_or_default()
                .contains("skipped the delete"),
            "the failed row must carry the daemon's own words: {body}"
        );
        assert_eq!(
            rows[2]["ok"],
            json!(true),
            "a later id is unaffected: {body}"
        );
    }

    /// Why: the clean path must be reachable and must answer `ok: true`.
    /// Test: this is the test.
    #[tokio::test(flavor = "multi_thread")]
    async fn prune_of_a_clean_batch_reports_every_id_removed() {
        let base = stub_search_daemon_removing_only_doomed().await;
        let outcome = prune_indexes_on_daemon(
            &client(),
            &base,
            &ids(&["doomed-a", "doomed-b"]),
            false,
            Instant::now() + PRUNE_BUDGET,
        )
        .await;

        assert_eq!(outcome.removed, 2);
        assert_eq!(outcome.failed, 0);
        assert_eq!(outcome.status(), StatusCode::OK);
        assert_eq!(outcome.body()["ok"], json!(true));
    }

    /// Why: an unreachable daemon must fail every row rather than produce a
    /// short batch that reads as a partial success.
    /// Test: this is the test.
    #[tokio::test(flavor = "multi_thread")]
    async fn prune_reports_a_dead_daemon_as_a_failure_on_every_id() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        drop(listener);

        let outcome = prune_indexes_on_daemon(
            &client(),
            &format!("http://{addr}"),
            &ids(&["a", "b"]),
            false,
            Instant::now() + PRUNE_BUDGET,
        )
        .await;

        assert_eq!(outcome.removed, 0);
        assert_eq!(outcome.failed, 2);
        for row in outcome.body()["results"].as_array().expect("rows") {
            assert_eq!(row["ok"], json!(false));
            assert!(
                !row["error"].as_str().unwrap_or_default().is_empty(),
                "every failed row must say why: {row}"
            );
        }
    }

    /// Why (#6371): an id the batch ran out of time for is NOT removed, and the
    /// operator has to be told which ones those were. A missing row would leave
    /// them to infer it.
    /// Test: this is the test — the deadline is already past when the batch
    /// starts, so no id is attempted.
    #[tokio::test(flavor = "multi_thread")]
    async fn prune_reports_an_expired_budget_as_unattempted() {
        let base = stub_search_daemon_removing_only_doomed().await;
        let outcome = prune_indexes_on_daemon(
            &client(),
            &base,
            &ids(&["doomed-a", "doomed-b"]),
            false,
            Instant::now(),
        )
        .await;

        assert_eq!(outcome.removed, 0, "nothing may be deleted past the budget");
        assert_eq!(outcome.failed, 2);
        let body = outcome.body();
        for row in body["results"].as_array().expect("rows") {
            assert!(
                row["error"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("not attempted"),
                "an unattempted id must say so: {body}"
            );
        }
    }

    /// Why: `delete_data` is the operator's choice in the confirm step and must
    /// reach the daemon; a batch that silently deregistered while the operator
    /// asked for the bytes would report a corpus as reclaimed while it is still
    /// on disk (#3049).
    /// Test: this is the test — the stub answers `data_deleted: false`, which
    /// the underlying delete refuses only when the data was actually asked for.
    #[tokio::test(flavor = "multi_thread")]
    async fn prune_forwards_the_delete_data_choice() {
        let base = stub_search_daemon_removing_only_doomed().await;
        let without = prune_indexes_on_daemon(
            &client(),
            &base,
            &ids(&["doomed-a"]),
            false,
            Instant::now() + PRUNE_BUDGET,
        )
        .await;
        assert_eq!(without.removed, 1, "a deregister-only prune succeeds");

        let with = prune_indexes_on_daemon(
            &client(),
            &base,
            &ids(&["doomed-a"]),
            true,
            Instant::now() + PRUNE_BUDGET,
        )
        .await;
        assert_eq!(
            with.failed, 1,
            "asking for the data and not getting it is a failure, not a success"
        );
    }

    // ── prune: route wiring ──────────────────────────────────────────────────

    /// Why: an empty batch is a caller bug, and answering `200 ok` for it would
    /// report a cleanup that removed nothing as a cleanup.
    /// Test: this is the test.
    #[tokio::test]
    async fn prune_route_rejects_an_empty_batch() {
        let (status, body) =
            post_through_router("/api/console/search/prune-indexes", json!({ "ids": [] })).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "body: {body}");
        assert_eq!(body["ok"], json!(false));
    }

    /// Why (#6371): a malformed id must stop the WHOLE batch before any delete
    /// runs. Validating per-id inside the loop would delete the ids ahead of it
    /// and then refuse — the operator confirmed a list, not a prefix.
    /// Test: this is the test; the 400 proves validation ran ahead of the daemon
    /// resolution that would otherwise answer 503 against an empty AppState.
    #[tokio::test]
    async fn prune_route_rejects_a_bad_id_without_dialling() {
        let (status, body) = post_through_router(
            "/api/console/search/prune-indexes",
            json!({ "ids": ["good-one", "../etc"] }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "body: {body}");
        assert!(
            body["error"].as_str().unwrap_or_default().contains(".."),
            "the error must name the offending id: {body}"
        );
    }

    /// Why: an unbounded batch would hold a console worker for an unbounded
    /// time; the cap has to be enforced, not documented.
    /// Test: this is the test.
    #[tokio::test]
    async fn prune_route_rejects_an_oversized_batch() {
        let many: Vec<String> = (0..=MAX_PRUNE_BATCH).map(|n| format!("idx-{n}")).collect();
        let (status, body) =
            post_through_router("/api/console/search/prune-indexes", json!({ "ids": many })).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "body: {body}");
    }

    /// Why: with no poller snapshot the route must say trusty-search is
    /// unreachable rather than claim a prune.
    /// Test: this is the test.
    #[tokio::test]
    async fn prune_route_reports_an_unresolved_daemon_as_unreachable() {
        let (status, body) = post_through_router(
            "/api/console/search/prune-indexes",
            json!({ "ids": ["scratch"] }),
        )
        .await;
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
        for uri in [
            "/api/console/search/prune-indexes",
            "/api/console/memory/palaces/scratch/compact",
        ] {
            let router = build_router(AppState::new(vec![]));
            let req = Request::builder()
                .method("POST")
                .uri(uri)
                .header("origin", "https://evil.example")
                .header("content-type", "application/json")
                .body(Body::from(json!({ "ids": ["scratch"] }).to_string()))
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
