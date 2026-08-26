//! Tests for `stores::status` — live store-status resolution, including the
//! issue #4115 corpus-open-failure regression (#3816/#3864/#3892/#4115).
//!
//! Why: split out of `status.rs` to keep that file under the 500-SLOC
//! production cap (issue #610) after the #4115 fix added the `stages`-based
//! health derivation, `failed_stages`, and their regression tests — mirrors
//! the sibling `index_feed.rs` / `index_feed_tests.rs` split in this same
//! directory.
//! What: a mock trusty-search HTTP router and a mock trusty-memory socket,
//! covering connected, missing, corpus-open-failed, and unreachable;
//! config-validation short-circuiting covered without any network at all.
//! The memory half is a socket since #6286 — a stub that kept answering HTTP
//! would pass against a transport this code no longer speaks.
//! Test: this file IS the test module (`stores::status::tests`).

use super::*;
use axum::{Json, Router, extract::Path, http::StatusCode, routing::get};
use serde_json::{Value, json};
use tokio::net::TcpListener;

use crate::uds_mock::{self, MockMemoryDaemon, RpcError};

/// Spin up a mock trusty-search exposing the index status route, and return
/// its base URL.
///
/// Why: Testing against the developer's real daemons would make the suite
/// depend on machine state; a mock keeps the probe logic (status codes,
/// body parsing, reason strings) under test deterministically.
async fn mock_daemon() -> String {
    let app = Router::new().route(
        "/indexes/{id}/status",
        get(|Path(id): Path<String>| async move {
            if id == "bob-kb" {
                (
                    StatusCode::OK,
                    Json(json!({
                        "index_id": "bob-kb",
                        "chunk_count": 552,
                        "root_path": "/Users/masa/trusty-agents/bob-kb",
                        "status": "ready",
                        "stages": {
                            "lexical": {"status": "ready"},
                            "semantic": {"status": "ready"},
                            "graph": {"status": "ready"},
                        },
                    })),
                )
            } else if id == "cto-duetto" {
                // Issue #4115's exact real-world shape: reachable,
                // `status: "ready"`, `chunk_count: 0`, but every
                // stage failed to open — the false-green case.
                (
                    StatusCode::OK,
                    Json(json!({
                        "index_id": "cto-duetto",
                        "chunk_count": 0,
                        "status": "ready",
                        "stages": {
                            "lexical": {"status": "failed", "failure": "corpus open failed"},
                            "semantic": {"status": "failed", "failure": "corpus open failed"},
                            "graph": {"status": "failed", "failure": "corpus open failed"},
                        },
                    })),
                )
            } else {
                (
                    StatusCode::NOT_FOUND,
                    Json(json!({"error": "no such index"})),
                )
            }
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

/// Spin up a mock trusty-memory answering `memory.drawers_list` (#6286).
///
/// Why: the palace probe is the existence check `resolve_one` runs for a
/// binding that names one, and it now dials a socket. Answering it here keeps
/// the three outcomes the HTTP stub covered — present, absent, and a palace
/// that exists but will not open — under test on the transport the code uses.
///
/// What: `owner-profile` answers an empty page; `unopenable-palace` is refused
/// with an internal error (#5592: trusty-memory reports a palace it cannot open
/// as a failure rather than as absent); every other palace is refused
/// not-found.
async fn mock_memory() -> MockMemoryDaemon {
    uds_mock::spawn(|_method: &str, params: Value| {
        let palace = params
            .get("palace_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        Box::pin(async move {
            match palace.as_str() {
                "owner-profile" => Ok(json!({ "drawers": [] })),
                "unopenable-palace" => Err(RpcError::internal("palace could not be loaded")),
                other => Err(RpcError::new(
                    trusty_common::memory_rpc::CODE_NOT_FOUND,
                    format!("palace not found: {other}"),
                )),
            }
        })
    })
    .await
}

fn stores_toml(raw: &str) -> StoresConfig {
    #[derive(serde::Deserialize)]
    struct W {
        #[serde(default)]
        stores: StoresConfig,
    }
    toml::from_str::<W>(raw).unwrap().stores
}

#[tokio::test]
async fn resolves_connected_store_with_stats() {
    let base = mock_daemon().await;
    let memory = mock_memory().await;
    let stores = stores_toml(
        r#"
[[stores]]
name = "bob-kb"
tree = "okg://izzie"
index = "bob-kb"
palace = "owner-profile"
"#,
    );
    let out = resolve_store_statuses("izzie", &stores, Some(&base), Some(memory.socket())).await;
    assert_eq!(out.len(), 1);
    let s = &out[0];
    assert!(s.connected, "expected connected, got reason {:?}", s.reason);
    assert_eq!(s.reason, None);
    assert_eq!(s.chunk_count, Some(552));
    assert_eq!(s.index_status.as_deref(), Some("ready"));
    assert_eq!(s.tree, "okg://izzie");
    assert_eq!(s.palace_connected, Some(true));
    assert_eq!(s.palace_reason, None);
    assert!(s.failed_stages.is_empty());
}

/// Issue #4115 regression: a warm-booted index whose corpus failed to
/// open answers HTTP 2xx with `status: "ready"` and `chunk_count: 0`, but
/// every stage reports `failed`. `connected` must be `false` — HTTP
/// reachability is not corpus health.
#[tokio::test]
async fn reports_corpus_open_failed_as_not_connected() {
    let base = mock_daemon().await;
    let stores = stores_toml(
        r#"
[[stores]]
name = "cto-duetto-kb"
index = "cto-duetto"
"#,
    );
    let out = resolve_store_statuses("cto-assistant", &stores, Some(&base), None).await;
    assert_eq!(out.len(), 1);
    let s = &out[0];
    assert!(
        !s.connected,
        "a reachable-but-broken index must not report connected"
    );
    assert_eq!(s.chunk_count, Some(0));
    assert_eq!(
        s.index_status.as_deref(),
        Some("ready"),
        "the daemon's own (misleading) status string is still echoed for debugging"
    );
    assert_eq!(
        s.failed_stages,
        vec![
            "lexical".to_string(),
            "semantic".to_string(),
            "graph".to_string()
        ]
    );
    let reason = s.reason.as_deref().unwrap();
    assert!(reason.contains("corpus failed to open"), "reason: {reason}");
    assert!(reason.contains("lexical"), "reason: {reason}");
}

#[test]
fn failed_stages_ignores_a_missing_stages_object() {
    // An older trusty-search predating issue #109's staged pipeline has
    // no `stages` key at all — must degrade to "nothing failed", never
    // panic or false-positive.
    assert!(failed_stages(&json!({"status": "ready"})).is_empty());
}

#[test]
fn failed_stages_reports_only_the_failed_lanes() {
    let body = json!({
        "stages": {
            "lexical": {"status": "ready"},
            "semantic": {"status": "failed"},
            "graph": {"status": "skipped"},
        }
    });
    assert_eq!(failed_stages(&body), vec!["semantic".to_string()]);
}

#[tokio::test]
async fn reports_missing_index_as_not_connected() {
    let base = mock_daemon().await;
    let memory = mock_memory().await;
    let stores = stores_toml("[[stores]]\nname = \"nope\"\n");
    let out = resolve_store_statuses("izzie", &stores, Some(&base), Some(memory.socket())).await;
    assert!(!out[0].connected);
    let reason = out[0].reason.as_deref().unwrap();
    assert!(reason.contains("not registered"), "reason was: {reason}");
    // The binding identity must still round-trip so the GUI can render
    // WHAT is disconnected, not just that something is.
    assert_eq!(out[0].index, "nope");
    assert_eq!(out[0].tree, "okg://izzie");
}

#[tokio::test]
async fn reports_missing_palace_without_downgrading_index() {
    let base = mock_daemon().await;
    let memory = mock_memory().await;
    let stores = stores_toml(
        "[[stores]]\nname = \"bob-kb\"\nindex = \"bob-kb\"\npalace = \"ghost-palace\"\n",
    );
    let out = resolve_store_statuses("izzie", &stores, Some(&base), Some(memory.socket())).await;
    assert!(
        out[0].connected,
        "index health must not depend on the palace"
    );
    assert_eq!(out[0].palace_connected, Some(false));
    assert!(
        out[0]
            .palace_reason
            .as_deref()
            .unwrap()
            .contains("does not exist")
    );
}

/// Why (#5592): `probe_palace` is one of three trusty-agents HTTP clients that
/// consume `/api/v1/palaces/{id}/drawers`, and the only one whose OPERATOR-VISIBLE
/// output changes when trusty-memory stops answering 404 for a palace it could
/// not open. Telling an operator a palace "does not exist" sends them looking
/// for a deleted palace when the real cause was a denied read or a jammed redb
/// lock; the reason string has to carry the status through instead.
/// What: points a store at the mock's unopenable palace and asserts the reason
/// carries the daemon's own message and specifically does NOT claim absence —
/// while the index stays connected, as in the sibling missing-palace test.
/// #6286 changed what the daemon says, not what the distinction is: the HTTP
/// 500 the reason used to name is now a coded JSON-RPC refusal, and the
/// not-found code is what marks the absence case.
/// Test: this test itself.
#[tokio::test]
async fn reports_unopenable_palace_as_a_server_error_not_an_absence() {
    let base = mock_daemon().await;
    let memory = mock_memory().await;
    let stores = stores_toml(
        "[[stores]]\nname = \"bob-kb\"\nindex = \"bob-kb\"\npalace = \"unopenable-palace\"\n",
    );
    let out = resolve_store_statuses("izzie", &stores, Some(&base), Some(memory.socket())).await;
    assert!(
        out[0].connected,
        "index health must not depend on the palace"
    );
    assert_eq!(out[0].palace_connected, Some(false));
    let reason = out[0].palace_reason.as_deref().unwrap();
    assert!(
        !reason.contains("does not exist"),
        "a palace that could not be opened was reported as absent: {reason}"
    );
    assert!(
        reason.contains("palace could not be loaded"),
        "the daemon's own reason must survive: {reason}"
    );
}

#[tokio::test]
async fn reports_undiscoverable_search_daemon() {
    let stores = stores_toml("[[stores]]\nname = \"bob-kb\"\n");
    let out = resolve_store_statuses("izzie", &stores, None, None).await;
    assert!(!out[0].connected);
    assert!(
        out[0]
            .reason
            .as_deref()
            .unwrap()
            .contains("not discoverable")
    );
}

#[tokio::test]
async fn invalid_binding_short_circuits_without_network() {
    // A bad tree scheme must be reported as the reason WITHOUT any probe
    // — note the deliberately unroutable base URL: if this test made a
    // network call it would report a connection error instead.
    let stores = stores_toml("[[stores]]\nname = \"kb\"\ntree = \"https://example.com\"\n");
    let out = resolve_store_statuses("izzie", &stores, Some("http://127.0.0.1:1"), None).await;
    assert!(!out[0].connected);
    assert!(out[0].reason.as_deref().unwrap().contains("okg://"));
}

/// Why (#3892): "connected" only ever meant "the index exists". A store can
/// be connected, hold 200,090 chunks, and contain NOTHING the agent
/// ingested — the exact state the issue documents. The card must therefore
/// carry the tree side too: where `okg://` actually resolves, and how much
/// of that tree is not yet searchable.
/// What: sandboxes `KB_KNOWLEDGE_DIR`, builds a real ingested-but-unfed KB
/// tree at the bound `okg://` path, and asserts the resolved status reports
/// the tree path and the pending backlog alongside `connected`.
/// Test: self-contained.
/// Sandbox `KB_KNOWLEDGE_DIR` for one test, restoring it on drop.
///
/// Why: the guard has to survive `.await` points (the probes are async), and
/// a bare `MutexGuard` held across an await is both a clippy error and a
/// genuine deadlock hazard. Owning the guard inside a struct — the same
/// shape as `tools::okg::tests::KnowledgeDirGuard` — keeps the critical
/// section scoped to the test body without hand-written restore paths.
struct KnowledgeDirGuard {
    _lock: std::sync::MutexGuard<'static, ()>,
    prior: Option<std::ffi::OsString>,
    dir: tempfile::TempDir,
}

impl KnowledgeDirGuard {
    fn new() -> Self {
        let lock = crate::test_env::HOME_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let prior = std::env::var_os("KB_KNOWLEDGE_DIR");
        // SAFETY: guarded by HOME_LOCK; restored in Drop.
        unsafe { std::env::set_var("KB_KNOWLEDGE_DIR", dir.path()) };
        Self {
            _lock: lock,
            prior,
            dir,
        }
    }
}

impl Drop for KnowledgeDirGuard {
    fn drop(&mut self) {
        // SAFETY: still holding HOME_LOCK for this guard's lifetime.
        unsafe {
            match &self.prior {
                Some(v) => std::env::set_var("KB_KNOWLEDGE_DIR", v),
                None => std::env::remove_var("KB_KNOWLEDGE_DIR"),
            }
        }
    }
}

#[tokio::test]
async fn reports_the_unsearchable_backlog_for_a_bound_tree() {
    let guard = KnowledgeDirGuard::new();
    let tmp = guard.dir.path();

    // A tree at okg://izzie with one ingested, never-indexed entity.
    let corpus = tmp.join("corpus");
    std::fs::create_dir_all(&corpus).unwrap();
    std::fs::write(corpus.join("one.md"), "a note").unwrap();
    let store = trusty_kb::store::KbStore::new(
        tmp.join("izzie"),
        trusty_kb::schema::Profile::default_profile(),
    );
    store
        .okg_register_source(trusty_kb::okg::registry::SourceSpec::new(
            "corpus",
            Some("notes"),
            trusty_kb::okg::registry::Locator::DocStore {
                path: corpus.to_string_lossy().to_string(),
                extensions: vec![],
                recursive: true,
            },
            "t0",
        ))
        .unwrap();
    store
        .okg_ingest_docstore(
            "corpus",
            &trusty_kb::okg::policy::DocStorePolicy::new(vec![tmp.canonicalize().unwrap()]),
            "t0",
        )
        .unwrap();

    let base = mock_daemon().await;
    let stores = stores_toml("[[stores]]\nname = \"bob-kb\"\ntree = \"okg://izzie\"\n");
    let out = resolve_store_statuses("izzie", &stores, Some(&base), None).await;

    assert!(out[0].connected, "reason: {:?}", out[0].reason);
    assert_eq!(
        out[0].tree_path.as_deref(),
        Some(tmp.join("izzie").display().to_string().as_str()),
        "the okg:// URI must resolve to a real directory"
    );
    assert_eq!(
        (out[0].pending_index, out[0].synced_index),
        (Some(1), Some(0)),
        "connected, and still holding nothing the tree holds"
    );
}

#[tokio::test]
async fn no_bindings_resolves_to_empty() {
    let out = resolve_store_statuses("izzie", &StoresConfig::default(), None, None).await;
    assert!(out.is_empty());
}
