//! Reindex driver: POST /reindex, consume the SSE progress stream, render the
//! 4-bar UI, and run the post-`--force` health check.
//!
//! Why: this is the orchestration spine shared by `index`, `reindex`, `add`,
//! `convert`, and the doctor auto-repair path; it owns the kickoff handshake,
//! the wait/timeout strategy, and the final summary.
//! What: `run_reindex{,_opts,_force_opts}` are thin wrappers over
//! `run_reindex_with`, which connects to the stream, pumps events through
//! [`events::handle_event`], drives the [`ticker`], and finishes the UI.
//! Test: `cargo test -p trusty-search`; live-daemon coverage under
//! `--include-ignored`.

use super::events::{handle_event, LoopState};
use super::options::{ReindexOptions, ReindexOutcome};
use super::progress_state::SharedProgress;
use super::registration::fetch_chunk_count;
use super::ticker::spawn_ticker;
use super::verify::verify_reindex_health;
use crate::commands::daemon_utils::daemon_base_url;
use crate::commands::format::{fmt_elapsed, format_with_commas};
use crate::commands::reindex_ui::{print_timing_breakdown, ReindexUi};
use anyhow::Result;
use colored::Colorize;
use eventsource_stream::Eventsource;
use futures_util::stream::StreamExt;
use std::io::IsTerminal;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

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
    let started = std::time::Instant::now();
    let progress = SharedProgress::new(started);
    let tick_done = Arc::new(AtomicBool::new(false));

    // Issue #744: wall-clock ticker — refreshes the stats line every second so
    // the operator sees movement even when no SSE event has arrived (see
    // `ticker::spawn_ticker` for the Files-N/total, ETA and embed/s fixes).
    let ticker = spawn_ticker(progress.clone(), ui.stats_bar(), tick_done.clone());

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
    // skip event (inside `LoopState`).  When the stall window expires with no
    // advance, we detach.  Only used when `timeout_explicit` is false.
    let stall_deadline_dur = Duration::from_secs(opts.stall_secs);

    // `eventsource-stream` handles SSE framing. The daemon emits walk_complete,
    // start, embedder_init/ready, chunk_progress, batch, skip, kg_start/complete,
    // complete, and error events (see `events::handle_event` for the full
    // protocol and `crates/trusty-search/src/service/reindex.rs::spawn_reindex`
    // for the emitter side).
    let byte_stream = resp.bytes_stream();
    let stream = byte_stream.eventsource();
    tokio::pin!(stream);

    // Per-run state machine (phase flags + stall clock + accumulating outcome).
    let mut state = LoopState::new(started);

    while !state.done {
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
                    let current_indexed = progress.indexed_now.load(Ordering::Acquire);
                    if current_indexed > state.last_indexed_snapshot {
                        // Progress observed — reset the stall clock.
                        state.last_indexed_snapshot = current_indexed;
                        state.last_progress = std::time::Instant::now();
                    } else if state.last_progress.elapsed() >= stall_deadline_dur {
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
        handle_event(&mut state, &mut ui, &progress, &evt, index_id);
    }

    // Stop the ticker before finishing the UI.
    tick_done.store(true, Ordering::Release);
    let _ = ticker.await;

    let outcome = state.outcome;

    if timed_out {
        // Hard cap (explicit --timeout) fired.
        let still_progressing = progress.indexed_now.load(Ordering::Acquire)
            > state.last_indexed_snapshot
            || state.last_progress.elapsed() < stall_deadline_dur;
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
        let indexed = progress.indexed_now.load(Ordering::Acquire);
        let total = outcome.indexed.max(indexed);
        ui.abandon(format!(
            "{} No indexing progress for {}s (Files {}/{}) \u{2014} detaching; \
             daemon continues in background",
            "\u{26a0}".yellow(),
            opts.stall_secs,
            format_with_commas(indexed),
            format_with_commas(total),
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
    // Issue #929: all three completion branches include the index_id so piped
    // / non-TTY multi-index runs can clearly associate each block with its index.
    let final_msg = if outcome.errors > 0 {
        format!(
            "{} '{}' — indexed {} files \u{2192} {} chunks  [took {}, {} errors, {} unchanged]",
            "\u{2713}".green(),
            index_id,
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
        // Issue #929: include the index_id in the normal completion line so
        // piped / non-TTY multi-index runs clearly show which index each
        // completion block belongs to. Format mirrors the "up to date" line
        // above which already includes the id.
        format!(
            "{} '{}' — indexed {} changed file{} \u{2192} {} chunks  [took {}, {} unchanged]",
            "\u{2713}".green(),
            index_id,
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
        // Issue #929: pass defer_embed + lexical_only so the embed timing
        // line is context-aware (suppressed when deferred, calm when
        // lexical-only, loud when the embedder was expected but absent).
        print_timing_breakdown(
            &t,
            outcome.total_chunks,
            outcome.elapsed_ms,
            state.defer_embed,
            state.lexical_only,
        );
    }

    // Issue #929: if the daemon is running embedding in the background, print a
    // clear "searchable now; embedding running in background" note so the user
    // knows:
    //   1. The index is already queryable via lexical + KG search.
    //   2. Semantic (vector) search will be available once the background job
    //      finishes — they can track it via `trusty-search status <id> --watch`.
    if state.defer_embed {
        println!();
        println!("{} Searchable now (lexical + graph).", "\u{2713}".green());
        println!("\u{23f3} Semantic embedding running in background.");
        println!(
            "   Track:  trusty-search status {} --watch",
            index_id.cyan()
        );
    }

    // Post-reindex health check (blue-green safety net).
    if opts.verify_after {
        verify_reindex_health(&client, &base, index_id, &outcome, opts.prior_chunk_count).await?;
    }

    Ok(outcome)
}
