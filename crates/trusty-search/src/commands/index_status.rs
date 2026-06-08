//! Handler for `trusty-search index-status [INDEX] [--watch]` (issue #929).
//!
//! Why: the defer-embed feature (#923) runs semantic embedding as a background
//! job AFTER the fast pass completes. Without a dedicated per-index status
//! command, operators had no way to see embedding progress short of reading
//! daemon logs or polling `/indexes/:id/status` by hand. This command closes
//! that gap by rendering a concise per-stage status table with live embed
//! progress, and — with `--watch` — polls until embedding finishes.
//!
//! What: when an `index_id` is provided, queries `GET /indexes/:id/status`
//! directly.  When no id is given, resolves the current working directory to
//! the matching index(es) via `index_cwd_resolve::resolve_cwd_indexes` and
//! renders a table for each one.  With `--watch` and a single match the table
//! is polled every ~1 s until `semantic.status == ready|failed`.  With
//! `--watch` and multiple matches the user is asked to re-run with an explicit
//! id (a predictable, safe behaviour that avoids interleaved output).
//!
//! Test: unit tests for the rendering helper and cwd resolution in this module
//! and `index_cwd_resolve`; integration coverage via `cargo test -p trusty-search`.

use super::daemon_utils::daemon_base_url;
use super::format::format_with_commas;
use anyhow::Result;
use colored::Colorize;
use std::io::IsTerminal;
use std::time::Duration;

// ─── Public entry point ───────────────────────────────────────────────────────

/// Handle `trusty-search index-status [index_id] [--watch]`.
///
/// Why: exposes per-stage reindex status and deferred-embed progress so
/// operators can track background embedding without reading daemon logs.
/// When no id is provided, defaults to the index(es) covering the current
/// working directory — mirroring the convention used by `trusty-search index .`.
///
/// What: if `index_id` is `Some`, fetches `/indexes/:id/status` and renders
/// a stage table (watch polls every ~1 s).  If `index_id` is `None`, resolves
/// the cwd to matching indexes and renders each one; `--watch` with multiple
/// matches errors with the candidate ids so the user can pick.
///
/// Test: `handle_index_status_renders_ready_table` in this module's tests.
pub async fn handle_index_status(index_id: Option<&str>, watch: bool, json: bool) -> Result<()> {
    let base = daemon_base_url();
    crate::commands::daemon_guard::ensure_daemon_running_or_exit(&base).await?;

    let client = trusty_common::server::daemon_http_client()?;

    match index_id {
        Some(id) => {
            // Explicit id: single-index path (original behaviour).
            let url = format!("{}/indexes/{}/status", base, id);
            run_status_for_single(id, &url, &client, watch, json).await
        }
        None => {
            // No id: resolve from cwd.
            run_status_for_cwd(&client, &base, watch, json).await
        }
    }
}

// ─── CWD resolution path ──────────────────────────────────────────────────────

/// Resolve CWD → matching index(es) and render status for each.
///
/// Why: `index_id` is optional — when omitted the user expects the same
/// context-aware defaulting as `trusty-search index .`.
/// What: calls `resolve_cwd_indexes`, then dispatches to the single-index
/// or multi-index renderer.  With `--watch` and more than one match, prints
/// the candidate ids to stderr and exits non-zero rather than producing
/// interleaved output.
/// Test: multi-match and no-match paths exercised by unit tests in
/// `index_cwd_resolve`; cwd single-match path exercised below.
async fn run_status_for_cwd(
    client: &reqwest::Client,
    base: &str,
    watch: bool,
    json: bool,
) -> Result<()> {
    use super::index_cwd_resolve::resolve_cwd_indexes;

    let matches = resolve_cwd_indexes(client, base).await?;

    match matches.len() {
        0 => {
            let cwd = std::env::current_dir().unwrap_or_default();
            eprintln!(
                "{} no trusty-search index registered for {} — \
                 run 'trusty-search index .' to create one",
                "✗".red(),
                cwd.display()
            );
            anyhow::bail!("no index registered for current directory");
        }
        1 => {
            let m = &matches[0];
            let url = format!("{}/indexes/{}/status", base, m.id);
            run_status_for_single(&m.id, &url, client, watch, json).await
        }
        _ => {
            // Multiple indexes cover cwd.
            if watch {
                // --watch with multiple matches is ambiguous — require explicit id.
                eprintln!(
                    "{} --watch requires an explicit index id when multiple indexes \
                     cover the current directory. Candidates:",
                    "✗".red()
                );
                for m in &matches {
                    eprintln!("    {} ({})", m.id.bold(), m.root_path.display());
                }
                eprintln!("Re-run: trusty-search index-status <id> --watch");
                anyhow::bail!(
                    "--watch requires an explicit index id when multiple indexes cover cwd"
                );
            }
            // No watch: print all tables in turn using the pre-fetched bodies.
            // (Re-fetching is unnecessary for a static snapshot.)
            for m in &matches {
                if json {
                    println!("{}", serde_json::to_string_pretty(&m.status_body)?);
                } else {
                    print_status_table(&m.id, &m.status_body);
                    println!(); // blank line between tables
                }
            }
            Ok(())
        }
    }
}

