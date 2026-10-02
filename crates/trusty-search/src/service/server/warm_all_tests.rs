//! Warm-all: start, join, status, failure, window and expiry (#9027).
//!
//! Why: the dashboard decides from `GET /warm/status` whether an all-index
//! search will cover every index, so each state it reports — and the window it
//! promises — is pinned here against real corpus-backed indexes.
//! What: every test runs on a paused tokio clock; waits are virtual sleeps, so
//! no test spends real time on a window or a poll. RSS reads come from a fixed
//! probe so the memory ceiling is deterministic.
//! Test: this module.

use super::*;
use crate::core::corpus::CorpusStore;
use crate::core::indexer::CodeIndexer;
use crate::core::registry::{IndexHandle, IndexRegistry};

/// A fixed RSS reading, far below every ceiling these tests configure.
fn rss_100() -> Option<u64> {
    Some(100)
}

/// A fixed RSS reading at the ceiling the refusal test configures.
fn rss_9000() -> Option<u64> {
    Some(9_000)
}

fn config(window_secs: u64) -> WarmConfig {
    WarmConfig {
        window: Duration::from_secs(window_secs),
        concurrency: 2,
        index_timeout: Duration::from_secs(600),
        max_rss_mb: Some(8_000),
    }
}

/// Register a corpus-backed index holding one file; returns its tempdir and corpus.
async fn add_index(registry: &IndexRegistry, id: &str) -> (tempfile::TempDir, Arc<CorpusStore>) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let corpus = Arc::new(CorpusStore::open(&tmp.path().join("index.redb")).expect("corpus"));
    let mut indexer = CodeIndexer::new(id, tmp.path());
    indexer.set_corpus_store(Arc::clone(&corpus));
    indexer
        .index_files_batch(&[("src/a.rs".into(), "fn warm_target() {}\n".into())])
        .await
        .expect("index batch");
    registry.register(IndexHandle::bare(
        IndexId::new(id.to_string()),
        Arc::new(tokio::sync::RwLock::new(indexer)),
        tmp.path().to_path_buf(),
    ));
    (tmp, corpus)
}

async fn evict(state: &SearchAppState, id: &str) {
    let handle = state
        .registry
        .get(&IndexId::new(id.to_string()))
        .expect("registered");
    assert!(handle.indexer.read().await.reclaim_memory_now().await > 0);
}

fn new_state(registry: IndexRegistry) -> Arc<SearchAppState> {
    let state = Arc::new(SearchAppState::new(registry));
    state.warm.set_rss_probe(rss_100);
    state
}

