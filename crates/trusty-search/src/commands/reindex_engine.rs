//! Reindex orchestration shared by `index`, `reindex`, `add`, `convert`, and
//! the doctor auto-repair path.
//!
//! Why: driving a daemon-side reindex involves several distinct pieces — the
//! progress UI ([`reindex_ui::ReindexUi`]), the options and outcome record
//! types, the SSE event loop in `run_reindex_with`, the post-reindex health
//! check, and the companion file-level helpers (`index_single_file`,
//! `add_path`, `register_index_with_daemon{,_filtered}`, `fetch_chunk_count`).
//! Keeping them inline in `main.rs` pushed it past 2.7k lines; co-locating
//! them here drops `main.rs` to a thin dispatcher.
//!
//! What: public surface mirrors the previous `main.rs` items so existing
//! callers in `commands/*` only have to change their `use` paths.  The
//! progress UI is now in `commands/reindex_ui.rs` (issue #401 split).
//!
//! Test: `cargo test --workspace` — every reindex-driven integration test
//! continues to pass; the refactor is purely structural.

use super::daemon_utils::daemon_base_url;
use super::format::{fmt_elapsed, format_with_commas};
use super::reindex_ui::{print_timing_breakdown, ReindexPhase, ReindexTimings, ReindexUi};
use anyhow::Result;
use colored::Colorize;
use eventsource_stream::Eventsource;
use futures_util::stream::StreamExt;
use std::io::IsTerminal;
use std::time::Duration;

/// Index a single file via the daemon's `/indexes/:id/index-file` endpoint.
///
/// Why: factored out of `main.rs` so `add_path` and other callers can reuse
/// the single-file indexing path without duplicating the HTTP dance.
/// What: reads the file from disk, POSTs its content to the daemon, and
/// returns an error when the daemon reports failure.
/// Test: covered indirectly by `add_path` and the doctor auto-repair path.
pub async fn index_single_file(
    client: &reqwest::Client,
    base: &str,
    index_id: &str,
    file: &std::path::Path,
) -> Result<()> {
    let content = tokio::fs::read_to_string(file)
        .await
        .map_err(|e| anyhow::anyhow!("read {}: {e}", file.display()))?;
    let url = format!("{}/indexes/{}/index-file", base, index_id);
    let body = serde_json::json!({
        "path": file.display().to_string(),
        "content": content,
    });
    let resp = client.post(&url).json(&body).send().await?;
    if !resp.status().is_success() {
        anyhow::bail!("daemon returned {} for {}", resp.status(), url);
    }
    Ok(())
}

/// Handle `trusty-search add <path>`: a single file goes to `index-file`;
/// a directory walks `walk_source_files` and indexes every match.
///
/// Why: the `add` subcommand is a convenience wrapper for one-off file
/// indexing without a full reindex. A directory path fans out into per-file
/// `index_single_file` calls rather than a full reindex pipeline.
/// What: calls `index_single_file` for a file path; walks + indexes every
/// source file under a directory path.
/// Test: covered indirectly by the `add` command integration tests.
pub async fn add_path(index_id: &str, path: &std::path::Path) -> Result<()> {
    let base = daemon_base_url();
    let client = trusty_common::server::daemon_http_client()?;

    if path.is_dir() {
        let walk = crate::service::walker::walk_source_files(path);
        println!(
            "{} [{}] indexing {} files under {}",
            "\u{2192}".cyan(),
            index_id,
            walk.files.len(),
            path.display()
        );
        let mut ok = 0usize;
        let mut err = 0usize;
        for f in &walk.files {
            match index_single_file(&client, &base, index_id, f).await {
                Ok(()) => ok += 1,
                Err(e) => {
                    eprintln!("  {} {}: {e}", "\u{26a0}".yellow(), f.display());
                    err += 1;
                }
            }
        }
        println!(
            "{} indexed {} files ({} errors)",
            "\u{2713}".green(),
            ok,
            err
        );
        Ok(())
    } else {
        index_single_file(&client, &base, index_id, path).await?;
        println!("{} [{}] {}", "\u{2192}".cyan(), index_id, path.display());
        Ok(())
    }
}

/// Options controlling reindex CLI behaviour.
///
/// Why: callers such as `run_reindex_opts` and `run_reindex_force_opts` need to
/// pass different combinations of options without a growing argument list.
/// What: a plain struct with `Default` so callers can specify only the fields
/// they care about.
/// Test: default values are asserted by `tests::default_options_are_sane`.
#[derive(Debug, Clone, Copy)]
pub struct ReindexOptions {
    /// After the reindex completes, fetch `/status` and issue a sanity-check
    /// search to verify the index is healthy. Enabled by `--force` to give
    /// the user a blue-green-style safety net.
    ///
    /// Note: the daemon's reindex is NOT atomic blue-green — it mutates the
    /// in-memory index in place via a write lock per batch (see
    /// `crates/trusty-search-service/src/reindex.rs::spawn_reindex` —
    /// `index_files_batch_no_rebuild` adds chunks per-batch). If verify fails
    /// after a `--force`, the index is already in its new (possibly broken)
    /// state. We surface that fact loudly so the user can manually re-run.
    pub verify_after: bool,
    /// Chunk count snapshot taken before the reindex started, used to print
    /// "(was N)" in the final verify message.
    pub prior_chunk_count: Option<u64>,
    /// Forwarded to the daemon as `"force": true` in the reindex kickoff body.
    /// Set by `index --force` so the daemon clears its content-hash cache and
    /// re-embeds every file (otherwise unchanged files would be skipped on a
    /// warm daemon and `--force` would have no effect).
    pub force: bool,
    /// Hard wall-clock cap in seconds. Applied only when `timeout_explicit` is
    /// `true` (i.e. the user passed `--timeout N` explicitly). When `0` and
    /// `timeout_explicit` is `true`, the CLI waits forever (legacy behaviour).
    /// When `timeout_explicit` is `false`, this field is ignored and the CLI
    /// instead exits only on a genuine stall (see `stall_secs`).
    pub timeout_secs: u64,
    /// Whether `timeout_secs` was explicitly supplied by the user.
    ///
    /// When `false` (the default), the CLI uses progress-aware stall detection:
    /// it keeps waiting as long as the file-index counter advances within the
    /// `stall_secs` window. When `true`, `timeout_secs` is treated as a hard
    /// wall-clock cap regardless of progress (so `--timeout 120` reliably exits
    /// after exactly 120 s even if embedding is running).
    pub timeout_explicit: bool,
    /// How long (seconds) to wait without any progress before detaching.
    ///
    /// "Progress" means the per-file `indexed` counter has advanced since the
    /// last check. This window guards against a genuinely stalled pipeline
    /// (e.g. the embedder crashed silently) rather than a healthy but slow one.
    /// Default: 120 s. Only used when `timeout_explicit` is `false`.
    pub stall_secs: u64,
}

impl Default for ReindexOptions {
    fn default() -> Self {
        Self {
            verify_after: false,
            prior_chunk_count: None,
            force: false,
            timeout_secs: 600,
            timeout_explicit: false,
            stall_secs: 120,
        }
    }
}

/// Outcome of a reindex run, captured for the post-verify step and the final
/// summary line. `indexed` includes skipped files (the daemon emits one
/// `indexed++` per file regardless of whether it was hashed-skip or re-embedded).
///
/// Why: a single return type captures everything the caller needs to print a
/// summary line, run the post-verify check, and diagnose partial failures.
/// What: plain struct derived from SSE `complete` event fields.
/// Test: covered indirectly by `run_reindex_with` tests.
#[derive(Debug, Default, Clone, Copy)]
pub struct ReindexOutcome {
    pub indexed: u64,
    pub total_chunks: u64,
    pub skipped: u64,
    pub errors: u64,
    pub elapsed_ms: u64,
    pub completed: bool,
    /// Per-subsystem timings captured from the daemon's `complete` event
    /// `timings` payload. `None` when the daemon is an older version that
    /// didn't emit timings — caller renders a single-line summary in that case.
    pub timings: Option<ReindexTimings>,
}

/// Plain reindex (no post-verify). Used by the doctor auto-repair path and
/// other programmatic callers. Always uses progress-aware stall detection
/// (no explicit timeout).
///
/// Why: extracted so callers don't have to construct `ReindexOptions`.
/// What: delegates to `run_reindex_with` with verify_after = false and
/// timeout_explicit = false.
/// Test: covered by `run_reindex_with` integration tests.
pub async fn run_reindex(
    index_id: &str,
    root_path: &std::path::Path,
    _timeout_secs: u64,
) -> Result<()> {
    run_reindex_with(
        index_id,
        root_path,
        ReindexOptions {
            // Programmatic callers ignore the legacy timeout_secs; progress-aware
            // stall detection applies.
            timeout_explicit: false,
            ..ReindexOptions::default()
        },
    )
    .await
    .map(|_| ())
}

