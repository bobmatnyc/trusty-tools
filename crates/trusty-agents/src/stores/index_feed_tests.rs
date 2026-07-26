//! Ordering and convergence tests for the index feed (#3892).
//!
//! Why: every guarantee in the module doc's contract is invisible in normal
//! operation and catastrophic when it regresses — a lost push is silent on both
//! sides, which is the exact bug #3892 reports. The seam exists so all of it can
//! be proved against a FAKE feed with no daemon: push-then-record ordering, a
//! failed push staying pending and healing on a later run, replace-before-push
//! on an edit, and withdrawal on a tombstone.
//! What: a recording fake with an arm-able failure, driven over a real temp KB
//! tree and a real doc-store ingest; plus one mock-HTTP test pinning the live
//! daemon's route and body shape.
//! Test: `cargo test -p trusty-agents stores::index_feed`.

use std::sync::Mutex;

use super::*;
use trusty_kb::okg::policy::DocStorePolicy;
use trusty_kb::okg::registry::Locator;
use trusty_kb::schema::Profile;

/// One recorded call against the fake feed.
#[derive(Debug, Clone, PartialEq)]
enum Call {
    Index { path: String, content: String },
    Remove { path: String },
}

/// A feed that records every call and can be armed to fail.
#[derive(Default)]
struct FakeFeed {
    calls: Mutex<Vec<Call>>,
    /// When true, every operation fails — the "daemon is broken" case.
    failing: Mutex<bool>,
    /// The root this fake index claims. `None` = daemon reported none, which
    /// skips the coverage preflight.
    root: Mutex<Option<std::path::PathBuf>>,
}

impl FakeFeed {
    fn arm_failure(&self, failing: bool) {
        *self.failing.lock().unwrap() = failing;
    }
    fn set_root(&self, root: Option<std::path::PathBuf>) {
        *self.root.lock().unwrap() = root;
    }
    fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap().clone()
    }
    fn check(&self) -> anyhow::Result<()> {
        if *self.failing.lock().unwrap() {
            anyhow::bail!("search daemon unavailable");
        }
        Ok(())
    }
}

#[async_trait]
impl IndexFeed for FakeFeed {
    async fn index_file(&self, _index: &str, path: &str, content: &str) -> anyhow::Result<()> {
        self.check()?;
        self.calls.lock().unwrap().push(Call::Index {
            path: path.to_string(),
            content: content.to_string(),
        });
        Ok(())
    }
    async fn remove_file(&self, _index: &str, path: &str) -> anyhow::Result<()> {
        self.check()?;
        self.calls.lock().unwrap().push(Call::Remove {
            path: path.to_string(),
        });
        Ok(())
    }
    async fn index_root(&self, _index: &str) -> anyhow::Result<Option<std::path::PathBuf>> {
        // The preflight is a status read, so it is NOT gated by `failing` —
        // arming a failure must exercise the push path, not short-circuit it.
        Ok(self.root.lock().unwrap().clone())
    }
}

/// A KB store plus a doc-store corpus under one tempdir.
struct Fixture {
    _tmp: tempfile::TempDir,
    store: KbStore,
    corpus: std::path::PathBuf,
    policy: DocStorePolicy,
}

impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let store = KbStore::new(tmp.path().join("kb"), Profile::default_profile());
        let corpus = tmp.path().join("corpus");
        std::fs::create_dir_all(&corpus).unwrap();
        let policy = DocStorePolicy::new(vec![tmp.path().canonicalize().unwrap()]);
        Self {
            _tmp: tmp,
            store,
            corpus,
            policy,
        }
    }

    fn register(&self) -> SourceSpec {
        let mut spec = SourceSpec::new(
            "corpus",
            Some("notes"),
            Locator::DocStore {
                path: self.corpus.to_string_lossy().to_string(),
                extensions: vec![],
                recursive: true,
            },
            "t0",
        );
        spec.tombstone_deleted = true;
        self.store.okg_register_source(spec).unwrap();
        self.store.okg_source("corpus").unwrap()
    }

    fn ingest(&self, now: &str) {
        self.store
            .okg_ingest_docstore("corpus", &self.policy, now)
            .unwrap();
    }
}

