//! Index-freshness probe of one sandbox index (#9993 G7).
//!
//! Why: the perf plan's freshness row — how long a file change takes to
//! become searchable — had no instrument. The probe writes, so it runs only
//! against a `scripts/sandbox_search_daemon.sh` daemon and a marked corpus copy.
//! What: [`freshness_probe_measures_write_to_searchable_lag`] connects through
//! G1's socket fixture (the live socket is refused), requires G6's sandbox
//! pid file and argv, checks the index's `root_path` against the corpus copy,
//! and refuses a corpus under a temp root. It then creates a NEW subdirectory
//! `freshness-probe-9993-<nonce>/` in the corpus copy and measures two write
//! paths, 5 rounds x 10 files each, files written one after another:
//! - `watcher`: a plain disk write of `<nonce>.rs`; the daemon's watcher
//!   indexes it.
//! - `file.put`: `search.index.file.put` of the same shape of file under the
//!   same directory; nothing is written to disk.
//!
//! A sample is the time from the write returning to the first
//! `search.query` (`stage: lexical`, `ripgrep_fallback: false`, the index
//! named by `index_id`) whose reply has a hit naming that file, polled every
//! 100 ms. A 60 s timeout, a write error, a search error or a cleanup error
//! fails the test; no sample is dropped. Cleanup always runs: pushed paths
//! are removed with `search.index.file.remove`, then the directory's files and
//! the directory itself. Rows go to the schema-1 envelope when
//! `TRUSTY_SEARCH_BENCH_ENVELOPE` is set: dimension `indexing`, unit `ms`.
//! Run with:
//! ```bash
//! TRUSTY_SEARCH_BENCH_SOCKET=<sandbox dir>/data/trusty-search.sock \
//! TRUSTY_SEARCH_BENCH_INDEX=<index id> \
//! TRUSTY_SEARCH_TEST_CORPUS_ROOT=<marked corpus copy, not under /tmp> \
//! TRUSTY_SEARCH_BENCH_ENVELOPE=<absolute path> \
//! cargo test -p trusty-search --test integration freshness_probe:: -- --include-ignored --nocapture
//! ```
//! Test: this file; the helpers' unit tests live in
//! `support/freshness_probe_tests.rs`.

#[path = "support/freshness_probe.rs"]
mod freshness;

use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::time::Duration;

use futures::FutureExt;
use serde_json::json;
use tokio::time::Instant;

// `freshness` reaches these as `super::latency_stats` / `super::perf_envelope`.
use crate::storage_probe::isolated_benchmark;
use crate::storage_probe::latency_stats;
use crate::storage_probe::perf_envelope::{self, load_now, RunContext};
use crate::storage_probe::probe::{sandbox_pid, storage_dirs};
use crate::storage_probe::socket_benchmark::{self, Fixture};
use freshness::{
    check_write_target, freshness_rows, hit_names_file, nonce, nonce_content, poll_until_hit,
    refuse_temp_root, summarize, NonceDir, Sample, WritePath, DIR_PREFIX, DIR_SETTLE,
    FILES_PER_ROUND, POLL_INTERVAL, ROUNDS, SAMPLE_TIMEOUT,
};

/// The lexical-lane `search.query` body for one nonce.
fn lexical_body(nonce: &str) -> serde_json::Value {
    json!({
        "text": nonce,
        "stage": "lexical",
        "expand_graph": false,
        "top_k": 10,
        "ripgrep_fallback": false,
    })
}

/// Write one nonce file by `path` and measure its lag to the first hit.
///
/// What: guards the index root, writes, stamps `t0` when the write returns,
/// then polls. A `file.put` reply without `indexed: true` is an `Err`.
async fn sample(fx: &Fixture, dir: &mut NonceDir, path: WritePath) -> Result<Sample, String> {
    let nonce = nonce();
    let name = format!("{nonce}.rs");
    let dir_name = dir
        .path()
        .file_name()
        .ok_or_else(|| format!("nonce dir {} has no name", dir.path().display()))?;
    let rel = Path::new(dir_name).join(&name);
    let content = nonce_content(&nonce);
    fx.guard().await?;
    let t0 = match path {
        WritePath::Watcher => {
            dir.write(&name, &content)?;
            Instant::now()
        }
        WritePath::Put => {
            let abs = dir.path().join(&name);
            dir.record_pushed(abs.clone());
            let body = json!({ "path": abs.to_string_lossy(), "content": content });
            let (_, reply) = fx.index_call("search.index.file.put", Some(body)).await?;
            if reply["indexed"] != true {
                return Err(format!(
                    "file.put of {} did not index: {reply}",
                    abs.display()
                ));
            }
            Instant::now()
        }
    };
    let body = lexical_body(&nonce);
    let rel = rel.as_path();
    poll_until_hit(t0, POLL_INTERVAL, SAMPLE_TIMEOUT, || {
        let body = body.clone();
        async move {
            let (_, reply) = fx.index_call("search.query", Some(body)).await?;
            Ok(hit_names_file(&reply, rel))
        }
    })
    .await
    .map_err(|e| format!("{}: {e}", rel.display()))
}