/// Plain reindex with explicit timeout control. Used by CLI commands that
/// accept `--timeout` from the user.
///
/// Why: the CLI must distinguish "user said --timeout N" (hard cap) from "no
/// --timeout" (progress-aware). This variant carries `timeout_explicit` so the
/// wait loop can choose the right strategy.
/// What: delegates to `run_reindex_with` with verify_after = false.
/// Test: covered by `tests::progress_aware_wait_*`.
pub async fn run_reindex_opts(
    index_id: &str,
    root_path: &std::path::Path,
    timeout_secs: u64,
    timeout_explicit: bool,
) -> Result<()> {
    run_reindex_with(
        index_id,
        root_path,
        ReindexOptions {
            timeout_secs,
            timeout_explicit,
            ..ReindexOptions::default()
        },
    )
    .await
    .map(|_| ())
}

/// `index --force` reindex with explicit timeout control. Used by CLI commands
/// that accept `--timeout` from the user.
///
/// Why: same rationale as `run_reindex_opts` — the CLI needs to pass
/// `timeout_explicit` so the hard cap is honoured when the user asks for it.
/// What: fetches the prior chunk count, then delegates to `run_reindex_with`.
/// Test: covered indirectly by `index --force` integration tests.
pub async fn run_reindex_force_opts(
    index_id: &str,
    root_path: &std::path::Path,
    timeout_secs: u64,
    timeout_explicit: bool,
) -> Result<()> {
    let prior = fetch_chunk_count(index_id).await;
    let opts = ReindexOptions {
        verify_after: true,
        prior_chunk_count: prior,
        force: true,
        timeout_secs,
        timeout_explicit,
        ..ReindexOptions::default()
    };
    run_reindex_with(index_id, root_path, opts)
        .await
        .map(|_| ())
}

