//! Baseline performance regression tests for trusty-search against the trusty-tools project.
//!
//! Why: Provides a reproducible regression gate for query latency, result
//! relevance, graph scoring, and concurrent request throughput — the three axes
//! most likely to degrade silently as the indexing pipeline evolves.
//! Community-detection quality (Louvain) was removed in v0.11.0 per the
//! PROVENANCE-ONLY decision in issue #145 / #152.
//!
//! What: Each test talks JSON-RPC to a sandbox daemon over its Unix socket
//! (#9993: the daemon has bound no TCP port since #9214). The fixture lives in
//! `support/socket_benchmark.rs`; thresholds are documented in
//! `docs/trusty-search/regression-testing/baseline-performance-2026-05-22.md`.
//!
//! Test: All live tests are marked `#[ignore]` so the normal `cargo test` run
//! stays fast. Run with:
//! ```bash
//! TRUSTY_SEARCH_BENCH_SOCKET=<sandbox dir>/data/trusty-search.sock \
//! TRUSTY_SEARCH_BENCH_INDEX=<index id> \
//! TRUSTY_SEARCH_TEST_CORPUS_ROOT=<marked corpus copy> \
//! cargo test -p trusty-search --test baseline_trusty_tools -- --include-ignored --nocapture
//! ```
//!
//! # Prerequisites
//! 1. A sandbox daemon from `scripts/sandbox_search_daemon.sh`. Its socket is
//!    `<dir>/data/trusty-search.sock`; name it in `TRUSTY_SEARCH_BENCH_SOCKET`.
//!    There is no default, and the live install's socket is refused.
//! 2. A disposable corpus copy without `.git`, marked
//!    `.trusty-search-test-corpus`, named in `TRUSTY_SEARCH_TEST_CORPUS_ROOT`.
//! 3. That copy registered and fully indexed under the id named in
//!    `TRUSTY_SEARCH_BENCH_INDEX`. Every call names that id and first checks
//!    that its `root_path` equals the corpus copy.
//!
//! A missing daemon, variable, index or root mismatch fails the test, naming
//! what is missing; nothing is skipped.
//!
//! # Results envelope (#9993 G2)
//! Each latency test reports n, nearest-rank p50/p95/p99, max and qps per
//! operation (`support/latency_stats.rs`). With
//! `TRUSTY_SEARCH_BENCH_ENVELOPE=<absolute path>` set, the rows are written
//! there as the schema-1 envelope shared with trusty-memory
//! (`support/perf_envelope.rs`); unset, nothing is written. Optional:
//! `TRUSTY_SEARCH_BENCH_COMMIT`, `TRUSTY_SEARCH_BENCH_LABEL` (`mac`/`evo`).
//! Run with `--test-threads=1` for a measurement, so tests do not share the
//! daemon while timed.
//!
//! # Regression thresholds
//! - Query latency p50: <= 500 ms
//! - Query latency p99: <= 2000 ms
//! - Index node count:  >= 1 000 (indicates indexing succeeded)

// #9993: the socket fixture reuses the corpus and root checks below.
#[path = "support/isolated_benchmark.rs"]
mod isolated_benchmark;
#[path = "support/socket_benchmark.rs"]
mod socket_benchmark;
use socket_benchmark::{fixture, Fixture};
// #9993 G2: nearest-rank percentiles + qps, written as the schema-1 envelope.
#[path = "support/latency_stats.rs"]
mod latency_stats;
#[path = "support/perf_envelope.rs"]
mod perf_envelope;
use latency_stats::{latency_stats, LatencyStats};
use perf_envelope::{latency_rows, load_now, RunContext};

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

// ── Constants ──────────────────────────────────────────────────────────────

/// Maximum acceptable p50 query latency.
const LATENCY_P50_THRESHOLD_MS: u128 = 500;

/// Maximum acceptable p99 query latency.
const LATENCY_P99_THRESHOLD_MS: u128 = 2000;

/// Minimum graph node count that confirms a successful full reindex.
const MIN_NODE_COUNT: u64 = 1_000;

