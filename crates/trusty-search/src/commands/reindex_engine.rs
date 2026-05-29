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
/// Why: callers such as `run_reindex` (plain) and `run_reindex_force` need to
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
    /// Maximum wall-clock seconds to wait for the SSE reindex stream to emit
    /// a `complete` event. Default: 600. Use `--timeout 0` to disable (wait
    /// forever). When the deadline is exceeded the CLI prints a warning and
    /// exits; the daemon continues indexing in the background.
    pub timeout_secs: u64,
}

impl Default for ReindexOptions {
    fn default() -> Self {
        Self {
            verify_after: false,
            prior_chunk_count: None,
            force: false,
            timeout_secs: 600,
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

/// Plain reindex (no post-verify). Used by the non-force `index` command, the
/// bare `reindex` command, and the doctor auto-repair path. The daemon's
/// hash-skip optimization (see `reindex.rs::hash_content`) means unchanged
/// files are cheap, so calling this even when nothing changed is fine.
///
/// `timeout_secs` caps how long the CLI waits for the SSE stream's `complete`
/// event. 0 means no limit (wait forever). Default for callers that don't have
/// an explicit user-supplied value: 600.
///
/// Why: extracted so callers don't have to construct `ReindexOptions`.
/// What: delegates to `run_reindex_with` with verify_after = false.
/// Test: covered by `run_reindex_with` integration tests.
pub async fn run_reindex(
    index_id: &str,
    root_path: &std::path::Path,
    timeout_secs: u64,
) -> Result<()> {
    run_reindex_with(
        index_id,
        root_path,
        ReindexOptions {
            timeout_secs,
            ..ReindexOptions::default()
        },
    )
    .await
    .map(|_| ())
}

/// `index --force` reindex: snapshot the prior chunk count, kick off a full
/// reindex, and run a post-reindex health check. Exits 1 if the new index
/// looks unhealthy (no chunks or empty sanity query).
///
/// Why: the `--force` path differs from the plain path only in verify_after
/// and force = true; extracting it keeps `index.rs` free of options wiring.
/// What: fetches the prior chunk count, then delegates to `run_reindex_with`.
/// Test: covered indirectly by `index --force` integration tests.
pub async fn run_reindex_force(
    index_id: &str,
    root_path: &std::path::Path,
    timeout_secs: u64,
) -> Result<()> {
    let prior = fetch_chunk_count(index_id).await;
    let opts = ReindexOptions {
        verify_after: true,
        prior_chunk_count: prior,
        force: true,
        timeout_secs,
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
    let skipped_now = StdArc::new(AtomicU64::new(0));
    let cps_now = StdArc::new(AtomicU64::new(0));
    let tick_done = StdArc::new(AtomicBool::new(false));

    // Clone the bars the ticker needs — `ProgressBar` is Arc-wrapped so clones
    // are cheap and the ticker can write to them independently.
    let ticker_stats_bar = ui.stats_bar();
    let ticker_embed_bar = ui.embed_bar();

    let ticker = {
        let indexed_now = indexed_now.clone();
        let chunks_now = chunks_now.clone();
        let skipped_now = skipped_now.clone();
        let cps_now = cps_now.clone();
        let tick_done = tick_done.clone();
        let stats_bar = ticker_stats_bar;
        let embed_bar = ticker_embed_bar;
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
                let chunks = chunks_now.load(Ordering::Acquire);
                let skipped = skipped_now.load(Ordering::Acquire);
                let cps = cps_now.load(Ordering::Acquire);
                let fps = indexed.checked_div(elapsed).unwrap_or(0);
                let total = embed_bar.length().unwrap_or(0);
                let eta = if fps > 0 && total > indexed {
                    super::format::fmt_secs((total - indexed) / fps)
                } else {
                    "?".to_string()
                };
                stats_bar.set_message(format!(
                    "Embedding\u{2026} {chunks} chunks \u{2014} {cps} cps \u{2014} \
                     Files {indexed}/{total}  Skipped {skipped}  Elapsed {elapsed}s  ETA {eta}",
                    chunks = format_with_commas(chunks),
                    cps = cps,
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
    let mut timed_out = false;

    // Optional wall-clock deadline for the SSE stream. `timeout_secs == 0`
    // means wait forever (legacy behaviour). Otherwise each `stream.next()`
    // is raced against `tokio::time::sleep_until(deadline)` via
    // `tokio::select!`. When the sleep wins we set `timed_out = true` and
    // break so the post-loop path can print the canonical warning.
    let deadline: Option<tokio::time::Instant> = if opts.timeout_secs > 0 {
        Some(tokio::time::Instant::now() + Duration::from_secs(opts.timeout_secs))
    } else {
        None
    };

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
        let maybe_event = if let Some(dl) = deadline {
            tokio::select! {
                biased;
                ev = stream.next() => ev,
                _ = tokio::time::sleep_until(dl) => {
                    timed_out = true;
                    break;
                }
            }
        } else {
            stream.next().await
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
                ui.set_phase(ReindexPhase::Walking, index_id);
                ui.set_total(total);
                // Walk is already done by the time this event arrives (sync on
                // daemon). Fill the bar to 100% and freeze it with a near-zero
                // elapsed time (walk is a fast synchronous scan on the daemon).
                ui.set_position(total);
                ui.mark_stage_done(0, 0);
            }
            // ── start ──────────────────────────────────────────────────────
            Some("start") => {
                let total = evt.get("total_files").and_then(|v| v.as_u64()).unwrap_or(0);
                lexical_only = evt
                    .get("lexical_only")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);

                if received_walk_complete {
                    // Three-phase flow: Walk bar is already done; enter Chunking.
                    chunk_started_ms = started.elapsed().as_millis() as u64;
                    ui.set_phase(ReindexPhase::Chunking, index_id);
                    ui.set_total(total);
                } else {
                    // Legacy two-phase flow (old daemon, no walk_complete):
                    // jump straight to Embed (or Chunking for lexical-only).
                    ui.set_total(total);
                    if lexical_only {
                        chunk_started_ms = started.elapsed().as_millis() as u64;
                        ui.set_phase(ReindexPhase::Chunking, index_id);
                    } else {
                        embed_started_ms = started.elapsed().as_millis() as u64;
                        ui.set_phase(ReindexPhase::Embedding, index_id);
                        entered_embedding = true;
                    }
                }
            }
            // ── batch ──────────────────────────────────────────────────────
            Some("batch") => {
                // Flip Chunking → Embedding on the first batch event (three-phase
                // flow only). Skip when lexical_only (no embed batches).
                if received_walk_complete && !entered_embedding && !lexical_only {
                    // Mark Chunk bar done before activating Embed.
                    let chunk_ms = started.elapsed().as_millis() as u64 - chunk_started_ms;
                    ui.mark_stage_done(1, chunk_ms);
                    embed_started_ms = started.elapsed().as_millis() as u64;
                    ui.set_phase(ReindexPhase::Embedding, index_id);
                    entered_embedding = true;
                }

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
                    ui.set_total(total);
                }
                indexed_now.store(indexed, Ordering::Release);
                cps_now.store(chunks_per_sec, Ordering::Release);
                let new_chunks =
                    chunks_now.fetch_add(batch_chunks, Ordering::AcqRel) + batch_chunks;
                ui.set_position(indexed);
                ui.update_stats(
                    indexed,
                    new_chunks,
                    skipped_now.load(Ordering::Acquire),
                    chunks_per_sec,
                    started.elapsed().as_secs(),
                );
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
            }
            // ── kg_start ───────────────────────────────────────────────────
            // New event added by issue #401. The daemon emits this immediately
            // before `rebuild_symbol_graph_for_reindex`. The CLI marks the Embed
            // bar done and activates the KG bar. Old daemons omit this event;
            // the KG bar stays pending and is cleared at `complete`.
            Some("kg_start") => {
                // Embed bar done (if it was active).
                if entered_embedding {
                    let embed_ms = started.elapsed().as_millis() as u64 - embed_started_ms;
                    ui.mark_stage_done(2, embed_ms);
                }
                ui.clear_stats();
                ui.set_phase(ReindexPhase::KnowledgeGraph, index_id);
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

                // Snap the Embed bar to full position (final authoritative count).
                ui.set_position(outcome.indexed);

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

                // Mark Chunk bar done if it wasn't already (two-phase / lexical path).
                if !received_walk_complete || lexical_only {
                    let chunk_ms = outcome.timings.map(|t| t.parse_ms).unwrap_or(0);
                    ui.mark_stage_done(1, chunk_ms);
                }

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
        ui.abandon(format!(
            "{} trusty-search index timed out after {}s \u{2014} continuing; re-run later if needed",
            "\u{26a0}".yellow(),
            opts.timeout_secs,
        ));
        eprintln!(
            "{} Daemon is still indexing in the background. \
             Use `trusty-search status` or re-run `trusty-search index` to check progress. \
             Pass `--timeout <seconds>` to wait longer (e.g. `--timeout 1200`).",
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
    if let Some(t) = outcome.timings {
        print_timing_breakdown(&t, outcome.total_chunks);
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
/// Test: covered indirectly by `run_reindex_force`.
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
    /// that rely on `Default::default()` get a reasonable timeout.
    ///
    /// Why: a zero timeout would make every plain reindex exit immediately.
    /// What: asserts the default field values.
    /// Test: this test.
    #[test]
    fn default_options_are_sane() {
        let opts = ReindexOptions::default();
        assert!(!opts.verify_after);
        assert!(opts.prior_chunk_count.is_none());
        assert!(!opts.force);
        assert_eq!(opts.timeout_secs, 600);
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
}
