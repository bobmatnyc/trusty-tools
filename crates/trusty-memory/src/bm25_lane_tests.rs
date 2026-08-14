//! Unit tests for [`Bm25Lane`](super::Bm25Lane).
//!
//! Why: these replace `bm25_supervisor_tests.rs` (#5329). Each test that used to
//! pin a subprocess control — the live-daemon cap, the RSS ceiling, the
//! external-mode opt-out, idempotent shutdown — has a counterpart here pinning
//! whatever took its place, or is retired with a reason. The env-knob tests use
//! `with_limits` rather than mutating process-global env vars, except the two
//! that are explicitly ABOUT parsing the env.

use super::*;

fn tempdir() -> tempfile::TempDir {
    tempfile::tempdir().expect("tempdir")
}

/// Why: `<data_root>/<palace>/bm25/` is what `trusty-bm25-daemon` was handed as
/// `--data-dir`. If the in-process lane computed anything else, every existing
/// snapshot would be invisible and the migration promise would be empty.
/// Test: this test itself.
#[tokio::test]
async fn data_dir_matches_the_daemon_era_layout() {
    let root = std::path::Path::new("/data/root");
    assert_eq!(
        data_dir_for_palace(root, "my-palace"),
        std::path::Path::new("/data/root/my-palace/bm25")
    );
    let lane = Bm25Lane::with_limits(root.to_path_buf(), 3, None);
    assert_eq!(
        lane.data_dir_for_palace("my-palace"),
        data_dir_for_palace(root, "my-palace"),
        "the method and the free function must not drift"
    );
    lane.shutdown().await;
}

#[tokio::test]
async fn default_cap_is_three() {
    assert_eq!(DEFAULT_MAX_RESIDENT, 3);
}

/// Why: replaces `max_live_daemons_honours_env_override`. A cap that silently
/// ignores a typo is worse than no cap.
/// Test: this test itself.
#[test]
#[serial_test::serial]
fn max_resident_honours_env_override() {
    let prev = std::env::var(ENV_MAX_PALACES).ok();
    // Safety: `#[serial]` makes this test the sole writer of this key.
    unsafe { std::env::set_var(ENV_MAX_PALACES, "7") };
    assert_eq!(max_resident_from_env(), 7);
    unsafe { std::env::set_var(ENV_MAX_PALACES, "not-a-number") };
    assert_eq!(max_resident_from_env(), DEFAULT_MAX_RESIDENT);
    unsafe { std::env::set_var(ENV_MAX_PALACES, "0") };
    assert_eq!(
        max_resident_from_env(),
        DEFAULT_MAX_RESIDENT,
        "zero is a typo, not a request to evict everything"
    );
    match prev {
        Some(v) => unsafe { std::env::set_var(ENV_MAX_PALACES, v) },
        None => unsafe { std::env::remove_var(ENV_MAX_PALACES) },
    }
}

/// Why: replaces `rss_limit_honours_env_override`. `0` must stay an explicit,
/// documented way to switch enforcement off rather than an accident of parsing.
/// Test: this test itself.
#[test]
#[serial_test::serial]
fn text_budget_honours_env_override() {
    let prev = std::env::var(ENV_TEXT_BUDGET_MB).ok();
    // Safety: `#[serial]` makes this test the sole writer of this key.
    unsafe { std::env::set_var(ENV_TEXT_BUDGET_MB, "64") };
    assert_eq!(text_budget_from_env(), Some(64));
    unsafe { std::env::set_var(ENV_TEXT_BUDGET_MB, "0") };
    assert_eq!(text_budget_from_env(), None, "0 disables enforcement");
    unsafe { std::env::set_var(ENV_TEXT_BUDGET_MB, "garbage") };
    assert_eq!(text_budget_from_env(), Some(DEFAULT_TEXT_BUDGET_MB));
    match prev {
        Some(v) => unsafe { std::env::set_var(ENV_TEXT_BUDGET_MB, v) },
        None => unsafe { std::env::remove_var(ENV_TEXT_BUDGET_MB) },
    }
}