/// Canonical regression query set.
///
/// Each entry: (query_text, expected_top_file_fragment, intent_label)
///
/// Why: A fixed query set with known expected top files lets us detect both
/// latency regressions (slow search) and relevance regressions (wrong results)
/// in the same run.
const REGRESSION_QUERIES: &[(&str, &str, &str)] = &[
    ("symbol graph BFS expansion", "symbol_graph", "definition"),
    (
        "axum middleware concurrency limiter",
        "concurrency",
        "usage",
    ),
    // Issue #82: fragment was "corpus" but the indexer now returns
    // store/hnsw_store paths as the top hit for redb-transaction queries.
    // Both `store.rs` (HNSW + redb) and `corpus.rs` (redb chunk store) are
    // valid hits — relax the fragment to "store" so any path containing
    // it (store.rs, hnsw_store.rs, persistence) passes.
    ("redb persistence write transaction", "store", "usage"),
    // Issue #82: production top hits include service/server.rs (which
    // orchestrates the embed pool). Both `embed_pool.rs` and `server.rs`
    // are valid; "embed" matches the embedder family and "server" matches
    // the orchestrator. Use the shared substring "embed" which still hits
    // `embed_pool.rs`, `embed.rs`, and `candle_embedder.rs`.
    ("embed batch async worker pool", "embed", "usage"),
    (
        "chunker AST tree-sitter code split",
        "chunker",
        "definition",
    ),
    // #201 anchored this query on UsearchStore::search, query dimension checks,
    // and VectorHit in core/store.rs. The #1137 module split moved that exact
    // implementation to core/store/usearch_impl.rs; retain the intended symbol
    // target instead of the unrelated vector_search.rs ranking captured in #129.
    (
        "usearch search query dim mismatch VectorHit",
        "core/store/usearch_impl.rs",
        "conceptual",
    ),
    // #556 moved detect_project_marker from commands/discover.rs into this
    // dedicated module. Match its source path rather than the old "detect"
    // filename fragment; the query and top-three requirement remain unchanged.
    (
        "detect project context git root marker file",
        "commands/discover/marker.rs",
        "definition",
    ),
];

// ── Helper ─────────────────────────────────────────────────────────────────

/// The `search.query` body every timed query sends.
fn query_body(query: &str) -> Value {
    json!({
        "text": query,
        "top_k": 10,
        "expand_graph": true,
        "compact": true,
    })
}

/// Issue `search.query` over the socket and return `(latency, body)`.
///
/// Why: Factoring out the search call keeps the latency-measurement tests
/// DRY and ensures every call uses the same `top_k` and `expand_graph`
/// settings so results are comparable across runs.
/// What: Guards the index root, then sends `{text, top_k: 10, expand_graph:
/// true, compact: true}` and times only the call. Any refusal panics.
/// Test: called by `test_query_latency_p50_under_threshold` and friends.
async fn search(fx: &Fixture, query: &str) -> (Duration, Value) {
    fx.index_call("search.query", Some(query_body(query)))
        .await
        .unwrap_or_else(|e| panic!("query '{query}': {e}"))
}

/// The envelope header this test knows: daemon version, index, load before.
async fn run_context(fx: &Fixture, load_before: Option<f64>) -> RunContext {
    RunContext {
        version: fx.health().await["version"].as_str().map(str::to_string),
        corpus: Some(fx.index.clone()),
        load_before,
    }
}

/// Summarize serial `samples`, print them, and record them in the envelope.
///
/// What: a serial loop's window is the sum of its samples, because the gaps
/// between calls are untimed guard work. A summary or write failure panics.
fn summarize_serial(
    ctx: &RunContext,
    op: &str,
    samples: &[Duration],
    p99_threshold_ms: Option<f64>,
) -> LatencyStats {
    let window = samples.iter().sum();
    summarize(ctx, op, samples, window, p99_threshold_ms)
}

/// Summarize `samples` over `window`, print, and record the envelope rows.
fn summarize(
    ctx: &RunContext,
    op: &str,
    samples: &[Duration],
    window: Duration,
    p99_threshold_ms: Option<f64>,
) -> LatencyStats {
    let stats = latency_stats(samples, window).unwrap_or_else(|e| panic!("{op}: {e}"));
    println!(
        "{op}: n={} p50={:.2} ms p95={:.2} ms p99={:.2} ms max={:.2} ms qps={:.2}",
        stats.n, stats.p50_ms, stats.p95_ms, stats.p99_ms, stats.max_ms, stats.qps
    );
    perf_envelope::record(ctx, latency_rows(op, &stats, p99_threshold_ms))
        .unwrap_or_else(|e| panic!("{op}: {e}"));
    stats
}

