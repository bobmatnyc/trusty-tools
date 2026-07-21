//! Memory soak harness for the Python/MPS embedder sidecar (epic #3524
//! slice 6, PR 5/5, issue #24).
//!
//! Why: slices 2-4 proved the Python/torch sidecar is functionally correct
//! and faster than the ort path; PR-4 added the swap-back watchdog for when
//! it dies. Before `TRUSTY_PY_DEFAULT` can be flipped on for real users, the
//! repo owner needs real numbers on whether the sidecar (a long-lived torch
//! process, unlike the short-lived-per-batch ort child) leaks memory over a
//! sustained workload — a single short e2e run (`trusty-embedderd-py/tests/e2e.rs`)
//! cannot answer that question. This harness drives a large, sustained batch
//! of embeddings through the REAL supervisor path and records RSS-over-time
//! for both the host (this test process, standing in for the trusty-search
//! daemon) and the sidecar process, so the owner can ratify (or reject) the
//! proposed ceilings against real data.
//!
//! What: two `#[ignore]`d tests reusing the exact same real-process path as
//! `trusty-embedderd-py/tests/e2e.rs` — `EmbedderSupervisor::spawn_stdio` on
//! the `trusty-embedderd-py` launcher, zero changes to supervisor/wire code:
//!
//!   - [`sanity::python_sidecar_sanity_check`] — a cheap single-batch smoke
//!     test proving the local Python/MPS path works at all before attempting
//!     the full soak. Run alone first if the soak fails to bootstrap.
//!   - [`soak::py_sidecar_memory_soak`] — drives `TRUSTY_SOAK_TOTAL_EMBEDS`
//!     (default 100,000) embeddings through the sidecar in
//!     `TRUSTY_SOAK_BATCH_SIZE`-sized batches (default 64), sampling RSS of
//!     both processes every batch, spot-checking correctness (cosine vs. a
//!     reference vector captured at the start) at regular intervals, and
//!     inducing one supervisor crash-restart at the halfway point via
//!     `SIGKILL` on the sidecar's own PID (read from the supervisor's
//!     `child_pid_slot` — the same slot `/health` reads) to prove
//!     correctness survives a swap. Prints a full report (peak RSS, RSS
//!     slope over the last 50% of the run via least-squares regression,
//!     throughput, restart events, correctness log) to stderr for the owner
//!     to review against the proposed (not-yet-ratified) thresholds; the
//!     thresholds themselves are NOT enforced here — this harness reports,
//!     it does not gate CI on a memory ceiling nobody has ratified yet.
//!
//! Gating (matches `trusty-embedderd-py/tests/e2e.rs`'s convention): both
//! tests are `#[ignore]`d — they require a real built Python venv (torch +
//! sentence-transformers, ~2-3 GB, network on a cold cache) — so
//! `cargo test --workspace` never runs them. The soak additionally checks
//! `TRUSTY_SOAK=1` internally (mirroring `TRUSTY_RUN_PY_E2E` in `e2e.rs`) so
//! even an accidental `--include-ignored` run does not accidentally burn
//! several minutes; the sanity check has no such extra gate since it is cheap.
//!
//! Test: run the sanity check first —
//!   `cargo test -p trusty-search --test py_sidecar_memory_soak sanity -- --ignored --nocapture`
//! then the full soak —
//!   `TRUSTY_SOAK=1 cargo test -p trusty-search --test py_sidecar_memory_soak -- --ignored --nocapture`

/// Fixed probe text used for every correctness spot-check in this file. A
/// constant, non-empty, code-shaped string exercises the same tokenizer path
/// as real usage without needing a corpus fixture.
const PROBE_TEXT: &str = "fn authenticate(user: &str, token: &str) -> Result<Session, AuthError>";

/// Cosine similarity between two equal-length vectors. Returns `0.0` if
/// either vector is all-zero (degenerate — should never happen for a real
/// embedding, but avoids a NaN from a zero denominator).
fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    dot / (norm_a * norm_b)
}