/// Poll the status until no run is in progress (virtual time only).
async fn wait_for_run_end(state: &Arc<SearchAppState>) -> Value {
    for _ in 0..2_000 {
        let status = warm_status_report(state);
        if status["run"]["running"] == json!(false) {
            return status;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the warm run never finished: {}", warm_status_report(state));
}

/// Why: the acceptance path — evicted indexes become resident, the status
/// reports them warm with the window, and the RSS after warming is reported.
/// What: evicts two indexes, warms, and reads the status before and after.
/// Test: this test.
#[tokio::test(start_paused = true)]
async fn warm_all_rehydrates_an_evicted_index_and_reports_it_warm() {
    let registry = IndexRegistry::new();
    let _a = add_index(&registry, "alpha").await;
    let _b = add_index(&registry, "beta").await;
    let state = new_state(registry);
    evict(&state, "alpha").await;
    evict(&state, "beta").await;

    let before = warm_status_report(&state);
    assert_eq!(before["totals"]["cold"], json!(2), "{before}");
    assert_eq!(before["all_warm"], json!(false));
    assert_eq!(before["run"], Value::Null);

    let started = start_with(&state, config(1_800)).expect("start");
    assert_eq!(started["joined"], json!(false));
    assert_eq!(started["total"], json!(2));

    let after = wait_for_run_end(&state).await;
    assert_eq!(after["totals"]["warm"], json!(2), "{after}");
    assert_eq!(after["all_warm"], json!(true));
    assert_eq!(after["window_secs"], json!(1_800));
    assert_eq!(after["memory"]["rss_mb_after"], json!(100));
    assert_eq!(after["memory"]["ceiling_hit"], json!(false));
    for id in ["alpha", "beta"] {
        let handle = state
            .registry
            .get(&IndexId::new(id.to_string()))
            .expect("registered");
        assert!(
            !handle.indexer.read().await.corpus_evicted(),
            "{id} is resident"
        );
    }
}

/// Why: Fail-Open Check — one index that cannot warm is reported failed, the
/// rest still warm, and the set is never reported warm.
/// What: breaks one corpus so its rehydrate fails, warms both, asserts the
/// per-index verdicts, the totals, `all_warm: false`, and that only the good
/// index was pinned.
/// Test: this test.
#[tokio::test(start_paused = true)]
async fn a_failed_index_is_reported_failed_and_the_set_is_not_warm() {
    const CHUNKS_TABLE: redb::TableDefinition<'static, &str, &[u8]> =
        redb::TableDefinition::new("chunks");
    let registry = IndexRegistry::new();
    let _good = add_index(&registry, "good").await;
    let (_bad_dir, bad_corpus) = add_index(&registry, "broken").await;
    let state = new_state(registry);
    let txn = bad_corpus.db().begin_write().expect("begin write");
    assert!(txn.delete_table(CHUNKS_TABLE).expect("drop chunks"));
    txn.commit().expect("commit");
    evict(&state, "broken").await;
    evict(&state, "good").await;

    start_with(&state, config(1_800)).expect("start");
    let status = wait_for_run_end(&state).await;

    assert_eq!(status["totals"]["failed"], json!(1), "{status}");
    assert_eq!(status["totals"]["warm"], json!(1), "{status}");
    assert_eq!(
        status["all_warm"],
        json!(false),
        "a failed index never reads as warm"
    );
    let broken = status["indexes"]
        .as_array()
        .expect("rows")
        .iter()
        .find(|r| r["index_id"] == json!("broken"))
        .expect("broken row");
    assert_eq!(broken["state"], json!("failed"));
    assert!(
        broken["error"]
            .as_str()
            .is_some_and(|e| e.contains("rehydrate failed")),
        "{broken}"
    );
    assert!(
        !state.warm.is_pinned("broken"),
        "a failed index is never pinned"
    );
    assert!(state.warm.is_pinned("good"));
}

/// Why: re-requesting while a warm runs must join it, never start a second.
/// What: holds one index's write lock so the warm stalls on it, starts twice,
/// and asserts one run id, `joined: true`, and a `warming` row; then releases.
/// Test: this test.
#[tokio::test(start_paused = true)]
async fn a_second_start_joins_the_running_warm() {
    let registry = IndexRegistry::new();
    let _a = add_index(&registry, "held").await;
    let state = new_state(registry);
    let handle = state
        .registry
        .get(&IndexId::new("held".to_string()))
        .expect("registered");
    let lock = handle.indexer.write().await;

    let first = start_with(&state, config(60)).expect("first start");
    let second = start_with(&state, config(60)).expect("second start");
    assert_eq!(first["joined"], json!(false));
    assert_eq!(second["joined"], json!(true));
    assert_eq!(first["run_id"], second["run_id"]);

    let mut warming = false;
    for _ in 0..100 {
        tokio::task::yield_now().await;
        if warm_status_report(&state)["totals"]["warming"] == json!(1) {
            warming = true;
            break;
        }
    }
    assert!(warming, "{}", warm_status_report(&state));
    assert_eq!(warm_status_report(&state)["all_warm"], json!(false));

    drop(lock);
    let done = wait_for_run_end(&state).await;
    assert_eq!(done["totals"]["warm"], json!(1), "{done}");
    let third = start_with(&state, config(60)).expect("a new run after the first ended");
    assert_ne!(third["run_id"], first["run_id"]);
}

/// Why: warmed indexes stay resident for the window, and the status shows it.
/// What: warms with a 1800 s window, reads the pin and the expiry, advances the
/// paused clock past the window and asserts the pin is gone.
/// Test: this test.
#[tokio::test(start_paused = true)]
async fn a_warmed_index_is_pinned_for_the_window_then_released() {
    let registry = IndexRegistry::new();
    let _a = add_index(&registry, "pinned").await;
    let state = new_state(registry);

    start_with(&state, config(1_800)).expect("start");
    let status = wait_for_run_end(&state).await;
    assert!(state.warm.is_pinned("pinned"));
    let expires = status["expires_in_secs"]
        .as_u64()
        .expect("an expiry while pinned");
    assert!((1_790..=1_800).contains(&expires), "{status}");
    assert!(status["expires_at_unix_ms"].as_u64().is_some());
    assert!(status["indexes"][0]["pin_expires_in_secs"]
        .as_u64()
        .is_some());

    tokio::time::advance(Duration::from_secs(1_801)).await;
    assert!(
        !state.warm.is_pinned("pinned"),
        "the pin ends with the window"
    );
    let later = warm_status_report(&state);
    assert_eq!(later["expires_in_secs"], Value::Null, "{later}");
}

/// Why: warming past the configured RSS ceiling must be refused, not run.
/// What: a probe at the ceiling refuses the start with `warm_memory_ceiling`
/// and leaves no run behind.
/// Test: this test.
#[tokio::test(start_paused = true)]
async fn a_start_past_the_memory_ceiling_is_refused_and_runs_nothing() {
    let registry = IndexRegistry::new();
    let _a = add_index(&registry, "big").await;
    let state = new_state(registry);
    state.warm.set_rss_probe(rss_9000);

    let (status, body) = start_with(&state, config(60)).expect_err("refused");
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["error"], json!("warm_memory_ceiling"));
    assert_eq!(body["max_rss_mb"], json!(8_000));
    assert_eq!(warm_status_report(&state)["run"], Value::Null);
}

/// Why: Fail-Open Check — RSS can cross the ceiling after the start check
/// passed. The run must stop warming, flag `ceiling_hit`, and report every
/// skipped index failed, never warm or pinned.
/// What: a probe under the ceiling for the start check and over it for every
/// read after, so each per-index check refuses.
/// Test: this test.
#[tokio::test(start_paused = true)]
async fn a_ceiling_hit_mid_run_fails_the_remaining_indexes() {
    static READS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    fn rising() -> Option<u64> {
        let n = READS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Some(if n == 0 { 100 } else { 9_000 })
    }
    let registry = IndexRegistry::new();
    let _a = add_index(&registry, "one").await;
    let _b = add_index(&registry, "two").await;
    let state = new_state(registry);
    evict(&state, "one").await;
    evict(&state, "two").await;
    state.warm.set_rss_probe(rising);

    start_with(&state, config(60)).expect("the start check passes");
    let status = wait_for_run_end(&state).await;

    assert_eq!(status["memory"]["ceiling_hit"], json!(true), "{status}");
    assert_eq!(status["totals"]["failed"], json!(2), "{status}");
    assert_eq!(status["all_warm"], json!(false));
    for row in status["indexes"].as_array().expect("rows") {
        assert!(
            row["error"]
                .as_str()
                .is_some_and(|e| e.contains("warm ceiling")),
            "{row}"
        );
    }
    assert!(!state.warm.is_pinned("one") && !state.warm.is_pinned("two"));
}

/// Why: Fail-Open Check — an index that does not warm within the per-index
/// timeout is reported failed, never warm, and the run still ends.
/// What: holds the index's write lock so its warm cannot proceed, runs with a
/// 1 s per-index timeout on the paused clock, and asserts a failed row that
/// names the timeout.
/// Test: this test.
#[tokio::test(start_paused = true)]
async fn an_index_that_misses_the_warm_timeout_is_reported_failed() {
    let registry = IndexRegistry::new();
    let _a = add_index(&registry, "stuck").await;
    let state = new_state(registry);
    let handle = state
        .registry
        .get(&IndexId::new("stuck".to_string()))
        .expect("registered");
    let _lock = handle.indexer.write().await;

    let mut cfg = config(60);
    cfg.index_timeout = Duration::from_secs(1);
    start_with(&state, cfg).expect("start");
    let status = wait_for_run_end(&state).await;

    let row = &status["indexes"][0];
    assert_eq!(row["state"], json!("failed"), "{status}");
    assert!(
        row["error"]
            .as_str()
            .is_some_and(|e| e.contains("timed out")),
        "{row}"
    );
    assert!(!state.warm.is_pinned("stuck"));
}

/// Why: each field resolves request, then env, then default.
/// Test: this test.
#[test]
fn warm_config_prefers_the_request_over_the_defaults() {
    let cfg = WarmConfig::resolve(&WarmStartRequest {
        window_secs: Some(90),
        concurrency: Some(0),
    });
    assert_eq!(cfg.window, Duration::from_secs(90));
    assert_eq!(cfg.concurrency, 1, "concurrency clamps to >= 1");
}
