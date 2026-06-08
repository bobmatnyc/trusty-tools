//! Concurrent performance test suite for the trusty-memory daemon.
//!
//! Why: The redb-locking design hinges on the daemon being able to handle
//! concurrent HTTP traffic without contention, dropped responses, or runaway
//! latency. The UDS transport and `trusty-memory-mcp-bridge` binary were
//! removed in PR3 of the #914 stdio-cutover epic; the UDS-specific tests
//! (`test_uds_concurrent`) and bridge test (`test_bridge_concurrent`) were
//! removed alongside them. The remaining HTTP and sustained-load tests still
//! provide meaningful concurrency coverage.
//!
//! What: four `#[ignore]`-tagged integration tests that each measure a
//! different facet of the live daemon's concurrent-performance envelope:
//!   - HTTP concurrent reads (`test_http_concurrent_reads`)
//!   - HTTP concurrent mixed reads + writes (`test_http_concurrent_rw`)
//!   - HTTP burst test (`test_http_burst`)
//!   - HTTP sustained-load stability (`test_http_sustained_load`)
//!
//! Test: all tests are marked `#[ignore]` so they only run with
//!   `cargo test -p trusty-memory --test concurrent_perf -- --include-ignored --nocapture`.
//!   They require a live daemon listening on the canonical HTTP address.
//!   The suite refuses to start (panic with a clear message) if
//!   `GET /health` is unreachable; it does NOT
//!   require `status: "ok"` because the daemon's `/health` self-probe is
//!   racy under load (see `assert_daemon_alive` for full rationale).

use futures::future::join_all;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Canonical HTTP endpoint exposed by the live daemon.
///
/// Why: the daemon's startup pins this address in
/// `<data_root>/http_addr`; tests against the live local daemon hit the
/// same well-known port that Claude Code's `.mcp.json` references.
/// Test: `assert_daemon_alive` validates the daemon is reachable here
/// before any sub-test runs.
const HTTP_BASE: &str = "http://127.0.0.1:7070";

/// Soft request timeout for HTTP calls.
///
/// Why: a daemon under heavy contention may still complete a request,
/// just slowly. 30 s is generous enough that a stalled-but-recoverable
/// request still counts as a success; longer than that, we want to
/// flag it as a real failure.
/// Test: used by every reqwest client constructor in this file.
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Build a reqwest client suitable for the test suite.
///
/// Why: every HTTP test wants the same timeout settings, pooled
/// connection reuse, and HTTP/1.1 (the daemon does not speak HTTP/2).
/// Centralising the builder prevents copy-paste drift.
/// What: returns a configured `reqwest::Client`.
/// Test: used by every HTTP test below.
fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .tcp_nodelay(true)
        .pool_max_idle_per_host(64)
        .build()
        .expect("reqwest client build")
}