/// Ordinary least-squares slope of `points` (x = elapsed seconds, y = RSS in
/// MB). Returns `0.0` for fewer than 2 points (degenerate — can't fit a
/// line) rather than panicking, since the soak must still print a report
/// even if RSS sampling came back sparse.
fn least_squares_slope(points: &[(f64, f64)]) -> f64 {
    let n = points.len() as f64;
    if n < 2.0 {
        return 0.0;
    }
    let sum_x: f64 = points.iter().map(|(x, _)| x).sum();
    let sum_y: f64 = points.iter().map(|(_, y)| y).sum();
    let sum_xy: f64 = points.iter().map(|(x, y)| x * y).sum();
    let sum_xx: f64 = points.iter().map(|(x, _)| x * x).sum();
    let denom = n * sum_xx - sum_x * sum_x;
    if denom.abs() < f64::EPSILON {
        return 0.0;
    }
    (n * sum_xy - sum_x * sum_y) / denom
}

/// Slope (MB/sec) over the last 50% of `samples` — the window the owner's
/// proposed "~flat / no upward growth" threshold applies to. The first half
/// of a soak run typically includes warmup/allocator-settling noise that
/// would bias a whole-run slope; restricting to the tail isolates steady-
/// state behavior.
fn slope_over_last_half(samples: &[(f64, f64)]) -> f64 {
    if samples.len() < 4 {
        return 0.0;
    }
    let half = samples.len() / 2;
    least_squares_slope(&samples[half..])
}

/// Deterministic, code-shaped corpus generator. Five rotating templates keep
/// generation cheap (no RNG, no I/O) while still producing enough lexical
/// variety that the sidecar tokenizes/embeds a realistic mix rather than the
/// exact same string 100,000 times.
fn corpus_batch(start_idx: usize, batch_size: usize) -> Vec<String> {
    const TEMPLATES: [&str; 5] = [
        "fn compute_ID(a: i32, b: i32) -> i32 { a.wrapping_add(b).wrapping_mul(ID) }",
        "struct RecordID { id: u64, name: String, value: f64 }",
        "impl Handler for ServiceID { fn handle(&self, req: Request) -> Response { todo!() } }",
        "async fn fetch_page_ID(url: &str) -> Result<Vec<u8>, Error> { reqwest::get(url).await?.bytes().await.map(|b| b.to_vec()) }",
        "// soak probe ID: the quick brown fox jumps over the lazy dog ID times",
    ];
    (start_idx..start_idx + batch_size)
        .map(|i| TEMPLATES[i % TEMPLATES.len()].replace("ID", &i.to_string()))
        .collect()
}

/// Cheap single-batch smoke test for the local Python/MPS sidecar path.
///
/// Why: the full soak takes minutes and fails loudly but unhelpfully if the
/// underlying venv/launcher path is broken — this test isolates that
/// question to a ~15s run so a bootstrap failure is diagnosed quickly.
mod sanity {
    use trusty_common::embedder_client::{EmbedderSupervisor, SupervisorConfig};

    #[tokio::test]
    #[ignore = "requires a built Python venv (torch); run with --ignored"]
    async fn python_sidecar_sanity_check() {
        let layout = trusty_embedderd_py::ensure_venv_eager().expect("venv bootstrap");
        let launcher = trusty_embedderd_py::locate_launcher_binary().expect("locate launcher");
        eprintln!(
            "sanity: launcher={} venv_python={}",
            launcher.display(),
            layout.venv_python.display()
        );

        let config = SupervisorConfig {
            startup_timeout_secs: 180,
            ..SupervisorConfig::default()
        };
        let (supervisor, client_slot, _pid) = EmbedderSupervisor::spawn_stdio(launcher, config)
            .await
            .expect("supervisor spawn_stdio");
        let handle = supervisor.start_supervisor_task();

        let client = client_slot.read().await.clone();
        let vecs = client
            .embed_batch(vec![super::PROBE_TEXT.to_string()])
            .await
            .expect("embed_batch");

        assert_eq!(vecs.len(), 1, "one vector for one input text");
        assert_eq!(vecs[0].len(), 384, "384-dim");
        let norm: f32 = vecs[0].iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-2, "unit-norm, got {norm}");
        let self_cosine = super::cosine_similarity(&vecs[0], &vecs[0]);
        assert!(
            (self_cosine - 1.0).abs() < 1e-4,
            "a vector's cosine similarity with itself must be ~1.0, got {self_cosine}"
        );