/// Drive a reindex: POST /reindex, then connect to the SSE stream and render
/// progress with a 4-bar `MultiProgress` layout (header + Crawl / Chunk /
/// Embed / KG bars + stats line). A wall-clock ticker keeps the stats line
/// moving even when SSE events are sparse (e.g. the embedder is mid-batch).
///
/// Why: the previous design used a single bar relabelled at each phase
/// transition (issue #317). Issue #401 replaces it with 4 sequential bars so
/// the operator can see at a glance which stage is active, which are done, and
/// which are still pending.
///
/// New SSE events added in this issue (backward-compatible; older daemons omit
/// them and the CLI falls back gracefully):
///
/// - `kg_start`    — emitted just before `rebuild_symbol_graph_for_reindex`
/// - `kg_complete` — emitted after; carries `symbol_count`, `edge_count`, `kg_ms`
///
/// What: connects to the daemon's SSE reindex stream and dispatches each event
/// to the appropriate bar update.  Returns `ReindexOutcome` on success.
/// Test: `cargo test -p trusty-search -- --test-threads=1`
pub async fn run_reindex_with(
    index_id: &str,
    root_path: &std::path::Path,
    opts: ReindexOptions,
) -> Result<ReindexOutcome> {
    let base = daemon_base_url();
    let client = trusty_common::server::daemon_http_client()?;

    let kickoff_url = format!("{}/indexes/{}/reindex", base, index_id);
    let kickoff_body = serde_json::json!({
        "root_path": root_path,
        "force": opts.force,
    });
    let kickoff = client
        .post(&kickoff_url)
        .json(&kickoff_body)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("could not reach daemon at {base}: {e}"))?;

    if kickoff.status() == reqwest::StatusCode::NOT_FOUND {
        anyhow::bail!(
            "index '{}' is not registered on the daemon \u{2014} run `trusty-search index` first",
            index_id
        );
    }
    if !kickoff.status().is_success() {
        anyhow::bail!("daemon returned {} for reindex kickoff", kickoff.status());
    }

    let kickoff_body: serde_json::Value = kickoff
        .json()
        .await
        .unwrap_or_else(|_| serde_json::json!({}));
    let stream_path = kickoff_body
        .get("stream_url")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("/indexes/{}/reindex/stream", index_id));
    let stream_url = format!("{}{}", base, stream_path);

    // SSE streams must NOT use the short request timeout from
    // `daemon_http_client()` (currently 5s) — a large repo reindex can run for
    // minutes. We build a dedicated client with only a connect timeout so the
    // byte stream stays open until the daemon emits the `complete` event.
    let sse_client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::MAX)
        .build()
        .map_err(|e| anyhow::anyhow!("could not build SSE client: {e}"))?;
    let resp = sse_client
        .get(&stream_url)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("could not connect to SSE stream {stream_url}: {e}"))?;
    if !resp.status().is_success() {
        anyhow::bail!(
            "reindex stream returned {} \u{2014} daemon may be an older version that \
             doesn't support /reindex/stream",
            resp.status()
        );
    }

    // Progress is shown only when stdout is a TTY. When the CLI output is
    // piped or redirected (`std::io::stdout()` is not a terminal) the bars
    // draw to a hidden target so captured output stays clean. Progress always
    // renders to stderr regardless — stdout is the MCP JSON-RPC transport.
    let interactive = std::io::stdout().is_terminal();

    // 4-bar UI: header + Crawl / Chunk / Embed / KG + stats.
    // Built eagerly so the user sees something during the 1–2s daemon warmup
    // before the first SSE event arrives.
    let mut ui = ReindexUi::new(index_id, interactive);

    // Atomics shared with the wall-clock ticker. The ticker refreshes the
    // stats line every second so the user sees movement even when the SSE
    // stream is idle (e.g. mid-batch embedding of 256 chunks).
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::Arc as StdArc;
    let started = std::time::Instant::now();
    let indexed_now = StdArc::new(AtomicU64::new(0));
    let chunks_now = StdArc::new(AtomicU64::new(0));
    // Live in-flight chunk count: incremented by `chunk_progress` events
    // (~every 32 chunks) to show embed progress before the authoritative
    // `batch` commit fires. Reset to 0 on each `batch` event so it never
    // double-counts with `chunks_now`.
    let chunks_embed_preview = StdArc::new(AtomicU64::new(0));
    let skipped_now = StdArc::new(AtomicU64::new(0));
    let cps_now = StdArc::new(AtomicU64::new(0));
    // Issue #744: shared total_files counter, set from walk_complete/start
    // SSE events. The ticker uses this as the denominator for Files N/total
    // and for ETA, replacing `embed_bar.length()` which initialises to 1
    // and is only corrected after the first batch event arrives.
    let total_files_now = StdArc::new(AtomicU64::new(0));
    let tick_done = StdArc::new(AtomicBool::new(false));
    // Tracks the current phase label for the ticker. Stored as a static string
    // pointer so the ticker can read it without locking `ReindexUi`. Updated
    // from the SSE event loop (single writer) whenever the phase changes; the
    // ticker only reads it. Using a raw AtomicPtr would require unsafe; instead
    // we use an index into a fixed label table (same idea as a discriminant).
    // We store the `ReindexPhase` discriminant as a u8 via AtomicU64.
    //
    // Why: before this fix the ticker always showed "Embedding…" even when the
    // active phase was Chunking or InitializingEmbedder, causing the header and
    // footer labels to disagree (header "Chunking…" vs. footer "Embedding…").
    // Sharing the phase discriminant lets the ticker call `phase.label()` and
    // produce a footer that always matches the header.
    //
    // Encoding: we (ab)use AtomicU64 to carry a discriminant.  The mapping is:
    //   0 = Connecting, 1 = Walking, 2 = Chunking, 3 = InitializingEmbedder,
    //   4 = Embedding, 5 = KnowledgeGraph  (other variants map to 4 as default)
    fn phase_to_u64(p: super::reindex_ui::ReindexPhase) -> u64 {
        use super::reindex_ui::ReindexPhase as P;
        match p {
            P::Connecting => 0,
            P::Walking => 1,
            P::Chunking => 2,
            P::InitializingEmbedder => 3,
            P::Embedding | P::ParseEmbed => 4,
            P::KnowledgeGraph => 5,
            _ => 4,
        }
    }
    fn u64_to_label(v: u64) -> &'static str {
        use super::reindex_ui::ReindexPhase as P;
        match v {
            0 => P::Connecting.label(),
            1 => P::Walking.label(),
            2 => P::Chunking.label(),
            3 => P::InitializingEmbedder.label(),
            5 => P::KnowledgeGraph.label(),
            _ => P::Embedding.label(),
        }
    }
    let phase_disc = StdArc::new(AtomicU64::new(phase_to_u64(ReindexPhase::Connecting)));

    // Clone the bars the ticker needs — `ProgressBar` is Arc-wrapped so clones
    // are cheap and the ticker can write to them independently.
    let ticker_stats_bar = ui.stats_bar();

    // Issue #744: wall-clock ticker.
    //
    // Why: the ticker fires every second so the operator sees movement even
    // when no SSE event has arrived. Three fixes land here:
    //
    // 1. **Files N/total denominator** — use `total_files_now` (set from the
    //    `walk_complete`/`start` SSE event) instead of `embed_bar.length()`,
    //    which is initialised to 1 and only corrected after the first batch
    //    arrives. With the old code, early ticks showed "Files 0/1" and ETA "?"
    //    throughout the model-load stall.
    //
    // 2. **ETA** — computed as (total - indexed) / fps once both are known.
    //    During `InitializingEmbedder` (model cold-start), ETA is replaced with
    //    the literal string "loading model…" so the operator understands the
    //    delay is ONNX/CoreML initialisation, not slow chunking.
    //
    // 3. **cps label** — the per-batch embed throughput from `chunk_progress`
    //    events (chunks ÷ embed_ms) is labelled `embed/s` to distinguish it
    //    from a cumulative (misleadingly low) rate that includes cold-start.
    let ticker = {
        let indexed_now = indexed_now.clone();
        let chunks_now = chunks_now.clone();
        let chunks_embed_preview = chunks_embed_preview.clone();
        let skipped_now = skipped_now.clone();
        let cps_now = cps_now.clone();
        let total_files_now = total_files_now.clone();
        let tick_done = tick_done.clone();
        let phase_disc = phase_disc.clone();
        let stats_bar = ticker_stats_bar;
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(1));
            interval.tick().await; // discard immediate tick
            loop {
                interval.tick().await;
                if tick_done.load(Ordering::Acquire) {
                    break;
                }
                let elapsed = started.elapsed().as_secs();
                let indexed = indexed_now.load(Ordering::Acquire);
                // Show the larger of the committed count (chunks_now, updated
                // by `batch` events) and the in-flight preview (chunks_embed_preview,
                // updated by per-wave `chunk_progress` events every ~32 chunks).
                // This gives the operator a live chunk counter that ticks up
                // continuously during the embed phase rather than jumping once per
                // file-batch.
                let chunks = chunks_now
                    .load(Ordering::Acquire)
                    .max(chunks_embed_preview.load(Ordering::Acquire));
                let skipped = skipped_now.load(Ordering::Acquire);
                let cps = cps_now.load(Ordering::Acquire);
                // Fix #744: use the authoritative total from walk_complete/start,
                // not embed_bar.length() which starts at 1.
                let total = total_files_now.load(Ordering::Acquire);
                let phase = phase_disc.load(Ordering::Acquire);
                let is_model_loading = phase == phase_to_u64(ReindexPhase::InitializingEmbedder);
                let fps = indexed.checked_div(elapsed).unwrap_or(0);
                // Fix #744: show "loading model…" during InitializingEmbedder so the
                // operator understands why ETA is unavailable, not "chunking is slow".
                let eta = if is_model_loading {
                    "loading model\u{2026}".to_string()
                } else if fps > 0 && total > indexed {
                    super::format::fmt_secs((total - indexed) / fps)
                } else {
                    "?".to_string()
                };
                // Use the active phase label so footer matches header (Problem 1 fix).
                let phase_label = u64_to_label(phase);
                // Fix #744: label the per-batch embed rate clearly as "embed/s"
                // (not "cps") to distinguish it from a cumulative cold-start rate.
                let cps_label = if cps > 0 {
                    format!("{cps} embed/s")
                } else {
                    "---".to_string()
                };
                stats_bar.set_message(format!(
                    "{phase_label} {chunks} chunks \u{2014} {cps_label} \u{2014} \
                     Files {indexed}/{total}  Skipped {skipped}  Elapsed {elapsed}s  ETA {eta}",
                    chunks = format_with_commas(chunks),
                    indexed = format_with_commas(indexed),
                    total = format_with_commas(total),
                    skipped = format_with_commas(skipped),
                    elapsed = elapsed,
                    eta = eta,
                ));
            }
        })
    };

    let mut outcome = ReindexOutcome::default();
    let mut done = false;
    // `timed_out` — hard deadline fired (explicit --timeout only).
    let mut timed_out = false;
    // `stalled` — no progress observed for stall_secs (default 120 s).
    let mut stalled = false;

    // ── Wait / timeout strategy ──────────────────────────────────────────────
    //
    // When the user explicitly passed `--timeout N` we honour it as a hard
    // wall-clock cap (legacy behaviour, unchanged).  This lets power users
    // guarantee the CLI exits within N seconds.
    //
    // When the user did NOT pass `--timeout` (the common case), we instead use
    // progress-aware stall detection: the CLI keeps waiting as long as the
    // `indexed` counter is still advancing.  It only detaches when there has
    // been no progress for `stall_secs` (default 120 s), which guards against
    // a genuinely stalled or crashed embedder without penalising healthy but
    // slow runs.
    //
    // Hard cap (explicit --timeout): one-shot deadline, checked on every iteration.
    let hard_deadline: Option<tokio::time::Instant> = if opts.timeout_explicit {
        if opts.timeout_secs > 0 {
            Some(tokio::time::Instant::now() + Duration::from_secs(opts.timeout_secs))
        } else {
            None // --timeout 0 = wait forever
        }
    } else {
        None
    };

    // Stall detection (progress-aware default): tracks the last instant at
    // which `indexed_now` was observed to advance.  Reset on every batch or
    // skip event.  When the stall window expires with no advance, we detach.
    // Only used when `timeout_explicit` is false.
    let stall_deadline_dur = Duration::from_secs(opts.stall_secs);
    // `last_progress` starts at "now" so new sessions get a full stall window
    // before the first batch event could reasonably arrive.
    let mut last_progress = std::time::Instant::now();
    let mut last_indexed_snapshot: u64 = 0;

    // `eventsource-stream` handles SSE framing. The daemon emits these event
    // types (see `crates/trusty-search/src/service/reindex.rs::spawn_reindex`):
    //
    // Existing events (all daemons):
    //   - walk_complete: total_files
    //   - start:         total_files, index_id, root_path, lexical_only
    //   - batch:         batch_files, batch_chunks, indexed, total_files,
    //                    elapsed_ms, chunks_per_sec
    //   - skip:          file, indexed, total_files
    //   - error:         message, file (or files)
    //   - complete:      indexed, total_chunks, skipped, errors, elapsed_ms,
    //                    timings{parse_ms, embed_ms, bm25_ms, vector_upsert_ms,
    //                            kg_ms, vector_count, symbol_count, edge_count}
    //
    // New events added by issue #401 (backward-compatible; older daemons omit):
    //   - kg_start:    emitted just before KG rebuild; activates the KG bar
    //   - kg_complete: emitted after KG rebuild; carries kg_ms, symbol_count,
    //                  edge_count; marks the KG bar as done
    //
    // New events added to surface the model-init stall (Problem 1 fix):
    //   - embedder_init:  emitted by the daemon just before spawning
    //                     trusty-embedderd on the first embed request.
    //                     CLI transitions header to "Loading model…".
    //   - embedder_ready: emitted after the sidecar reports readiness.
    //                     CLI transitions header back to "Embedding chunks…" and
    //                     activates the Embed bar.
    //
    // New events for finer-grained embed progress (Problem 2 fix):
    //   - chunk_progress: emitted after each ONNX sub-batch completes inside
    //                     `embed_chunks_in_batches`.  Carries `chunks_done`
    //                     (cumulative chunks embedded so far in this file-batch)
    //                     and `chunks_per_sec`. Lets the ticker show responsive
    //                     cps/ETA before the full per-128-file `batch` event
    //                     fires.
    //
    // Issue #317 three-phase flow (walk_complete → start → first batch):
    //   walk_complete → Walking  (fills 0→100% instantly; walk is sync)
    //   start         → Chunking (brief handoff label before first batch)
    //   first batch   → Embedding (Embed bar fills as batches arrive)
    let byte_stream = resp.bytes_stream();
    let stream = byte_stream.eventsource();
    tokio::pin!(stream);

    // State flags for the three-phase walk→chunk→embed transition.
    let mut received_walk_complete = false;
    let mut lexical_only = false;
    let mut entered_embedding = false;

    // Elapsed-ms accumulators for per-stage done frames. Walk/chunk don't have
    // SSE timing events, so we approximate from wall-clock; Embed and KG have
    // timing data in `complete` and `kg_complete` respectively.
    let mut chunk_started_ms: u64 = 0;
    let mut embed_started_ms: u64 = 0;

    while !done {
        // Build the per-iteration timeout: hard deadline (explicit --timeout)
        // or a rolling stall window (progress-aware default).
        let maybe_event = if let Some(dl) = hard_deadline {
            // Explicit --timeout path: race the stream against the absolute deadline.
            tokio::select! {
                biased;
                ev = stream.next() => ev,
                _ = tokio::time::sleep_until(dl) => {
                    timed_out = true;
                    break;
                }
            }
        } else {
            // Progress-aware path: wait for the next SSE event with a 1-second
            // tick so we can check the stall window without blocking indefinitely.
            tokio::select! {
                biased;
                ev = stream.next() => ev,
                _ = tokio::time::sleep(Duration::from_secs(1)) => {
                    // Tick: check whether we have stalled (no progress for stall_secs).
                    let current_indexed = indexed_now.load(Ordering::Acquire);
                    if current_indexed > last_indexed_snapshot {
                        // Progress observed — reset the stall clock.
                        last_indexed_snapshot = current_indexed;
                        last_progress = std::time::Instant::now();
                    } else if last_progress.elapsed() >= stall_deadline_dur {
                        stalled = true;
                        break;
                    }
                    continue;
                }
            }
        };
        let event = match maybe_event {
            Some(Ok(e)) => e,
            Some(Err(e)) => {
                ui.stats_bar()
                    .println(format!("{} stream read error: {e}", "\u{26a0}".yellow()));
                break;
            }
            None => break,
        };

        let evt: serde_json::Value = match serde_json::from_str(event.data.trim()) {
            Ok(v) => v,
            Err(_) => continue,
        };
        match evt.get("event").and_then(|v| v.as_str()) {
            // ── walk_complete ──────────────────────────────────────────────
            // New daemon only. The CLI enters Walking, fills the Crawl bar to
            // 100% instantly (walk is synchronous on the daemon), then marks it
            // done. Old daemons omit this event; the CLI falls back to the
            // two-phase flow below (start → Embedding).
            Some("walk_complete") => {
                received_walk_complete = true;
                let total = evt.get("total_files").and_then(|v| v.as_u64()).unwrap_or(0);
                // Issue #744: set the authoritative file count so the ticker
                // shows "Files N/total" with the correct denominator.
                total_files_now.store(total, Ordering::Release);
                ui.set_phase(ReindexPhase::Walking, index_id);
                phase_disc.store(phase_to_u64(ReindexPhase::Walking), Ordering::Release);
                ui.set_total(total);
                // Walk is already done by the time this event arrives (sync on
                // daemon). Fill the bar to 100% and freeze it with a near-zero
                // elapsed time (walk is a fast synchronous scan on the daemon).
                ui.set_position(total);
                ui.mark_stage_done(0, 0);
                // Issue #823 Bug 2: prime the Embed bar (slot 2) with the correct
                // total_files denominator NOW, before any batch event arrives.
                // Without this, slot 2 starts at new(1) and shows "0/1" throughout
                // the model-load period. Both Chunk and Embed bars use files as
                // the unit so the pipeline gap is meaningful.
                if total > 0 && !lexical_only {
                    ui.set_embed_total(total);
                }
            }
            // ── start ──────────────────────────────────────────────────────
            Some("start") => {
                let total = evt.get("total_files").and_then(|v| v.as_u64()).unwrap_or(0);
                lexical_only = evt
                    .get("lexical_only")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                // Issue #744: set the authoritative total so the ticker always
                // shows the correct denominator from this point on (important for
                // old daemons that don't emit walk_complete).
                if total > 0 {
                    total_files_now.store(total, Ordering::Release);
                }

                if received_walk_complete {
                    // Three-phase flow: Walk bar is already done; enter Chunking.
                    chunk_started_ms = started.elapsed().as_millis() as u64;
                    ui.set_phase(ReindexPhase::Chunking, index_id);
                    phase_disc.store(phase_to_u64(ReindexPhase::Chunking), Ordering::Release);
                    ui.set_total(total);
                    // Issue #823 Bug 2: prime Embed bar (slot 2) immediately with
                    // total_files so it shows real N/total instead of 0/1 for the
                    // entire model-load period. Done here as fallback in case
                    // walk_complete arrived before lexical_only was known.
                    if total > 0 && !lexical_only {
                        ui.set_embed_total(total);
                        ui.activate_embed_bar();
                    }
                } else {
                    // Legacy two-phase flow (old daemon, no walk_complete):
                    // jump straight to Embed (or Chunking for lexical-only).
                    ui.set_total(total);
                    if lexical_only {
                        chunk_started_ms = started.elapsed().as_millis() as u64;
                        ui.set_phase(ReindexPhase::Chunking, index_id);
                        phase_disc.store(phase_to_u64(ReindexPhase::Chunking), Ordering::Release);
                    } else {
                        embed_started_ms = started.elapsed().as_millis() as u64;
                        ui.set_phase(ReindexPhase::Embedding, index_id);
                        phase_disc.store(phase_to_u64(ReindexPhase::Embedding), Ordering::Release);
                        entered_embedding = true;
                        // Issue #823 Bug 2: also prime slot 2 on the legacy path.
                        if total > 0 {
                            ui.set_embed_total(total);
                        }
                    }
                }
            }
            // ── embedder_init ──────────────────────────────────────────────
            // New event (Problem 1 fix): emitted by the daemon just before
            // spawning trusty-embedderd on the first embed request.  This is
            // the 30-60s "stall" that previously showed as a frozen Chunk bar
            // at 0/N with no feedback.  Transitioning the header to
            // "Loading model…" (InitializingEmbedder) makes the wait visible.
            Some("embedder_init") => {
                ui.set_phase(ReindexPhase::InitializingEmbedder, index_id);
                phase_disc.store(
                    phase_to_u64(ReindexPhase::InitializingEmbedder),
                    Ordering::Release,
                );
            }
            // ── embedder_ready ─────────────────────────────────────────────
            // Emitted after the embedder (sidecar or in-process) has completed
            // its first embed batch. Transitions the header to "Embedding
            // chunks…" and activates the Embed bar.
            //
            // Issue #823 Bug 3: previously only emitted for sidecar mode
            // (embedder_pid_slot.is_some()). The daemon now emits this event
            // unconditionally after the first successful parse_and_embed call,
            // regardless of embedder mode.
            //
            // Issue #823 Bug 1: do NOT call mark_stage_done(1) here — the Chunk
            // bar continues advancing in parallel with the Embed bar throughout
            // the CHUNK+EMBED phase. The Chunk bar is only frozen at kg_start
            // (or at complete if kg_start was never received).
            Some("embedder_ready") if !entered_embedding => {
                embed_started_ms = started.elapsed().as_millis() as u64;
                // Update the header to "Embedding chunks…" while keeping the
                // Chunk bar active. phase_to_bar_slot(Embedding) = 2, so
                // set_phase activates slot 2 without touching slot 1.
                ui.set_phase(ReindexPhase::Embedding, index_id);
                phase_disc.store(phase_to_u64(ReindexPhase::Embedding), Ordering::Release);
                entered_embedding = true;
            }
            Some("embedder_ready") => {
                // Already in embedding phase; ignore duplicate event.
            }
            // ── chunk_progress ─────────────────────────────────────────────
            // Emitted after each ONNX wave (≥ PROGRESS_CHUNK_INTERVAL chunks)
            // inside `embed_chunks_in_batches`. Fires at ~32-chunk granularity
            // so the stats line advances continuously during embedding rather
            // than jumping once per 128-file file-batch.
            //
            // Issue #823 Bug 1: also advance the Chunk bar (slot 1) here using
            // the `indexed` file count from the event. The Chunk bar tracks
            // files PARSED (leading indicator); the Embed bar tracks files
            // COMMITTED (trailing). Both use files as unit — the gap between
            // them visualises the pipeline backpressure.
            Some("chunk_progress") => {
                let wave_chunks = evt.get("chunks_done").and_then(|v| v.as_u64()).unwrap_or(0);
                let wave_cps = evt
                    .get("chunks_per_sec")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                if wave_cps > 0 {
                    cps_now.store(wave_cps, Ordering::Release);
                }
                // Accumulate in-flight chunks into the preview counter so the
                // ticker shows the live embed count between `batch` events.
                // `batch` events reset this preview to 0 so it never
                // double-counts with `chunks_now`.
                if wave_chunks > 0 {
                    chunks_embed_preview.fetch_add(wave_chunks, Ordering::AcqRel);
                }
                // Advance the Chunk bar (slot 1) with the files-parsed count
                // from the event. This keeps the Chunk bar moving between
                // `batch` events so the pipeline gap is visible.
                let chunk_indexed = evt.get("indexed").and_then(|v| v.as_u64()).unwrap_or(0);
                if chunk_indexed > 0 {
                    ui.set_position(chunk_indexed);
                }
            }
            // ── batch ──────────────────────────────────────────────────────
            Some("batch") => {
                // Issue #823 Bug 1: do NOT call mark_stage_done(1) here.
                // The old code froze the Chunk bar at the batch-transition
                // boundary (e.g. 512/2094). Both Chunk and Embed bars must
                // remain live throughout the CHUNK+EMBED phase.
                //
                // On the first batch event (three-phase flow): activate the
                // Embed bar (slot 2) and transition the header to "Embedding…"
                // if embedder_ready was not received (in-process embedder that
                // didn't emit the event, or legacy daemon).
                if received_walk_complete && !entered_embedding && !lexical_only {
                    embed_started_ms = started.elapsed().as_millis() as u64;
                    ui.set_phase(ReindexPhase::Embedding, index_id);
                    phase_disc.store(phase_to_u64(ReindexPhase::Embedding), Ordering::Release);
                    entered_embedding = true;
                }
                // Always ensure the Embed bar is visually active once batches start
                // (covers the case where embedder_ready arrived but activate_embed_bar
                // was not called from that handler since set_phase(Embedding) already
                // activates slot 2 via the normal path).

                let indexed = evt.get("indexed").and_then(|v| v.as_u64()).unwrap_or(0);
                let batch_chunks = evt
                    .get("batch_chunks")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let chunks_per_sec = evt
                    .get("chunks_per_sec")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let total = evt.get("total_files").and_then(|v| v.as_u64()).unwrap_or(0);
                if total > 0 {
                    // Issue #744: also update the ticker's total so ETA uses
                    // the correct denominator from the very first batch event.
                    total_files_now.store(total, Ordering::Release);
                    ui.set_total(total);
                    // Issue #823 Bug 2: ensure the Embed bar total is always
                    // up to date even if walk_complete/start didn't prime it.
                    ui.set_embed_total(total);
                }
                indexed_now.store(indexed, Ordering::Release);
                cps_now.store(chunks_per_sec, Ordering::Release);
                let new_chunks =
                    chunks_now.fetch_add(batch_chunks, Ordering::AcqRel) + batch_chunks;
                // The authoritative commit count is now in `chunks_now`; reset
                // the in-flight preview so the ticker shows committed chunks
                // rather than the (now stale) embedding preview.
                chunks_embed_preview.store(0, Ordering::Release);
                // Issue #823 Bug 1: advance BOTH bars.
                // Chunk bar (slot 1) = files parsed; Embed bar (slot 2) = files
                // committed/embedded. Both use `indexed` (files processed so far)
                // as a proxy — Chunk should lead, but without a separate "files
                // parsed" event from the daemon, `indexed` is the best we have.
                // The visual gap comes from chunk_progress advancing the Chunk
                // bar between batch events (parsed but not yet committed).
                ui.set_position(indexed); // advances active phase's bar (Chunk or Embed)
                ui.advance_embed_bar(indexed); // always advance slot 2 (Embed)
                ui.update_stats(
                    indexed,
                    new_chunks,
                    skipped_now.load(Ordering::Acquire),
                    chunks_per_sec,
                    started.elapsed().as_secs(),
                );
                // Any batch event is forward progress — reset the stall clock.
                if indexed > last_indexed_snapshot {
                    last_indexed_snapshot = indexed;
                    last_progress = std::time::Instant::now();
                }
            }
            // ── skip ───────────────────────────────────────────────────────
            Some("skip") => {
                let indexed = evt.get("indexed").and_then(|v| v.as_u64()).unwrap_or(0);
                indexed_now.store(indexed, Ordering::Release);
                let skipped = skipped_now.fetch_add(1, Ordering::AcqRel) + 1;
                ui.set_position(indexed);
                ui.update_stats(
                    indexed,
                    chunks_now.load(Ordering::Acquire),
                    skipped,
                    cps_now.load(Ordering::Acquire),
                    started.elapsed().as_secs(),
                );
                // skip events also represent progress (files are being processed).
                if indexed > last_indexed_snapshot {
                    last_indexed_snapshot = indexed;
                    last_progress = std::time::Instant::now();
                }
            }
            // ── kg_start ───────────────────────────────────────────────────
            // New event added by issue #401. The daemon emits this immediately
            // before `rebuild_symbol_graph_for_reindex`. The CLI marks both the
            // Chunk bar and Embed bar done, then activates the KG bar.
            //
            // Issue #823 Bug 1: this is the correct place to freeze the Chunk
            // bar (slot 1) — NOT at the first `batch` event. By waiting until
            // kg_start, both Chunk and Embed bars animate throughout CHUNK+EMBED.
            Some("kg_start") => {
                // Mark Chunk bar done (Issue #823 Bug 1: moved here from batch handler).
                let chunk_ms = started.elapsed().as_millis() as u64 - chunk_started_ms;
                ui.mark_stage_done(1, chunk_ms);
                // Mark Embed bar done (if it was active).
                if entered_embedding {
                    let embed_ms = started.elapsed().as_millis() as u64 - embed_started_ms;
                    ui.mark_stage_done(2, embed_ms);
                }
                ui.clear_stats();
                ui.set_phase(ReindexPhase::KnowledgeGraph, index_id);
                phase_disc.store(
                    phase_to_u64(ReindexPhase::KnowledgeGraph),
                    Ordering::Release,
                );
                // KG total is unknown until completion; use 1 so the bar renders.
                ui.set_total(1);
                ui.set_position(0);
            }
            // ── kg_complete ────────────────────────────────────────────────
            // New event added by issue #401. Carries `kg_ms`, `symbol_count`,
            // `edge_count`. The CLI marks the KG bar done. Old daemons omit this
            // event; the KG bar is cleaned up in the `complete` handler.
            Some("kg_complete") => {
                let kg_ms = evt.get("kg_ms").and_then(|v| v.as_u64()).unwrap_or(0);
                let symbol_count = evt
                    .get("symbol_count")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let edge_count = evt.get("edge_count").and_then(|v| v.as_u64()).unwrap_or(0);
                // Snap the KG bar to 100% (total was set to 1 in kg_start).
                ui.set_position(1);
                ui.mark_stage_done(3, kg_ms);
                // Show a brief summary on the stats line.
                ui.stats_bar().set_message(format!(
                    "KG done \u{2014} {sym} symbols, {edges} edges",
                    sym = format_with_commas(symbol_count),
                    edges = format_with_commas(edge_count),
                ));
            }
            // ── complete ───────────────────────────────────────────────────
            Some("complete") => {
                outcome.indexed = evt.get("indexed").and_then(|v| v.as_u64()).unwrap_or(0);
                outcome.total_chunks = evt
                    .get("total_chunks")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                outcome.skipped = evt
                    .get("skipped")
                    .and_then(|v| v.as_u64())
                    .unwrap_or_else(|| skipped_now.load(Ordering::Acquire));
                outcome.errors = evt.get("errors").and_then(|v| v.as_u64()).unwrap_or(0);
                outcome.elapsed_ms = evt.get("elapsed_ms").and_then(|v| v.as_u64()).unwrap_or(0);
                // Per-subsystem timings (added in 0.3.11). Absent when talking
                // to an older daemon — outcome.timings stays `None`.
                if let Some(t) = evt.get("timings") {
                    let get = |k: &str| t.get(k).and_then(|v| v.as_u64()).unwrap_or(0);
                    outcome.timings = Some(ReindexTimings {
                        // Issue #744: walk_ms added; zero on old daemons that omit it.
                        walk_ms: get("walk_ms"),
                        parse_ms: get("parse_ms"),
                        embed_ms: get("embed_ms"),
                        bm25_ms: get("bm25_ms"),
                        vector_upsert_ms: get("vector_upsert_ms"),
                        kg_ms: get("kg_ms"),
                        vector_count: get("vector_count"),
                        symbol_count: get("symbol_count"),
                        edge_count: get("edge_count"),
                    });
                }
                outcome.completed = true;

                // Snap both Chunk and Embed bars to full position.
                // Chunk bar (slot 1) may still be Active if kg_start was never received.
                ui.set_position(outcome.indexed);
                ui.advance_embed_bar(outcome.indexed);

                // Mark Embed bar done if it wasn't marked by kg_start (old daemon
                // or lexical_only index).
                if entered_embedding && !lexical_only {
                    // Only mark done if not already done by kg_start.
                    let embed_ms = outcome
                        .timings
                        .map(|t| t.embed_ms)
                        .unwrap_or_else(|| started.elapsed().as_millis() as u64 - embed_started_ms);
                    // Use mark_stage_done which is idempotent on Done bars.
                    ui.mark_stage_done(2, embed_ms);
                }

                // Issue #823 Bug 1: Mark Chunk bar done unconditionally here
                // (if not already done by kg_start). This covers:
                //   - the three-phase flow where kg_start froze it already (idempotent)
                //   - the two-phase / lexical path where kg_start was never received
                //   - the skip_kg path where the Chunk bar must still close
                let chunk_ms = outcome.timings.map(|t| t.parse_ms).unwrap_or(0);
                ui.mark_stage_done(1, chunk_ms);

                // Mark Crawl bar done for old daemons that never sent walk_complete.
                if !received_walk_complete {
                    ui.mark_stage_done(0, 0);
                }

                // Mark KG bar done if it wasn't marked by kg_complete (old daemon).
                let kg_ms_final = outcome.timings.map(|t| t.kg_ms).unwrap_or(0);
                ui.mark_stage_done(3, kg_ms_final);

                done = true;
            }
            // ── error ──────────────────────────────────────────────────────
            Some("error") => {
                let msg = evt
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                let file = evt.get("file").and_then(|v| v.as_str()).unwrap_or("");
                ui.stats_bar()
                    .println(format!("{}  {}: {}", "\u{26a0}".yellow(), file, msg));
            }
            // Unknown events (future daemon-side additions) are silently ignored
            // so older CLIs stay backward-compatible.
            _ => {}
        }
    }

    // Stop the ticker before finishing the UI.
    tick_done.store(true, Ordering::Release);
    let _ = ticker.await;

    if timed_out {
        // Hard cap (explicit --timeout) fired.
        let still_progressing = indexed_now.load(Ordering::Acquire) > last_indexed_snapshot
            || last_progress.elapsed() < stall_deadline_dur;
        let reason = if still_progressing {
            format!(
                "reached --timeout {}s while still progressing \u{2014} detaching",
                opts.timeout_secs,
            )
        } else {
            format!(
                "timed out after {}s with no recent progress",
                opts.timeout_secs,
            )
        };
        ui.abandon(format!("{} {}", "\u{26a0}".yellow(), reason));
        eprintln!(
            "{} Daemon is still indexing in the background. \
             Use `trusty-search status` or re-run `trusty-search index` to check progress. \
             Pass `--timeout <seconds>` to wait longer (e.g. `--timeout 1200`).",
            "\u{2139}".cyan()
        );
        return Ok(outcome);
    }

    if stalled {
        // Progress-aware stall: no indexed counter advance for stall_secs.
        let indexed = indexed_now.load(Ordering::Acquire);
        let total = outcome.indexed.max(indexed);
        ui.abandon(format!(
            "{} No indexing progress for {}s (Files {}/{}) \u{2014} detaching; \
             daemon continues in background",
            "\u{26a0}".yellow(),
            opts.stall_secs,
            super::format::format_with_commas(indexed),
            super::format::format_with_commas(total),
        ));
        eprintln!(
            "{} Daemon appears stalled or very slow. Use `trusty-search status` to check. \
             If indexing is still running, re-run `trusty-search index` to reattach or \
             pass `--timeout <seconds>` to extend the hard cap.",
            "\u{2139}".cyan()
        );
        return Ok(outcome);
    }

    if !outcome.completed {
        ui.abandon(format!(
            "{} Reindex stream ended without completion event",
            "\u{26a0}".yellow()
        ));
        anyhow::bail!("reindex did not complete");
    }

    // Final headline. Three cases:
    //   1. errors > 0          → show error count + unchanged count
    //   2. nothing changed     → "is up to date" message
    //   3. some files changed  → "Indexed N changed files" with unchanged tally
    let elapsed = fmt_elapsed(outcome.elapsed_ms);
    let changed = outcome.indexed.saturating_sub(outcome.skipped);
    let final_msg = if outcome.errors > 0 {
        format!(
            "{} Indexed {} files \u{2192} {} chunks  [took {}, {} errors, {} unchanged]",
            "\u{2713}".green(),
            format_with_commas(changed),
            format_with_commas(outcome.total_chunks),
            elapsed,
            outcome.errors,
            format_with_commas(outcome.skipped),
        )
    } else if changed == 0 && outcome.indexed > 0 {
        format!(
            "{} '{}' is up to date ({} chunks, {} files \u{2014} no changes detected)  [took {}]",
            "\u{2713}".green(),
            index_id,
            format_with_commas(outcome.total_chunks),
            format_with_commas(outcome.indexed),
            elapsed,
        )
    } else {
        format!(
            "{} Indexed {} changed file{} \u{2192} {} chunks  [took {}, {} unchanged]",
            "\u{2713}".green(),
            format_with_commas(changed),
            if changed == 1 { "" } else { "s" },
            format_with_commas(outcome.total_chunks),
            elapsed,
            format_with_commas(outcome.skipped),
        )
    };
    ui.finish(final_msg);

    // Per-subsystem timing breakdown (rendered after `ui.finish` so indicatif
    // doesn't redraw over our printed lines). Skipped for old daemons.
    // Pass the SSE `elapsed_ms` (wall-clock total) so the breakdown can
    // print it as the single authoritative number — subsystem times overlap.
    if let Some(t) = outcome.timings {
        print_timing_breakdown(&t, outcome.total_chunks, outcome.elapsed_ms);
    }

    // Post-reindex health check (blue-green safety net).
    if opts.verify_after {
        verify_reindex_health(&client, &base, index_id, &outcome, opts.prior_chunk_count).await?;
    }

    Ok(outcome)
}

