//! Recall hit-log write tests (#9141).
//!
//! Why: `PalaceHandle::log_recall` wrote one redb commit per recall hit, and
//! discarded every write error. These pin one commit per recall and a `warn`
//! for a write that fails.
//! What: drives `log_recall` directly with fabricated L2 results against a real
//! `RecallLog`, then reads the log's rows and its test-only commit counter.
//! Test: this file IS the tests.

use super::types::RecallResult;
use super::*;
use crate::memory_core::analytics::RecallLog;
use crate::memory_core::palace::{Drawer, PalaceId};
use crate::memory_core::store::{kg::KnowledgeGraph, vector::UsearchStore};
use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tempfile::tempdir;
use tracing_subscriber::fmt::MakeWriter;

/// Collects formatted log output so a test can assert on what reached the log.
#[derive(Clone, Default)]
pub(crate) struct Capture(Arc<Mutex<Vec<u8>>>);

impl Capture {
    pub(crate) fn text(&self) -> String {
        let bytes = self.0.lock().unwrap_or_else(|p| p.into_inner());
        String::from_utf8_lossy(&bytes).into_owned()
    }
}

impl Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let mut bytes = self.0.lock().unwrap_or_else(|p| p.into_inner());
        bytes.extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for Capture {
    type Writer = Capture;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// A handle with a real recall log under `dir`, and that log.
fn handle_with_log(dir: &std::path::Path) -> (PalaceHandle, Arc<RecallLog>) {
    let vs = UsearchStore::new(dir.join("idx.usearch"), 384).expect("vector store");
    let kg = KnowledgeGraph::open(&dir.join("kg.db")).expect("kg");
    let log = Arc::new(RecallLog::open(&dir.join("recall.db")).expect("recall log"));
    let handle = PalaceHandle::new(PalaceId::new("test"), "Test".to_string(), vs, kg)
        .with_recall_log(Arc::clone(&log));
    (handle, log)
}

/// `n` L2 results over fresh drawers.
fn l2_results(n: usize) -> Vec<RecallResult> {
    (0..n)
        .map(|i| RecallResult {
            drawer: Drawer::new(uuid::Uuid::new_v4(), format!("hit {i}")),
            score: 0.5,
            layer: 2,
        })
        .collect()
}

/// Logged hits across `results`, polled until `want` land or ~2 s pass.
async fn wait_for_hits(log: &RecallLog, results: &[RecallResult], want: u64) -> u64 {
    let mut total = 0;
    for _ in 0..80 {
        total = 0;
        for r in results {
            total += log.hit_count(r.drawer.id).await.expect("hit_count");
        }
        if total >= want {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    total
}

/// Why (#9141): one commit per hit is one fsync per hit; a recall-all paid
/// hundreds per query. All of a recall's rows must land, in one commit.
/// What: logs a recall of five L2 hits and waits for all five rows, then
/// reads the log's commit counter.
/// Test: this test.
#[tokio::test]
async fn log_recall_writes_every_hit_in_one_commit() {
    let dir = tempdir().expect("tempdir");
    let (handle, log) = handle_with_log(dir.path());
    let results = l2_results(5);

    handle.log_recall("which drawers", &results);

    assert_eq!(
        wait_for_hits(&log, &results, 5).await,
        5,
        "a hit row was dropped"
    );
    assert_eq!(log.commit_count(), 1, "one recall must cost one commit");
}

/// Why (#9141, Fail-Open Check): the recall must still answer when its hit log
/// cannot be written, but the lost rows must not vanish without a trace — the
/// old loop discarded every error with `let _ =`.
/// What: makes the log's writes fail, logs a recall of three hits, and waits
/// for a `warn` naming the failure and the three lost rows; the log stays empty.
/// Test: this test.
#[tokio::test]
async fn log_recall_reports_a_failed_write() {
    let cap = Capture::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(cap.clone())
        .with_ansi(false)
        .with_max_level(tracing::Level::WARN)
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);
    let dir = tempdir().expect("tempdir");
    let (handle, log) = handle_with_log(dir.path());
    log.fail_writes(true);
    let results = l2_results(3);

    handle.log_recall("which drawers", &results);

    let mut text = String::new();
    for _ in 0..80 {
        text = cap.text();
        if text.contains("recall log write failed") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(
        text.contains("recall log write failed") && text.contains("rows=3"),
        "a failed hit-log write must be logged with its row count; log was: {text:?}"
    );
    assert_eq!(
        wait_for_hits(&log, &results, 1).await,
        0,
        "a failed batch wrote rows"
    );
}