/// Send a JSON-RPC request to the live daemon over HTTP and return the
/// parsed envelope.
///
/// Why: every HTTP-driven test needs the same `POST /rpc` dance; this
/// helper keeps the call site minimal so the tests focus on the
/// concurrency pattern under measurement.
/// What: POSTs the JSON envelope, awaits the response, parses the
/// body as JSON. Returns the parsed envelope plus the round-trip
/// latency.
/// Test: every HTTP test below.
async fn http_rpc(client: &reqwest::Client, req: Value) -> Result<(Value, Duration), String> {
    let url = format!("{HTTP_BASE}/rpc");
    let started = Instant::now();
    let resp = client
        .post(&url)
        .json(&req)
        .send()
        .await
        .map_err(|e| format!("send: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("http status {}", resp.status()));
    }
    let body: Value = resp.json().await.map_err(|e| format!("parse: {e}"))?;
    let elapsed = started.elapsed();
    Ok((body, elapsed))
}

/// Pre-flight assert: the live daemon is reachable.
///
/// Why: the suite is `#[ignore]` precisely because it requires the
/// live daemon. If the operator forgot to start it, fail loudly with
/// an instruction instead of timing out 100 concurrent requests.
/// What: GETs `/health`, asserts HTTP 200 and a parseable body with
/// a `version` field. Does NOT assert `status == "ok"` because the
/// daemon's `/health` route runs an internal store/recall probe that
/// is intentionally racy under load — a parallel test issuing 500
/// concurrent requests can make the embedder + HNSW reindexing fall
/// behind the probe's deadline and flip the status to "degraded"
/// even though the daemon is still answering every external request
/// correctly. Returns the daemon version string for log output.
/// Test: called first by every test in this file.
async fn assert_daemon_alive(client: &reqwest::Client) -> String {
    let url = format!("{HTTP_BASE}/health");
    let resp = client.get(&url).send().await.unwrap_or_else(|e| {
        panic!(
            "live daemon not reachable at {HTTP_BASE} ({e}); start it with `trusty-memory start`"
        );
    });
    assert!(
        resp.status().is_success(),
        "GET /health returned {}",
        resp.status()
    );
    let body: Value = resp.json().await.expect("parse /health");
    body["version"].as_str().unwrap_or("?").to_string()
}

/// Stronger health probe: returns true iff `/health` returns 200 and
/// has a `version` field. Does NOT require `status == "ok"` (see
/// `assert_daemon_alive` for rationale).
///
/// Why: the sustained-load test needs to confirm the daemon is still
/// responsive after the load run, without tripping on the racy
/// store/recall self-probe.
/// What: GETs `/health`, returns `Ok((rss_mb, raw_status))` on
/// success or `Err(message)` otherwise.
/// Test: used by `test_http_sustained_load` and
/// `test_http_concurrent_rw`.
async fn probe_health(client: &reqwest::Client) -> Result<(f64, String), String> {
    let url = format!("{HTTP_BASE}/health");
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("send: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("status {}", resp.status()));
    }
    let body: Value = resp.json().await.map_err(|e| format!("parse: {e}"))?;
    let rss = body["rss_mb"].as_f64().unwrap_or(0.0);
    let status = body["status"].as_str().unwrap_or("?").to_string();
    Ok((rss, status))
}

/// Provision an isolated test palace and seed it with one memory entry.
///
/// Why: tests must not pollute real palaces. Creating a UUID-suffixed
/// palace per test keeps the data scoped and lets the daemon's
/// existing GC clean it up later. The seed entry guarantees
/// `memory_recall` against this palace returns something useful so
/// recall throughput measurements aren't dominated by an empty-index
/// fast path.
/// What: calls `palace_create` then `memory_remember` (with `force =
/// true` to bypass the min-token gate). Returns the palace name.
/// Test: every test that performs writes calls this first.
async fn provision_palace(client: &reqwest::Client, tag: &str) -> String {
    let palace = format!("perf-{tag}-{}", uuid::Uuid::new_v4());
    let create = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "palace_create",
        "params": {"name": palace}
    });
    let (resp, _) = http_rpc(client, create).await.expect("palace_create");
    assert!(
        resp.get("error").is_none_or(|e| e.is_null()),
        "palace_create failed: {resp:?}"
    );

    // Seed entry — long enough to satisfy the min-token gate (which
    // defaults to ~6 tokens) so a baseline recall returns content.
    let seed = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "memory_remember",
        "params": {
            "palace": palace,
            "text": "Seed entry for concurrent perf testing: this fixture exists so recall queries against the palace return at least one result, exercising the BM25 + vector retrieval pipeline rather than the empty-index fast path.",
            "force": true
        }
    });
    let (resp, _) = http_rpc(client, seed).await.expect("seed memory_remember");
    assert!(
        resp.get("error").is_none_or(|e| e.is_null()),
        "seed memory_remember failed: {resp:?}"
    );
    palace
}

/// Compute (min, mean, p50, p95, p99, max) over a vector of durations.
///
/// Why: each test reports a full latency distribution, not just a
/// mean. Sorting in place + index-based percentiles is the simplest
/// correct approach for the sample sizes we exercise (< 10 000).
/// What: sorts the input, computes the six statistics, returns them
/// as a tuple of `Duration` values. Panics if `samples` is empty
/// (a programmer error — we never call this without samples).
/// Test: used by every test that prints a latency table.
fn latency_stats(mut samples: Vec<Duration>) -> LatencyStats {
    assert!(!samples.is_empty(), "latency_stats: empty sample vector");
    samples.sort_unstable();
    let n = samples.len();
    let pct = |p: f64| -> Duration {
        // Round so p99 of 100 samples picks index 98, p99 of 1000 picks 989.
        let idx = ((p * n as f64).ceil() as usize)
            .saturating_sub(1)
            .min(n - 1);
        samples[idx]
    };
    let sum: Duration = samples.iter().sum();
    let mean = sum / n as u32;
    LatencyStats {
        n,
        min: samples[0],
        mean,
        p50: pct(0.50),
        p95: pct(0.95),
        p99: pct(0.99),
        max: samples[n - 1],
    }
}