/// Why: replaces `cap_is_clamped_to_at_least_one`. A cap of zero would evict
/// every index the instant it loaded, turning every operation into a reload.
/// Test: this test itself.
#[tokio::test]
async fn cap_is_clamped_to_at_least_one() {
    let dir = tempdir();
    let lane = Bm25Lane::with_limits(dir.path().to_path_buf(), 0, None);
    assert_eq!(lane.max_resident(), 1);
    lane.index("p", "d", "alpha").await.unwrap();
    assert_eq!(lane.resident_count().await, 1);
    lane.shutdown().await;
}

#[tokio::test]
async fn index_then_search_finds_the_document() {
    let dir = tempdir();
    let lane = Bm25Lane::with_limits(dir.path().to_path_buf(), 3, None);
    lane.index("alpha", "d1", "authentication login password")
        .await
        .unwrap();
    lane.index("alpha", "d2", "rendering ui components")
        .await
        .unwrap();

    let hits = lane.search("alpha", "authentication", 5).await.unwrap();
    assert_eq!(hits.len(), 1, "got: {hits:?}");
    assert_eq!(hits[0].doc_id, "d1");
    lane.shutdown().await;
}

/// Why: this is the property `#5036` was filed against — the daemon-era client
/// was pinned to the default palace's socket, so a write for palace X landed in
/// the default palace's corpus. Palace is now an argument on every call, so the
/// bug is unrepresentable; this pins that.
/// What: writes the same doc id into two palaces with different text and asserts
/// neither query crosses over.
/// Test: this test itself.
#[tokio::test]
async fn palaces_do_not_share_a_corpus() {
    let dir = tempdir();
    let lane = Bm25Lane::with_limits(dir.path().to_path_buf(), 3, None);
    lane.index("alpha", "shared-id", "kangaroo").await.unwrap();
    lane.index("beta", "shared-id", "platypus").await.unwrap();

    assert_eq!(lane.search("alpha", "kangaroo", 5).await.unwrap().len(), 1);
    assert!(lane
        .search("alpha", "platypus", 5)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(lane.search("beta", "platypus", 5).await.unwrap().len(), 1);
    assert!(lane.search("beta", "kangaroo", 5).await.unwrap().is_empty());
    lane.shutdown().await;
}

#[tokio::test]
async fn delete_removes_the_document() {
    let dir = tempdir();
    let lane = Bm25Lane::with_limits(dir.path().to_path_buf(), 3, None);
    lane.index("alpha", "d1", "ephemeral note").await.unwrap();
    assert_eq!(lane.search("alpha", "ephemeral", 5).await.unwrap().len(), 1);

    lane.delete("alpha", "d1").await.unwrap();
    assert!(lane
        .search("alpha", "ephemeral", 5)
        .await
        .unwrap()
        .is_empty());
    // Idempotent — deleting an absent id is not an error.
    lane.delete("alpha", "never-existed").await.unwrap();
    lane.shutdown().await;
}

#[tokio::test]
async fn stats_report_docs_and_bytes() {
    let dir = tempdir();
    let lane = Bm25Lane::with_limits(dir.path().to_path_buf(), 3, None);
    let empty = lane.stats("alpha").await.unwrap();
    assert_eq!(empty.doc_count, 0);
    assert_eq!(empty.total_text_bytes, 0);

    lane.index("alpha", "a", "hello").await.unwrap();
    lane.index("alpha", "b", "world!").await.unwrap();
    let stats = lane.stats("alpha").await.unwrap();
    assert_eq!(stats.doc_count, 2);
    assert_eq!(stats.total_text_bytes, 11);
    lane.shutdown().await;
}

/// Why: coverage must be a set statement. A palace holding one stale document
/// and missing one real one satisfies every count comparison.
/// Test: this test itself.
#[tokio::test]
async fn missing_docs_answers_by_identity() {
    let dir = tempdir();
    let lane = Bm25Lane::with_limits(dir.path().to_path_buf(), 3, None);
    lane.index("alpha", "a", "alpha").await.unwrap();
    lane.index("alpha", "stale", "a forgotten drawer")
        .await
        .unwrap();

    let asked = vec!["a".to_string(), "b".to_string()];
    let cov = lane.missing_docs("alpha", &asked).await.unwrap();
    assert_eq!(cov.checked, 2);
    assert_eq!(cov.missing, vec!["b".to_string()]);

    lane.index("alpha", "b", "beta").await.unwrap();
    assert!(lane
        .missing_docs("alpha", &asked)
        .await
        .unwrap()
        .missing
        .is_empty());
    lane.shutdown().await;
}

/// Why: replaces the daemon's `shutdown_flush.rs` coverage. The write path only
/// marks the index dirty, so if the flush tick never fired, every write would
/// live in memory until the process exited.
/// What: writes, then polls the snapshot file until the ticker persists it —
/// no explicit `flush` call anywhere.
/// Test: this test itself.
#[tokio::test]
async fn a_write_reaches_disk_without_an_explicit_flush() {
    let dir = tempdir();
    let lane = Bm25Lane::with_limits(dir.path().to_path_buf(), 3, None);
    lane.index("alpha", "d1", "persisted by the ticker")
        .await
        .unwrap();

    let snapshot = lane
        .data_dir_for_palace("alpha")
        .join(crate::bm25_index::SNAPSHOT_FILENAME);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !snapshot.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "the flush ticker never wrote {}",
            snapshot.display()
        );
        tokio::time::sleep(FLUSH_INTERVAL).await;
    }
    let raw = std::fs::read_to_string(&snapshot).unwrap();
    assert!(raw.contains("persisted by the ticker"), "got: {raw}");
    lane.shutdown().await;
}