        eprintln!(
            "sanity: OK — embedded 1 vector, 384-dim, unit-norm, self-cosine={self_cosine:.6}"
        );
        handle.shutdown().await;
    }
}

/// The real soak: drives >=100k embeddings through the sidecar with RSS
/// sampling, correctness spot-checks, and an induced crash-restart.
mod soak {
    use std::sync::atomic::Ordering;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use tokio::sync::RwLock;
    use trusty_common::embedder_client::{EmbedderClient, EmbedderSupervisor, SupervisorConfig};
    use trusty_search::core::memguard::{current_rss_mb, current_rss_mb_for_pid};

    const DEFAULT_TOTAL_EMBEDS: usize = 100_000;
    const DEFAULT_BATCH_SIZE: usize = 64;
    /// How many progress/correctness checkpoints to print over the run
    /// (independent of total embed count / batch size).
    const CORRECTNESS_CHECKPOINTS: usize = 20;
    /// How long to keep retrying an embed call against a freshly-killed
    /// sidecar before giving up — covers the supervisor's own respawn +
    /// cold-start window (torch import + model load + one MPS warmup, ~2-3s
    /// per the python-arm idle-shutdown doc, generously bounded).
    const RESPAWN_RETRY_BUDGET: Duration = Duration::from_secs(90);

