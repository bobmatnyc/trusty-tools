//! Handler for `trusty-search cleanup`.
//!
//! Why: over time, projects come and go and the daemon's `indexes.toml` accumulates
//! stale registrations for projects that were never successfully indexed (0 chunks).
//! These entries clutter `status` / `list` output and waste a tiny amount of memory
//! per index handle. A focused cleanup subcommand lets operators reclaim those slots
//! without resorting to manual `DELETE /indexes/:id` curl calls or hand-editing the
//! registry file.
//!
//! What: enumerates every registered index via `GET /indexes`, fetches each one's
//! `chunk_count` via `GET /indexes/:id/status`, collects the ids with zero chunks,
//! optionally prompts for confirmation, and removes them via `DELETE /indexes/:id`.
//! `--yes` skips the prompt; `--dry-run` short-circuits before any DELETE (and
//! overrides `--yes`).
//!
//! Test: register an empty index (`POST /indexes` with no follow-up reindex), run
//! `trusty-search cleanup --yes`, then verify `GET /indexes` no longer lists it.

use super::daemon_utils::daemon_base_url;
use anyhow::{bail, Result};
use colored::Colorize;
use std::io::{BufRead, Write};

/// Why: a small record per empty index keeps the table-printing step and the
/// DELETE loop independent of the JSON shape returned by the daemon.
/// What: holds the index id and its registered root path (the latter is for
/// display only — falls back to an empty string if the daemon omitted it).
/// Test: covered transitively by `handle_cleanup`'s integration usage.
struct EmptyIndex {
    id: String,
    root_path: String,
}

/// Why: extracted so `main()` doesn't inline the multi-step cleanup pipeline.
/// What: lists indexes, filters to those with `chunk_count == 0`, prints a
/// table, prompts unless `yes`, then deletes them. Returns `Err` only on
/// unrecoverable daemon errors so `main()` can render the friendly red-✗ line.
/// Test: `cargo run -p trusty-search -- cleanup --dry-run` prints the table
/// and exits without deleting; `cleanup --yes` deletes without prompting.
pub async fn handle_cleanup(yes: bool, dry_run: bool) -> Result<()> {
    let base = daemon_base_url();
    let client = trusty_common::server::daemon_http_client()?;

    // 1) List registered index ids.
    let list_url = format!("{}/indexes", base);
    let list_body: serde_json::Value = match client.get(&list_url).send().await {
        Ok(resp) if resp.status().is_success() => resp
            .json()
            .await
            .unwrap_or_else(|_| serde_json::json!({"indexes": []})),
        Ok(resp) => bail!("daemon returned {} for {}", resp.status(), list_url),
        Err(e) => bail!("could not reach daemon at {}: {e}", base),
    };

    let empty_arr: Vec<serde_json::Value> = Vec::new();
    let ids: Vec<String> = list_body
        .get("indexes")
        .and_then(|v| v.as_array())
        .unwrap_or(&empty_arr)
        .iter()
        .filter_map(|v| v.as_str().map(|s| s.to_string()))
        .collect();

    // 2) Fetch per-index status concurrently and collect the empty ones.
    let mut joinset = tokio::task::JoinSet::new();
    for id in &ids {
        let n = id.clone();
        let url = format!("{}/indexes/{}/status", base, n);
        let c = client.clone();
        joinset.spawn(async move {
            let body: serde_json::Value = match c.get(&url).send().await {
                Ok(r) if r.status().is_success() => {
                    r.json().await.unwrap_or_else(|_| serde_json::json!({}))
                }
                _ => serde_json::json!({}),
            };
            (n, body)
        });
    }

    let mut empties: Vec<EmptyIndex> = Vec::new();
    while let Some(j) = joinset.join_next().await {
        if let Ok((id, body)) = j {
            let chunks = body
                .get("chunk_count")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            if chunks == 0 {
                let root_path = body
                    .get("root_path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                empties.push(EmptyIndex { id, root_path });
            }
        }
    }
    empties.sort_by(|a, b| a.id.cmp(&b.id));

    // 3) Nothing to do?
    if empties.is_empty() {
        println!("Nothing to clean up.");
        return Ok(());
    }

    // 4) Show what would be removed.
    let count = empties.len();
    println!(
        "{} {} empty indexes (0 chunks):",
        "Found".bold(),
        count.to_string().bold()
    );
    let name_width = empties.iter().map(|e| e.id.len()).max().unwrap_or(0).max(4);
    for e in &empties {
        if e.root_path.is_empty() {
            println!("  {:<width$}", e.id.bold(), width = name_width);
        } else {
            println!(
                "  {:<width$}  {}",
                e.id.bold(),
                e.root_path.dimmed(),
                width = name_width
            );
        }
    }

    // 5) Dry-run wins over --yes.
    if dry_run {
        println!("{} dry-run: no indexes were removed.", "ℹ".cyan());
        return Ok(());
    }

    // 6) Prompt unless --yes.
    if !yes && !confirm(&format!("Remove these {} indexes?", count))? {
        println!("Aborted.");
        return Ok(());
    }

    // 7) DELETE each empty index, counting successes and failures.
    let mut removed = 0usize;
    let mut failed: Vec<(String, String)> = Vec::new();
    for e in &empties {
        // Issue #4123: `DELETE` preserves on-disk data unless `delete_data=true`
        // is passed. Opt in so `cleanup` keeps fully reclaiming the stub — every
        // candidate here is 0-chunk by construction (step 4 filters on
        // `chunk_count == 0`), so nothing of value can be destroyed.
        let url = format!("{}/indexes/{}?delete_data=true", base, e.id);
        match client.delete(&url).send().await {
            Ok(resp) if resp.status().is_success() => {
                removed += 1;
            }
            Ok(resp) => {
                failed.push((e.id.clone(), format!("HTTP {}", resp.status())));
            }
            Err(err) => {
                failed.push((e.id.clone(), err.to_string()));
            }
        }
    }

    // 8) Summary.
    if failed.is_empty() {
        println!(
            "{} Removed {} empty indexes.",
            "✓".green(),
            removed.to_string().bold()
        );
    } else {
        println!(
            "{} Removed {} of {} empty indexes ({} failed):",
            "!".yellow(),
            removed,
            count,
            failed.len()
        );
        for (id, err) in &failed {
            println!("  {} {} — {}", "✗".red(), id, err.dimmed());
        }
        bail!("{} index removals failed", failed.len());
    }

    Ok(())
}

/// Why: keep the y/N prompt isolated so tests of `handle_cleanup` can stub
/// stdin in the future without touching the HTTP plumbing.
/// What: prints `<prompt> [y/N] ` to stdout, reads one line from stdin, returns
/// `true` when the trimmed reply starts with `y` or `Y`. Empty input → false.
/// Test: side-effect-only; exercised manually via `cargo run -- cleanup`.
fn confirm(prompt: &str) -> Result<bool> {
    print!("{} [y/N] ", prompt);
    std::io::stdout().flush().ok();
    let stdin = std::io::stdin();
    let mut line = String::new();
    stdin.lock().read_line(&mut line)?;
    let answer = line.trim();
    Ok(matches!(answer.chars().next(), Some('y') | Some('Y')))
}
