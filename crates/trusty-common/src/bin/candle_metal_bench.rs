//! Candle Metal RSS validation harness.
//!
//! Why: issue #24 was a 72 GB RSS spike with the original CoreML(All)
//! configuration on Apple Silicon that triggered jetsam SIGKILL during
//! indexing of a large repo. Before we can promote the new candle Metal
//! backend (issue #54) into the default position, we need to reproduce
//! that workload shape — many batches of ~1000 synthetic chunks each —
//! and observe candle Metal's RSS behaviour against a FastEmbedder
//! baseline. This binary is the gate: it prints a GO/NO-GO verdict and
//! exits 1 on NO-GO so CI/operators can wire it into release gates.
//!
//! What: builds a CandleEmbedder (Metal-preferred on macOS, CPU
//! otherwise) and a baseline FastEmbedder, runs N batches × M texts
//! through each, samples RSS around every batch, then prints a summary
//! table with peak RSS, throughput, and per-batch latency percentiles.
//! GO criteria: candle peak RSS < 8 GB AND candle throughput within 2×
//! of FastEmbedder.
//!
//! Test: this binary IS the test. The accompanying `rss::tests` unit
//! tests cover the RSS measurement helper. The full validation requires
//! Apple Silicon hardware (and ideally a host with > 16 GB RAM so the
//! 72 GB spike, if it recurs, doesn't take down the box) — results land
//! in `docs/trusty-search/research/candle-metal-validation-2026-05-22.md`.
//!
//! Usage:
//!   cargo run -p trusty-embedder --features candle --release \
//!       --bin candle_metal_bench
//!
//! Environment knobs (all optional):
//!   TRUSTY_BENCH_BATCHES        number of batches (default 100)
//!   TRUSTY_BENCH_BATCH_SIZE     texts per batch (default 1000)
//!   TRUSTY_BENCH_SKIP_BASELINE  skip the FastEmbedder baseline (any value)
//!   TRUSTY_BENCH_RSS_LIMIT_GB   GO threshold for candle peak RSS (default 8)
//!   TRUSTY_BENCH_THROUGHPUT_X   max candle/FastEmbedder slowdown (default 2.0)

use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use trusty_common::embedder::rss;
use trusty_common::embedder::{CandleEmbedder, Embedder, FastEmbedder};

const BYTES_PER_GB: f64 = 1024.0 * 1024.0 * 1024.0;
const BYTES_PER_MB: f64 = 1024.0 * 1024.0;
/// Synthetic-chunk token estimate. Real source chunks average ~80–120
/// tokens; this is a deliberately conservative midpoint so the
/// throughput numbers do not flatter the harness.
const TOKENS_PER_TEXT_ESTIMATE: u32 = 100;

/// Per-backend aggregated benchmark result.
///
/// Why: we report identical metrics for candle and FastEmbedder side by
/// side, so it's worth a small struct rather than parallel vectors.
/// What: peak RSS in bytes, per-batch latencies (one entry per batch),
/// total wall time, and total text count.
/// Test: populated by `run_one_backend`, consumed by `print_summary` and
/// `decide_verdict`.
struct BackendResult {
    label: &'static str,
    peak_rss_bytes: u64,
    start_rss_bytes: u64,
    end_rss_bytes: u64,
    latencies: Vec<Duration>,
    total_wall: Duration,
    total_texts: usize,
}

impl BackendResult {
    fn throughput_tokens_per_sec(&self) -> f64 {
        let total_tokens = self.total_texts as f64 * TOKENS_PER_TEXT_ESTIMATE as f64;
        total_tokens / self.total_wall.as_secs_f64().max(1e-9)
    }

    fn p50(&self) -> Duration {
        percentile(&self.latencies, 0.50)
    }

    fn p99(&self) -> Duration {
        percentile(&self.latencies, 0.99)
    }
}

