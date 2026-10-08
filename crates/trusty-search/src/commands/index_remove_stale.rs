//! `index remove <PATH>` for a PATH the daemon no longer registers (#8687).
//!
//! Why: an index deleted through another surface (MCP `delete_index`) left its
//! `allowlist.toml` row behind, and `index remove <PATH>` aborted with "no
//! index registered" before the cleanup that would clear it.
//! What: [`registered_or_cleared`] resolves PATH through the shared,
//! parked-aware [`lookup_index_by_path`], and — only when every registration
//! was read and none owns PATH — clears PATH's stale rows via
//! [`clear_stale_path_rows`]. An unreadable registration still refuses.
//! Test: `tests/index_remove_residency_8687.rs`.

use std::path::{Path, PathBuf};

use anyhow::{bail, Result};
use colored::Colorize;

use super::explicit_target::lookup_index_by_path;
use crate::config::GlobalConfig;
use trusty_search::service::daemon_client::DaemonClient;

/// Resolve PATH to its registration, or — when the daemon no longer registers
/// it — clear its stale allowlist and config rows and stop (#8687).
///
/// What: a registration (resident or parked) returns `Some((id, root))`. None
/// clears PATH's rows and returns `None` — the removal is complete — or bails
/// when there was nothing at all to remove.
/// Test: `removing_an_already_deleted_index_clears_its_stale_allowlist_row`,
/// `an_unreadable_status_refuses_instead_of_reporting_not_registered`.
pub(crate) async fn registered_or_cleared(
    client: &DaemonClient,
    path: &Path,
) -> Result<Option<(String, PathBuf)>> {
    // #9214: a lookup error (unreachable, unreadable list or status) returns
    // here, before any local row is touched.
    if let Some(found) = lookup_index_by_path(client, path).await? {
        return Ok(Some((found.id, found.root)));
    }
    if clear_stale_path_rows(path)? == 0 {
        bail!(
            "no index registered for path {}; run `trusty-search list` to see registered \
             indexes",
            path.display()
        );
    }
    println!(
        "{} No daemon registration for {} (already removed); cleared its stale allowlist/config rows",
        "✓".green(),
        path.display()
    );
    Ok(None)
}

/// Drop PATH's rows from the allowlist and the global config.
///
/// What: removes each file's row for PATH, saving only a file that changed.
/// Returns how many rows went; `0` means there was nothing to clean.
/// Test: `removing_an_already_deleted_index_clears_its_stale_allowlist_row`.
fn clear_stale_path_rows(path: &Path) -> Result<usize> {
    let mut cleared = 0;
    let allowlist_path = crate::allowlist::AllowlistConfig::default_path();
    let mut allowlist = crate::allowlist::AllowlistConfig::load_from(&allowlist_path)?;
    if allowlist.remove(path).is_some() {
        allowlist.save_to(&allowlist_path)?;
        cleared += 1;
    }
    let mut cfg = GlobalConfig::load()?;
    if cfg.remove_collection_by_path(path).is_some() {
        cfg.save()?;
        cleared += 1;
    }
    Ok(cleared)
}
