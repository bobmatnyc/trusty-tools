//! Handler for `trusty-search add <file>`.

use super::index_resolve::{print_index_header, resolve_index};
use super::reindex_engine::add_path;
use anyhow::Result;
use trusty_search::service::daemon_client::DaemonClient;

/// Why: thin wrapper so `main()` doesn't need to know about `add_path` (which
/// handles both single-file and directory walks). Auto-starts the daemon if
/// it isn't running — `add` always talks to it.
/// What: resolves the active index, prints header, ensures the daemon is up
/// (#9214: on its socket), delegates to `add_path`.
/// Test: `cargo run -- add src/main.rs` POSTs to `/indexes/<id>/index-file`.
pub async fn handle_add(explicit_index: &Option<String>, file: std::path::PathBuf) -> Result<()> {
    let (index_id, warned) = resolve_index(explicit_index)?;
    print_index_header(&index_id, warned);
    // #9214: wait on the socket; `add_path` resolves its own transport.
    crate::commands::daemon_guard::ensure_daemon_up(&DaemonClient::resolve()?).await?;
    add_path(&index_id, &file).await
}