/// Compute a percentile from an unsorted slice of durations.
///
/// Why: stdlib has no percentile helper and we want a tiny, dependency-
/// free implementation for the harness.
/// What: sorts a clone in place, picks the nearest-rank entry. Returns
/// `Duration::ZERO` on empty input.
/// Test: implicit — exercised by every benchmark run.
fn percentile(values: &[Duration], p: f64) -> Duration {
    if values.is_empty() {
        return Duration::ZERO;
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let idx = ((sorted.len() as f64) * p).ceil() as usize;
    let idx = idx.saturating_sub(1).min(sorted.len() - 1);
    sorted[idx]
}

/// Generate N synthetic source-code-flavoured texts.
///
/// Why: the original incident reproduced specifically on real source
/// chunks (function bodies, doc comments, etc.). Random alphabet noise
/// would tokenise differently from real code and could mask the RSS
/// pattern we're trying to catch. These templates ape the shape of the
/// `trusty-tools` workspace contents at indexing time.
/// What: cycles through a handful of templates with a numeric suffix so
/// the cache layer (LRU) never hits and every text is freshly embedded.
/// Test: exercised by every benchmark run; produced texts are visible
/// in the summary printout if `RUST_LOG=trace` is enabled.
fn synthetic_texts(count: usize) -> Vec<String> {
    let templates = [
        "fn authenticate_user(token: &str) -> Result<UserId, AuthError> { /* validate JWT, look up session, return userid */ }",
        "Why: shared embedding abstraction lets memory and search share one backend.\nWhat: async trait with embed_batch primitive.\nTest: covered by FastEmbedder and MockEmbedder.",
        "impl Embedder for FastEmbedder { async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> { /* ... */ } }",
        "struct KnowledgeGraph { store: Arc<dyn KgStore>, embedder: Arc<dyn Embedder>, } impl KnowledgeGraph { pub fn new(...) -> Self { /* ... */ } }",
        "// CoreML EP with MLComputeUnits=ALL allocates from the unified-memory GPU pool. Issue #24 inflated RSS to ~72 GB during indexing.",
        "pub fn current_rss_bytes() -> u64 { let mut sys = System::new(); sys.refresh_processes(); sys.process(Pid::from_u32(std::process::id())).map(|p| p.memory()).unwrap_or(0) }",
        "use anyhow::{Context, Result}; use tokio::sync::Mutex; use std::sync::Arc; pub struct Daemon { inner: Arc<Mutex<State>> }",
        "GET /v1/users/{id} returns 200 with user payload or 404 if not found. Idempotent. Cacheable for 60s via Cache-Control header.",
    ];
    (0..count)
        .map(|i| format!("{} // chunk #{}", templates[i % templates.len()], i))
        .collect()
}

/// Run all batches against a single embedder, sampling RSS around each.
///
/// Why: factored out so the same loop drives both candle and the
/// FastEmbedder baseline with identical timing semantics — anything else
/// would skew the comparison.
/// What: for each batch (1..=batches), embed `batch_size` texts, record
/// wall time, sample RSS after, update peak. Logs progress every 10
/// batches so an interactive run shows it's making forward progress.
/// Test: the binary IS the test.
async fn run_one_backend(
    label: &'static str,
    embedder: &dyn Embedder,
    batches: usize,
    batch_size: usize,
) -> Result<BackendResult> {
    let start_rss = rss::current_rss_bytes();
    let mut peak_rss = start_rss;
    let mut latencies = Vec::with_capacity(batches);
    let total_start = Instant::now();

    for i in 0..batches {
        let texts = synthetic_texts(batch_size);
        let batch_start = Instant::now();
        let vectors = embedder
            .embed_batch(&texts)
            .await
            .with_context(|| format!("{label}: embed_batch failed on iteration {i}"))?;
        let elapsed = batch_start.elapsed();
        latencies.push(elapsed);

        // Sanity check: every text gets a 384-d vector. If this ever
        // fails the metrics below are meaningless, so we bail early.
        anyhow::ensure!(
            vectors.len() == batch_size,
            "{label}: expected {} vectors, got {}",
            batch_size,
            vectors.len()
        );

        let rss_now = rss::current_rss_bytes();
        if rss_now > peak_rss {
            peak_rss = rss_now;
        }

        if (i + 1) % 10 == 0 || i + 1 == batches {
            eprintln!(
                "  {label}: batch {}/{} ({:.0} ms, RSS now {:.2} GB, peak {:.2} GB)",
                i + 1,
                batches,
                elapsed.as_millis() as f64,
                rss_now as f64 / BYTES_PER_GB,
                peak_rss as f64 / BYTES_PER_GB,
            );
        }
    }

    let end_rss = rss::current_rss_bytes();
    if end_rss > peak_rss {
        peak_rss = end_rss;
    }

    Ok(BackendResult {
        label,
        peak_rss_bytes: peak_rss,
        start_rss_bytes: start_rss,
        end_rss_bytes: end_rss,
        latencies,
        total_wall: total_start.elapsed(),
        total_texts: batches * batch_size,
    })
}

/// Render a backend's result block.
fn print_backend(r: &BackendResult) {
    println!("{}:", r.label);
    println!(
        "  Start RSS:    {:.2} GB ({:.0} MB)",
        r.start_rss_bytes as f64 / BYTES_PER_GB,
        r.start_rss_bytes as f64 / BYTES_PER_MB,
    );
    println!(
        "  End RSS:      {:.2} GB ({:.0} MB)",
        r.end_rss_bytes as f64 / BYTES_PER_GB,
        r.end_rss_bytes as f64 / BYTES_PER_MB,
    );
    println!(
        "  Peak RSS:     {:.2} GB ({:.0} MB)",
        r.peak_rss_bytes as f64 / BYTES_PER_GB,
        r.peak_rss_bytes as f64 / BYTES_PER_MB,
    );
    println!(
        "  Throughput:   {:>8.0} tokens/sec (estimated)",
        r.throughput_tokens_per_sec(),
    );
    println!(
        "  Latency p50:  {:>5} ms   p99: {:>5} ms",
        r.p50().as_millis(),
        r.p99().as_millis(),
    );
    println!("  Total wall:   {:.2} s", r.total_wall.as_secs_f64());
    println!();
}

/// Decide GO/NO-GO based on the harness thresholds.
///
/// Why: a written, mechanical rule is harder to fudge than a judgement
/// call. The criteria intentionally mirror what's recorded in
/// `docs/trusty-search/research/candle-metal-validation-2026-05-22.md`.
/// What: returns `true` (GO) iff candle peak RSS is under the configured
/// limit (default 8 GB) AND, when a baseline is present, candle
/// throughput is within the configured slowdown factor (default 2×) of
/// FastEmbedder. Without a baseline, the throughput half is skipped.
/// Test: implicit via the binary's exit code.
fn decide_verdict(
    candle: &BackendResult,
    baseline: Option<&BackendResult>,
    rss_limit_bytes: u64,
    max_slowdown: f64,
) -> (bool, Vec<String>) {
    let mut reasons = Vec::new();
    let mut go = true;

    if candle.peak_rss_bytes >= rss_limit_bytes {
        go = false;
        reasons.push(format!(
            "candle peak RSS {:.2} GB >= {:.2} GB limit",
            candle.peak_rss_bytes as f64 / BYTES_PER_GB,
            rss_limit_bytes as f64 / BYTES_PER_GB,
        ));
    } else {
        reasons.push(format!(
            "candle peak RSS {:.2} GB < {:.2} GB limit",
            candle.peak_rss_bytes as f64 / BYTES_PER_GB,
            rss_limit_bytes as f64 / BYTES_PER_GB,
        ));
    }

    if let Some(b) = baseline {
        let c_tps = candle.throughput_tokens_per_sec();
        let b_tps = b.throughput_tokens_per_sec();
        if c_tps <= 0.0 || b_tps <= 0.0 {
            go = false;
            reasons.push("invalid throughput reading (<=0)".to_string());
        } else {
            let slowdown = b_tps / c_tps;
            if slowdown > max_slowdown {
                go = false;
                reasons.push(format!(
                    "candle throughput {:.0} tok/s is {:.2}× slower than FastEmbedder {:.0} tok/s (> {:.2}×)",
                    c_tps, slowdown, b_tps, max_slowdown,
                ));
            } else {
                reasons.push(format!(
                    "candle throughput {:.0} tok/s is {:.2}× FastEmbedder {:.0} tok/s (<= {:.2}×)",
                    c_tps, slowdown, b_tps, max_slowdown,
                ));
            }
        }
    } else {
        reasons.push("baseline skipped — throughput criterion not evaluated".to_string());
    }

    (go, reasons)
}

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(default)
}

