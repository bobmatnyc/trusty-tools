//! Embedder throughput bench: Rust `ort` sidecar vs Python/MPS sidecar (epic
//! #3524 slice 7).
//!
//! Why: the epic's acceptance criteria call for an automated throughput
//! measurement "through the supervisor" (matching how the epic's own spike
//! was measured — 561 emb/s end-to-end via a real, isolated daemon), wired
//! into the repo's existing bench-harness conventions
//! (`tests/benchmark_harness.rs`'s side-by-side comparison-table style,
//! `trusty-common/src/bin/candle_metal_bench.rs`'s GO/NO-GO-flavoured
//! backend-vs-backend shape). Two gotchas the epic explicitly flags, learned
//! the hard way during the spike:
//!   1. **Use a BUFFERED stderr reader.** An unbuffered byte-at-a-time reader
//!      over the child's piped stderr distorts the measurement (extra
//!      syscalls/latency on the hot path being timed) and risks missing or
//!      splitting lines under load.
//!   2. **Real throughput is in the `deferred_embed[...]: embedded N/N
//!      chunks` log line, NOT the reindex `complete` event.** Reindexing is
//!      two-phase (issue #923 DEFER-EMBED): the fast BM25-only pass (C1)
//!      returns/logs "complete" in milliseconds; the actual embedding work
//!      (C2, the deferred catch-up pass) runs afterward in the background
//!      and is what this bench needs to time. Measuring the wrong line
//!      yields a confidently wrong (huge) throughput number.
//!
//! What: spawns the REAL `trusty-search` binary (`--foreground`, isolated
//! `--data-dir` and socket directory, dedicated port) once per embedder backend
//! (`TRUSTY_EMBEDDER=stdio` / `TRUSTY_EMBEDDER=python`), registers a synthetic
//! corpus, triggers a reindex over HTTP, and watches the daemon's stderr
//! (via a buffered `tokio::io::BufReader::lines()`) for the
//! `deferred_embed[...]: embedded N/N chunks` line. Elapsed time from the
//! reindex trigger to that line, divided by N, is the measured throughput.
//! Prints a side-by-side comparison table, mirroring
//! `tests/benchmark_harness.rs`'s comparison-table convention.
//!
//! Where this runs: `#[ignore]`-gated — requires a real `trusty-embedderd`
//! (ort) binary alongside the test binary AND, for the python arm, a real
//! `trusty-embedderd-py` launcher with an already-bootstrapped `uv` venv
//! (Apple Silicon + `uv`). Never runs in CI (no MPS/GPU on CI runners, and a
//! cold venv bootstrap alone can take minutes). Set
//! `TRUSTY_SEARCH_TEST_CORPUS_ROOT` to a marked disposable workspace copy
//! outside the allowlist's denied OS temporary roots. Each arm creates its
//! own short runtime directory; the Python venv follows that runtime override,
//! so a warm HOME alone does not provide a warm venv. Bootstrap may consume
//! part of the unchanged 600-second readiness/embedding deadline. Run manually:
//!   `cargo test -p trusty-search --test embedder_throughput_bench -- \
//!      --ignored --nocapture --test-threads=1`
//!
//! Test: `throughput_bench_ort_vs_python` (the only test in this file — a
//! single test so the comparison table is printed once, not once per arm).

#[path = "support/isolated_benchmark.rs"]
mod isolated_benchmark;

use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};

/// Chunk count target for the synthetic corpus. Large enough for a stable
/// throughput reading (the epic's own spike used ~24k embeds; this bench
/// runs per-invocation rather than as a long soak, so a few thousand chunks
/// is enough to amortize daemon-startup/model-load overhead without making
/// every manual run take many minutes).
const SYNTHETIC_FILE_COUNT: usize = 300;
/// Functions per synthetic file — `SYNTHETIC_FILE_COUNT * FUNCTIONS_PER_FILE`
/// is roughly the resulting chunk count (tree-sitter chunks per function).
const FUNCTIONS_PER_FILE: usize = 8;

