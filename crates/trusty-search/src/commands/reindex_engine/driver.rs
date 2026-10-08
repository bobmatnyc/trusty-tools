//! Reindex driver: kick off the reindex, consume its progress stream, render
//! the 4-bar UI, and run the post-`--force` health check.
//!
//! Why: this is the orchestration spine shared by `index`, `reindex`, `add`,
//! `convert`, and the doctor auto-repair path; it owns the kickoff handshake,
//! the wait/timeout strategy, and the final summary.
//! What: `run_reindex{,_opts,_force_opts}` are thin wrappers over
//! `run_reindex_with`, which sends `search.index.reindex`, opens
//! `search.index.reindex.stream` over the daemon socket (#9214), pumps events
//! through [`events::handle_event`], drives the [`ticker`], and finishes the UI.
//! Test: `driver_tests.rs`; live-daemon coverage under `--include-ignored`.

use super::events::{handle_event, LoopState};
use super::options::{ReindexOptions, ReindexOutcome};
use super::progress_state::SharedProgress;
use super::registration::fetch_chunk_count;
use super::ticker::spawn_ticker;
use super::verify::verify_reindex_health;
use crate::commands::daemon_rpc::rpc_error;
use crate::commands::format::{fmt_elapsed, format_with_commas};
use crate::commands::reindex_ui::{print_timing_breakdown, ReindexUi};
use anyhow::{Context as _, Result};
use colored::Colorize;
use futures_util::stream::StreamExt;
use std::io::IsTerminal;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use trusty_search::service::daemon_client::DaemonClient;
use trusty_search::service::rpc::streams::METHOD_INDEX_REINDEX_STREAM;
use trusty_search::service::rpc::writes::METHOD_INDEX_REINDEX;

#[cfg(test)]
#[path = "driver_tests.rs"]
mod tests;

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
    let client = DaemonClient::resolve()?;
    let prior = fetch_chunk_count(&client, index_id).await;
    let opts = ReindexOptions {
        verify_after: true,
        prior_chunk_count: prior,
        force: true,
        timeout_secs,
        timeout_explicit,
        ..ReindexOptions::default()
    };
    run_reindex_on(&client, index_id, root_path, opts)
        .await
        .map(|_| ())
}

/// Drive a reindex: kick it off, then read its progress stream and render
/// progress with a 4-bar `MultiProgress` layout (header + Crawl / Chunk /
/// Embed / KG bars + stats line). A wall-clock ticker keeps the stats line
/// moving even when progress events are sparse (e.g. the embedder is mid-batch).
///
/// Why: the previous design used a single bar relabelled at each phase
/// transition (issue #317). Issue #401 replaces it with 4 sequential bars so
/// the operator can see at a glance which stage is active, which are done, and
/// which are still pending.
///
/// Progress events added by #401 (backward-compatible; older daemons omit
/// them and the CLI falls back gracefully):
///
/// - `kg_start`    — emitted just before `rebuild_symbol_graph_for_reindex`
/// - `kg_complete` — emitted after; carries `symbol_count`, `edge_count`, `kg_ms`
///
/// What: resolves the daemon socket and runs [`run_reindex_on`].
/// Test: `driver_tests.rs`.
pub async fn run_reindex_with(
    index_id: &str,
    root_path: &std::path::Path,
    opts: ReindexOptions,
) -> Result<ReindexOutcome> {
    run_reindex_on(&DaemonClient::resolve()?, index_id, root_path, opts).await
}

