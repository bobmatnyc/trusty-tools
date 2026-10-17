//! Unit tests for the freshness probe helpers (#9993 G7). Hermetic: no daemon.

use std::collections::HashSet;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use serde_json::json;

use super::*;

/// A scratch dir, removed on drop. [`NonceDir`] works under any parent; the
/// temp-root refusal is `refuse_temp_root`'s job, tested on its own.
fn scratch() -> tempfile::TempDir {
    tempfile::tempdir().expect("scratch dir")
}

#[test]
fn nonces_are_unique_lowercase_letters() {
    let all: Vec<String> = (0..10_000).map(|_| nonce()).collect();
    let distinct: HashSet<&String> = all.iter().collect();
    assert_eq!(distinct.len(), all.len(), "a nonce repeated");
    for n in &all {
        assert!(n.starts_with(NONCE_PREFIX), "{n}");
        assert_eq!(n.len(), NONCE_PREFIX.len() + NONCE_LETTERS, "{n}");
        assert!(n.bytes().all(|b| b.is_ascii_lowercase()), "{n}");
    }
    // Same clock and pid: the counter alone separates calls.
    assert_ne!(nonce_from(7, 42, 0), nonce_from(7, 42, 1));
    assert_ne!(nonce_from(7, 42, 0), nonce_from(7, 43, 0));
    assert_ne!(nonce_from(7, 42, 0), nonce_from(8, 42, 0));
    // The extremes still encode in the fixed width.
    assert_eq!(
        nonce_from(u64::MAX, u32::MAX, u32::MAX).len(),
        NONCE_PREFIX.len() + NONCE_LETTERS
    );
    assert_eq!(
        nonce_from(0, 0, 0),
        format!("{NONCE_PREFIX}{}", "a".repeat(28))
    );
}

#[tokio::test(start_paused = true)]
async fn poll_returns_the_lag_of_the_first_hit() {
    // State kept across attempts: misses twice, then hits.
    let calls = Arc::new(AtomicU32::new(0));
    let seen = Arc::clone(&calls);
    let t0 = Instant::now();
    let sample = poll_until_hit(t0, POLL_INTERVAL, SAMPLE_TIMEOUT, move || {
        let n = seen.fetch_add(1, Ordering::SeqCst);
        async move { Ok(n >= 2) }
    })
    .await
    .expect("third search hits");
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    assert_eq!(
        sample,
        Sample {
            lag: Duration::from_millis(200),
            attempts: 3
        }
    );
}

#[tokio::test(start_paused = true)]
async fn poll_fails_on_timeout_and_on_a_search_error() {
    // Never hits: one search at 0 ms and every 100 ms through 60 s, then fails.
    let calls = Arc::new(AtomicU32::new(0));
    let seen = Arc::clone(&calls);
    let error = poll_until_hit(Instant::now(), POLL_INTERVAL, SAMPLE_TIMEOUT, move || {
        seen.fetch_add(1, Ordering::SeqCst);
        async { Ok(false) }
    })
    .await
    .expect_err("no hit must fail, not drop the sample");
    assert_eq!(
        error,
        PollError::Timeout {
            attempts: 601,
            waited: SAMPLE_TIMEOUT
        }
    );
    assert_eq!(calls.load(Ordering::SeqCst), 601);

    // A search that never answers is bounded by the same budget.
    let error = poll_until_hit(Instant::now(), POLL_INTERVAL, SAMPLE_TIMEOUT, || {
        std::future::pending::<Result<bool, String>>()
    })
    .await
    .expect_err("a hung search must time out");
    assert_eq!(
        error,
        PollError::Timeout {
            attempts: 1,
            waited: SAMPLE_TIMEOUT
        }
    );

    // A failing search ends the poll at once, carrying the error.
    let calls = Arc::new(AtomicU32::new(0));
    let seen = Arc::clone(&calls);
    let error = poll_until_hit(Instant::now(), POLL_INTERVAL, SAMPLE_TIMEOUT, move || {
        let n = seen.fetch_add(1, Ordering::SeqCst);
        async move {
            if n == 1 {
                Err("socket closed".to_string())
            } else {
                Ok(false)
            }
        }
    })
    .await
    .expect_err("a search error must fail the sample");
    assert_eq!(
        error,
        PollError::Search {
            attempts: 2,
            error: "socket closed".into()
        }
    );
}

#[test]
fn hit_matching_names_only_the_nonce_file() {
    let rel = Path::new("freshness-probe-9993-x/zqfpabc.rs");
    let by_path = json!({"results": [{"path": "freshness-probe-9993-x/zqfpabc.rs",
        "file": "/c/freshness-probe-9993-x/zqfpabc.rs", "match_reason": "bm25"}]});
    let by_file = json!({"results": [{"file": "/c/freshness-probe-9993-x/zqfpabc.rs"}]});
    assert!(hit_names_file(&by_path, rel));
    assert!(hit_names_file(&by_file, rel));
    let other = json!({"results": [{"path": "src/zqfpabc.rs"},
        {"path": "freshness-probe-9993-x/zqfpabcd.rs"}]});
    assert!(!hit_names_file(&other, rel), "a different file is no hit");
    let from_disk = json!({"results": [{"path": "freshness-probe-9993-x/zqfpabc.rs",
        "match_reason": "fallback:ripgrep"}]});
    assert!(!hit_names_file(&from_disk, rel), "a disk scan is no hit");
    assert!(!hit_names_file(&json!({"results": []}), rel));
    assert!(!hit_names_file(&json!({"error": "x"}), rel));
}