/// Overall bound on daemon readiness + reindex + embed-pass completion. The
/// python arm's cold model load can take real wall-clock time; generous on
/// purpose since this never runs in CI.
const BENCH_TIMEOUT: Duration = Duration::from_secs(600);

/// One backend's measured result.
#[derive(Debug, Clone)]
struct BenchResult {
    label: &'static str,
    chunks_embedded: u64,
    elapsed: Duration,
}

impl BenchResult {
    fn chunks_per_sec(&self) -> f64 {
        self.chunks_embedded as f64 / self.elapsed.as_secs_f64().max(1e-9)
    }
}

/// Write `SYNTHETIC_FILE_COUNT` small `.rs` files (each `FUNCTIONS_PER_FILE`
/// template functions) into `dir` — a synthetic corpus sized for a stable
/// throughput reading, independent of whatever real source tree happens to
/// be checked out.
///
/// Why: reusing `crates/trusty-search/tests/benchmark_corpus` (47 files, used
/// by `benchmark_harness.rs` for search-QUALITY scoring) would work but is
/// small and its chunk count isn't tunable; a purpose-generated corpus gives
/// a controlled, reproducible chunk count across ort/python runs so the
/// comparison is apples-to-apples.
fn write_synthetic_corpus(dir: &Path) -> std::io::Result<()> {
    const TEMPLATES: &[&str] = &[
        "fn authenticate_user_{i}(token: &str) -> Result<UserId, AuthError> {{ validate(token) }}",
        "/// Why: shared embedding abstraction.\n/// What: async trait with embed_batch.\nfn embed_helper_{i}(x: usize) -> usize {{ x + 1 }}",
        "struct Widget{i} {{ id: u64, name: String }}\nimpl Widget{i} {{ fn new(id: u64) -> Self {{ Self {{ id, name: String::new() }} }} }}",
        "fn compute_checksum_{i}(data: &[u8]) -> u32 {{ data.iter().map(|b| *b as u32).sum() }}",
        "// CoreML EP with MLComputeUnits=ALL allocates from the unified-memory pool.\nfn note_{i}() {{}}",
        "pub fn current_rss_bytes_{i}() -> u64 {{ 0 }}",
        "struct Daemon{i} {{ inner: std::sync::Arc<std::sync::Mutex<usize>> }}",
        "// GET /v1/users/{{id}} returns 200 or 404.\nfn handler_{i}() -> u16 {{ 200 }}",
    ];
    for file_idx in 0..SYNTHETIC_FILE_COUNT {
        let mut contents = String::new();
        for fn_idx in 0..FUNCTIONS_PER_FILE {
            let i = file_idx * FUNCTIONS_PER_FILE + fn_idx;
            let template = TEMPLATES[i % TEMPLATES.len()];
            contents.push_str(&template.replace("{i}", &i.to_string()));
            contents.push_str("\n\n");
        }
        let path = dir.join(format!("synthetic_{file_idx:04}.rs"));
        std::fs::write(&path, contents)?;
    }
    Ok(())
}

/// Spawn `trusty-search start --foreground` for one embedder backend, piping
/// stderr/stderr so the caller can watch for the deferred-embed log line.
///
/// `embedder_env` is the value for `TRUSTY_EMBEDDER` (`"ort"` or
/// `"python"`). Uses `TRUSTY_EMBEDDER=stdio` semantics for `ort` (the
/// existing unset/auto/stdio arm — `stdio` is the explicit, unambiguous
/// spelling) and `TRUSTY_EMBEDDER=python` (the existing eager python arm) for
/// python, rather than the Apple-Silicon-default `GracefulPython` arm — the
/// eager arm blocks daemon startup on the python bootstrap, which is exactly
/// what a single-shot bench wants (no race between "daemon is up" and
/// "python backend is actually the one serving").
fn spawn_daemon(
    embedder_env: &str,
    data_dir: &Path,
    runtime_root: &Path,
    project_registry: &Path,
    port: u16,
) -> Child {
    Command::new(env!("CARGO_BIN_EXE_trusty-search"))
        .args([
            "start",
            "--foreground",
            "--port",
            &port.to_string(),
            "--data-dir",
        ])
        .arg(data_dir)
        .arg("--no-auto-discover")
        .env("TRUSTY_EMBEDDER", embedder_env)
        .env(
            "RUST_LOG",
            "warn,trusty_search::service::reindex::defer_embed=info",
        )
        .env("TRUSTY_DATA_DIR", data_dir)
        // The shared resolver uses a different override for the UDS and
        // Python venv. Never inherit another daemon's socket directory.
        .env("TRUSTY_DATA_DIR_OVERRIDE", runtime_root)
        .env("TRUSTY_MPM_ROOT", project_registry)
        .stdout(Stdio::inherit())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("failed to spawn trusty-search start --foreground")
}

