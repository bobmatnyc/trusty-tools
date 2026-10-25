//! #10195: per-stage search timings, the logged deadline cancellation, the
//! `search_all` info line, and a `latency_ms` that spans the cold load.
//!
//! Why: search latency could not be attributed from the daemon log. A search
//! cancelled at the deadline logged no index id, `search_all` logged below
//! the daemon's `info` filter, and `latency_ms` started after a cold load.
//! What: drives `search_report` / `global_search_report` and asserts on the
//! reply and on the log lines a thread-local subscriber captures. The deadline
//! test holds the index write lock and injects a 50 ms deadline, so nothing
//! waits on a sleep.
//! Test: this module.

use std::time::Duration;

use axum::extract::State;

use super::*;
use crate::core::indexer::CodeIndexer;
use crate::core::registry::{IndexHandle, IndexId, IndexRegistry};
use crate::core::Embedder;
use crate::service::query_timeout::{with_query_deadline, QueryTimeoutConfig};

/// Collects every log line at `level` and above written on this thread.
#[derive(Clone, Default)]
struct Capture(Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("capture lock").extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Capture {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().expect("capture lock")).into_owned()
    }

    /// The first captured line containing `needle`, or a panic naming the log.
    fn line(&self, needle: &str) -> String {
        let log = self.text();
        log.lines()
            .find(|l| l.contains(needle))
            .map(str::to_owned)
            .unwrap_or_else(|| panic!("no line containing `{needle}` in:\n{log}"))
    }
}

/// The daemon's default filter is `info`, so the tests capture at `info`.
fn capture(level: tracing::Level) -> (Capture, tracing::subscriber::DefaultGuard) {
    let cap = Capture::default();
    let writer = cap.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || writer.clone())
        .with_max_level(level)
        .with_ansi(false)
        .without_time()
        .finish();
    (cap, tracing::subscriber::set_default(subscriber))
}

/// Register `id` holding one BM25-searchable file.
async fn add_index(registry: &IndexRegistry, id: &str) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("tempdir");
    let indexer = CodeIndexer::new(id, tmp.path());
    indexer
        .index_files_batch(&[("src/a.rs".into(), "fn shared_symbol() {}\n".into())])
        .await
        .expect("index batch");
    registry.register(IndexHandle::bare(
        IndexId::new(id.to_string()),
        Arc::new(tokio::sync::RwLock::new(indexer)),
        tmp.path().to_path_buf(),
    ));
    tmp
}

fn query(text: &str) -> crate::core::indexer::SearchQuery {
    serde_json::from_value(serde_json::json!({ "text": text })).expect("SearchQuery")
}

/// The search log line and `meta.timings` carry every stage, and
/// `total_ms` is the reply's `latency_ms`.
///
/// Pre-fix the reply had no `meta.timings` and the log line one number.
/// Test: this test.
#[tokio::test]
#[serial_test::parallel]
async fn search_reports_per_stage_timings_in_log_and_meta() {
    let registry = IndexRegistry::new();
    let _tmp = add_index(&registry, "timings-10195").await;
    let state = Arc::new(SearchAppState::new(registry));

    let (cap, _guard) = capture(tracing::Level::INFO);
    let body = super::search::search_report(&state, "timings-10195", query("shared_symbol"))
        .await
        .unwrap_or_else(|(s, b)| panic!("expected 200, got {s}: {b}"));

    let timings = &body["meta"]["timings"];
    for key in [
        "resolve_ms",
        "lock_ms",
        "embed_ms",
        "vector_ms",
        "lexical_ms",
        "ripgrep_ms",
        "fusion_ms",
        "rehydrate_wait_ms",
        "total_ms",
    ] {
        assert!(timings[key].is_u64(), "meta.timings.{key} missing: {body}");
    }
    assert!(timings["rehydrate_ran"].is_boolean(), "{body}");
    assert!(timings["hnsw_promoted"].is_boolean(), "{body}");
    assert_eq!(timings["total_ms"], body["latency_ms"], "{body}");

    let line = cap.line(": search index_id=timings-10195 ");
    for field in [
        "latency_ms=",
        "resolve_ms=",
        "lock_ms=",
        "embed_ms=",
        "vector_ms=",
        "lexical_ms=",
        "ripgrep_ms=",
        "fusion_ms=",
        "rehydrate_wait_ms=",
        "rehydrate_ran=",
        "hnsw_promoted=",
    ] {
        assert!(line.contains(field), "missing `{field}` in: {line}");
    }
}