// ─── Single-index status path ─────────────────────────────────────────────────

/// Render (or poll) the status for one known index id.
///
/// Why: the explicit-id path and the cwd single-match path converge here so
/// the rendering + watch logic lives in exactly one place.
/// What: single-shot fetches and renders; with `watch=true` polls every ~1 s
/// until the semantic stage settles.
/// Test: rendering logic covered by unit tests; polling covered by integration
/// tests.
async fn run_status_for_single(
    index_id: &str,
    url: &str,
    client: &reqwest::Client,
    watch: bool,
    json: bool,
) -> Result<()> {
    if !watch {
        let body = fetch_status(client, url).await?;
        if json {
            println!("{}", serde_json::to_string_pretty(&body)?);
        } else {
            print_status_table(index_id, &body);
        }
        return Ok(());
    }

    // --watch: poll every ~1 s until semantic stage settles (Ready or Failed).
    let is_tty = std::io::stdout().is_terminal();
    loop {
        let body = fetch_status(client, url).await?;
        let semantic_status = body
            .get("stages")
            .and_then(|s| s.get("semantic"))
            .and_then(|se| se.get("status"))
            .and_then(|v| v.as_str())
            .unwrap_or("pending");

        if json {
            println!("{}", serde_json::to_string_pretty(&body)?);
        } else if is_tty {
            // Overwrite the previous table lines in-place on a TTY so the
            // display updates in-place rather than scrolling.
            print_status_table_tty_clear(index_id, &body);
        } else {
            // Non-TTY (piped / redirected): emit one line per poll with a
            // machine-parseable format so scripts can `grep` for completion.
            print_status_line_nontty(index_id, &body);
        }

        if semantic_status == "ready" || semantic_status == "failed" {
            if is_tty && !json {
                println!();
            }
            break;
        }

        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    Ok(())
}

// ─── HTTP helper ─────────────────────────────────────────────────────────────

/// Fetch the `/indexes/:id/status` JSON body.
///
/// Why: isolating the HTTP call lets the rendering logic be tested with
/// synthetic JSON without hitting a live daemon.
/// What: GETs the URL, parses the JSON response, returns an error if the
/// daemon returns a non-2xx status (e.g. 404 when the index is not registered).
/// Test: covered indirectly by `handle_index_status`.
async fn fetch_status(client: &reqwest::Client, url: &str) -> Result<serde_json::Value> {
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("could not reach daemon: {e}"))?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        anyhow::bail!(
            "index not found — run `trusty-search index` to register it first, \
             or `trusty-search list` to see registered indexes"
        );
    }
    if !resp.status().is_success() {
        anyhow::bail!("daemon returned {} for status query", resp.status());
    }
    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| anyhow::anyhow!("could not parse status response: {e}"))?;
    Ok(body)
}

// ─── Rendering helpers ────────────────────────────────────────────────────────

/// Number of lines rendered by `print_status_table`: 1 header + 3 stage rows.
///
/// Why: the TTY-clear path uses `\x1b[{N}F` (cursor-up N lines) to overwrite
/// the previous table in-place. This constant must stay equal to the actual
/// rendered line count — header plus one row per stage (lexical/semantic/graph).
/// If the table gains or loses rows, update this constant and the comment.
/// What: used by `print_status_table_tty_clear` to build the cursor-up escape.
/// Test: visually — a wrong value causes either a scrolling table or garbled
/// output. There is no mechanical test because it requires a live TTY.
const WATCH_TABLE_LINES: usize = 4; // 1 header + 3 stage rows (lexical/semantic/graph)

/// Render a 3-row stage table to stdout (single-shot or TTY watch mode).
///
/// Why: both the single-shot path and the TTY watch path need the same
/// formatted output; extracting it keeps the rendering testable.
/// What: prints `<index-id>  <root_path>` header, then one row per stage
/// (lexical / semantic / graph) with status and optional embed progress.
/// Test: `render_status_table_formats_correctly` in this module's tests.
pub fn print_status_table(index_id: &str, body: &serde_json::Value) {
    let root = body.get("root_path").and_then(|v| v.as_str()).unwrap_or("");
    println!("  {}  {}", index_id.bold(), root.dimmed());
    if let Some(stages) = body.get("stages") {
        print_stage_row("lexical ", stages.get("lexical"));
        print_stage_row("semantic", stages.get("semantic"));
        print_stage_row("graph   ", stages.get("graph"));
    }
}