/// Whether `search.indexes.list` names `index`, as a bare id or an `{id}` row.
fn lists_index(listing: &Value, index: &str) -> bool {
    listing["indexes"].as_array().is_some_and(|rows| {
        rows.iter()
            .any(|v| v.as_str() == Some(index) || v["id"].as_str() == Some(index))
    })
}

// ── Tests ──────────────────────────────────────────────────────────────────

/// Verify the daemon answers `search.health` and owns the fixture index.
///
/// Why: If health fails, all downstream tests would produce misleading errors.
/// Failing fast here names the socket the operator must start a daemon on.
/// What: [`fixture`] requires a `search.health` answer; the body must carry
/// `"status":"ok"`, and the index root must equal the corpus copy.
/// Test: this IS the test.
#[tokio::test]
#[ignore = "needs a sandbox trusty-search daemon (support/socket_benchmark.rs)"]
async fn test_daemon_health() {
    let fx = fixture().await;
    println!("daemon socket: {}", fx.socket().display());
    fx.guard().await.unwrap_or_else(|e| panic!("{e}"));
    let body = fx.health().await;
    assert_eq!(
        body["status"], "ok",
        "health.status should be 'ok', got: {body}"
    );
    println!("daemon health: {body}");
}

/// Confirm that the fixture index exists and contains a meaningful graph.
///
/// Why: Subsequent latency and relevance tests are meaningless if the index is
/// empty or absent. Failing here with a clear message saves debugging time.
/// What: `search.indexes.list` names the index;
///       `search.graph.stats` → `node_count >= MIN_NODE_COUNT`.
/// Test: this IS the test.
#[tokio::test]
#[ignore = "needs a sandbox trusty-search daemon (support/socket_benchmark.rs)"]
async fn test_index_exists_and_has_content() {
    let fx = fixture().await;
    fx.assert_ready().await;

    let listing = fx.list_indexes().await;
    assert!(
        lists_index(&listing, &fx.index),
        "index '{}' not found in registered indexes: {listing}",
        fx.index
    );
    println!("registered indexes: {}", listing["indexes"]);

    let (_, stats) = fx
        .index_call("search.graph.stats", None)
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    let node_count = stats["node_count"].as_u64().unwrap_or(0);
    assert!(
        node_count >= MIN_NODE_COUNT,
        "graph node_count {node_count} < MIN_NODE_COUNT {MIN_NODE_COUNT} — \
         has the index been fully reindexed?"
    );
    println!(
        "graph stats: nodes={}, edges={}",
        stats["node_count"], stats["edge_count"]
    );
}

/// Assert that the p50 query latency over the regression set stays below
/// `LATENCY_P50_THRESHOLD_MS`.
///
/// Why: The p50 threshold represents the interactive-feel bar — half of agent
/// queries should complete well within this budget.
/// What: Runs all `REGRESSION_QUERIES` once, collects wall-clock latencies,
/// prints a table, and asserts p50 < threshold.
/// Test: this IS the test.
#[tokio::test]
#[ignore = "needs a sandbox trusty-search daemon (support/socket_benchmark.rs)"]
async fn test_query_latency_p50_under_threshold() {
    let load_before = load_now();
    let fx = fixture().await;
    fx.assert_ready().await;
    let mut latencies: Vec<Duration> = Vec::with_capacity(REGRESSION_QUERIES.len());

    println!("\n{:<50} {:>12}  top_file", "query", "latency_ms");
    println!("{}", "-".repeat(90));

    for (query, _expected_file, _intent) in REGRESSION_QUERIES {
        let (elapsed, body) = search(&fx, query).await;
        let top_file = body["results"][0]["file"]
            .as_str()
            .unwrap_or("<no results>");
        println!("{:<50} {:>12}  {top_file}", query, elapsed.as_millis());
        latencies.push(elapsed);
    }

    assert_eq!(latencies.len(), REGRESSION_QUERIES.len(), "missing samples");
    let ctx = run_context(&fx, load_before).await;
    let stats = summarize_serial(&ctx, "search.query single pass", &latencies, None);
    println!("p50 threshold: {LATENCY_P50_THRESHOLD_MS} ms");

    assert!(
        stats.p50_ms <= LATENCY_P50_THRESHOLD_MS as f64,
        "p50 query latency {:.2} ms exceeds threshold {LATENCY_P50_THRESHOLD_MS} ms",
        stats.p50_ms
    );
}

