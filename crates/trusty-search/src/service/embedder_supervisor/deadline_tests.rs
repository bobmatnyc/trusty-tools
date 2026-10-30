//! #10194: the query-embed deadline against a real `LazyEmbedderHandle` and a
//! real child process.
//!
//! Why: the handler tests in `service::server::tests_10194` use a stub; these
//! prove the property the stub cannot — a query that gives up does not cancel
//! the sidecar spawn, a spawn that never becomes ready leaves no child behind,
//! and a spawn that fails is reported, not waited on.
//! What: a POSIX shell mock of `trusty-embedderd --stdio` (same wire protocol
//! as `tests::shutdown_tests`) that records its pid per start, behind a test
//! adapter that forwards `embed` through `embed_via` like the daemon's
//! `LazySlotEmbedderAdapter`.
//! Test: this file.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::{LazyEmbedderHandle, SupervisorConfig};
use crate::core::embed::Embedder;
use crate::core::indexer::CodeIndexer;
use crate::core::store::{UsearchStore, VectorStore};

const DEADLINE: Duration = Duration::from_millis(300);
const SLACK: Duration = Duration::from_millis(1700);
const CEILING: Duration = Duration::from_secs(30);

/// Forwards `embed` through `embed_via`, as the daemon's lazy adapter does.
struct LazyAdapter(Arc<LazyEmbedderHandle>);

#[async_trait::async_trait]
impl Embedder for LazyAdapter {
    async fn embed(&self, text: &str) -> anyhow::Result<Vec<f32>> {
        let text = text.to_string();
        let mut v = self
            .0
            .embed_via(|c| async move { c.embed_batch(vec![text]).await })
            .await
            .map_err(|e| anyhow::anyhow!("lazy-stdio embed failed: {e}"))?;
        v.pop().ok_or_else(|| anyhow::anyhow!("no vector"))
    }
    async fn embed_batch(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        let owned: Vec<String> = texts.iter().map(|s| (*s).to_owned()).collect();
        self.0
            .embed_via(|c| async move { c.embed_batch(owned).await })
            .await
            .map_err(|e| anyhow::anyhow!("lazy-stdio embed_batch failed: {e}"))
    }
    fn dimension(&self) -> usize {
        1
    }
}

/// Write a mock sidecar that appends its pid to `<dir>/pids`, runs `prelude`,
/// then answers every JSON-RPC line.
fn write_mock(dir: &Path, prelude: &str) -> PathBuf {
    let path = dir.join("mock-embedderd.sh");
    let pids = dir.join("pids");
    // See #10248
    trusty_common::exec_stub::write_executable(
        &path,
        format!(
            r#"#!/bin/sh
echo $$ >> '{pids}'
{prelude}
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  [ -n "$id" ] || id=1
  printf '{{"jsonrpc":"2.0","result":{{"embeddings":[[0.1]]}},"id":%s}}\n' "$id"
done
"#,
            pids = pids.display()
        ),
    )
    .expect("write mock");
    path
}

fn pids(dir: &Path) -> Vec<u32> {
    std::fs::read_to_string(dir.join("pids"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.trim().parse().ok())
        .collect()
}

/// `true` while `pid` exists and is not a zombie.
fn running(pid: u32) -> bool {
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        // Field 3 is the state; `Z` is an unreaped zombie.
        Ok(stat) => stat
            .rsplit(')')
            .next()
            .and_then(|rest| rest.split_whitespace().next())
            .is_some_and(|state| state != "Z"),
        Err(_) => std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success()),
    }
}