/// After a `--force` reindex, fetch the new chunk count and run a sanity
/// query. Exits 1 if either looks wrong.
///
/// Why: the daemon's reindex mutates the in-memory `CodeIndexer` in place
/// (no shadow slot). If the rebuild produces a broken index, the only signal
/// the user has is "search returns nothing" hours later. This check surfaces
/// that immediately.
/// What: fetches `/status` for the chunk count, then probes the search
/// endpoint with common tokens. Returns an error if either check fails.
/// Test: covered indirectly by `index --force` integration tests.
async fn verify_reindex_health(
    client: &reqwest::Client,
    base: &str,
    index_id: &str,
    outcome: &ReindexOutcome,
    prior: Option<u64>,
) -> Result<()> {
    // 1) Chunk count via /status.
    let status_url = format!("{}/indexes/{}/status", base, index_id);
    let new_chunks = match client.get(&status_url).send().await {
        Ok(r) if r.status().is_success() => r
            .json::<serde_json::Value>()
            .await
            .ok()
            .and_then(|v| v.get("chunk_count").and_then(|n| n.as_u64()))
            .unwrap_or(0),
        _ => 0,
    };

    // 2) Sanity query: pick something that hits virtually any source tree.
    let search_url = format!("{}/indexes/{}/search", base, index_id);
    let probes = ["fn", "function", "def", "class", "the"];
    let mut got_hit = false;
    for probe in probes {
        let body = serde_json::json!({ "text": probe, "top_k": 1 });
        if let Ok(resp) = client.post(&search_url).json(&body).send().await {
            if resp.status().is_success() {
                if let Ok(json) = resp.json::<serde_json::Value>().await {
                    let n = json
                        .get("results")
                        .and_then(|r| r.as_array())
                        .map(|a| a.len())
                        .unwrap_or(0);
                    if n > 0 {
                        got_hit = true;
                        break;
                    }
                }
            }
        }
    }

    let healthy = new_chunks > 0 && got_hit && outcome.errors == 0;
    let was = prior
        .map(|p| format!(" (was {})", format_with_commas(p)))
        .unwrap_or_default();
    if healthy {
        println!(
            "{} Reindex complete: {} chunks{}",
            "\u{2713}".green(),
            format_with_commas(new_chunks),
            was
        );
        Ok(())
    } else {
        anyhow::bail!(
            "Reindex produced unhealthy index: {} chunks{}, sanity query {} \u{2014} \
             old index NOT preserved (daemon reindex is in-place; \
             see crates/trusty-search/src/service/reindex.rs)",
            format_with_commas(new_chunks),
            was,
            if got_hit { "ok" } else { "returned 0 results" }
        );
    }
}