/// Why: the backfill calls `flush` explicitly when it finishes so a hard kill
/// straight afterwards cannot lose a whole sweep's work waiting for a tick.
/// Test: this test itself.
#[tokio::test]
async fn flush_persists_a_pending_write() {
    let dir = tempdir();
    let lane = Bm25Lane::with_limits(dir.path().to_path_buf(), 3, None);
    lane.index("alpha", "d1", "flushed on demand")
        .await
        .unwrap();
    lane.flush("alpha").await.unwrap();

    let snapshot = lane
        .data_dir_for_palace("alpha")
        .join(crate::bm25_index::SNAPSHOT_FILENAME);
    let raw =
        std::fs::read_to_string(&snapshot).expect("snapshot must exist after an explicit flush");
    assert!(raw.contains("flushed on demand"), "got: {raw}");

    // A palace that was never touched is not resident, and flushing it is a
    // no-op rather than an error.
    lane.flush("never-touched").await.unwrap();
    lane.shutdown().await;
}

/// Why: replaces `shutdown_with_no_children_is_noop` and the e2e test's
/// reap-and-unlink assertion. The exit path must persist everything and must
/// tolerate being called twice — `run_http_on` calls it, and a test harness may
/// call it again.
/// Test: this test itself.
#[tokio::test]
async fn shutdown_flushes_and_is_idempotent() {
    let dir = tempdir();
    let lane = Bm25Lane::with_limits(dir.path().to_path_buf(), 3, None);
    lane.index("alpha", "d1", "written just before exit")
        .await
        .unwrap();
    lane.shutdown().await;

    let snapshot = lane
        .data_dir_for_palace("alpha")
        .join(crate::bm25_index::SNAPSHOT_FILENAME);
    let raw = std::fs::read_to_string(&snapshot).expect("shutdown must flush");
    assert!(raw.contains("written just before exit"), "got: {raw}");

    // Second call: no ticker left to stop, nothing dirty left to write.
    lane.shutdown().await;

    // And a fresh lane over the same root sees the corpus.
    let reopened = Bm25Lane::with_limits(dir.path().to_path_buf(), 3, None);
    let hits = reopened.search("alpha", "exit", 5).await.unwrap();
    assert_eq!(hits.len(), 1, "got: {hits:?}");
    reopened.shutdown().await;
}

