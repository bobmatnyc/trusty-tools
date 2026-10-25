//! #10195: a rehydrate logs `rehydrate_ms` with its stage split and the
//! index id. Asserts on the log only, so it also runs against the pre-fix code.

use super::*;
use std::time::Duration;

/// Collects every log line at INFO and above written on this thread.
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
}

fn capture() -> (Capture, tracing::subscriber::DefaultGuard) {
    let cap = Capture::default();
    let writer = cap.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || writer.clone())
        .with_max_level(tracing::Level::INFO)
        .with_ansi(false)
        .without_time()
        .finish();
    (cap, tracing::subscriber::set_default(subscriber))
}

/// The rehydrate a search starts logs `rehydrate_ms` and the time of each
/// stage (scan, sort, entities, BM25 build, commit) with the index id.
///
/// Pre-fix the rehydrate line carried only `rehydrate_ms`.
/// Test: this test.
#[tokio::test]
#[serial_test::serial]
async fn a_rehydrate_logs_its_stage_split() {
    let dir = tempfile::tempdir().expect("tempdir");
    let idx = make_indexer_with_corpus(&dir.path().join("index.redb"));
    idx.index_files_batch(&[("src/auth.rs".into(), "fn authenticate() {}".into())])
        .await
        .expect("index batch");
    tokio::time::sleep(Duration::from_millis(5)).await;
    idx.evict_bm25_entities_if_idle(Duration::from_nanos(1))
        .await;
    idx.evict_chunks_if_idle(Duration::from_nanos(1)).await;
    assert!(idx.corpus_evicted(), "fixture: the corpus is evicted");

    let (cap, _guard) = capture();
    let q = SearchQuery {
        text: "authenticate".to_string(),
        top_k: 5,
        ..Default::default()
    };
    idx.search(&q).await.expect("search");
    idx.ensure_corpus_rehydrated().await;
    assert!(!idx.corpus_evicted(), "the rehydrate committed");

    let log = cap.text();
    let line = log
        .lines()
        .find(|l| l.contains("rehydrate cost"))
        .unwrap_or_else(|| panic!("no rehydrate line in:\n{log}"));
    for field in [
        "index_id=corpus-test",
        "rehydrate_ms=",
        "scan_ms=",
        "sort_ms=",
        "entities_ms=",
        "bm25_build_ms=",
        "commit_ms=",
    ] {
        assert!(line.contains(field), "missing `{field}` in: {line}");
    }
}