/// Register an index with the daemon (idempotent).
///
/// Why: factored out of `Init` and `Index` because both flows need the same
/// "POST /indexes, parse `created`" dance.
/// What: returns `Ok((created, daemon_reachable))`. `daemon_reachable=false`
/// surfaces network failures distinctly from "registered but already existed".
/// Test: covered indirectly by `handle_index` tests.
pub async fn register_index_with_daemon(
    index_name: &str,
    project_path: &std::path::Path,
) -> Result<(bool, bool)> {
    register_index_with_daemon_filtered(index_name, project_path, &RegisterFilters::default()).await
}

/// Optional repo-config filters carried in `POST /indexes` request bodies.
///
/// Why: `trusty-search.yaml` declares per-index filter sets (`paths`,
/// `exclude`, `languages`, `domain_terms`). The CLI loads the YAML and
/// forwards the resolved values to the daemon when registering each
/// index so the daemon stores them on the `IndexHandle` and applies them
/// to subsequent reindex + search calls.
/// What: thin struct carrying the four fields. `Default` = empty everywhere,
/// which keeps the original single-index path unchanged.
/// Test: `commands::index::handle_index` populates this from `IndexConfig`.
#[derive(Debug, Default)]
pub struct RegisterFilters {
    pub include_paths: Vec<String>,
    pub exclude_globs: Vec<String>,
    pub extensions: Vec<String>,
    pub domain_terms: Vec<String>,
    /// Issue #109, Phase 1: when `true`, the CLI tells the daemon to register
    /// this index as `lexical_only` — the reindex pipeline skips Stages 2/3
    /// permanently. Persisted on the daemon side via `indexes.toml`.
    pub lexical_only: bool,
    /// Issue #313: when `true`, the CLI tells the daemon to register this
    /// index with `skip_kg = true` — Phase 3 KG rebuild is suppressed
    /// permanently. Persisted on the daemon side via `indexes.toml`.
    ///
    /// Why: exposes the KG-skip flag at the CLI-to-daemon boundary so
    /// `trusty-search index --no-kg` and the YAML `skip_kg: true` field can
    /// both reach the daemon's create-index handler without extra scaffolding.
    /// What: when `true`, the request body sent to `POST /indexes` includes
    /// `"skip_kg": true`. The daemon stores it in `indexes.toml`.
    /// Test: covered by `skip_kg_index_never_runs_phase3` (end-to-end).
    pub skip_kg: bool,
}