/// Assert that the p99 query latency over 3× the regression set stays below
/// `LATENCY_P99_THRESHOLD_MS`.
///
/// Why: The p99 threshold catches tail latency caused by lock contention,
/// garbage collection, or cold embedding cache.
/// What: Runs all `REGRESSION_QUERIES` three times each, collects latencies,
/// and asserts p99 < threshold.
/// Test: this IS the test.
#[tokio::test]
#[ignore = "needs a sandbox trusty-search daemon (support/socket_benchmark.rs)"]
async fn test_query_latency_p99_under_threshold() {
    let load_before = load_now();
    let fx = fixture().await;
    fx.assert_ready().await;
    let mut latencies: Vec<Duration> = Vec::with_capacity(REGRESSION_QUERIES.len() * 3);

    for _ in 0..3 {
        for (query, _, _) in REGRESSION_QUERIES {
            let (elapsed, _) = search(&fx, query).await;
            latencies.push(elapsed);
        }
    }

    assert_eq!(
        latencies.len(),
        REGRESSION_QUERIES.len() * 3,
        "missing samples"
    );
    let ctx = run_context(&fx, load_before).await;
    let threshold = LATENCY_P99_THRESHOLD_MS as f64;
    let stats = summarize_serial(&ctx, "search.query serial", &latencies, Some(threshold));
    println!("p99 threshold: {LATENCY_P99_THRESHOLD_MS} ms");

    assert!(
        stats.p99_ms <= threshold,
        "p99 query latency {:.2} ms exceeds threshold {LATENCY_P99_THRESHOLD_MS} ms",
        stats.p99_ms
    );
}

/// Verify that each regression query returns the expected top-result file.
///
/// Why: Latency regressions and relevance regressions are independent failure
/// modes. A reranking bug that swaps result order does not change latency; this
/// test catches it.
/// What: For each `(query, expected_file_fragment, intent)`, asserts that at
/// least one of the top-3 results has a `file` path containing
/// `expected_file_fragment`. Prints a pass/fail table for diagnostics.
/// Test: this IS the test.
#[tokio::test]
#[ignore = "needs a sandbox trusty-search daemon (support/socket_benchmark.rs)"]
async fn test_result_relevance() {
    let fx = fixture().await;
    fx.assert_ready().await;
    let mut failures = 0usize;

    println!(
        "\n{:<50} {:<15} {:<12} result",
        "query", "expected_frag", "pass/fail"
    );
    println!("{}", "-".repeat(100));

    for (query, expected_frag, intent) in REGRESSION_QUERIES {
        let (elapsed, body) = search(&fx, query).await;
        let ms = elapsed.as_millis();

        let results = body["results"].as_array().cloned().unwrap_or_default();
        // Accept a match anywhere in the top-3 to allow minor reranking variance.
        let hit = results.iter().take(3).any(|r| {
            r["file"]
                .as_str()
                .map(|f| f.contains(expected_frag))
                .unwrap_or(false)
        });

        let top_file = results
            .first()
            .and_then(|r| r["file"].as_str())
            .unwrap_or("<no results>");
        let flag = if hit { "PASS" } else { "FAIL" };
        println!(
            "{:<50} {:<15} {:<12} {top_file}  [{ms} ms, intent={}]",
            query,
            expected_frag,
            flag,
            body["intent"].as_str().unwrap_or("?"),
        );

        if !hit {
            failures += 1;
            eprintln!(
                "FAIL relevance: query='{query}' (intent={intent}) — expected top-3 file \
                 containing '{expected_frag}', got: {top_file}"
            );
        }
    }

    assert_eq!(
        failures, 0,
        "{failures} relevance failures — see table above"
    );
}