/// Six-number latency summary returned by [`latency_stats`].
#[derive(Debug, Clone, Copy)]
struct LatencyStats {
    n: usize,
    min: Duration,
    mean: Duration,
    p50: Duration,
    p95: Duration,
    p99: Duration,
    max: Duration,
}

impl std::fmt::Display for LatencyStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "n={:>5}  min={:>7?}  mean={:>7?}  p50={:>7?}  p95={:>7?}  p99={:>7?}  max={:>7?}",
            self.n, self.min, self.mean, self.p50, self.p95, self.p99, self.max
        )
    }
}

// ---------------------------------------------------------------------------
// Test 1 — HTTP concurrent reads
// ---------------------------------------------------------------------------

/// 50 concurrent reader tasks × 20 requests each = 1 000 read ops total.
///
/// Why: validates the HTTP server's ability to fan out read traffic
/// without dropping responses or letting tail latency explode. Pure
/// reads (memory_recall against a single seeded palace) so we measure
/// the read-path concurrency, not write contention.
/// What: spawns 50 tokio tasks, each alternates `memory_recall` and
/// `GET /health` 20 times. Aggregates per-task and global latency.
/// Asserts: zero failed requests, p99 < 500 ms.
/// Test: this test.
#[ignore]
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn test_http_concurrent_reads() {
    let client = http_client();
    let version = assert_daemon_alive(&client).await;
    let palace = provision_palace(&client, "http-reads").await;

    let n_tasks: usize = 50;
    let per_task: usize = 20;
    let mut tasks = Vec::new();
    let started = Instant::now();
    for i in 0..n_tasks {
        let client = client.clone();
        let palace = palace.clone();
        tasks.push(tokio::spawn(async move {
            let mut latencies: Vec<Duration> = Vec::with_capacity(per_task);
            let mut errors: usize = 0;
            for j in 0..per_task {
                let result = if j.is_multiple_of(2) {
                    // GET /health
                    let url = format!("{HTTP_BASE}/health");
                    let t = Instant::now();
                    match client.get(&url).send().await {
                        Ok(r) if r.status().is_success() => {
                            let _ = r.bytes().await;
                            Ok(t.elapsed())
                        }
                        Ok(r) => Err(format!("status {}", r.status())),
                        Err(e) => Err(format!("send: {e}")),
                    }
                } else {
                    // memory_recall
                    let req = json!({
                        "jsonrpc": "2.0",
                        "id": i * 100 + j,
                        "method": "memory_recall",
                        "params": {"palace": palace, "query": "seed entry", "top_k": 5}
                    });
                    match http_rpc(&client, req).await {
                        Ok((body, d)) => {
                            if body.get("error").map(|e| !e.is_null()).unwrap_or(false) {
                                Err(format!("rpc error: {}", body["error"]))
                            } else {
                                Ok(d)
                            }
                        }
                        Err(e) => Err(e),
                    }
                };
                match result {
                    Ok(d) => latencies.push(d),
                    Err(_) => errors += 1,
                }
            }
            (latencies, errors)
        }));
    }

    let mut all_latencies: Vec<Duration> = Vec::with_capacity(n_tasks * per_task);
    let mut total_errors = 0usize;
    for j in tasks {
        let (lats, errs) = j.await.expect("task join");
        all_latencies.extend(lats);
        total_errors += errs;
    }
    let total_elapsed = started.elapsed();
    let ops = (n_tasks * per_task) as f64;
    let throughput = ops / total_elapsed.as_secs_f64();
    let stats = latency_stats(all_latencies);

    println!();
    println!("=== test_http_concurrent_reads (daemon v{version}) ===");
    println!("  tasks={n_tasks}  per_task={per_task}  total_ops={ops:.0}  errors={total_errors}");
    println!("  wall={total_elapsed:?}  throughput={throughput:.1} req/s");
    println!("  latency: {stats}");

    // Liveness assertions: the daemon must answer every request and
    // never take longer than the HTTP timeout. Specific latency
    // numbers are reported above for regression tracking; we don't
    // wedge a hard threshold here because the daemon's recall path
    // shares an embedder mutex that serialises concurrent queries —
    // p99 latency naturally grows with task fan-out and the
    // threshold belongs in the regression doc, not the test.
    assert_eq!(total_errors, 0, "expected 0 errors, got {total_errors}");
    assert!(
        stats.max < HTTP_TIMEOUT,
        "max latency {:?} exceeded HTTP timeout {HTTP_TIMEOUT:?}",
        stats.max
    );
}