async fn wait_for(what: &str, mut cond: impl FnMut() -> bool) {
    let until = Instant::now() + CEILING;
    while !cond() {
        assert!(Instant::now() < until, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn indexer(handle: &Arc<LazyEmbedderHandle>, dir: &Path) -> CodeIndexer {
    let store: Arc<dyn VectorStore> = Arc::new(UsearchStore::new(1).expect("usearch"));
    CodeIndexer::new("deadline-10194", dir)
        .with_components(Arc::new(LazyAdapter(Arc::clone(handle))), store)
}

fn handle(binary: PathBuf, startup_timeout_secs: u64) -> Arc<LazyEmbedderHandle> {
    Arc::new(LazyEmbedderHandle::new(
        binary,
        SupervisorConfig {
            startup_timeout_secs,
            idle_shutdown_secs: 0,
            ..SupervisorConfig::default()
        },
    ))
}

/// A spawn slower than the deadline: the query degrades in time, the spawn
/// finishes on its own, and the next query embeds on that same child.
///
/// Test: this test.
#[tokio::test]
async fn a_slow_spawn_misses_the_deadline_then_serves_the_next_query() {
    let dir = tempfile::tempdir().expect("tempdir");
    let lazy = handle(write_mock(dir.path(), "sleep 2"), 60);
    let idx = indexer(&lazy, dir.path());

    let started = Instant::now();
    let err = idx
        .embed_query_within("first query", Some(DEADLINE))
        .await
        .expect_err("the cold spawn outlasts the deadline");
    assert!(
        started.elapsed() < DEADLINE + SLACK,
        "{:?}",
        started.elapsed()
    );
    assert!(
        format!("{err:#}").contains("missed its 300 ms deadline"),
        "{err:#}"
    );

    wait_for("the spawn to finish", || {
        lazy.app_pid_slot().load(Ordering::Acquire) > 0
    })
    .await;
    let pid = lazy.app_pid_slot().load(Ordering::Acquire);

    let until = Instant::now() + CEILING;
    let vector = loop {
        // The abandoned embed clears its gate a moment after the spawn.
        match idx.embed_query_within("second query", Some(DEADLINE)).await {
            Ok(v) => break v,
            Err(e) => assert!(Instant::now() < until, "never served: {e:#}"),
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert_eq!(vector, Some(vec![0.1]));
    assert_eq!(pids(dir.path()), vec![pid], "one spawn, reused");
    lazy.shutdown().await;
}

/// Concurrent cold queries all degrade in time and share one spawn.
///
/// Test: this test.
#[tokio::test]
async fn concurrent_cold_queries_start_one_child() {
    let dir = tempfile::tempdir().expect("tempdir");
    let lazy = handle(write_mock(dir.path(), "sleep 2"), 60);
    let idx = Arc::new(indexer(&lazy, dir.path()));

    let started = Instant::now();
    let calls = (0..4).map(|n| {
        let idx = Arc::clone(&idx);
        async move {
            idx.embed_query_within(&format!("query {n}"), Some(DEADLINE))
                .await
        }
    });
    for r in futures::future::join_all(calls).await {
        assert!(r.is_err(), "every cold query degrades");
    }
    assert!(
        started.elapsed() < DEADLINE + SLACK,
        "{:?}",
        started.elapsed()
    );

    wait_for("the spawn to finish", || {
        lazy.app_pid_slot().load(Ordering::Acquire) > 0
    })
    .await;
    assert_eq!(pids(dir.path()).len(), 1, "no spawn storm");
    lazy.shutdown().await;
}

/// A spawn that never answers: the query degrades at the deadline, the
/// startup timeout kills the child (no zombie left), and a later query is
/// not stuck behind it — it starts a fresh spawn.
///
/// Test: this test.
#[tokio::test]
async fn a_spawn_that_never_becomes_ready_is_reaped_and_does_not_wedge_queries() {
    let dir = tempfile::tempdir().expect("tempdir");
    let lazy = handle(write_mock(dir.path(), "exec sleep 600"), 1);
    let idx = indexer(&lazy, dir.path());

    let started = Instant::now();
    idx.embed_query_within("first query", Some(DEADLINE))
        .await
        .expect_err("a never-ready spawn cannot embed");
    assert!(
        started.elapsed() < DEADLINE + SLACK,
        "{:?}",
        started.elapsed()
    );

    wait_for("the child to start", || !pids(dir.path()).is_empty()).await;
    let first = pids(dir.path())[0];
    wait_for("the never-ready child to be killed and reaped", || {
        !running(first)
    })
    .await;
    assert_eq!(lazy.app_pid_slot().load(Ordering::Acquire), 0);

    // The failed spawn released the gate: a later query tries again.
    let until = Instant::now() + CEILING;
    while pids(dir.path()).len() < 2 {
        assert!(Instant::now() < until, "no query ever retried the spawn");
        let _ = idx.embed_query_within("retry", Some(DEADLINE)).await;
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let second = pids(dir.path())[1];
    wait_for("the second child to be reaped", || !running(second)).await;
}

/// A spawn that fails outright is reported with its cause inside the
/// deadline, not waited on and not reported as a deadline miss.
///
/// Test: this test.
#[tokio::test]
// Reads TRUSTY_QUERY_EMBED_DEADLINE_MS; must not overlap a `#[serial]` writer.
#[serial_test::parallel]
async fn a_failed_spawn_reports_its_cause_inside_the_deadline() {
    let dir = tempfile::tempdir().expect("tempdir");
    let lazy = handle(dir.path().join("no-such-embedderd"), 60);
    let idx = indexer(&lazy, dir.path());

    let started = Instant::now();
    let err = idx
        .embed_query_within("query", Some(Duration::from_secs(5)))
        .await
        .expect_err("a missing binary cannot spawn");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
    let text = format!("{err:#}");
    assert!(text.contains("spawn failed"), "{text}");
    assert!(!text.contains("missed its"), "not a deadline miss: {text}");

    let (vector, detail) = idx
        .embed_query_or_degrade("query", false)
        .await
        .expect("an unpinned query degrades");
    assert!(vector.is_none());
    assert!(detail.is_some_and(|d| d.contains("spawn failed")));
}