/// A search the deadline cancels logs `query_timeout` at WARN with its index
/// id, its query, the stage it was in and the elapsed time.
///
/// Pre-fix the only line was `query_timeout ... timeout_secs=0`, with no index
/// id, query or stage; then both lines were written for one cancellation.
/// Test: this test.
#[tokio::test]
#[serial_test::parallel]
async fn a_search_cancelled_at_the_deadline_logs_index_query_and_stage() {
    let registry = IndexRegistry::new();
    let _tmp = add_index(&registry, "stalled-10195").await;
    let state = Arc::new(SearchAppState::new(registry));
    let handle = state
        .registry
        .get(&IndexId::new("stalled-10195".to_string()))
        .expect("index");
    let held = handle.indexer.write().await;

    let (cap, _guard) = capture(tracing::Level::WARN);
    let cfg = QueryTimeoutConfig::from_duration(Duration::from_millis(50));
    let search = super::search::search_report(&state, "stalled-10195", query("stalled query"));
    let (status, _) = with_query_deadline(&cfg, search)
        .await
        .expect_err("the held lock outlasts the deadline");
    assert_eq!(status, axum::http::StatusCode::REQUEST_TIMEOUT);
    drop(held);

    let line = cap.line("index_id=stalled-10195");
    assert!(line.contains("WARN"), "{line}");
    assert!(line.contains("query_timeout"), "{line}");
    assert!(line.contains("query=stalled query"), "{line}");
    assert!(line.contains("stage=lock"), "{line}");
    let elapsed: u64 = line
        .split("elapsed_ms=")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("no elapsed_ms in: {line}"));
    assert!(elapsed >= 50, "elapsed_ms is at least the deadline: {line}");
    // #10195: the deadline wrapper adds no second `query_timeout` line.
    let log = cap.text();
    let timeouts = log.lines().filter(|l| l.contains("query_timeout")).count();
    assert_eq!(
        timeouts, 1,
        "one query_timeout line per cancellation:\n{log}"
    );
}

/// `search_all` writes one `info` line naming its target index ids.
///
/// Pre-fix the fan-out logged only at debug, so nothing reached an `info`
/// filter.
/// Test: this test.
#[tokio::test]
#[serial_test::parallel]
async fn search_all_logs_one_info_line_with_its_target_indexes() {
    let registry = IndexRegistry::new();
    let _a = add_index(&registry, "fan-a-10195").await;
    let _b = add_index(&registry, "fan-b-10195").await;
    let state = Arc::new(SearchAppState::new(registry));
    let req: GlobalSearchRequest =
        serde_json::from_value(serde_json::json!({ "query": "shared_symbol", "top_k": 5 }))
            .expect("request");

    let (cap, _guard) = capture(tracing::Level::INFO);
    super::search_global::global_search_report(&state, req)
        .await
        .unwrap_or_else(|(s, b)| panic!("expected 200, got {s}: {b}"));

    let log = cap.text();
    let lines: Vec<&str> = log.lines().filter(|l| l.contains("search_all")).collect();
    assert_eq!(lines.len(), 1, "one line per call:\n{log}");
    let line = lines[0];
    assert!(line.contains(" INFO "), "{line}");
    assert!(
        line.contains("fan-a-10195") && line.contains("fan-b-10195"),
        "{line}"
    );
    assert!(line.contains("latency_ms="), "{line}");
}

/// A search that lazy-loads a cold index counts the load in `latency_ms`.
///
/// Pre-fix the clock started after `resolve_or_load_index`, so `latency_ms`
/// left out the injected load time.
/// Test: this test.
#[tokio::test]
#[serial_test::parallel]
async fn a_cold_lazy_load_counts_in_latency_ms() {
    const LOAD: Duration = Duration::from_millis(300);
    let state = SearchAppState::new(IndexRegistry::new());
    let embedder: Arc<dyn Embedder> = Arc::new(crate::core::embed::MockEmbedder::new(8));
    state.install_embedder(embedder).await;
    let state = Arc::new(state);
    let (_dir, root) = super::test_support::allowlisted_index_root("ts-10195-cold-");
    let id = "cold-latency-10195";
    let mut entry = crate::service::persistence::PersistedIndex::new(id.to_string(), root);
    entry.colocated = true;
    state.cold_store.register_cold_entries(vec![entry]);
    *super::index_resolve::TEST_COLD_LOAD_DELAY
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = Some((id.to_string(), LOAD));

    let (cap, _guard) = capture(tracing::Level::INFO);
    let reply = super::search::search_handler(
        State(Arc::clone(&state)),
        axum::extract::Path(id.to_string()),
        axum::Json(query("authenticate")),
    )
    .await;
    *super::index_resolve::TEST_COLD_LOAD_DELAY
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = None;
    let axum::Json(body) = reply.unwrap_or_else(|(s, axum::Json(b))| panic!("{s}: {b}"));

    let load_ms = LOAD.as_millis() as u64;
    let latency = body["latency_ms"].as_u64().expect("latency_ms");
    assert!(latency >= load_ms, "latency_ms spans the load: {body}");
    let line = cap.line(&format!(": search index_id={id} "));
    let logged: u64 = line
        .split("latency_ms=")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("no latency_ms in: {line}"));
    assert!(
        logged >= load_ms,
        "logged latency_ms spans the load: {line}"
    );
}