/// Buffered (per the epic's gotcha #1) line-watcher: reads `reader` line by
/// line, forwards every line to stderr (so `--nocapture` shows daemon logs
/// live for debugging), and resolves with `(chunks_embedded, Instant)` the
/// moment a line matches `deferred_embed[...]: embedded N/N chunks`.
async fn watch_for_deferred_embed_complete<R>(reader: &mut R) -> (u64, Instant)
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader
            .read_line(&mut line)
            .await
            .expect("reading daemon stderr failed");
        if n == 0 {
            panic!(
                "daemon stderr closed (process exited?) before the deferred_embed \
                 completion line ever appeared"
            );
        }
        eprint!("[daemon] {line}");
        assert!(
            !is_python_fallback(&line),
            "Python benchmark fell back to ORT: {line}"
        );
        if let Some(chunks) = parse_deferred_embed_line(&line) {
            return (chunks, Instant::now());
        }
    }
}

/// Parse `deferred_embed[<id>]: embedded N/N chunks` (see
/// `crates/trusty-search/src/service/reindex/defer_embed.rs`'s
/// `"deferred_embed[{}]: embedded {}/{} chunks — marking semantic Ready"`
/// log line) and return the chunk count when both numerator and denominator
/// match (a partial/failed pass logs a different message and must not be
/// mistaken for completion).
fn parse_deferred_embed_line(line: &str) -> Option<u64> {
    let marker = "embedded ";
    let idx = line.find(marker)?;
    let rest = &line[idx + marker.len()..];
    let slash = rest.find('/')?;
    let numerator: u64 = rest[..slash].trim().parse().ok()?;
    let after_slash = &rest[slash + 1..];
    let end = after_slash.find(" chunks")?;
    let denominator: u64 = after_slash[..end].trim().parse().ok()?;
    if numerator == denominator && numerator > 0 {
        Some(numerator)
    } else {
        None
    }
}

/// The runtime fallback adapter can retain the outer Python backend label.
/// Its explicit fallback diagnostic must also disqualify the measurement.
fn is_python_fallback(line: &str) -> bool {
    line.contains("FALLING BACK to the") && line.contains("ort")
}

/// Readiness is separate from HTTP liveness. Require the actual requested
/// backend so the Python arm cannot silently measure its ORT fallback.
fn embedder_ready(health: &serde_json::Value, expected: &str) -> Result<bool, String> {
    match health["embedder"].as_str() {
        Some("initializing") => Ok(false),
        Some("ready") => match health["embedder_info"]["backend"].as_str() {
            Some(backend) if backend == expected => Ok(true),
            None | Some("unknown") => Ok(false),
            Some(backend) => Err(format!(
                "expected {expected} embedder, daemon reports {backend}: {health}"
            )),
        },
        Some("error" | "stalled") => Err(format!(
            "embedder initialization or health failed: {health}"
        )),
        other => Err(format!("unexpected embedder health {other:?}: {health}")),
    }
}