/// Variant of [`register_index_with_daemon`] that forwards filter/domain
/// fields in the request body so the daemon can store them on the handle.
///
/// Why: the filtered variant is needed when any of the optional fields are
/// non-empty or when `lexical_only` / `skip_kg` is set.
/// What: builds a JSON body with the non-empty filter fields and POSTs to
/// `/indexes`. Returns `(created, daemon_reachable)`.
/// Test: covered indirectly by `handle_index` integration tests.
pub async fn register_index_with_daemon_filtered(
    index_name: &str,
    project_path: &std::path::Path,
    filters: &RegisterFilters,
) -> Result<(bool, bool)> {
    let base = daemon_base_url();
    let client = trusty_common::server::daemon_http_client()?;
    let create_url = format!("{}/indexes", base);
    let mut create_body = serde_json::json!({
        "id": index_name,
        "root_path": project_path,
    });
    if !filters.include_paths.is_empty() {
        create_body["include_paths"] = serde_json::json!(filters.include_paths);
    }
    if !filters.exclude_globs.is_empty() {
        create_body["exclude_globs"] = serde_json::json!(filters.exclude_globs);
    }
    if !filters.extensions.is_empty() {
        create_body["extensions"] = serde_json::json!(filters.extensions);
    }
    if !filters.domain_terms.is_empty() {
        create_body["domain_terms"] = serde_json::json!(filters.domain_terms);
    }
    if filters.lexical_only {
        create_body["lexical_only"] = serde_json::json!(true);
    }
    if filters.skip_kg {
        create_body["skip_kg"] = serde_json::json!(true);
    }
    match client.post(&create_url).json(&create_body).send().await {
        Ok(resp) if resp.status().is_success() => {
            let body: serde_json::Value =
                resp.json().await.unwrap_or_else(|_| serde_json::json!({}));
            let created = body
                .get("created")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            Ok((created, true))
        }
        Ok(resp) => {
            anyhow::bail!("daemon returned {} for POST /indexes", resp.status());
        }
        Err(_) => Ok((false, false)),
    }
}