/// [`run_reindex_with`] against an explicit daemon client.
///
/// Why (#9214): the socket replaces the HTTP kickoff and the SSE body; a stream
/// the daemon ends, or a socket that breaks, before the `complete` event must
/// fail the run (Q5) rather than print success.
/// What: `search.index.reindex {index_id, body: {root_path, force}}`, then
/// `search.index.reindex.stream {index_id}`, one item per progress event.
/// `not found` on the kickoff names the unregistered index; every other
/// refusal carries the daemon's text (#8889: the running reindex).
/// Test: `a_stream_ending_without_complete_is_an_error`,
/// `a_complete_event_finishes_the_run`, `an_unknown_index_kickoff_names_it`.
pub(super) async fn run_reindex_on(
    client: &DaemonClient,
    index_id: &str,
    root_path: &std::path::Path,
    opts: ReindexOptions,
) -> Result<ReindexOutcome> {
    let kickoff = serde_json::json!({
        "index_id": index_id,
        "body": { "root_path": root_path, "force": opts.force },
    });
    match client.call(METHOD_INDEX_REINDEX, kickoff).await {
        Ok(_) => {}
        Err(e) if e.is_not_found() => anyhow::bail!(
            "index '{}' is not registered on the daemon \u{2014} run `trusty-search index` first",
            index_id
        ),
        Err(e) => {
            return Err(rpc_error(e))
                .with_context(|| format!("reindex kickoff for '{index_id}' failed"));
        }
    }

    // #9214: no `stream_url` to follow — the stream is its own method. A
    // refusal before the first item arrives as the stream's one error item.
    let frames = client
        .stream(
            METHOD_INDEX_REINDEX_STREAM,
            serde_json::json!({ "index_id": index_id }),
        )
        .await
        .map_err(rpc_error)
        .with_context(|| format!("could not open the reindex stream for '{index_id}'"))?;
    // `into_stream` keeps the in-flight read inside the stream's state, so the
    // `select!` below can drop a `next()` future without losing a frame.
    let stream = frames.into_stream();
    tokio::pin!(stream);

    // Progress is shown only when stdout is a TTY. When the CLI output is
    // piped or redirected (`std::io::stdout()` is not a terminal) the bars
    // draw to a hidden target so captured output stays clean. Progress always
    // renders to stderr regardless — stdout is the MCP JSON-RPC transport.
    let interactive = std::io::stdout().is_terminal();

    // 4-bar UI: header + Crawl / Chunk / Embed / KG + stats.
    // Built eagerly so the user sees something during the 1–2s daemon warmup
    // before the first progress event arrives.
    let mut ui = ReindexUi::new(index_id, interactive);

    // Atomics shared with the wall-clock ticker. The ticker refreshes the
    // stats line every second so the user sees movement even when the
    // stream is idle (e.g. mid-batch embedding of 256 chunks).
    let started = std::time::Instant::now();
    let progress = SharedProgress::new(started);
    let tick_done = Arc::new(AtomicBool::new(false));

    // Issue #744: wall-clock ticker — refreshes the stats line every second so
    // the operator sees movement even when no progress event has arrived (see
    // `ticker::spawn_ticker` for the Files-N/total, ETA and embed/s fixes).
    let ticker = spawn_ticker(progress.clone(), ui.stats_bar(), tick_done.clone());

    // `timed_out` — hard deadline fired (explicit --timeout only).
    let mut timed_out = false;
    // `stalled` — no progress observed for stall_secs (default 120 s).
    let mut stalled = false;
    // #9214 (Q5): why the stream stopped short of `complete`, for the error.
    let mut ended_early: Option<String> = None;

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

    // The daemon emits walk_complete, start, embedder_init/ready,
    // chunk_progress, batch, skip, kg_start/complete, complete, and error
    // events (see `events::handle_event` for the full protocol and
    // `crates/trusty-search/src/service/reindex.rs::spawn_reindex` for the
    // emitter side).

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
            // Progress-aware path: wait for the next event with a 1-second
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
        let evt: serde_json::Value = match maybe_event {
            Some(Ok(e)) => e,
            Some(Err(e)) => {
                ui.stats_bar()
                    .println(format!("{} stream read error: {e}", "\u{26a0}".yellow()));
                ended_early = Some(format!("the progress stream failed: {e}"));
                break;
            }
            None => {
                ended_early =
                    Some("the daemon closed the progress stream before `complete`".to_string());
                break;
            }
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
        // #9214 (Q5): a stream that ends short of `complete` is a failure.
        anyhow::bail!(
            "reindex of '{index_id}' did not complete: {}",
            ended_early
                .as_deref()
                .unwrap_or("no `complete` event arrived")
        );
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
    // Pass the stream's `elapsed_ms` (wall-clock total) so the breakdown can
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
        verify_reindex_health(client, index_id, &outcome, opts.prior_chunk_count).await?;
    }

    Ok(outcome)
}