/// Fire 8 concurrent queries and assert all complete within 5 seconds with no
/// errors.
///
/// Why: Multiple agents running in parallel (a common production pattern for
/// MPM) must not starve each other. Lock contention in the `RwLock<CodeIndexer>`
/// or the embedder pool would surface here as refusals or timeouts.
/// What: Issues 8 queries via `tokio::task::JoinSet`, each on its own socket
/// connection, asserts all succeed and the wall-clock total stays under 5 s.
/// Each task runs the root guard, then waits on a barrier; the window opens
/// when the barrier releases, so no guard is inside it (#9993 G2).
/// Test: this IS the test.
#[tokio::test]
#[ignore = "needs a sandbox trusty-search daemon (support/socket_benchmark.rs)"]
async fn test_concurrent_queries_no_errors() {
    use std::sync::Arc;
    use tokio::sync::Barrier;
    use tokio::task::JoinSet;

    const TASKS: usize = 8;
    let load_before = load_now();
    let fx = fixture().await;
    fx.assert_ready().await;

    // 8 queries drawn from the regression set (cycled if shorter).
    let queries: Vec<&str> = REGRESSION_QUERIES
        .iter()
        .map(|(q, _, _)| *q)
        .cycle()
        .take(TASKS)
        .collect();

    // Every task plus this one; the window opens when all have arrived.
    let start = Arc::new(Barrier::new(TASKS + 1));
    let mut join_set: JoinSet<Result<Duration, String>> = JoinSet::new();

    for query in queries {
        let body = query_body(query);
        let fx = fx.clone();
        let start = Arc::clone(&start);
        join_set.spawn(async move {
            fx.index_call_after("search.query", Some(body), &start)
                .await
                .map(|(elapsed, _)| elapsed)
        });
    }

    start.wait().await;
    let t0 = Instant::now();
    let mut outcomes: Vec<Result<Duration, String>> = Vec::new();
    while let Some(result) = join_set.join_next().await {
        outcomes.push(result.expect("task should not panic"));
    }

    let window = t0.elapsed();
    let total_ms = window.as_millis();
    println!(
        "\n{} concurrent queries completed in {total_ms} ms",
        outcomes.len()
    );
    for (i, outcome) in outcomes.iter().enumerate() {
        println!("  query {i}: {outcome:?}");
    }

    assert_eq!(outcomes.len(), TASKS, "missing concurrent samples");
    let errors: Vec<_> = outcomes.iter().filter_map(|o| o.as_ref().err()).collect();
    assert!(
        errors.is_empty(),
        "{} concurrent queries were refused: {errors:?}",
        errors.len()
    );
    let samples: Vec<Duration> = outcomes.iter().filter_map(|o| o.clone().ok()).collect();
    let ctx = run_context(&fx, load_before).await;
    summarize(&ctx, "search.query concurrent x8", &samples, window, None);

    assert!(
        total_ms < 5_000,
        "8 concurrent queries took {total_ms} ms — exceeds 5 000 ms wall-clock budget"
    );
}

// ── search.grep vs system ripgrep ──────────────────────────────────────────

/// Five representative grep patterns covering literal, regex, and word-boundary
/// shapes — picked to match real LLM grep calls against indexed Rust code.
const GREP_LATENCY_PATTERNS: &[&str] = &[
    "CodeChunk",
    "fn search",
    r"fn \w+_index",
    "HnswIndex",
    "tokenize",
];