/// Poll `GET /health` until it responds successfully or `deadline` passes.
async fn wait_for_daemon_ready(
    child: &mut Child,
    data_dir: &Path,
    client: &reqwest::Client,
    base: &str,
    expected_backend: &str,
    deadline: Instant,
) {
    loop {
        if let Some(status) = child.try_wait().expect("inspect daemon process status") {
            panic!("benchmark daemon exited before readiness: {status}; see child stderr");
        }
        // This fresh private file is published only after HTTP and UDS bind.
        // Fail closed on auto-port shifting instead of contacting an unrelated
        // process that already owns the benchmark's requested port.
        let advertised = std::fs::read_to_string(data_dir.join("http_addr"));
        if let Ok(addr) = &advertised {
            assert_eq!(
                format!("http://{}", addr.trim()),
                base,
                "benchmark port was unavailable"
            );
        }
        if advertised.is_ok() {
            if let Ok(resp) = client.get(format!("{base}/health")).send().await {
                if resp.status().is_success() {
                    let health: serde_json::Value =
                        resp.json().await.expect("decode daemon health");
                    if embedder_ready(&health, expected_backend)
                        .unwrap_or_else(|error| panic!("{error}"))
                    {
                        return;
                    }
                }
            }
        }
        assert!(
            Instant::now() < deadline,
            "daemon at {base} never became ready (GET /health)"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// Run one full bench arm: spawn the daemon, register+reindex the synthetic
/// corpus, and time the deferred-embed completion.
async fn run_bench_arm(label: &'static str, embedder_env: &str, port: u16) -> BenchResult {
    // Keep Unix socket paths beneath sockaddr_un's short path limit. Source
    // files must instead live outside the allowlist's denied temporary roots.
    let runtime_parent = if cfg!(unix) {
        std::path::PathBuf::from("/tmp")
    } else {
        std::env::temp_dir()
    };
    let runtime_root = tempfile::Builder::new()
        .prefix("ts-tp-")
        .tempdir_in(runtime_parent)
        .expect("short temporary runtime directory");
    let data_dir = runtime_root.path().join("trusty-search");
    std::fs::create_dir(&data_dir).expect("create isolated daemon data directory");
    let corpus_dir = tempfile::Builder::new()
        .prefix("throughput-")
        .tempdir_in(isolated_benchmark::corpus_root(""))
        .expect("temporary corpus inside approved disposable workspace");
    let project_registry = runtime_root.path().join("mpm");
    std::fs::create_dir(&project_registry).expect("create private project registry");
    std::fs::write(
        project_registry.join("project-paths.json"),
        serde_json::to_vec(&serde_json::json!([{ "path": corpus_dir.path() }]))
            .expect("serialize synthetic corpus approval"),
    )
    .expect("write child-only synthetic corpus approval");
    write_synthetic_corpus(corpus_dir.path()).expect("write synthetic corpus");

    let mut child = spawn_daemon(
        embedder_env,
        &data_dir,
        runtime_root.path(),
        &project_registry,
        port,
    );
    let mut stderr = BufReader::new(child.stderr.take().expect("child stderr was not piped"));
    // Read from startup onward so bootstrap diagnostics remain visible and
    // a full stderr pipe cannot stall readiness. Continue draining after the
    // completion event until the daemon exits.
    let (complete_tx, complete_rx) = tokio::sync::oneshot::channel();
    let log_task = tokio::spawn(async move {
        let completed = watch_for_deferred_embed_complete(&mut stderr).await;
        let _ = complete_tx.send(completed);
        let mut line = String::new();
        loop {
            line.clear();
            match stderr.read_line(&mut line).await {
                Ok(0) | Err(_) => break,
                Ok(_) => eprint!("[daemon] {line}"),
            }
        }
    });

    let deadline = Instant::now() + BENCH_TIMEOUT;
    let base = format!("http://127.0.0.1:{port}");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .expect("reqwest client");

    wait_for_daemon_ready(&mut child, &data_dir, &client, &base, label, deadline).await;
    eprintln!("[bench:{label}] daemon ready on {base}");

    let create_resp = client
        .post(format!("{base}/indexes"))
        .json(&serde_json::json!({
            "id": "throughput-bench",
            "root_path": corpus_dir.path(),
        }))
        .send()
        .await
        .expect("POST /indexes failed");
    assert!(
        create_resp.status().is_success(),
        "POST /indexes returned {}: {}",
        create_resp.status(),
        create_resp.text().await.unwrap_or_default()
    );

    // Trigger (or re-trigger) an explicit reindex so the deferred-embed pass
    // this bench times definitely runs — registration alone may or may not
    // have already kicked one off; an explicit trigger removes the ambiguity.
    let t_reindex_start = Instant::now();
    let reindex_resp = client
        .post(format!("{base}/indexes/throughput-bench/reindex"))
        .json(&serde_json::json!({ "force": true }))
        .send()
        .await
        .expect("POST /indexes/:id/reindex failed");
    assert!(
        reindex_resp.status().is_success(),
        "POST /reindex returned {}: {}",
        reindex_resp.status(),
        reindex_resp.text().await.unwrap_or_default()
    );

    // THE gotcha: wait for the `deferred_embed[...]: embedded N/N chunks`
    // line, NOT the reindex HTTP response and NOT any "complete" event — the
    // fast BM25-only pass (C1) finishes long before embedding does.
    let (chunks_embedded, t_embedded) = tokio::time::timeout(
        deadline.saturating_duration_since(Instant::now()),
        complete_rx,
    )
    .await
    .expect("timed out waiting for the deferred_embed completion line")
    .expect("daemon log reader exited before embedding completed");

    let elapsed = t_embedded.duration_since(t_reindex_start);
    // An eager Python bootstrap or later sidecar failure can fall back to ORT.
    // Do not label that measurement as Python throughput.
    let health: serde_json::Value = client
        .get(format!("{base}/health"))
        .send()
        .await
        .expect("post-embedding health request")
        .error_for_status()
        .expect("post-embedding health status")
        .json()
        .await
        .expect("decode post-embedding health");
    assert!(
        embedder_ready(&health, label).unwrap_or_else(|error| panic!("{error}")),
        "embedder lost readiness after embedding: {health}"
    );
    eprintln!(
        "[bench:{label}] observed embedder: {}",
        health["embedder_info"]
    );
    eprintln!(
        "[bench:{label}] embedded {chunks_embedded} chunks in {:.2}s ({:.1} chunks/sec)",
        elapsed.as_secs_f64(),
        chunks_embedded as f64 / elapsed.as_secs_f64().max(1e-9),
    );

    // Best-effort clean shutdown; `kill_on_drop(true)` on `child` is the
    // backstop if this hangs.
    let _ = client.post(format!("{base}/admin/stop")).send().await;
    let _ = tokio::time::timeout(Duration::from_secs(10), child.wait()).await;
    log_task.abort();

    BenchResult {
        label,
        chunks_embedded,
        elapsed,
    }
}

/// Print a side-by-side comparison table, mirroring
/// `tests/benchmark_harness.rs`'s comparison-table convention.
fn print_comparison(results: &[BenchResult]) {
    println!("\n=== embedder throughput: ort vs python ===");
    println!(
        "| {:<10} | {:>14} | {:>10} | {:>14} |",
        "backend", "chunks_embedded", "elapsed_s", "chunks/sec"
    );
    println!("|{:-<12}|{:-<16}|{:-<12}|{:-<16}|", "", "", "", "");
    for r in results {
        println!(
            "| {:<10} | {:>14} | {:>10.2} | {:>14.1} |",
            r.label,
            r.chunks_embedded,
            r.elapsed.as_secs_f64(),
            r.chunks_per_sec(),
        );
    }
    if let (Some(ort), Some(python)) = (
        results.iter().find(|r| r.label == "ort"),
        results.iter().find(|r| r.label == "python"),
    ) {
        let speedup = python.chunks_per_sec() / ort.chunks_per_sec().max(1e-9);
        println!(
            "\npython is {:.2}x the ort throughput ({:.1} vs {:.1} chunks/sec)",
            speedup,
            python.chunks_per_sec(),
            ort.chunks_per_sec(),
        );
    }
}

/// Real, `#[ignore]`d throughput comparison. A single test (not one per arm)
/// so both daemons run sequentially on the same port and the comparison
/// table prints once, covering both backends.
///
/// Requires: a real `trusty-embedderd` binary discoverable (built alongside
/// this test binary — `cargo build -p trusty-search` builds it) for the
/// `ort` arm, and a real `trusty-embedderd-py` launcher with an
/// already-bootstrapped `uv` venv for the `python` arm (Apple Silicon +
/// `uv`). Never runs in CI — no MPS/GPU on CI runners.
#[ignore = "spawns real trusty-search daemons (ort + python/MPS sidecars); requires a \
            bootstrapped uv venv for the python arm; not run in CI"]
#[tokio::test(flavor = "multi_thread")]
async fn throughput_bench_ort_vs_python() {
    let mut results = Vec::new();

    eprintln!("=== bench arm: ort ===");
    results.push(run_bench_arm("ort", "stdio", 17_878).await);

    eprintln!("=== bench arm: python ===");
    results.push(run_bench_arm("python", "python", 17_879).await);

    print_comparison(&results);

    for r in &results {
        assert!(
            r.chunks_embedded > 0,
            "{}: no chunks were embedded — the bench measured nothing",
            r.label
        );
    }
}

#[cfg(test)]
mod parse_tests {
    use super::parse_deferred_embed_line;

    #[test]
    fn detects_python_fallback_diagnostics() {
        assert!(super::is_python_fallback("TRUSTY_EMBEDDER=python: venv bootstrap failed — FALLING BACK to the Rust ort stdio sidecar"));
        assert!(super::is_python_fallback("FALLING BACK to the Rust ort stdio sidecar for the remainder of this daemon's lifetime"));
        assert!(!super::is_python_fallback(
            "embedder mode: python/MPS sidecar lazy"
        ));
    }

    #[test]
    fn readiness_requires_requested_backend() {
        for backend in ["ort", "python"] {
            let health =
                serde_json::json!({"embedder":"ready", "embedder_info":{"backend":backend}});
            assert_eq!(super::embedder_ready(&health, backend), Ok(true));
        }
        let fallback = serde_json::json!({"embedder":"ready", "embedder_info":{"backend":"ort"}});
        assert!(super::embedder_ready(&fallback, "python").is_err());
    }

    #[test]
    fn readiness_waits_for_initialization_and_backend_publication() {
        for health in [
            serde_json::json!({"embedder":"initializing"}),
            serde_json::json!({"embedder":"ready"}),
            serde_json::json!({"embedder":"ready", "embedder_info":{"backend":"unknown"}}),
        ] {
            assert_eq!(super::embedder_ready(&health, "ort"), Ok(false));
        }
    }

    #[test]
    fn readiness_rejects_failed_or_malformed_health() {
        for state in ["error", "stalled", "unrecognized"] {
            assert!(super::embedder_ready(&serde_json::json!({"embedder":state}), "ort").is_err());
        }
        assert!(super::embedder_ready(&serde_json::json!({}), "ort").is_err());
    }

    #[test]
    fn matches_a_complete_deferred_embed_line() {
        let line = "2026-07-21T12:00:00Z INFO deferred_embed[my-index]: embedded 2400/2400 chunks — marking semantic Ready\n";
        assert_eq!(parse_deferred_embed_line(line), Some(2400));
    }

    #[test]
    fn ignores_the_reindex_complete_event() {
        // The exact gotcha the epic names: a "complete" event from the FAST
        // BM25-only pass must never be mistaken for the embed-pass line.
        let line =
            "2026-07-21T12:00:00Z INFO reindex[my-index]: complete (chunks=2400, embedded=0)\n";
        assert_eq!(parse_deferred_embed_line(line), None);
    }

    #[test]
    fn ignores_a_zero_chunk_line() {
        let line = "deferred_embed[my-index]: embedded 0/0 chunks — marking semantic Ready\n";
        assert_eq!(parse_deferred_embed_line(line), None);
    }

    #[test]
    fn ignores_a_partial_in_progress_line() {
        let line = "deferred_embed[my-index]: embedded 100/2400 chunks so far\n";
        assert_eq!(parse_deferred_embed_line(line), None);
    }
}