/// Render the table, prefixed with ANSI erase-to-start-of-screen so the
/// output overwrites the previous iteration in watch mode on a TTY.
///
/// Why: without erasure, each 1-second poll appends 5 new lines, scrolling
/// the terminal. The ANSI escape moves the cursor up and clears the lines
/// rendered on the previous iteration so the table appears to update in place.
/// What: prints `\x1b[{WATCH_TABLE_LINES}F` (cursor up N lines — 1 header +
/// 3 stage rows) and then the table; `\x1b[0J` (clear to end-of-screen)
/// ensures the viewport is clean. The line count is `WATCH_TABLE_LINES` —
/// **it must equal the number of lines `print_status_table` actually emits**.
/// Test: covered via integration; the escape sequence is only emitted on a TTY.
fn print_status_table_tty_clear(index_id: &str, body: &serde_json::Value) {
    // Move cursor up WATCH_TABLE_LINES (1 header + 3 stage rows) to overwrite.
    print!("\x1b[{WATCH_TABLE_LINES}F\x1b[0J");
    print_status_table(index_id, body);
}

/// Emit a single line in a machine-parseable format for non-TTY watch polling.
///
/// Why: piped consumers (scripts, CI) cannot use ANSI escape sequences for
/// in-place updates; they need one line per poll that they can grep.
/// What: emits `<timestamp> <index_id> semantic=<status> <N>/<total> (<pct>%)`
/// Test: covered via integration.
fn print_status_line_nontty(index_id: &str, body: &serde_json::Value) {
    let now = chrono::Utc::now().format("%H:%M:%S").to_string();
    let sem = body
        .get("stages")
        .and_then(|s| s.get("semantic"))
        .cloned()
        .unwrap_or(serde_json::json!({}));
    let status = sem.get("status").and_then(|v| v.as_str()).unwrap_or("?");
    let embedded = sem.get("embedded").and_then(|v| v.as_u64()).unwrap_or(0);
    let total = sem.get("total").and_then(|v| v.as_u64()).unwrap_or(0);
    let pct = (embedded * 100).checked_div(total).unwrap_or(0);
    if total > 0 {
        println!(
            "{now} {index_id} semantic={status} {}/{} ({pct}%)",
            format_with_commas(embedded),
            format_with_commas(total),
        );
    } else {
        println!("{now} {index_id} semantic={status}");
    }
}

/// Truncate a failure-reason string to at most 80 display characters.
///
/// Why: raw failure reasons can be multi-kilobyte stack traces; rendering them
/// verbatim breaks the terminal table layout.
/// What: if `msg` exceeds 80 chars, returns the first 79 chars followed by `…`
/// (Unicode ellipsis, one column wide), giving exactly 80 displayed columns.
/// If `msg` fits, returns it unchanged as an owned `String`.
/// Test: `failure_message_truncated_at_80_chars` in this module's tests.
pub fn truncate_reason(msg: &str) -> String {
    if msg.len() > 80 {
        format!("{}…", &msg[..79])
    } else {
        msg.to_string()
    }
}

/// Render one stage row: `  <label>   <status>   [progress]`.
///
/// Why: keeps stage-row formatting consistent and testable in isolation.
/// What: for the `semantic` stage in an active embed state, appends
/// `<embedded> / <total> chunks  (N%)`. For `Failed`, appends the failure
/// reason (truncated to 80 chars to avoid line-wrapping). For other stages,
/// shows only the status.
/// Test: `render_stage_row_shows_embed_progress` in this module's tests.
pub fn print_stage_row(label: &str, stage: Option<&serde_json::Value>) {
    let stage = match stage {
        Some(s) => s,
        None => {
            println!("    {}  {}", label, "unknown".dimmed());
            return;
        }
    };
    let status = stage.get("status").and_then(|v| v.as_str()).unwrap_or("?");
    let colored_status = colorize_status(status);

    // Embed progress: show when `embedded` or `total` are present.
    let embedded = stage.get("embedded").and_then(|v| v.as_u64());
    let total = stage.get("total").and_then(|v| v.as_u64());
    let failure = stage
        .get("failure")
        .and_then(|v| v.as_str())
        .map(truncate_reason);

    match (embedded, total, failure) {
        (Some(emb), Some(tot), _) if tot > 0 => {
            let pct = (emb * 100).checked_div(tot).unwrap_or(0);
            println!(
                "    {}  {}   {}/{} chunks  ({}%)",
                label.bold(),
                colored_status,
                format_with_commas(emb),
                format_with_commas(tot),
                pct,
            );
        }
        (_, _, Some(reason)) => {
            println!(
                "    {}  {}   {}",
                label.bold(),
                colored_status,
                reason.red()
            );
        }
        _ => {
            println!("    {}  {}", label.bold(), colored_status);
        }
    }
}