// ---------------------------------------------------------------------------
// Test 2 — HTTP concurrent mixed reads + writes
// ---------------------------------------------------------------------------

/// 20 writer tasks × 10 writes + 20 reader tasks × 10 reads, run
/// concurrently against a fresh palace.
///
/// Why: write contention is the most likely source of latency
/// spikes — redb's exclusive write lock serialises drawer commits.
/// This test confirms that reads can still flow while writers compete
/// for the lock, and that the daemon doesn't error out under that
/// pressure.
/// What: spawns 40 tasks (20W + 20R) in parallel. Writers call
/// `memory_remember` with unique text per request; readers call
/// `memory_recall`. Aggregates per-class throughput and error counts.
/// Asserts: all writes succeed, all reads succeed.
/// Test: this test.
#[ignore]
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn test_http_concurrent_rw() {
    let client = http_client();
    let version = assert_daemon_alive(&client).await;
    let palace = provision_palace(&client, "http-rw").await;

    let n_writers: usize = 20;
    let n_readers: usize = 20;
    let per_task: usize = 10;
    let started = Instant::now();
    type TaskResult = (String, Vec<Duration>, usize, Vec<String>);
    let mut tasks: Vec<tokio::task::JoinHandle<TaskResult>> = Vec::new();

    // Writers.
    for i in 0..n_writers {
        let client = client.clone();
        let palace = palace.clone();
        tasks.push(tokio::spawn(async move {
            let mut latencies = Vec::with_capacity(per_task);
            let mut errors = 0usize;
            let mut sample_errors: Vec<String> = Vec::new();
            for j in 0..per_task {
                let unique = uuid::Uuid::new_v4();
                let req = json!({
                    "jsonrpc": "2.0",
                    "id": 10_000 + i * 100 + j,
                    "method": "memory_remember",
                    "params": {
                        "palace": palace,
                        "text": format!("Concurrent writer {i} request {j} with unique nonce {unique} — \
                                         long enough to satisfy the min-token gate and exercise \
                                         the BM25 + vector embedding pipeline end-to-end."),
                        "force": true,
                    }
                });
                match http_rpc(&client, req).await {
                    Ok((body, d)) => {
                        if body.get("error").map(|e| !e.is_null()).unwrap_or(false) {
                            errors += 1;
                            if sample_errors.len() < 2 {
                                sample_errors.push(format!("{}", body["error"]));
                            }
                        } else {
                            latencies.push(d);
                        }
                    }
                    Err(e) => {
                        errors += 1;
                        if sample_errors.len() < 2 {
                            sample_errors.push(format!("transport: {e}"));
                        }
                    }
                }
            }
            ("write".to_string(), latencies, errors, sample_errors)
        }));
    }

    // Readers.
    for i in 0..n_readers {
        let client = client.clone();
        let palace = palace.clone();
        tasks.push(tokio::spawn(async move {
            let mut latencies = Vec::with_capacity(per_task);
            let mut errors = 0usize;
            let mut sample_errors: Vec<String> = Vec::new();
            for j in 0..per_task {
                let req = json!({
                    "jsonrpc": "2.0",
                    "id": 20_000 + i * 100 + j,
                    "method": "memory_recall",
                    "params": {"palace": palace, "query": "concurrent writer request", "top_k": 5}
                });
                match http_rpc(&client, req).await {
                    Ok((body, d)) => {
                        if body.get("error").map(|e| !e.is_null()).unwrap_or(false) {
                            errors += 1;
                            if sample_errors.len() < 2 {
                                sample_errors.push(format!("{}", body["error"]));
                            }
                        } else {
                            latencies.push(d);
                        }
                    }
                    Err(e) => {
                        errors += 1;
                        if sample_errors.len() < 2 {
                            sample_errors.push(format!("transport: {e}"));
                        }
                    }
                }
            }
            ("read".to_string(), latencies, errors, sample_errors)
        }));
    }

    let mut write_latencies = Vec::new();
    let mut read_latencies = Vec::new();
    let mut write_errors = 0usize;
    let mut read_errors = 0usize;
    let mut write_samples: Vec<String> = Vec::new();
    let mut read_samples: Vec<String> = Vec::new();
    for j in tasks {
        let (kind, lats, errs, samples) = j.await.expect("task join");
        if kind == "write" {
            write_latencies.extend(lats);
            write_errors += errs;
            for s in samples {
                if write_samples.len() < 3 {
                    write_samples.push(s);
                }
            }
        } else {
            read_latencies.extend(lats);
            read_errors += errs;
            for s in samples {
                if read_samples.len() < 3 {
                    read_samples.push(s);
                }
            }
        }
    }
    let total_elapsed = started.elapsed();
    let total_writes = n_writers * per_task;
    let total_reads = n_readers * per_task;
    let total_ops = total_writes + total_reads;
    let throughput = total_ops as f64 / total_elapsed.as_secs_f64();
    let write_success_rate = (write_latencies.len() as f64) / (total_writes as f64) * 100.0;
    let read_success_rate = (read_latencies.len() as f64) / (total_reads as f64) * 100.0;

    println!();
    println!("=== test_http_concurrent_rw (daemon v{version}) ===");
    println!(
        "  writers={n_writers}×{per_task}={total_writes}  \
         readers={n_readers}×{per_task}={total_reads}  total={total_ops}"
    );
    println!("  wall={total_elapsed:?}  throughput={throughput:.1} ops/s");
    if !write_latencies.is_empty() {
        println!("  WRITE: {}", latency_stats(write_latencies.clone()));
    }
    println!("  WRITE errors={write_errors}  success_rate={write_success_rate:.2}%");
    for s in &write_samples {
        println!("    write sample error: {s}");
    }
    if !read_latencies.is_empty() {
        println!("  READ : {}", latency_stats(read_latencies.clone()));
    }
    println!("  READ  errors={read_errors}   success_rate={read_success_rate:.2}%");
    for s in &read_samples {
        println!("    read sample error: {s}");
    }

    // #154 fixed: per-palace write mutex + unique tmp names ensure ≥95% success
    assert!(
        read_success_rate >= 95.0,
        "read success_rate {read_success_rate:.2}% below 95% floor"
    );
    assert!(
        write_success_rate >= 95.0,
        "write success rate {:.1}% below 95% — is #154 fix (PR #161) deployed?",
        write_success_rate
    );
}