/// Why: the base case the whole issue is about — after an ingest, the entities
/// must actually reach the index, and a second pass must push nothing.
/// What: ingests two files, feeds, asserts both were pushed with their real
/// content, then asserts the immediate re-feed is inert (upsert, not duplicate).
/// Test: self-contained.
#[tokio::test]
async fn feeds_the_backlog_then_settles() {
    let fx = Fixture::new();
    std::fs::write(fx.corpus.join("one.md"), "alpha note").unwrap();
    std::fs::write(fx.corpus.join("two.md"), "beta note").unwrap();
    let spec = fx.register();
    fx.ingest("t0");

    let feed = FakeFeed::default();
    let report = feed_source(&fx.store, &spec, "scratch-index", &feed, "t0")
        .await
        .unwrap();
    assert_eq!(
        (report.indexed, report.removed, report.pending),
        (2, 0, 0),
        "errors: {:?}",
        report.errors
    );
    assert_eq!(report.index.as_deref(), Some("scratch-index"));
    let calls = feed.calls();
    assert_eq!(calls.len(), 2, "one push per entity: {calls:?}");
    assert!(
        calls
            .iter()
            .any(|c| matches!(c, Call::Index { content, .. } if content.contains("alpha note"))),
        "the pushed content must be the entity file's real body: {calls:?}"
    );

    let second = feed_source(&fx.store, &spec, "scratch-index", &feed, "t1")
        .await
        .unwrap();
    assert_eq!((second.indexed, second.removed, second.pending), (0, 0, 0));
    assert_eq!(second.synced, 2);
    assert_eq!(feed.calls().len(), 2, "a settled feed makes no calls");
}

/// Why: THE convergence property. A push that fails (daemon down, 500, timeout)
/// must not lose the entity and must not be silently forgotten — the ingest
/// engine will skip the unchanged item on every later run, so only the index
/// backlog can bring it back.
/// What: fails every push, asserts the work is reported as pending with an error
/// line and nothing recorded, then re-runs the whole ingest + feed with the
/// daemon "back" and asserts the item is indexed after all.
/// Test: self-contained.
#[tokio::test]
async fn failed_push_stays_pending_and_heals_on_a_later_run() {
    let fx = Fixture::new();
    std::fs::write(fx.corpus.join("one.md"), "alpha note").unwrap();
    let spec = fx.register();
    fx.ingest("t0");

    let feed = FakeFeed::default();
    feed.arm_failure(true);
    let failed = feed_source(&fx.store, &spec, "scratch-index", &feed, "t0")
        .await
        .unwrap();
    assert_eq!((failed.indexed, failed.pending), (0, 1));
    assert_eq!(
        failed.errors.len(),
        1,
        "the failure is reported, not hidden"
    );
    assert!(
        failed.errors[0].contains("unavailable"),
        "{:?}",
        failed.errors
    );

    // A second ingest skips the unchanged file entirely — only the index
    // backlog can rescue it.
    fx.ingest("t1");
    feed.arm_failure(false);
    let healed = feed_source(&fx.store, &spec, "scratch-index", &feed, "t1")
        .await
        .unwrap();
    assert_eq!(
        (healed.indexed, healed.pending),
        (1, 0),
        "an unindexed entity must never be permanently skipped"
    );
    assert!(
        feed_source(&fx.store, &spec, "scratch-index", &feed, "t2")
            .await
            .unwrap()
            .indexed
            == 0,
        "and it settles afterwards"
    );
}

/// Why: an edited entity must REPLACE its index content. trusty-search chunks by
/// content, so pushing the new revision without withdrawing the old one leaves
/// stale text searchable — a subtler version of the same lie.
/// What: feeds, edits the source file, re-ingests and re-feeds, then asserts the
/// pass issued a withdrawal BEFORE the new push and that the pushed body is the
/// revised one.
/// Test: self-contained.
#[tokio::test]
async fn edited_entity_is_withdrawn_before_it_is_repushed() {
    let fx = Fixture::new();
    std::fs::write(fx.corpus.join("one.md"), "alpha note").unwrap();
    let spec = fx.register();
    fx.ingest("t0");
    let feed = FakeFeed::default();
    feed_source(&fx.store, &spec, "idx", &feed, "t0")
        .await
        .unwrap();

    std::fs::write(fx.corpus.join("one.md"), "alpha note, revised").unwrap();
    fx.ingest("t1");
    let report = feed_source(&fx.store, &spec, "idx", &feed, "t1")
        .await
        .unwrap();
    assert_eq!((report.indexed, report.pending), (1, 0));

    let calls = feed.calls();
    assert_eq!(calls.len(), 3, "index, remove, index: {calls:?}");
    assert!(
        matches!(&calls[1], Call::Remove { .. }),
        "the stale revision must be withdrawn first: {calls:?}"
    );
    assert!(
        matches!(&calls[2], Call::Index { content, .. } if content.contains("revised")),
        "{calls:?}"
    );
}