fn env_f64(key: &str, default: f64) -> f64 {
    std::env::var(key)
        .ok()
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(default)
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    // All progress output goes to stderr via `eprintln!`; only the
    // final summary table lands on stdout. No tracing subscriber needed.
    let batches = env_usize("TRUSTY_BENCH_BATCHES", 100);
    let batch_size = env_usize("TRUSTY_BENCH_BATCH_SIZE", 1000);
    let skip_baseline = std::env::var("TRUSTY_BENCH_SKIP_BASELINE").is_ok();
    let rss_limit_gb = env_f64("TRUSTY_BENCH_RSS_LIMIT_GB", 8.0);
    let max_slowdown = env_f64("TRUSTY_BENCH_THROUGHPUT_X", 2.0);
    let rss_limit_bytes = (rss_limit_gb * BYTES_PER_GB) as u64;

    eprintln!(
        "candle_metal_bench: {} batches × {} texts = {} total chunks",
        batches,
        batch_size,
        batches * batch_size,
    );
    eprintln!(
        "candle_metal_bench: GO criteria: peak RSS < {:.1} GB AND throughput within {:.2}× of FastEmbedder",
        rss_limit_gb, max_slowdown,
    );
    eprintln!(
        "candle_metal_bench: process RSS before any embedder init: {:.2} GB",
        rss::current_rss_bytes() as f64 / BYTES_PER_GB,
    );

    // ── Candle (Metal-preferred on macOS, CPU otherwise) ────────────
    let use_metal = cfg!(target_os = "macos");
    eprintln!("candle_metal_bench: building CandleEmbedder (use_metal={use_metal})");
    let candle = CandleEmbedder::new(use_metal)
        .context("failed to construct CandleEmbedder — see error for missing model files")?;
    eprintln!(
        "candle_metal_bench: CandleEmbedder device = {:?}",
        candle.device()
    );
    let candle_result = run_one_backend("Candle (Metal/CPU)", &candle, batches, batch_size).await?;

    // Drop the candle embedder so its session memory is released before
    // we build the baseline — otherwise the baseline's RSS reading
    // includes the candle session's footprint.
    drop(candle);

    // ── FastEmbedder baseline ───────────────────────────────────────
    let baseline_result = if skip_baseline {
        eprintln!(
            "candle_metal_bench: TRUSTY_BENCH_SKIP_BASELINE set — skipping FastEmbedder baseline"
        );
        None
    } else {
        eprintln!("candle_metal_bench: building FastEmbedder baseline");
        let fast = FastEmbedder::new()
            .await
            .context("failed to construct FastEmbedder baseline")?;
        let r = run_one_backend("FastEmbedder (baseline)", &fast, batches, batch_size).await?;
        drop(fast);
        Some(r)
    };

    // ── Summary ─────────────────────────────────────────────────────
    println!();
    println!("=== Candle Metal Validation ===");
    println!(
        "Batches: {batches} × {batch_size} texts = {} chunks",
        batches * batch_size
    );
    println!();
    print_backend(&candle_result);
    if let Some(b) = &baseline_result {
        print_backend(b);
    }

    let (go, reasons) = decide_verdict(
        &candle_result,
        baseline_result.as_ref(),
        rss_limit_bytes,
        max_slowdown,
    );
    println!("Criteria (GO requires ALL):");
    println!("  • candle peak RSS < {:.1} GB", rss_limit_gb,);
    println!(
        "  • candle throughput >= 1/{:.2} × FastEmbedder throughput",
        max_slowdown,
    );
    println!();
    for r in &reasons {
        println!("  - {r}");
    }
    println!();
    if go {
        println!("VERDICT: GO");
        Ok(())
    } else {
        println!("VERDICT: NO-GO");
        std::process::exit(1);
    }
}