/// Why: an evicted palace must lose nothing. The daemon reaped a child that had
/// already flushed on SIGTERM; the LRU must flush before it drops the index or
/// the write is gone with no error anywhere.
/// What: caps residency at 1, writes to two palaces, and reads the first back
/// through a reload.
/// Test: this test itself.
#[tokio::test]
async fn eviction_flushes_before_dropping_the_index() {
    let dir = tempdir();
    let lane = Bm25Lane::with_limits(dir.path().to_path_buf(), 1, None);
    lane.index("alpha", "d1", "evicted but not lost")
        .await
        .unwrap();
    lane.index("beta", "d2", "second palace").await.unwrap();

    assert_eq!(lane.resident_count().await, 1, "cap of 1 must hold");
    assert_eq!(lane.evicted_count(), 1);

    // Reading alpha reloads it from the snapshot eviction wrote.
    let hits = lane.search("alpha", "evicted", 5).await.unwrap();
    assert_eq!(hits.len(), 1, "the evicted write must survive: {hits:?}");
    lane.shutdown().await;
}

/// Why (#2846): replaces `rss_limit_honours_env_override`'s enforcement half.
/// The daemon-era ceiling was declared and never compared against anything; this
/// asserts the budget actually evicts.
/// What: a 0 MB budget (any non-empty corpus exceeds it) with two palaces
/// written under a cap of 8, so the CAP cannot be what evicts. The budget must
/// take residency down to one — never to zero, because the last index would
/// only be reloaded by the next call — and neither palace may lose its write.
///
/// The test asserts the post-condition rather than a two-resident
/// pre-condition: the background tick runs `enforce_text_budget` every
/// [`FLUSH_INTERVAL`], so a pre-condition assert would race it.
/// Test: this test itself.
#[tokio::test]
async fn over_budget_evicts_the_coldest() {
    let dir = tempdir();
    let lane = Bm25Lane::with_limits(dir.path().to_path_buf(), 8, Some(0));
    assert_eq!(lane.text_budget_bytes(), Some(0));
    lane.index("alpha", "d1", "alpha text").await.unwrap();
    lane.index("beta", "d2", "beta text").await.unwrap();

    lane.enforce_text_budget().await;

    assert_eq!(
        lane.resident_count().await,
        1,
        "the budget must evict down to one, and stop there"
    );
    assert!(
        lane.evicted_count() >= 1,
        "a zero budget over two palaces must have evicted"
    );
    // Neither palace lost its write to the eviction.
    assert_eq!(lane.search("beta", "beta", 5).await.unwrap().len(), 1);
    assert_eq!(lane.search("alpha", "alpha", 5).await.unwrap().len(), 1);
    lane.shutdown().await;
}

/// Why: a disabled budget must be a real no-op, not a budget of zero. `0` is the
/// documented off switch and the two must not be confused.
/// Test: this test itself.
#[tokio::test]
async fn a_disabled_budget_never_evicts() {
    let dir = tempdir();
    let lane = Bm25Lane::with_limits(dir.path().to_path_buf(), 8, None);
    assert_eq!(lane.text_budget_bytes(), None);
    lane.index("alpha", "d1", "alpha text").await.unwrap();
    lane.index("beta", "d2", "beta text").await.unwrap();

    lane.enforce_text_budget().await;

    assert_eq!(lane.resident_count().await, 2);
    assert_eq!(lane.evicted_count(), 0);
    lane.shutdown().await;
}