#[test]
fn write_target_refuses_temp_roots_and_storage_dirs() {
    let tmp = scratch();
    assert!(refuse_temp_root(tmp.path()).is_err(), "a tempfile dir");
    assert!(refuse_temp_root(Path::new("/tmp/corpus")).is_err());
    assert!(refuse_temp_root(Path::new("/private/tmp/corpus")).is_err());
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    assert_eq!(refuse_temp_root(manifest), Ok(()));

    let corpus = Path::new("/w/corpus");
    let dir = corpus.join("freshness-probe-9993-a");
    let storage = vec![PathBuf::from("/w/data"), corpus.join(".trusty-search")];
    assert_eq!(check_write_target(&dir, corpus, &storage), Ok(()));
    assert!(
        check_write_target(&corpus.join("sub/freshness-probe-9993-a"), corpus, &storage).is_err(),
        "not directly under the corpus"
    );
    assert!(
        check_write_target(&corpus.join(".trusty-search"), corpus, &storage).is_err(),
        "the colocated storage dir"
    );
    // A nonce dir that holds a daemon directory is refused too.
    let nested = vec![corpus.join("freshness-probe-9993-a/data")];
    assert!(check_write_target(&dir, corpus, &nested).is_err());
}

#[test]
fn nonce_dir_removes_its_files_on_early_return_and_panic() {
    let parent = scratch();

    // finish(): every file and the directory go; a missing file is fine.
    let mut dir = NonceDir::create(parent.path(), "finish").expect("create");
    let path = dir.path().to_path_buf();
    let a = dir.write("a.rs", "pub fn a() {}").expect("write a");
    dir.write("b.rs", "pub fn b() {}").expect("write b");
    std::fs::remove_file(&a).expect("pre-remove a");
    dir.finish().expect("finish removes everything");
    assert!(!path.exists());

    // An existing directory is refused, so nothing foreign is ever removed.
    std::fs::create_dir(parent.path().join(format!("{DIR_PREFIX}taken"))).expect("mkdir");
    assert!(NonceDir::create(parent.path(), "taken").is_err());

    // Early return: drop alone removes the files.
    fn early(parent: &Path) -> Result<PathBuf, String> {
        let mut dir = NonceDir::create(parent, "early")?;
        dir.write("x.rs", "pub fn x() {}")?;
        let path = dir.path().to_path_buf();
        Err(path.display().to_string())
    }
    let left = early(parent.path()).expect_err("the early return");
    assert!(
        !Path::new(&left).exists(),
        "{left} survived an early return"
    );

    // Panic: drop runs during unwinding and still removes the files.
    let root = parent.path().to_path_buf();
    let panicked = std::panic::catch_unwind(move || {
        let mut dir = NonceDir::create(&root, "panic").expect("create");
        dir.write("y.rs", "pub fn y() {}").expect("write");
        panic!("a sample failed");
    });
    assert!(panicked.is_err());
    assert!(!parent.path().join(format!("{DIR_PREFIX}panic")).exists());

    // A file the probe did not write stops the removal, and the drop says so.
    let foreign = std::panic::catch_unwind({
        let root = parent.path().to_path_buf();
        move || {
            let dir = NonceDir::create(&root, "foreign").expect("create");
            std::fs::write(dir.path().join("other.txt"), "not ours").expect("foreign");
            drop(dir);
        }
    });
    assert!(foreign.is_err(), "a failed cleanup must not pass");
    let kept = parent.path().join(format!("{DIR_PREFIX}foreign/other.txt"));
    assert!(
        kept.exists(),
        "a file the probe did not write is never removed"
    );
}

#[test]
fn summary_over_known_samples_is_exact() {
    // 50 lags of 10..=500 ms, shuffled: nearest-rank p50 is sample 25.
    let mut lags: Vec<Duration> = (1..=50).map(|i| Duration::from_millis(i * 10)).collect();
    lags.reverse();
    lags.swap(3, 40);
    let s = summarize(&lags).expect("summary");
    assert_eq!(
        s,
        Summary {
            n: 50,
            min_ms: 10.0,
            p50_ms: 250.0,
            p95_ms: 480.0,
            p99_ms: 500.0,
            max_ms: 500.0,
        }
    );
    assert!(summarize(&[]).is_err(), "no samples is not a measurement");

    let rows = freshness_rows(WritePath::Watcher, &s);
    assert_eq!(rows.len(), 2);
    let (dist, min) = (&rows[0], &rows[1]);
    assert_eq!(
        (dist.dimension.as_str(), dist.unit.as_str(), dist.n),
        ("indexing", "ms", Some(50))
    );
    assert_eq!(dist.metric, "freshness lag (watcher)");
    assert_eq!(
        (dist.p50, dist.p95, dist.p99, dist.max, dist.value),
        (Some(250.0), Some(480.0), Some(500.0), Some(500.0), None)
    );
    assert_eq!(min.metric, "freshness lag min (watcher)");
    assert_eq!((min.value, min.p50), (Some(10.0), None));
    let notes = dist.notes.as_deref().expect("notes");
    assert!(notes.contains("indicative"), "{notes}");
    assert!(notes.contains("one after another"), "{notes}");
}