/// Why: a deleted source item must stop being findable, or search keeps serving
/// content the tree has already flagged as gone.
/// What: feeds, deletes the file, re-ingests (tombstoning) and re-feeds, then
/// asserts exactly one withdrawal and a settled backlog.
/// Test: self-contained.
#[tokio::test]
async fn tombstoned_entity_is_withdrawn_from_the_index() {
    let fx = Fixture::new();
    std::fs::write(fx.corpus.join("one.md"), "alpha note").unwrap();
    let spec = fx.register();
    fx.ingest("t0");
    let feed = FakeFeed::default();
    feed_source(&fx.store, &spec, "idx", &feed, "t0")
        .await
        .unwrap();

    std::fs::remove_file(fx.corpus.join("one.md")).unwrap();
    fx.ingest("t1");
    let report = feed_source(&fx.store, &spec, "idx", &feed, "t1")
        .await
        .unwrap();
    assert_eq!((report.indexed, report.removed, report.pending), (0, 1, 0));
    assert!(matches!(feed.calls().last(), Some(Call::Remove { .. })));

    let settled = feed_source(&fx.store, &spec, "idx", &feed, "t2")
        .await
        .unwrap();
    assert_eq!((settled.removed, settled.pending), (0, 0));
}

/// Why: one poisoned entity must not block the rest of a 5,000-item backlog, and
/// the partial progress it does make has to be durable.
/// What: makes one of three entity files unreadable (removed from the tree after
/// ingest), feeds, and asserts the other two still land while the bad one is
/// reported.
/// Test: self-contained.
#[tokio::test]
async fn a_bad_entity_does_not_abort_the_pass() {
    let fx = Fixture::new();
    for name in ["one", "two", "three"] {
        std::fs::write(fx.corpus.join(format!("{name}.md")), format!("{name} note")).unwrap();
    }
    let spec = fx.register();
    fx.ingest("t0");
    // Delete one entity from the TREE (not the corpus) behind the ledger's back.
    std::fs::remove_file(fx.store.entity_path("notes", "two").unwrap()).unwrap();

    let feed = FakeFeed::default();
    let report = feed_source(&fx.store, &spec, "idx", &feed, "t0")
        .await
        .unwrap();
    assert_eq!(report.indexed, 2, "the healthy entities still land");
    assert_eq!(
        report.pending, 1,
        "an unresolvable entity is unsearchable and must be counted"
    );
    assert_eq!(report.errors.len(), 1, "errors: {:?}", report.errors);
    assert!(
        report.errors[0].contains("not readable"),
        "{:?}",
        report.errors
    );
}

