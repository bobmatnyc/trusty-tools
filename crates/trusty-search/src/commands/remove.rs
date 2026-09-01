//! Handler for `trusty-search remove <file>`.

use super::daemon_utils::daemon_base_url;
use super::index_resolve::{print_index_header, resolve_index};
use anyhow::{bail, Result};
use colored::Colorize;

/// Why: extracted so `main()` doesn't have to inline the daemon HTTP plumbing.
/// What: POST `/indexes/<id>/remove-file` with the file path; returns `Err`
/// when the daemon is unreachable or returns non-2xx so `main()` can print the
/// friendly ✗ line and exit (issue #104).
/// Test: `cargo run -- remove src/old.rs` removes the file from the index.
pub async fn handle_remove(
    explicit_index: &Option<String>,
    file: std::path::PathBuf,
) -> Result<()> {
    let (index_id, warned) = resolve_index(explicit_index)?;
    print_index_header(&index_id, warned);
    let base = daemon_base_url();
    crate::commands::daemon_guard::ensure_daemon_running_or_exit(&base).await?;
    let url = format!("{}/indexes/{}/remove-file", base, index_id);
    let client = trusty_common::server::daemon_http_client()?;
    let body = serde_json::json!({ "path": file.display().to_string() });
    match client.post(&url).json(&body).send().await {
        Ok(resp) if resp.status().is_success() => {
            println!("{} [{}] removed {}", "−".red(), index_id, file.display());
        }
        Ok(resp) => bail!("daemon returned {} for {}", resp.status(), url),
        Err(e) => bail!("could not reach daemon at {}: {e}", base),
    }
    Ok(())
}
