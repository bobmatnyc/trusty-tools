//! Single-file and directory `add` indexing helpers.
//!
//! Why: `add_path` and the doctor auto-repair path need to push individual
//! files to the daemon without driving a full reindex pipeline; co-locating
//! the per-file call here keeps the reindex driver focused on the event loop.
//! What: `index_single_file` sends one file as `search.index.file.put` over the
//! daemon socket (#9214); `add_path` fans a directory out into per-file
//! `index_single_file` calls (or indexes a single file directly).
//! Test: `a_single_file_is_sent_as_file_put`.

use crate::commands::daemon_rpc::call;
use anyhow::Result;
use colored::Colorize;
use trusty_search::service::daemon_client::DaemonClient;
use trusty_search::service::rpc::writes::METHOD_INDEX_FILE_PUT;

/// Index a single file via the daemon's `search.index.file.put` method.
///
/// Why: factored out of `main.rs` so `add_path` and other callers can reuse
/// the single-file indexing path without duplicating the call.
/// What: reads the file from disk, sends its content to the daemon, and
/// returns an error when the daemon refuses or cannot be reached.
/// Test: `a_single_file_is_sent_as_file_put`.
pub async fn index_single_file(
    client: &DaemonClient,
    index_id: &str,
    file: &std::path::Path,
) -> Result<()> {
    let content = tokio::fs::read_to_string(file)
        .await
        .map_err(|e| anyhow::anyhow!("read {}: {e}", file.display()))?;
    let params = serde_json::json!({
        "index_id": index_id,
        "body": { "path": file.display().to_string(), "content": content },
    });
    call(client, METHOD_INDEX_FILE_PUT, params).await?;
    Ok(())
}

/// Handle `trusty-search add <path>`: a single file goes to `index-file`;
/// a directory walks `walk_source_files` and indexes every match.
///
/// Why: the `add` subcommand is a convenience wrapper for one-off file
/// indexing without a full reindex. A directory path fans out into per-file
/// `index_single_file` calls rather than a full reindex pipeline.
/// What: calls `index_single_file` for a file path; walks + indexes every
/// source file under a directory path. The caller has already started the
/// daemon; this resolves its socket.
/// Test: covered indirectly by the `add` command integration tests.
pub async fn add_path(index_id: &str, path: &std::path::Path) -> Result<()> {
    let client = DaemonClient::resolve()?;

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
            match index_single_file(&client, index_id, f).await {
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
        index_single_file(&client, index_id, path).await?;
        println!("{} [{}] {}", "\u{2192}".cyan(), index_id, path.display());
        Ok(())
    }
}