/// Why: THE live-verification finding. trusty-search post-filters every search
/// result whose path escapes the index root (issues #64 / #541), so pushing a
/// tree's entities into an index rooted elsewhere stores them and hides them
/// from every query — `indexed: 2, pending: 0` and `vector_search` still empty.
/// That is a NEW silent failure in the shape of the one #3892 reports, so the
/// feed must refuse and say why rather than push.
/// What: claims a root that does not contain the tree, asserts NOTHING was
/// pushed, the reason names both paths, and the backlog is still reported.
/// A root that DOES contain the tree feeds normally.
/// Test: self-contained.
#[tokio::test]
async fn refuses_an_index_rooted_away_from_the_tree() {
    let fx = Fixture::new();
    std::fs::write(fx.corpus.join("one.md"), "alpha note").unwrap();
    let spec = fx.register();
    fx.ingest("t0");

    let feed = FakeFeed::default();
    let elsewhere = tempfile::tempdir().unwrap();
    feed.set_root(Some(elsewhere.path().to_path_buf()));
    let refused = feed_source(&fx.store, &spec, "wrong-root", &feed, "t0")
        .await
        .unwrap();
    assert_eq!(
        (refused.indexed, refused.pending),
        (0, 1),
        "nothing may be pushed into an index that cannot serve it"
    );
    assert!(feed.calls().is_empty(), "calls: {:?}", feed.calls());
    let reason = refused.reason.as_deref().unwrap_or_default();
    assert!(
        reason.contains("does not contain this tree") && reason.contains("never returned"),
        "the reason must be actionable: {reason}"
    );

    // The same feed, now rooted ABOVE the tree, works.
    feed.set_root(Some(fx.store.root.clone()));
    let fed = feed_source(&fx.store, &spec, "right-root", &feed, "t1")
        .await
        .unwrap();
    assert_eq!((fed.indexed, fed.pending), (1, 0), "{:?}", fed.errors);
}

/// Why: the live route shape is the one thing a fake cannot prove, and a wrong
/// path or body key would fail only against the real daemon.
/// What: stands up a mock exposing trusty-search's actual `index-file` /
/// `remove-file` routes, drives [`HttpIndexFeed`], and asserts the URL, the JSON
/// keys, and that a non-2xx answer surfaces as an error.
/// Test: self-contained.
#[tokio::test]
async fn http_feed_calls_the_daemon_routes() {
    use axum::{Json, Router, extract::Path, http::StatusCode, routing::post};
    use std::sync::Arc;
    use tokio::net::TcpListener;

    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let index_seen = seen.clone();
    let remove_seen = seen.clone();
    let app = Router::new()
        .route(
            "/indexes/{id}/index-file",
            post(
                move |Path(id): Path<String>, Json(body): Json<serde_json::Value>| {
                    let seen = index_seen.clone();
                    async move {
                        seen.lock().unwrap().push(format!(
                            "index {id} {} {}",
                            body["path"].as_str().unwrap_or_default(),
                            body["content"].as_str().unwrap_or_default()
                        ));
                        (StatusCode::OK, Json(serde_json::json!({"indexed": true})))
                    }
                },
            ),
        )
        .route(
            "/indexes/{id}/remove-file",
            post(
                move |Path(id): Path<String>, Json(body): Json<serde_json::Value>| {
                    let seen = remove_seen.clone();
                    async move {
                        // Mirror the daemon: an unregistered index is a 404.
                        if id == "ghost" {
                            return (
                                StatusCode::NOT_FOUND,
                                Json(serde_json::json!({"error": "no such index"})),
                            );
                        }
                        seen.lock().unwrap().push(format!(
                            "remove {id} {}",
                            body["path"].as_str().unwrap_or_default()
                        ));
                        (
                            StatusCode::OK,
                            Json(serde_json::json!({"removed_chunks": 3})),
                        )
                    }
                },
            ),
        )
        .route(
            "/indexes/{id}/status",
            axum::routing::get(|Path(id): Path<String>| async move {
                if id == "ghost" {
                    return (
                        StatusCode::NOT_FOUND,
                        Json(serde_json::json!({"error": "no such index"})),
                    );
                }
                (
                    StatusCode::OK,
                    Json(serde_json::json!({"index_id": id, "root_path": "/Users/masa/kb"})),
                )
            }),
        );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let feed = HttpIndexFeed::new(format!("http://{addr}/")).unwrap();
    assert_eq!(
        feed.index_root("kb").await.unwrap(),
        Some(std::path::PathBuf::from("/Users/masa/kb")),
        "the preflight must read root_path off the status route"
    );
    feed.index_file("kb", "/tmp/a.md", "hello").await.unwrap();
    feed.remove_file("kb", "/tmp/a.md").await.unwrap();
    assert_eq!(
        *seen.lock().unwrap(),
        vec!["index kb /tmp/a.md hello", "remove kb /tmp/a.md"]
    );

    // A 404 (unregistered index) must surface as an error, not a silent success.
    let err = feed
        .remove_file("ghost", "/tmp/a.md")
        .await
        .expect_err("an unknown index must fail loudly");
    assert!(err.to_string().contains("404"), "error was: {err}");
}