/// Every round of both paths; the first failure ends the run.
async fn run_rounds(
    fx: &Fixture,
    dir: &mut NonceDir,
) -> Result<Vec<(WritePath, Vec<Duration>)>, String> {
    let mut lags = vec![
        (WritePath::Watcher, Vec::new()),
        (WritePath::Put, Vec::new()),
    ];
    for round in 1..=ROUNDS {
        for (path, samples) in lags.iter_mut() {
            for file in 1..=FILES_PER_ROUND {
                let s = sample(fx, dir, *path)
                    .await
                    .map_err(|e| format!("{} round {round} file {file}: {e}", path.label()))?;
                println!(
                    "{} round {round} file {file}: {} ms ({} searches)",
                    path.label(),
                    s.lag.as_millis(),
                    s.attempts
                );
                samples.push(s.lag);
            }
        }
    }
    Ok(lags)
}

/// `search.index.file.remove` every pushed path; `Err` lists the failures.
async fn remove_pushed(fx: &Fixture, pushed: &[PathBuf]) -> Result<(), String> {
    let mut failures = Vec::new();
    for path in pushed {
        let body = json!({ "path": path.to_string_lossy() });
        if let Err(e) = fx.index_call("search.index.file.remove", Some(body)).await {
            failures.push(e);
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}

#[tokio::test]
#[ignore = "needs a sandbox trusty-search daemon (support/socket_benchmark.rs); writes nonce files into the corpus copy"]
async fn freshness_probe_measures_write_to_searchable_lag() {
    let fx = socket_benchmark::fixture().await;
    let pid = sandbox_pid(fx.socket()).unwrap_or_else(|e| panic!("{e}"));
    let status = fx.guard().await.unwrap_or_else(|e| panic!("{e}"));
    isolated_benchmark::validate_ready(&status)
        .unwrap_or_else(|e| panic!("index {} is not ready: {e}", fx.index));
    refuse_temp_root(&fx.corpus).unwrap_or_else(|e| panic!("{e}"));
    let data_dir = fx
        .socket()
        .parent()
        .unwrap_or_else(|| panic!("socket {} has no parent", fx.socket().display()))
        .to_path_buf();
    let mut forbidden: Vec<PathBuf> = storage_dirs(&data_dir, &fx.index, &fx.corpus)
        .unwrap_or_else(|e| panic!("{e}"))
        .into_iter()
        .map(|d| d.path)
        .collect();
    forbidden.push(data_dir);
    let tag = nonce();
    let target = fx.corpus.join(format!("{DIR_PREFIX}{tag}"));
    check_write_target(&target, &fx.corpus, &forbidden).unwrap_or_else(|e| panic!("{e}"));
    let ctx = RunContext {
        version: fx.health().await["version"].as_str().map(str::to_string),
        corpus: Some(fx.index.clone()),
        load_before: load_now(),
    };
    println!(
        "sandbox daemon pid {pid}, index {}, nonce dir {}",
        fx.index,
        target.display()
    );

    let mut dir = NonceDir::create(&fx.corpus, &tag).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(dir.path(), target, "nonce dir moved after the target check");
    tokio::time::sleep(DIR_SETTLE).await;
    let run = AssertUnwindSafe(run_rounds(&fx, &mut dir))
        .catch_unwind()
        .await;

    // Cleanup runs whatever the run did; its failures are reported below.
    let pushed = dir.pushed().len();
    let unpushed = remove_pushed(&fx, dir.pushed()).await;
    let removed = dir.finish();
    println!(
        "cleanup: {pushed} pushed paths removed: {unpushed:?}; nonce dir removed: {removed:?}; {} exists: {}",
        target.display(),
        target.exists()
    );
    let lags = match run {
        Err(panic) => std::panic::resume_unwind(panic),
        Ok(Err(e)) => panic!("freshness run failed: {e}"),
        Ok(Ok(lags)) => lags,
    };
    unpushed.unwrap_or_else(|e| panic!("file.remove cleanup: {e}"));
    removed.unwrap_or_else(|e| panic!("nonce dir cleanup: {e}"));
    assert!(!target.exists(), "{} survived cleanup", target.display());

    let mut rows = Vec::new();
    for (path, samples) in &lags {
        assert_eq!(
            samples.len(),
            ROUNDS * FILES_PER_ROUND,
            "{} samples",
            path.label()
        );
        let summary = summarize(samples).unwrap_or_else(|e| panic!("{}: {e}", path.label()));
        rows.extend(freshness_rows(*path, &summary));
    }
    for row in &rows {
        println!(
            "{} | {} | {} | n {:?} | p50 {:?} p95 {:?} p99 {:?} max {:?} value {:?} | {}",
            row.dimension,
            row.metric,
            row.unit,
            row.n,
            row.p50,
            row.p95,
            row.p99,
            row.max,
            row.value,
            row.notes.as_deref().unwrap_or("")
        );
    }
    perf_envelope::record(&ctx, rows).unwrap_or_else(|e| panic!("envelope: {e}"));
}