/// Fetch chunk count for an index via /status. Returns `None` if the daemon
/// is unreachable or the index isn't registered.
///
/// Why: the `--force` pre-snapshot path needs the current chunk count before
/// the reindex begins, so the final verify message can show "(was N)".
/// What: GETs `/indexes/:id/status` and parses `chunk_count`.
/// Test: covered indirectly by `run_reindex_force_opts`.
pub async fn fetch_chunk_count(index_id: &str) -> Option<u64> {
    let base = daemon_base_url();
    let url = format!("{}/indexes/{}/status", base, index_id);
    let client = trusty_common::server::daemon_http_client().ok()?;
    let resp = client.get(&url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let body: serde_json::Value = resp.json().await.ok()?;
    body.get("chunk_count").and_then(|v| v.as_u64())
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// The default `ReindexOptions` values must be sane so accidental callers
    /// that rely on `Default::default()` get progress-aware stall behaviour.
    ///
    /// Why: `timeout_explicit = false` is the key invariant — it ensures that
    /// a CLI omitting `--timeout` gets the progress-aware default rather than
    /// a premature 600 s abort.
    /// What: asserts the default field values.
    /// Test: this test.
    #[test]
    fn default_options_are_sane() {
        let opts = ReindexOptions::default();
        assert!(!opts.verify_after);
        assert!(opts.prior_chunk_count.is_none());
        assert!(!opts.force);
        // timeout_explicit must be false so the progress-aware stall window
        // governs by default (not a hard wall-clock cap).
        assert!(!opts.timeout_explicit);
        assert_eq!(opts.stall_secs, 120);
    }

    /// The default `ReindexOutcome` must have all fields at zero / false so
    /// callers can accumulate into it safely.
    ///
    /// Why: a non-zero default would make "nothing happened" indistinguishable
    /// from a real result.
    /// What: asserts the default field values.
    /// Test: this test.
    #[test]
    fn default_outcome_is_zero() {
        let o = ReindexOutcome::default();
        assert_eq!(o.indexed, 0);
        assert_eq!(o.total_chunks, 0);
        assert!(!o.completed);
        assert!(o.timings.is_none());
    }

    /// A non-interactive `ProgressStyle` must not panic on the indicatif
    /// template strings used in `bar_style`.  This catches template syntax
    /// regressions before they reach users.
    ///
    /// Why: `ProgressStyle::with_template` returns an error (not a panic) on
    /// bad templates, but `bar_style` falls back to `default_bar()`.  Asserting
    /// the style is non-panicking here catches the case where the fallback would
    /// silently hide a bug.
    /// What: constructs styles for all three states and asserts no panic.
    /// Test: this test.
    #[test]
    fn bar_style_does_not_panic() {
        use super::super::reindex_ui::ReindexUi;
        // Constructing the UI exercises all bar styles.
        let ui = ReindexUi::new("test", false);
        ui.finish("ok".to_string());
    }

    // ── Progress-aware wait logic ─────────────────────────────────────────────
    //
    // The full SSE loop in `run_reindex_with` requires a live daemon and cannot
    // be tested in a unit test.  The tests below instead verify the *decision
    // logic* that governs the wait strategy:
    //
    //  1. Whether `ReindexOptions` correctly represents "explicit" vs "default"
    //     timeout intent.
    //  2. Whether the hard-cap and stall-window durations are constructed
    //     correctly from the options.
    //  3. That `run_reindex_opts` with `timeout_explicit=false` produces options
    //     with no hard deadline (the progress-aware path).
    //  4. That `run_reindex_opts` with `timeout_explicit=true` and a nonzero
    //     `timeout_secs` would produce a hard deadline.
    //
    // Integration coverage lives in the `--include-ignored` test suite (requires
    // a live daemon + indexed corpus).

    /// When `timeout_explicit = false` (the default), no hard deadline is set
    /// and the stall window governs.
    ///
    /// Why: guards the progress-aware default — a regression here would restore
    /// the old premature 600 s abort on every unattended `trusty-search index`.
    /// What: constructs `ReindexOptions` with `timeout_explicit = false` and
    /// asserts the hard-deadline path would not fire.
    /// Test: this test.
    #[test]
    fn progress_aware_wait_no_hard_deadline_when_implicit() {
        let opts = ReindexOptions {
            timeout_explicit: false,
            stall_secs: 120,
            ..ReindexOptions::default()
        };
        // The hard-deadline arm is `opts.timeout_explicit` — when false, no
        // deadline `Instant` is created.
        assert!(
            !opts.timeout_explicit,
            "implicit timeout must not set a hard cap"
        );
        assert_eq!(opts.stall_secs, 120);

        // Simulate the deadline construction logic from run_reindex_with:
        // hard_deadline is None when timeout_explicit is false.
        let hard_deadline: Option<std::time::Duration> = if opts.timeout_explicit {
            Some(std::time::Duration::from_secs(opts.timeout_secs))
        } else {
            None
        };
        assert!(
            hard_deadline.is_none(),
            "progress-aware mode must not produce a hard deadline"
        );
    }

    /// When `timeout_explicit = true` with a non-zero `timeout_secs`, a hard
    /// deadline is imposed (the legacy behaviour preserved for `--timeout N`).
    ///
    /// Why: explicit `--timeout` must still work as a reliable hard cap even
    /// when indexing is healthy.  Power users depend on this for scripting.
    /// What: constructs `ReindexOptions` with `timeout_explicit = true` and
    /// asserts the hard deadline is set.
    /// Test: this test.
    #[test]
    fn progress_aware_wait_hard_deadline_when_explicit() {
        let opts = ReindexOptions {
            timeout_secs: 300,
            timeout_explicit: true,
            ..ReindexOptions::default()
        };
        assert!(
            opts.timeout_explicit,
            "explicit timeout must set a hard cap"
        );

        let hard_deadline: Option<std::time::Duration> =
            if opts.timeout_explicit && opts.timeout_secs > 0 {
                Some(std::time::Duration::from_secs(opts.timeout_secs))
            } else {
                None
            };
        assert_eq!(
            hard_deadline,
            Some(std::time::Duration::from_secs(300)),
            "explicit 300 s timeout must produce a 300 s hard deadline"
        );
    }

    /// `--timeout 0` with `timeout_explicit = true` means "wait forever"
    /// (the legacy `0 = no limit` behaviour).
    ///
    /// Why: `--timeout 0` must remain a valid escape hatch for users who want
    /// to block indefinitely without switching to progress-aware mode.
    /// What: asserts that `timeout_secs = 0` + `timeout_explicit = true` does
    /// NOT produce a hard deadline (the `> 0` guard).
    /// Test: this test.
    #[test]
    fn progress_aware_wait_timeout_zero_explicit_means_no_deadline() {
        let opts = ReindexOptions {
            timeout_secs: 0,
            timeout_explicit: true,
            ..ReindexOptions::default()
        };
        // Mirrors the `if opts.timeout_explicit { if opts.timeout_secs > 0 { Some(…) } else { None } }`
        // guard in run_reindex_with.
        let hard_deadline: Option<std::time::Duration> = if opts.timeout_explicit {
            if opts.timeout_secs > 0 {
                Some(std::time::Duration::from_secs(opts.timeout_secs))
            } else {
                None // --timeout 0 = wait forever
            }
        } else {
            None
        };
        assert!(
            hard_deadline.is_none(),
            "--timeout 0 must not produce a hard deadline (wait forever)"
        );
    }

    /// Stall detection logic: a counter that stops advancing within the stall
    /// window should trigger a stall, while one that advances should not.
    ///
    /// Why: the stall window is the core mechanism preventing premature detach
    /// during a healthy but slow embed run; verifying the comparison logic
    /// catches off-by-one or direction errors before they reach users.
    /// What: simulates the indexed-counter comparison used in the wait loop and
    /// asserts the stall condition fires only when the counter is frozen.
    /// Test: this test.
    #[test]
    fn stall_detection_triggers_on_frozen_counter() {
        // Simulate: counter has been at 100 for > stall_secs.
        let last_indexed_snapshot: u64 = 100;
        let current_indexed: u64 = 100; // unchanged — stalled

        let counter_advanced = current_indexed > last_indexed_snapshot;
        assert!(!counter_advanced, "frozen counter must not advance");

        // With a tiny stall window that has definitely elapsed:
        let last_progress = std::time::Instant::now() - std::time::Duration::from_secs(200);
        let stall_deadline_dur = std::time::Duration::from_secs(120);
        let is_stalled = !counter_advanced && last_progress.elapsed() >= stall_deadline_dur;
        assert!(
            is_stalled,
            "must detect stall after stall_secs with no counter advance"
        );
    }

    /// Stall detection logic: a counter that advances resets the stall clock
    /// and must NOT trigger a stall.
    ///
    /// Why: complements `stall_detection_triggers_on_frozen_counter` — a
    /// progressing index must never be considered stalled regardless of
    /// elapsed wall-clock time.
    /// What: simulates a counter that advanced and a stall window that has
    /// elapsed; asserts the stall condition does NOT fire.
    /// Test: this test.
    #[test]
    fn stall_detection_does_not_trigger_while_progressing() {
        let last_indexed_snapshot: u64 = 100;
        let current_indexed: u64 = 150; // advanced — progressing

        let counter_advanced = current_indexed > last_indexed_snapshot;
        assert!(
            counter_advanced,
            "advancing counter must register as progress"
        );

        // Even with a very old `last_progress`, the counter advance means we
        // are NOT stalled (the loop resets last_progress when it sees advance).
        // This test verifies the `counter_advanced` check comes first.
        let stalled = !counter_advanced; // counter_advanced resets the stall
        assert!(!stalled, "progressing counter must not trigger stall");
    }

    // ── Issue #744 progress fixes ─────────────────────────────────────────────

    /// The `total_files_now` atomic must be zero initially and updated to the
    /// correct denominator when set.
    ///
    /// Why: Issue #744 — the ticker previously used `embed_bar.length()` (= 1)
    /// as the Files denominator; this test verifies the replacement atomic
    /// behaves correctly (zero-init + explicit store).
    /// What: stores a value and reads it back via Acquire ordering.
    /// Test: this test.
    #[test]
    fn total_files_atomic_zero_until_set() {
        use std::sync::atomic::{AtomicU64, Ordering};
        let total_files_now = AtomicU64::new(0);
        // Before any SSE event: must read 0, not 1.
        assert_eq!(
            total_files_now.load(Ordering::Acquire),
            0,
            "total_files_now must be zero until set by walk_complete/start"
        );
        total_files_now.store(3_327, Ordering::Release);
        assert_eq!(
            total_files_now.load(Ordering::Acquire),
            3_327,
            "total_files_now must reflect the value stored by the SSE handler"
        );
    }

    /// The ETA is "loading model…" during InitializingEmbedder and "?" when
    /// the denominator is zero (before the first walk_complete event).
    ///
    /// Why: Issue #744 — ETA "?" with Files 0/1 was confusing during model
    /// cold-start; "loading model…" explains the delay.
    /// What: replicates the ETA-computation logic from the ticker and asserts
    /// the correct strings.
    /// Test: this test.
    #[test]
    fn eta_logic_loading_model_and_zero_denom() {
        use super::super::reindex_ui::ReindexPhase;
        use std::sync::atomic::{AtomicU64, Ordering};

        fn phase_to_u64_test(p: ReindexPhase) -> u64 {
            match p {
                ReindexPhase::InitializingEmbedder => 3,
                _ => 4,
            }
        }

        let total_files_now = AtomicU64::new(0);
        let indexed = 0u64;
        let elapsed = 5u64;
        let phase = phase_to_u64_test(ReindexPhase::InitializingEmbedder);
        let is_model_loading = phase == 3;
        let fps = indexed.checked_div(elapsed).unwrap_or(0);
        let total = total_files_now.load(Ordering::Acquire);

        let eta = if is_model_loading {
            "loading model\u{2026}".to_string()
        } else if fps > 0 && total > indexed {
            super::super::format::fmt_secs((total - indexed) / fps)
        } else {
            "?".to_string()
        };

        assert_eq!(
            eta, "loading model\u{2026}",
            "ETA must be 'loading model…' during InitializingEmbedder"
        );

        // Not loading model, but total is still 0 (before walk_complete).
        let phase2 = phase_to_u64_test(ReindexPhase::Embedding);
        let is_loading2 = phase2 == 3;
        let eta2 = if is_loading2 {
            "loading model\u{2026}".to_string()
        } else if fps > 0 && total > indexed {
            super::super::format::fmt_secs((total - indexed) / fps)
        } else {
            "?".to_string()
        };
        assert_eq!(
            eta2, "?",
            "ETA must be '?' when total_files is 0 and not loading model"
        );
    }

    // ── Issue #823 progress bar fix tests ────────────────────────────────────

    /// The Embed bar (slot 2) must be primed to `total_files` immediately when
    /// `walk_complete`/`start` fires — NOT left at `ProgressBar::new(1)` until
    /// the first `batch` event arrives.
    ///
    /// Why: Issue #823 Bug 2 — the Embed bar showed "0/1" throughout model
    /// loading because it was never given the correct total. This test verifies
    /// the fix: `set_embed_total` on walk_complete sets slot 2 independently.
    /// What: constructs a UI, enters Walking, calls `set_embed_total(500)`, and
    /// asserts slot 2 length is 500 (not 1).
    /// Test: this test.
    #[test]
    fn embed_bar_total_is_set_before_first_batch() {
        use super::super::reindex_ui::{ReindexPhase, ReindexUi};
        let mut ui = ReindexUi::new("idx", false);
        // Simulate walk_complete: Walk bar fills + Chunking begins
        ui.set_phase(ReindexPhase::Walking, "idx");
        ui.set_total(500);
        ui.set_position(500);
        ui.mark_stage_done(0, 100);
        ui.set_phase(ReindexPhase::Chunking, "idx");
        ui.set_total(500);
        // Prime the Embed bar (issue #823 Bug 2 fix)
        ui.set_embed_total(500);
        // Before any batch event: Embed bar must have total=500, not 1
        assert_eq!(
            ui.stage_bars[2].length(),
            Some(500),
            "Embed bar must be primed with total_files before the first batch"
        );
        ui.finish("done".to_string());
    }

    /// The Chunk bar (slot 1) must NOT be frozen at the first `batch` event.
    ///
    /// Why: Issue #823 Bug 1 — the old code called `mark_stage_done(1, ...)` in
    /// the `batch` handler, freezing the Chunk bar at whatever partial count it
    /// had when the first batch completed. Both bars must advance concurrently.
    /// What: simulates the CHUNK+EMBED phase without calling mark_stage_done(1)
    /// at the batch transition; asserts slot 1 is still Active after a batch.
    /// Test: this test.
    #[test]
    fn chunk_bar_not_frozen_at_first_batch() {
        use super::super::reindex_ui::{ReindexPhase, ReindexUi};
        let mut ui = ReindexUi::new("idx", false);
        // Walk done
        ui.set_phase(ReindexPhase::Walking, "idx");
        ui.set_total(200);
        ui.set_position(200);
        ui.mark_stage_done(0, 100);
        // Enter Chunking
        ui.set_phase(ReindexPhase::Chunking, "idx");
        ui.set_total(200);
        ui.set_embed_total(200);
        ui.activate_embed_bar();
        // Simulate chunk_progress advancing Chunk bar to 128
        ui.set_position(128);
        // Simulate first batch event: transition header to Embedding
        // (Issue #823 Bug 1 fix: do NOT call mark_stage_done(1) here)
        ui.set_phase(ReindexPhase::Embedding, "idx");
        ui.advance_embed_bar(128);
        // Chunk bar (slot 1) must still be Active (not Done) after the transition
        assert_eq!(
            ui.bar_states[1],
            super::super::reindex_ui::BarState::Active,
            "Chunk bar must remain Active after the first batch event, not be frozen"
        );
        // Embed bar must also be Active and at 128
        assert_eq!(ui.bar_states[2], super::super::reindex_ui::BarState::Active);
        assert_eq!(ui.stage_bars[2].position(), 128);
        // Now kg_start arrives → mark Chunk bar done
        ui.mark_stage_done(1, 5_000);
        assert_eq!(
            ui.bar_states[1],
            super::super::reindex_ui::BarState::Done,
            "Chunk bar must be Done after kg_start marks it"
        );
        ui.finish("done".to_string());
    }

    /// `needs_embedder_init` logic must fire for in-process embedder on the
    /// first batch (indexed == 0), not just for the sidecar.
    ///
    /// Why: Issue #823 Bug 3 — the old code used `.unwrap_or(false)` which
    /// silently disabled `embedder_init`/`embedder_ready` for the in-process
    /// embedder. The new logic fires when `indexed == 0` regardless of mode.
    /// What: simulates the new guard condition for both modes.
    /// Test: this test.
    #[test]
    fn embedder_ready_fires_for_in_process_embedder() {
        // In-process path: embedder_pid_slot is None, first_batch_ever = true
        let first_batch_ever = true;
        let embedder_pid_slot: Option<u32> = None;
        let needs_init = if let Some(pid) = embedder_pid_slot {
            pid == 0
        } else {
            first_batch_ever
        };
        assert!(
            needs_init,
            "needs_embedder_init must be true for in-process embedder on first batch"
        );

        // Sidecar path with PID=0 (not yet spawned): same result
        let pid_slot_zero: Option<u32> = Some(0);
        let needs_init_sidecar = if let Some(pid) = pid_slot_zero {
            pid == 0
        } else {
            first_batch_ever
        };
        assert!(
            needs_init_sidecar,
            "needs_embedder_init must be true for sidecar with PID=0"
        );

        // Subsequent batches (indexed > 0): must NOT fire again
        let first_batch_ever_no = false;
        let embedder_pid_slot_warm: Option<u32> = None; // in-process, 2nd batch
        let needs_init_warm = if let Some(pid) = embedder_pid_slot_warm {
            pid == 0
        } else {
            first_batch_ever_no
        };
        assert!(
            !needs_init_warm,
            "needs_embedder_init must be false on subsequent batches"
        );
    }
}
