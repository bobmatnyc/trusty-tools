//! Read-only storage probe of one sandbox index (#9993 G6).
//!
//! Why: the perf plan's storage rows — bytes per index file, the total, bytes
//! per chunk and daemon RSS — had no reader. The probe reads metadata and the
//! daemon's RSS only, so it can run against an indexed sandbox at any time.
//! What: [`storage_probe_records_index_bytes_and_daemon_rss`] connects through
//! G1's socket fixture (the live socket is refused), requires the
//! `sandbox_search_daemon.sh` pid file and argv, checks the index's
//! `root_path` against the corpus copy, then snapshots the storage
//! directories, probes, and snapshots again. A difference fails the test.
//! Rows go to the schema-1 envelope (`support/perf_envelope.rs`) when
//! `TRUSTY_SEARCH_BENCH_ENVELOPE` is set: dimension `storage`, unit `bytes`.
//! Run with:
//! ```bash
//! TRUSTY_SEARCH_BENCH_SOCKET=<sandbox dir>/data/trusty-search.sock \
//! TRUSTY_SEARCH_BENCH_INDEX=<index id> \
//! TRUSTY_SEARCH_TEST_CORPUS_ROOT=<marked corpus copy> \
//! TRUSTY_SEARCH_BENCH_ENVELOPE=<absolute path> \
//! cargo test -p trusty-search --test storage_probe -- --include-ignored --nocapture
//! ```
//! Test: this file; the helpers' unit tests live in
//! `support/storage_probe_tests.rs`.

#[path = "support/isolated_benchmark.rs"]
mod isolated_benchmark;
#[path = "support/latency_stats.rs"]
mod latency_stats;
#[path = "support/perf_envelope.rs"]
mod perf_envelope;
#[path = "support/socket_benchmark.rs"]
mod socket_benchmark;
#[path = "support/storage_probe.rs"]
mod storage_probe;

use perf_envelope::{load_now, RunContext};
use storage_probe::{rss_bytes, sandbox_pid, snapshots, storage_dirs, storage_rows, unchanged};

#[tokio::test]
#[ignore = "needs a sandbox trusty-search daemon (support/socket_benchmark.rs)"]
async fn storage_probe_records_index_bytes_and_daemon_rss() {
    let fx = socket_benchmark::fixture().await;
    let pid = sandbox_pid(fx.socket()).unwrap_or_else(|e| panic!("{e}"));
    let status = fx.guard().await.unwrap_or_else(|e| panic!("{e}"));
    isolated_benchmark::validate_ready(&status)
        .unwrap_or_else(|e| panic!("index {} is not ready: {e}", fx.index));
    let chunk_count = status["chunk_count"]
        .as_u64()
        .unwrap_or_else(|| panic!("index {} status has no chunk_count: {status}", fx.index));
    let data_dir = fx
        .socket()
        .parent()
        .unwrap_or_else(|| panic!("socket {} has no parent", fx.socket().display()));
    let dirs = storage_dirs(data_dir, &fx.index, &fx.corpus).unwrap_or_else(|e| panic!("{e}"));
    let ctx = RunContext {
        version: fx.health().await["version"].as_str().map(str::to_string),
        corpus: Some(fx.index.clone()),
        load_before: load_now(),
    };

    let before = snapshots(&dirs).unwrap_or_else(|e| panic!("before: {e}"));
    let probed = snapshots(&dirs).unwrap_or_else(|e| panic!("probe: {e}"));
    let rss = rss_bytes(pid).unwrap_or_else(|e| panic!("daemon rss: {e}"));
    let after = snapshots(&dirs).unwrap_or_else(|e| panic!("after: {e}"));
    unchanged(&before, &probed).unwrap_or_else(|e| panic!("{e}"));
    unchanged(&before, &after).unwrap_or_else(|e| panic!("{e}"));

    for (dir, (b, a)) in dirs.iter().zip(before.iter().zip(&after)) {
        println!(
            "storage dir {} ({}): before {} entries / {} bytes, after {} entries / {} bytes, identical",
            dir.path.display(),
            dir.layout,
            b.entries.len(),
            b.file_bytes(),
            a.entries.len(),
            a.file_bytes()
        );
        for e in &a.entries {
            println!("  {:?} {} {} {:?}", e.kind, e.rel.display(), e.len, e.modified);
        }
    }
    let rows =
        storage_rows(&dirs, &probed, chunk_count, rss, pid).unwrap_or_else(|e| panic!("{e}"));
    for row in &rows {
        println!(
            "{} | {} | {} | {:?} | {}",
            row.dimension,
            row.metric,
            row.unit,
            row.value,
            row.notes.as_deref().unwrap_or("")
        );
    }
    perf_envelope::record(&ctx, rows).unwrap_or_else(|e| panic!("envelope: {e}"));
}