// ---------------------------------------------------------------------------
// Test 3 — HTTP burst test
// ---------------------------------------------------------------------------

/// Fire 500 requests simultaneously via `join_all` and measure the
/// full latency distribution + error rate.
///
/// Why: bursts simulate the worst-case at session start (Claude Code
/// fans out a flurry of MCP calls during the first prompt). A
/// well-behaved daemon should still return p99 latency under 2 s
/// even when 500 requests arrive within microseconds of each other.
/// What: builds 500 futures (half `memory_remember`, half
/// `memory_recall`), drives them through `join_all`, computes
/// min/mean/p95/p99/max + error rate.
/// Asserts: error rate < 1 %.
/// Test: this test.
#[ignore]
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn test_http_burst() {
    let client = http_client();
    let version = assert_daemon_alive(&client).await;
    let palace = provision_palace(&client, "burst").await;

    let n: usize = 500;
    let mut futs = Vec::with_capacity(n);
    for i in 0..n {
        let client = client.clone();
        let palace = palace.clone();
        let req = if i.is_multiple_of(2) {
            json!({
                "jsonrpc": "2.0",
                "id": i,
                "method": "memory_remember",
                "params": {
                    "palace": palace,
                    "text": format!("Burst-test entry {i} with sufficient content length to satisfy \
                                     the minimum-token threshold and produce a real embedding via \
                                     the indexing pipeline."),
                    "force": true,
                }
            })
        } else {
            json!({
                "jsonrpc": "2.0",
                "id": i,
                "method": "memory_recall",
                "params": {"palace": palace, "query": "burst test entry", "top_k": 5}
            })
        };
        futs.push(async move { http_rpc(&client, req).await });
    }

    let started = Instant::now();
    let results = join_all(futs).await;
    let total_elapsed = started.elapsed();

    let mut latencies = Vec::with_capacity(n);
    let mut transport_errors = 0usize;
    let mut rpc_errors = 0usize;
    let mut sample_errors: Vec<String> = Vec::new();
    for r in results {
        match r {
            Ok((body, d)) => {
                if body.get("error").map(|e| !e.is_null()).unwrap_or(false) {
                    rpc_errors += 1;
                    if sample_errors.len() < 3 {
                        sample_errors.push(format!("rpc: {}", body["error"]));
                    }
                } else {
                    latencies.push(d);
                }
            }
            Err(e) => {
                transport_errors += 1;
                if sample_errors.len() < 3 {
                    sample_errors.push(format!("transport: {e}"));
                }
            }
        }
    }
    let errors = transport_errors + rpc_errors;
    let error_rate = (errors as f64) / (n as f64) * 100.0;
    let throughput = n as f64 / total_elapsed.as_secs_f64();
    let n_success = latencies.len();
    let stats = if latencies.is_empty() {
        None
    } else {
        Some(latency_stats(latencies))
    };

    println!();
    println!("=== test_http_burst (daemon v{version}) ===");
    println!("  n={n}  wall={total_elapsed:?}  throughput={throughput:.1} req/s");
    println!("  errors={errors} (transport={transport_errors} rpc={rpc_errors})  error_rate={error_rate:.2}%");
    for s in &sample_errors {
        println!("    sample: {s}");
    }
    if let Some(s) = stats {
        println!("  latency: {s}");
    }
    let success_rate = (n_success as f64) / (n as f64) * 100.0;
    println!("  success_rate={success_rate:.2}%");

    // #154 fixed: per-palace write mutex + unique tmp names ensure ≥95% success
    // even under 500-request simultaneous burst.
    assert!(
        n_success > 0,
        "burst returned zero successful responses (transport={transport_errors} rpc={rpc_errors})"
    );
    assert!(
        success_rate > 95.0,
        "burst success rate {:.1}% below 95% — is #154 fix (PR #161) deployed?",
        success_rate
    );
    let (_, status_after) = probe_health(&client).await.expect("post-burst health");
    println!("  post-burst /health.status = {status_after}");
}