    /// Send `SIGKILL` to `pid` — the same class of induced failure
    /// `embedder_supervisor_e2e.rs`'s `supervisor_restarts_after_crash` uses
    /// to prove the ort-sidecar crash-restart path; here it proves the same
    /// contract holds for the Python sidecar mid-soak.
    fn kill_pid(pid: u32) {
        let _ = nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(pid as i32),
            nix::sys::signal::Signal::SIGKILL,
        );
    }

    /// `embed_batch` with retry-through-respawn: re-reads the live client
    /// from `slot` on every attempt (the supervisor swaps it in place on
    /// respawn) and retries on error until `RESPAWN_RETRY_BUDGET` elapses.
    ///
    /// Why: immediately after an induced (or spontaneous) crash there is a
    /// real window where `slot` still holds the just-killed client until the
    /// supervision loop finishes respawning — a call issued in that window
    /// gets a broken-pipe-class error. The production `LazyEmbedderHandle`
    /// does not retry at this layer either (callers see the transient
    /// error); this harness retries so the soak's total-embed count and
    /// throughput reflect steady-state behavior around one deliberately
    /// injected crash rather than failing the whole run on it.
    async fn embed_with_retry(
        slot: &Arc<RwLock<Arc<dyn EmbedderClient>>>,
        texts: Vec<String>,
    ) -> Vec<Vec<f32>> {
        let deadline = Instant::now() + RESPAWN_RETRY_BUDGET;
        loop {
            let client = slot.read().await.clone();
            match client.embed_batch(texts.clone()).await {
                Ok(v) => return v,
                Err(e) => {
                    if Instant::now() >= deadline {
                        panic!(
                            "embed_batch kept failing for {RESPAWN_RETRY_BUDGET:?} \
                             (last error: {e}) — sidecar did not recover"
                        );
                    }
                    eprintln!("soak: transient embed_batch error (retrying): {e}");
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
            }
        }
    }

    #[tokio::test]
    #[ignore = "heavy 100k-embed memory soak; requires TRUSTY_SOAK=1 --ignored --nocapture"]
    async fn py_sidecar_memory_soak() {
        if std::env::var("TRUSTY_SOAK").as_deref() != Ok("1") {
            eprintln!("skipping: set TRUSTY_SOAK=1 to run the real memory soak");
            return;
        }

        let total_embeds: usize = std::env::var("TRUSTY_SOAK_TOTAL_EMBEDS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_TOTAL_EMBEDS);
        let batch_size: usize = std::env::var("TRUSTY_SOAK_BATCH_SIZE")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_BATCH_SIZE)
            .max(1);

        let layout = trusty_embedderd_py::ensure_venv_eager().expect("venv bootstrap");
        let launcher = trusty_embedderd_py::locate_launcher_binary().expect("locate launcher");
        eprintln!(
            "soak: launcher={} venv_python={} total_embeds={total_embeds} batch_size={batch_size}",
            launcher.display(),
            layout.venv_python.display(),
        );

        let config = SupervisorConfig {
            startup_timeout_secs: 180,
            max_restarts: 10,
            ..SupervisorConfig::default()
        };
        let (supervisor, client_slot, pid_slot) = EmbedderSupervisor::spawn_stdio(launcher, config)
            .await
            .expect("supervisor spawn_stdio");
        let handle = supervisor.start_supervisor_task();

        // Reference vector captured before any sustained load — every
        // periodic correctness check compares against this, including the
        // check taken right after the induced restart.
        let reference = embed_with_retry(&client_slot, vec![super::PROBE_TEXT.to_string()])
            .await
            .pop()
            .expect("reference embed returned no vector");
        eprintln!("soak: reference vector captured, dim={}", reference.len());

        let total_batches = total_embeds.div_ceil(batch_size);
        let correctness_every_n_batches = (total_batches / CORRECTNESS_CHECKPOINTS).max(1);
        let induce_restart_at_batch = total_batches / 2;

        let start = Instant::now();
        let mut host_samples: Vec<(f64, f64)> = Vec::new();
        let mut sidecar_samples: Vec<(f64, f64)> = Vec::new();
        let mut host_peak_mb: u64 = 0;
        let mut sidecar_peak_mb: u64 = 0;
        let mut correctness_log: Vec<(usize, f32)> = Vec::new();
        let mut restart_events: Vec<(usize, u32, u32)> = Vec::new();
        let mut induced_restart_done = false;
        let mut last_known_pid = pid_slot.load(Ordering::Acquire);
        let mut embeds_done: usize = 0;

        for batch_idx in 0..total_batches {
            let this_batch = batch_size.min(total_embeds - embeds_done);
            let texts = super::corpus_batch(embeds_done, this_batch);
            let vecs = embed_with_retry(&client_slot, texts).await;
            assert_eq!(
                vecs.len(),
                this_batch,
                "batch {batch_idx}: vector count mismatch"
            );
            for v in &vecs {
                assert_eq!(v.len(), 384, "batch {batch_idx}: expected 384-dim vector");
            }
            embeds_done += this_batch;

            // Detect any restart (induced or spontaneous) via a pid change.
            let cur_pid = pid_slot.load(Ordering::Acquire);
            if cur_pid != last_known_pid {
                eprintln!(
                    "soak: RESTART detected at embeds_done={embeds_done}: pid {last_known_pid} -> {cur_pid}"
                );
                restart_events.push((embeds_done, last_known_pid, cur_pid));
                last_known_pid = cur_pid;
            }

            // Sample RSS of both processes every batch.
            let elapsed_s = start.elapsed().as_secs_f64();
            if let Some(mb) = current_rss_mb() {
                host_peak_mb = host_peak_mb.max(mb);
                host_samples.push((elapsed_s, mb as f64));
            }
            if cur_pid != 0 {
                if let Some(mb) = current_rss_mb_for_pid(cur_pid) {
                    sidecar_peak_mb = sidecar_peak_mb.max(mb);
                    sidecar_samples.push((elapsed_s, mb as f64));
                }
            }

            // Periodic correctness spot-check + progress log.
            if (batch_idx + 1) % correctness_every_n_batches == 0 || batch_idx + 1 == total_batches
            {
                let probe = embed_with_retry(&client_slot, vec![super::PROBE_TEXT.to_string()])
                    .await
                    .pop()
                    .expect("probe embed returned no vector");
                let cos = super::cosine_similarity(&reference, &probe);
                correctness_log.push((embeds_done, cos));
                eprintln!(
                    "soak: progress embeds={embeds_done}/{total_embeds} elapsed={elapsed_s:.1}s \
                     host_rss={host_peak_mb}MB(peak) sidecar_rss={sidecar_peak_mb}MB(peak) cosine={cos:.6}"
                );
            }

            // Induce exactly one supervisor crash-restart at the halfway
            // point, then let the next iteration's embed_with_retry ride out
            // the respawn window.
            if !induced_restart_done && batch_idx >= induce_restart_at_batch && cur_pid != 0 {
                eprintln!(
                    "soak: inducing supervisor restart via SIGKILL on pid {cur_pid} at embeds_done={embeds_done}"
                );
                kill_pid(cur_pid);
                induced_restart_done = true;
                // Immediate post-kill correctness check — proves correctness
                // survives the swap itself, not just the eventual steady
                // state a periodic checkpoint might land on well after.
                let post_restart_probe =
                    embed_with_retry(&client_slot, vec![super::PROBE_TEXT.to_string()])
                        .await
                        .pop()
                        .expect("post-restart probe returned no vector");
                let post_restart_cosine = super::cosine_similarity(&reference, &post_restart_probe);
                eprintln!("soak: post-restart correctness check: cosine={post_restart_cosine:.6}");
                correctness_log.push((embeds_done, post_restart_cosine));
            }
        }

        let total_elapsed = start.elapsed();
        let embeds_per_sec = embeds_done as f64 / total_elapsed.as_secs_f64().max(f64::EPSILON);
        let host_slope_mb_per_sec = super::slope_over_last_half(&host_samples);
        let sidecar_slope_mb_per_sec = super::slope_over_last_half(&sidecar_samples);

        eprintln!("=== SOAK REPORT (epic #3524 slice 6, PR 5/5, issue #24) ===");
        eprintln!("total_embeds={embeds_done}");
        eprintln!("wall_clock_s={:.1}", total_elapsed.as_secs_f64());
        eprintln!("embeds_per_sec={embeds_per_sec:.1}");
        eprintln!("sidecar_peak_rss_mb={sidecar_peak_mb}  (proposed ceiling: <=1536 MB / 1.5 GB)");
        eprintln!("host_peak_rss_mb={host_peak_mb}");
        eprintln!(
            "sidecar_rss_slope_mb_per_sec_last50pct={sidecar_slope_mb_per_sec:.4}  (proposed: ~flat)"
        );
        eprintln!("host_rss_slope_mb_per_sec_last50pct={host_slope_mb_per_sec:.4}");
        eprintln!(
            "sidecar_rss_sample_count={} host_rss_sample_count={}",
            sidecar_samples.len(),
            host_samples.len()
        );
        eprintln!(
            "sidecar_rss_tail_samples(last 10, [elapsed_s, mb])={:?}",
            sidecar_samples.iter().rev().take(10).collect::<Vec<_>>()
        );
        eprintln!(
            "host_rss_tail_samples(last 10, [elapsed_s, mb])={:?}",
            host_samples.iter().rev().take(10).collect::<Vec<_>>()
        );
        eprintln!("restart_events(embeds_done, old_pid, new_pid)={restart_events:?}");
        eprintln!("correctness_log(embeds_done, cosine)={correctness_log:?}");

        // Basic engineering-correctness invariants only — NOT the owner's
        // proposed memory-ceiling/flat-slope policy, which this test
        // reports but deliberately does not enforce (see module docs).
        assert!(
            embeds_done >= total_embeds,
            "must complete the requested embed count: {embeds_done} < {total_embeds}"
        );
        assert!(
            induced_restart_done,
            "must have induced at least one supervisor crash-restart during the run"
        );
        for (n, cos) in &correctness_log {
            assert!(
                *cos > 0.90,
                "cosine similarity degraded to {cos} at embeds_done={n} — correctness broken"
            );
        }

        handle.shutdown().await;
    }
}
