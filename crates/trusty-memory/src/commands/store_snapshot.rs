//! Read a palace's KG store through a private copy, never the live file.
//!
//! Why: two read-only reports — `backfill-report` (#4891) and `audit secrets`
//! (#8645) — must read the drawer table without any chance of writing to the
//! palace. Opening the live `kg.redb` cannot promise that:
//! `OpenIntent::ReadOnlyClient` only snapshots when the file is *already*
//! locked, and on an unlocked palace it reaches `Database::create`, which runs a
//! table-init write transaction and renames an incompatible store aside
//! (`concurrent_open.rs`, #702). `PalaceHandle::open` is worse: its #61 sweep
//! deletes expired drawers. So both reports copy the file and open the copy.
//! What: [`with_store_copy`] copies `<data_dir>/kg.redb` into a fresh private
//! temp dir under a caller-chosen parent, opens the copy, hands the store to a
//! closure, and deletes the copy on return. It never opens, locks or renames
//! the live file, so it is safe while the daemon holds the palace open.
//! Test: `report_writes_nothing_to_the_palace`,
//! `incompatible_store_is_reported_not_recreated`,
//! `scan_leaves_palace_files_byte_identical_under_a_live_writer`.

use std::path::Path;

use anyhow::{Context, Result};
use trusty_common::memory_core::store::kg_redb::KgStoreRedb;
use trusty_common::memory_core::store::OpenIntent;

/// Filename of a palace's KG store.
pub(crate) const KG_FILE: &str = "kg.redb";

/// Suffix `redb_open::backup_incompatible_file` gives a store it moves aside.
const INCOMPATIBLE_SUFFIX: &str = ".v2-incompatible";

/// Open a private copy of a palace's KG store and run `read` against it.
///
/// Why: see the module doc — this is the one place the read-only guarantee is
/// made, and it is made by never handing the palace's own file to redb.
/// What: returns `Ok(None)` when the palace has no `kg.redb` (an empty palace,
/// not a failure). Otherwise copies the store into a `0700` temp dir created
/// under `scratch_parent`, opens the copy `ReadOnlyClient`, fails if redb had
/// to set the copy aside as an incompatible format (reading would have meant
/// recreating it), and returns `read`'s result. The temp dir, the copy and
/// anything redb wrote beside it are deleted when this returns.
///
/// A copy taken while the daemon commits can be torn. redb then either
/// recovers the copy to an earlier commit or fails to open it; the second case
/// surfaces as an error for that palace, never as a write to the live file.
/// Test: `report_writes_nothing_to_the_palace`,
/// `incompatible_store_is_reported_not_recreated`,
/// `scan_leaves_palace_files_byte_identical_under_a_live_writer`.
pub(crate) fn with_store_copy<T>(
    data_dir: &Path,
    scratch_parent: &Path,
    read: impl FnOnce(KgStoreRedb) -> Result<T>,
) -> Result<Option<T>> {
    let live = data_dir.join(KG_FILE);
    if !live.exists() {
        return Ok(None);
    }
    let scratch = tempfile::TempDir::with_prefix_in("trusty-memory-readonly-", scratch_parent)
        .context("create scratch dir for read-only palace copy")?;
    let copy = scratch.path().join(KG_FILE);
    std::fs::copy(&live, &copy)
        .with_context(|| format!("copy {} for read-only inspection", live.display()))?;

    let store = KgStoreRedb::open_with_intent(&copy, OpenIntent::ReadOnlyClient)
        .with_context(|| format!("open copy of KG store {}", live.display()))?;
    if scratch
        .path()
        .join(format!("{KG_FILE}{INCOMPATIBLE_SUFFIX}"))
        .exists()
    {
        anyhow::bail!(
            "KG store at {} is in an incompatible redb format — reading it would have \
             required recreating it, which a read-only report never does. Rebuild the palace.",
            live.display()
        );
    }
    read(store).map(Some)
}