/// Shell out to `rg` (preferred) or `grep` against `root` for `pattern`,
/// returning `(hit_count, latency)`.
///
/// Why: a fair comparison scans the same eligible source tree. Mirror the
/// indexer's default directory exclusions so intentionally unindexed build,
/// dependency, and fixture paths are not mistaken for lost search results.
/// What: counts matching Rust lines, as `search.grep` emits one hit per
/// matching line. Scan errors fail the benchmark instead of becoming zero
/// reference hits.
/// Test: used by `test_grep_endpoint_latency_vs_ripgrep`.
fn ripgrep_count(root: &Path, pattern: &str) -> (usize, Duration) {
    let started = Instant::now();
    let mut command = if Command::new("rg").arg("--version").output().is_ok() {
        let mut command = Command::new("rg");
        command.args(["--count", "--no-heading", "-t", "rust", pattern]);
        for dir in trusty_search::service::walker::SKIP_DIRS {
            command.arg("--glob").arg(format!("!**/{dir}/**"));
        }
        command
    } else {
        let mut command = Command::new("grep");
        command.args(["-rEc", "--include=*.rs", pattern]);
        for dir in trusty_search::service::walker::SKIP_DIRS {
            command.arg(format!("--exclude-dir={dir}"));
        }
        command
    };
    let out = command
        .current_dir(root)
        .arg(".")
        .output()
        .expect("reference grep must launch");
    let elapsed = started.elapsed();
    assert!(
        matches!(out.status.code(), Some(0 | 1)),
        "reference grep failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    let mut hits = 0_usize;
    for line in text.lines() {
        let (_, count) = line
            .rsplit_once(':')
            .expect("grep must return per-file counts");
        hits += count
            .trim()
            .parse::<usize>()
            .expect("grep count must be numeric");
    }
    (hits, elapsed)
}

/// Compare `search.grep` latency and hit count against system ripgrep for
/// a representative pattern set.
///
/// Why: `search.grep` must be (a) at least as recall-complete as ripgrep over
/// the indexed source tree and (b) within reach of ripgrep's wall-clock
/// latency — otherwise callers will just shell out themselves and we lose the
/// centralised regex/glob/context surface plus rate limiting.
/// What: the guard proves the index root is the corpus copy; for each of
/// `GREP_LATENCY_PATTERNS` it runs `search.grep` and `rg --count` against that
/// root and prints both latencies, both hit counts, and the ratio. Asserts
/// `search.grep` returns at least as many matches as ripgrep per pattern.
/// Test: this IS the test. Marked `#[ignore]` so the default `cargo test`
/// run stays fast.
#[tokio::test]
#[ignore = "needs a sandbox trusty-search daemon (support/socket_benchmark.rs)"]
async fn test_grep_endpoint_latency_vs_ripgrep() {
    let load_before = load_now();
    let fx = fixture().await;
    fx.assert_ready().await;
    let root = fx.corpus.clone();

    println!("\n=== search.grep vs ripgrep ===");
    println!(
        "socket: {}   index: {}   root: {}",
        fx.socket().display(),
        fx.index,
        root.display()
    );
    println!(
        "| {:<28} | {:>9} | {:>9} | {:>9} | {:>9} | {:>6} |",
        "pattern", "ep hits", "rg hits", "ep ms", "rg ms", "ratio"
    );
    println!(
        "|{:-<30}|{:-<11}|{:-<11}|{:-<11}|{:-<11}|{:-<8}|",
        "", "", "", "", "", ""
    );

    let mut shortfalls: Vec<String> = Vec::new();
    let mut ep_latencies: Vec<Duration> = Vec::with_capacity(GREP_LATENCY_PATTERNS.len());
    let mut rg_latencies: Vec<Duration> = Vec::with_capacity(GREP_LATENCY_PATTERNS.len());

    for pattern in GREP_LATENCY_PATTERNS {
        let req_body = json!({ "pattern": pattern, "max_results": 10_000 });
        let (ep_elapsed, ep_body) = fx
            .index_call("search.grep", Some(req_body))
            .await
            .unwrap_or_else(|e| panic!("grep {pattern}: {e}"));
        assert_eq!(
            ep_body["truncated"], false,
            "grep comparison requires an uncapped response for pattern {pattern}"
        );
        let ep_hits = ep_body["matches"]
            .as_array()
            .map(Vec::len)
            .unwrap_or_default();

        // System ripgrep against the same root.
        let (rg_hits, rg_elapsed) = ripgrep_count(&root, pattern);
        let (ep_ms, rg_ms) = (ep_elapsed.as_millis(), rg_elapsed.as_millis());

        let ratio = if rg_elapsed.is_zero() {
            f64::INFINITY
        } else {
            ep_elapsed.as_secs_f64() / rg_elapsed.as_secs_f64()
        };
        println!(
            "| {:<28} | {:>9} | {:>9} | {:>9} | {:>9} | {:>6.2} |",
            pattern, ep_hits, rg_hits, ep_ms, rg_ms, ratio
        );

        ep_latencies.push(ep_elapsed);
        rg_latencies.push(rg_elapsed);

        // Correctness: the daemon walks the indexed file set and may legitimately
        // see *more* matches than rg when the index covers files rg's type filter
        // misses. Both exclude the walker's default skipped directories.
        // Require ep_hits >= rg_hits as the floor.
        if ep_hits < rg_hits {
            shortfalls.push(format!(
                "pattern={pattern:?}: search.grep returned {ep_hits} matches, rg returned {rg_hits}"
            ));
        }
    }

    let ctx = run_context(&fx, load_before).await;
    summarize_serial(&ctx, "search.grep", &ep_latencies, None);
    summarize_serial(&ctx, "ripgrep reference", &rg_latencies, None);

    assert!(
        shortfalls.is_empty(),
        "search.grep returned fewer matches than ripgrep for {} pattern(s):\n  {}",
        shortfalls.len(),
        shortfalls.join("\n  ")
    );
}
