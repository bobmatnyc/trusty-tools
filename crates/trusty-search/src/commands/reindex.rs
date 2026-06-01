//! Handler for `trusty-search reindex` (bare reindex, no force/verify).

use super::daemon_utils::daemon_base_url;
use super::index_resolve::{print_index_header, resolve_index};
use super::reindex_engine::run_reindex_opts;
use crate::detect::detect_project;
use anyhow::Result;

/// Why: extracted from `main()`; behaviour unchanged.
/// What: resolves the active index, picks the path (CLI arg > detected
/// project root), then drives `run_reindex_opts` which renders the SSE
/// progress bar with the appropriate wait strategy.
/// Test: `cargo run -- reindex` from inside a registered project rebuilds it.
///
/// `timeout` is the user-supplied `--timeout` value: `None` means
/// progress-aware stall detection; `Some(n)` means hard cap at n seconds.
pub async fn handle_reindex(
    explicit_index: &Option<String>,
    path: Option<std::path::PathBuf>,
    timeout: Option<u64>,
) -> Result<()> {
    let (index_id, warned) = resolve_index(explicit_index);
    print_index_header(&index_id, warned);
    // Issue #24: prefer CPU EP for auto-spawned daemon (CoreML init OOMs the
    // indexing path on Apple Silicon). Already-running daemons are untouched.
    crate::commands::daemon_guard::ensure_daemon_running_for_indexing(&daemon_base_url()).await?;
    let reindex_path = path.unwrap_or_else(|| {
        let cwd = std::env::current_dir().unwrap_or_default();
        detect_project(&cwd).root_path
    });
    let (timeout_secs, timeout_explicit) = match timeout {
        Some(n) => (n, true),
        None => (0, false),
    };
    run_reindex_opts(&index_id, &reindex_path, timeout_secs, timeout_explicit).await
}
