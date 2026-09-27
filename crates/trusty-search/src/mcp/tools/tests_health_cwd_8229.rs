//! `search_health`'s working-directory fallback resolves by root, not by the
//! bare-basename id (#8229).
//!
//! Why: two checkouts named alike derive one id, and the fallback probed that
//! id without comparing roots — it reported a different clone's index as this
//! project's with `healthy: true`.
//! What: two scratch checkouts both named `api` under a loopback daemon that
//! serves `/health`, `/indexes?details=true` and per-id status. Each test first
//! asserts the pair collides under `derive_index_id`, then asserts the
//! resolution `search_health` reports for each tree.
//! Test: this file.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use super::health::{report_health, resolve_scope};
use super::{McpServer, HEALTH_INDEX_NOT_REGISTERED, HEALTH_OK};

/// A loopback daemon serving `/health`, the detailed index list, and a status
/// body per id (404 for an id it does not hold).
async fn spawn_daemon(entries: Vec<(&str, &Path, u64)>) -> String {
    use axum::extract::{Path as AxPath, State};
    use axum::http::StatusCode;
    use axum::routing::get;
    use axum::{Json, Router};
    use std::sync::Arc;

    let list: Vec<Value> = entries
        .iter()
        .map(|(id, root, _)| json!({ "id": id, "root_path": root.display().to_string() }))
        .collect();
    let counts: Arc<Vec<(String, u64)>> = Arc::new(
        entries
            .iter()
            .map(|(id, _, n)| ((*id).to_string(), *n))
            .collect(),
    );
    let indexes = Arc::new(json!({ "indexes": list }));

    let app = Router::new()
        .route(
            "/health",
            get(|| async { Json(json!({ "status": "ok", "version": "9.9.9", "indexes": 2 })) }),
        )
        .route(
            "/indexes",
            get({
                let indexes = Arc::clone(&indexes);
                move || async move { Json((*indexes).clone()) }
            }),
        )
        .route(
            "/indexes/{id}/status",
            get(
                |State(counts): State<Arc<Vec<(String, u64)>>>, AxPath(id): AxPath<String>| async move {
                    match counts.iter().find(|(known, _)| *known == id) {
                        Some((_, n)) => (StatusCode::OK, Json(json!({ "index_id": id, "chunk_count": n }))),
                        None => (StatusCode::NOT_FOUND, Json(json!({ "error": "unknown index" }))),
                    }
                },
            ),
        )
        .with_state(counts);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}")
}

/// Two checkouts named `api` under distinct parents, each a git root.
fn same_named_checkouts(tmp: &Path) -> (PathBuf, PathBuf) {
    let one = tmp.join("one").join("api");
    let two = tmp.join("two").join("api");
    for dir in [&one, &two] {
        std::fs::create_dir_all(dir.join(".git")).expect("create checkout");
    }
    // The collision #8229 is about: the old fallback derived this id for both.
    assert_eq!(
        trusty_common::derive_index_id(&one),
        trusty_common::derive_index_id(&two),
        "the fixture pair must collide under the bare-basename derivation"
    );
    (one, two)
}

/// Resolve and report as an unpinned session running in `cwd`.
async fn health_from(base: &str, cwd: &Path) -> Value {
    let server = McpServer::new(base.to_string());
    report_health(&server, resolve_scope(&server, &json!({}), Some(cwd))).await
}

/// Each of two same-basename checkouts resolves to the index rooted at it.
///
/// Before the fix both trees resolved to `api` with `resolved_from: "cwd"`,
/// so the second checkout was reported as the first one's 10-chunk index.
#[tokio::test(flavor = "multi_thread")]
async fn cwd_fallback_resolves_same_basename_checkouts_to_their_own_indexes() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (one, two) = same_named_checkouts(tmp.path());
    let base = spawn_daemon(vec![("api", &one, 10), ("api-2b1f00aa", &two, 20)]).await;

    let first = health_from(&base, &one).await;
    assert_eq!(first["status"], HEALTH_OK, "{first}");
    assert_eq!(first["index"]["index_id"], "api");
    assert_eq!(first["index"]["resolved_from"], "cwd");
    assert_eq!(first["index"]["chunk_count"], 10);

    let second = health_from(&base, &two).await;
    assert_eq!(second["status"], HEALTH_OK, "{second}");
    assert_eq!(
        second["index"]["index_id"], "api-2b1f00aa",
        "the second checkout must resolve to the index rooted at it: {second}"
    );
    assert_eq!(second["index"]["resolved_from"], "cwd_root_match");
    assert_eq!(second["index"]["chunk_count"], 20);
}

/// A derived id served from another tree, with nothing rooted here, is a
/// refusal naming the other tree — never `ok`.
#[tokio::test(flavor = "multi_thread")]
async fn cwd_fallback_refuses_an_id_served_from_another_tree() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (one, two) = same_named_checkouts(tmp.path());
    let base = spawn_daemon(vec![("api", &one, 10)]).await;

    let report = health_from(&base, &two).await;

    assert_eq!(report["status"], HEALTH_INDEX_NOT_REGISTERED, "{report}");
    assert_eq!(report["healthy"], Value::Bool(false));
    assert_eq!(report["index"]["index_id"], "api");
    assert_eq!(report["index"]["registered"], Value::Bool(false));
    assert_eq!(
        report["index"]["serving_root"],
        one.display().to_string(),
        "the refusal names the tree that owns the id: {report}"
    );
    let message = report["message"].as_str().expect("message");
    assert!(message.contains("DIFFERENT tree"), "{message}");
}