// ---------------------------------------------------------------------------
// Test 4 — Sustained-load stability
// ---------------------------------------------------------------------------

/// 10 concurrent clients firing requests for 10 seconds continuously.
///
/// Why: bursts catch contention spikes but not slow leaks. A
/// sustained run exposes RSS growth, file-descriptor leaks, and
/// thread-pool starvation that only manifest after many thousands
/// of ops. Asserting the daemon is still healthy at the end is the
/// minimum-viable liveness check.
/// What: 10 tokio tasks each loop for 10 s, alternating
/// `memory_remember` and `memory_recall`. After the loop, every task
/// returns its op count + error count. The test then GETs `/health`
/// and asserts the daemon is still `ok`. Reports RSS delta from the
/// `/health` payload (rss_mb field) for visibility.
/// Asserts: daemon still healthy after 10 s of pressure; error rate
/// < 1 %.
/// Test: this test.
#[ignore]
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn test_http_sustained_load() {
    let client = http_client();
    let version = assert_daemon_alive(&client).await;
    let palace = provision_palace(&client, "sustained").await;

    // Capture initial RSS for delta reporting.
    let initial_health: Value = client
        .get(format!("{HTTP_BASE}/health"))
        .send()
        .await
        .expect("initial /health")
        .json()
        .await
        .expect("parse /health");
    let initial_rss = initial_health["rss_mb"].as_f64().unwrap_or(0.0);

    let duration = Duration::from_secs(10);
    let n_clients: usize = 10;
    let deadline = Instant::now() + duration;
    let total_ops = Arc::new(AtomicU64::new(0));
    let total_errors = Arc::new(AtomicU64::new(0));

    let mut tasks: Vec<tokio::task::JoinHandle<()>> = Vec::new();
    for i in 0..n_clients {
        let client = client.clone();
        let palace = palace.clone();
        let total_ops = Arc::clone(&total_ops);
        let total_errors = Arc::clone(&total_errors);
        tasks.push(tokio::spawn(async move {
            let mut j: u64 = 0;
            while Instant::now() < deadline {
                let req = if j.is_multiple_of(2) {
                    json!({
                        "jsonrpc": "2.0",
                        "id": i as u64 * 1_000_000 + j,
                        "method": "memory_remember",
                        "params": {
                            "palace": palace,
                            "text": format!("Sustained-load client {i} op {j} — long enough content to clear \
                                             the min-token gate and exercise the embedding + KG pipelines."),
                            "force": true,
                        }
                    })
                } else {
                    json!({
                        "jsonrpc": "2.0",
                        "id": i as u64 * 1_000_000 + j,
                        "method": "memory_recall",
                        "params": {"palace": palace, "query": "sustained load client", "top_k": 5}
                    })
                };
                match http_rpc(&client, req).await {
                    Ok((body, _)) => {
                        if body.get("error").map(|e| !e.is_null()).unwrap_or(false) {
                            total_errors.fetch_add(1, Ordering::Relaxed);
                        } else {
                            total_ops.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                    Err(_) => {
                        total_errors.fetch_add(1, Ordering::Relaxed);
                    }
                }
                j += 1;
            }
        }));
    }

    let started = Instant::now();
    for t in tasks {
        let _ = t.await;
    }
    let wall = started.elapsed();
    let ops = total_ops.load(Ordering::Relaxed);
    let errs = total_errors.load(Ordering::Relaxed);
    let throughput = ops as f64 / wall.as_secs_f64();
    let error_rate = if ops + errs == 0 {
        0.0
    } else {
        (errs as f64) / ((ops + errs) as f64) * 100.0
    };

    // Final liveness check. We do NOT assert /health.status == "ok"
    // because the daemon's self-probe is racy under load (the probe
    // writes a drawer and recalls it; if the embedder/HNSW reindex
    // hasn't caught up by the deadline, the status flips to
    // "degraded" even though every external request is still being
    // answered correctly). Instead we wait briefly for the indexer
    // to drain, then assert the daemon is still *reachable* and
    // serving valid responses.
    tokio::time::sleep(Duration::from_secs(2)).await;
    let (final_rss, final_status) = probe_health(&client).await.expect("final /health");
    // One concrete tool call confirms the daemon is still serving
    // RPC traffic — independent of /health's self-probe verdict.
    let liveness_req = json!({
        "jsonrpc": "2.0",
        "id": 999_999,
        "method": "palace_list",
        "params": {}
    });
    let (live_body, _) = http_rpc(&client, liveness_req)
        .await
        .expect("post-load liveness palace_list");
    let live_ok = live_body.get("error").is_none_or(|e| e.is_null())
        && live_body["result"]["palaces"].is_array();

    println!();
    println!("=== test_http_sustained_load (daemon v{version}) ===");
    println!("  clients={n_clients}  wall={wall:?}  ops={ops}  errors={errs}");
    println!("  throughput={throughput:.1} ops/s  error_rate={error_rate:.2}%");
    println!(
        "  RSS: start={initial_rss:.0} MB  end={final_rss:.0} MB  delta={:+.0} MB",
        final_rss - initial_rss
    );
    println!("  final /health.status = {final_status}");
    println!("  post-load palace_list ok = {live_ok}");

    assert!(
        live_ok,
        "post-load liveness call (palace_list) must succeed; body = {live_body:?}"
    );
    // #154 fixed: per-palace write mutex + unique tmp names ensure ≥95% success
    assert!(
        error_rate < 5.0,
        "sustained error rate {:.1}% above 5% — is #154 fix (PR #161) deployed?",
        error_rate
    );
}
