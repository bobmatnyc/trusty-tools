//! Single-file and directory `add` indexing helpers.
//!
//! Why: `add_path` and the doctor auto-repair path need to push individual
//! files to the daemon without driving a full reindex pipeline; co-locating
//! the HTTP dance here keeps the reindex driver focused on the SSE loop.
//! What: `index_single_file` POSTs one file; `add_path` fans a directory out
//! into per-file `index_single_file` calls (or indexes a single file directly).
//! Test: covered indirectly by the `add` command integration tests.

use crate::commands::daemon_utils::daemon_base_url;
use anyhow::Result;
use colored::Colorize;

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