/// Why: replaces `a_concurrent_fanout_never_exceeds_the_cap`, which drove real
/// daemon children. The cap is the only thing standing between one
/// `memory_recall_all` and every palace on disk being held in memory at once.
/// What: 24 concurrent writers across 12 palaces against a cap of 3, then a
/// verification that every write survived its palace's evictions.
/// Test: this test itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_concurrent_fanout_never_exceeds_the_cap() {
    const CAP: usize = 3;
    const PALACES: usize = 12;

    let dir = tempdir();
    let lane = Bm25Lane::with_limits(dir.path().to_path_buf(), CAP, None);

    let mut tasks = Vec::new();
    for i in 0..PALACES {
        for doc in 0..2 {
            let lane = Arc::clone(&lane);
            tasks.push(tokio::spawn(async move {
                lane.index(
                    &format!("palace-{i}"),
                    &format!("doc-{doc}"),
                    &format!("unique-token-{i}-{doc}"),
                )
                .await
            }));
        }
    }
    for t in tasks {
        t.await.expect("task joined").expect("index succeeded");
    }

    assert!(
        lane.resident_count().await <= CAP,
        "resident={} exceeded cap={CAP}",
        lane.resident_count().await
    );
    assert!(
        lane.evicted_count() > 0,
        "a {PALACES}-palace fanout under a cap of {CAP} must have evicted something"
    );

    // Every document must be findable, which means every eviction flushed.
    // The assertion is "ranks first", not "is the only hit": the code-aware
    // tokenizer splits `unique-token-0-1` into shared subtokens, so a palace's
    // sibling document legitimately scores above zero for its neighbour's query.
    for i in 0..PALACES {
        for doc in 0..2 {
            let hits = lane
                .search(
                    &format!("palace-{i}"),
                    &format!("unique-token-{i}-{doc}"),
                    5,
                )
                .await
                .unwrap();
            assert_eq!(
                hits.first().map(|h| h.doc_id.as_str()),
                Some(format!("doc-{doc}").as_str()),
                "palace-{i}/doc-{doc} was lost across evictions: {hits:?}"
            );
        }
    }
    lane.shutdown().await;
}

/// Why: concurrent callers for ONE palace must converge on one index. Two
/// indexes over the same snapshot path would each flush the other's writes away
/// — the in-process analogue of the double-spawn the supervisor serialised
/// against (`concurrent_callers_for_one_palace_spawn_exactly_one_daemon`).
/// What: 16 concurrent writers into a single palace, then asserts exactly one
/// load happened and all 16 documents are present.
/// Test: this test itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_callers_for_one_palace_share_one_index() {
    let dir = tempdir();
    let lane = Bm25Lane::with_limits(dir.path().to_path_buf(), 3, None);

    let mut tasks = Vec::new();
    for i in 0..16 {
        let lane = Arc::clone(&lane);
        tasks.push(tokio::spawn(async move {
            lane.index("hot", &format!("doc-{i}"), &format!("token{i}"))
                .await
        }));
    }
    for t in tasks {
        t.await.expect("task joined").expect("index succeeded");
    }

    assert_eq!(
        lane.loaded_count(),
        1,
        "a single palace must be loaded exactly once no matter how many callers race"
    );
    let stats = lane.stats("hot").await.unwrap();
    assert_eq!(
        stats.doc_count, 16,
        "every concurrent write must have landed"
    );
    lane.shutdown().await;
}

/// Why: a load failure must reach the caller. The recall path degrades to
/// vector-only on `Err`, and swallowing the error here would instead serve an
/// empty lexical corpus as if it were a real answer.
/// What: plants a FILE where the palace's bm25 DIRECTORY should be, so
/// `create_dir_all` cannot succeed.
/// Test: this test itself.
#[tokio::test]
async fn a_cold_load_failure_propagates() {
    let dir = tempdir();
    let lane = Bm25Lane::with_limits(dir.path().to_path_buf(), 3, None);
    let palace_dir = dir.path().join("blocked");
    std::fs::create_dir_all(&palace_dir).unwrap();
    std::fs::write(palace_dir.join("bm25"), b"i am a file, not a directory").unwrap();

    let err = lane
        .search("blocked", "anything", 5)
        .await
        .expect_err("a palace whose bm25 dir cannot be created must error");
    assert!(
        format!("{err:#}").contains("blocked"),
        "the error must name the palace: {err:#}"
    );
    lane.shutdown().await;
}

#[test]
fn bm25_hit_round_trips() {
    let h = BM25Hit {
        doc_id: "drawer-1".into(),
        score: 0.42,
    };
    let s = serde_json::to_string(&h).unwrap();
    let back: BM25Hit = serde_json::from_str(&s).unwrap();
    assert_eq!(back.doc_id, "drawer-1");
    assert!((back.score - 0.42).abs() < 1e-6);
}