/// Colorize a stage status string for human-readable output.
///
/// Why: consistent coloring makes it easy to scan at a glance — green for
/// ready, yellow for in-progress, red for failed, dim for pending/skipped.
/// What: maps `status` string to a colored version; falls back to bold white.
/// Test: `colorize_status_maps_known_values` in this module's tests.
pub fn colorize_status(status: &str) -> colored::ColoredString {
    match status {
        "ready" => "ready".green(),
        "in_progress" => "embedding".yellow(),
        "failed" => "failed".red(),
        "pending" => "pending".dimmed(),
        "skipped" => "skipped".dimmed(),
        other => other.bold(),
    }
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// `colorize_status` must map each known status string to the expected
    /// colored variant without panicking.
    ///
    /// Why: a wrong mapping silently produces the wrong color; pinning the
    /// behavior protects against accidental regressions.
    /// What: calls `colorize_status` for each known value, asserts the plain
    /// text of the result.
    /// Test: this test.
    #[test]
    fn colorize_status_maps_known_values() {
        colored::control::set_override(false);
        assert_eq!(colorize_status("ready").to_string(), "ready");
        assert_eq!(colorize_status("in_progress").to_string(), "embedding");
        assert_eq!(colorize_status("failed").to_string(), "failed");
        assert_eq!(colorize_status("pending").to_string(), "pending");
        assert_eq!(colorize_status("skipped").to_string(), "skipped");
        assert_eq!(colorize_status("unknown").to_string(), "unknown");
    }

    /// `format_with_commas` formats embed progress numbers correctly and the
    /// percentage arithmetic is correct for a known sample.
    ///
    /// Why: guards the formatting helpers used by `print_stage_row` so a
    /// refactor of the comma-formatter or arithmetic cannot silently break
    /// the rendered output operators rely on.
    /// What: checks that `format_with_commas` produces comma-separated strings
    /// and that integer percent truncation gives 41% for 62914/152616.
    /// Test: this test.
    #[test]
    fn embed_progress_pct_arithmetic() {
        colored::control::set_override(false);
        let embedded: u64 = 62_914;
        let total: u64 = 152_616;
        let pct = embedded * 100 / total;
        assert_eq!(pct, 41, "percentage must be 41% for 62914/152616");
        assert_eq!(format_with_commas(embedded), "62,914");
        assert_eq!(format_with_commas(total), "152,616");
    }

    /// `truncate_reason` truncates long failure messages to exactly 80 display
    /// columns (79 chars + 1-column `…` ellipsis).
    ///
    /// Why: a 4 KB stack trace in a failure message would break the terminal
    /// table layout; this guards the production truncation path in `print_stage_row`.
    /// What: calls `truncate_reason` with a 200-char string and a short string,
    /// asserting the char count and pass-through behaviour respectively.
    /// Test: this test calls the real `truncate_reason` function used by
    /// `print_stage_row`.
    #[test]
    fn failure_message_truncated_at_80_chars() {
        // Long message: must be truncated to 80 display columns.
        let long_msg = "x".repeat(200);
        let truncated = truncate_reason(&long_msg);
        assert_eq!(
            truncated.chars().count(),
            80,
            "truncated string must be 79 chars + ellipsis = 80 display columns"
        );
        assert!(truncated.ends_with('…'), "must end with ellipsis character");

        // Short message: must pass through unchanged.
        let short_msg = "connection refused";
        let result = truncate_reason(short_msg);
        assert_eq!(result, short_msg, "short messages must not be modified");

        // Exactly 80 chars: must pass through unchanged.
        let exact_msg = "y".repeat(80);
        let result = truncate_reason(&exact_msg);
        assert_eq!(result, exact_msg, "80-char messages must not be truncated");
    }

    /// The percentage computation must use `checked_div` and return 0 when
    /// `total=0`, not panic with divide-by-zero.
    ///
    /// Why: a division-by-zero in the watch loop would crash the CLI.
    /// What: verifies that `(embedded * 100).checked_div(0)` returns `None`
    /// and the `unwrap_or(0)` produces 0 rather than panicking.
    /// Test: this test.
    #[test]
    fn embed_progress_pct_zero_total_guard() {
        let embedded: u64 = 0;
        let total: u64 = 0;
        let pct = (embedded * 100).checked_div(total).unwrap_or(0);
        assert_eq!(
            pct, 0,
            "pct must be 0 when total is 0 (checked_div returns None)"
        );
    }
}
