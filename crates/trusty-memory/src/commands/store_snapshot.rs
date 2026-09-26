//! Read a palace's KG store through a private copy, never the live file.
//!
//! Why: two read-only reports — `backfill-report` (#4891) and `audit secrets`
//! (#8645) — must read the drawer table without any chance of writing to the
//! palace. Opening the live `kg.redb` cannot promise that:
//! `OpenIntent::ReadOnlyClient` only snapshots when the file is *already*
//! locked, and on an unlocked palace it reaches `Database::create`, which runs a
//! table-init write transaction (`concurrent_open.rs`, #702). `PalaceHandle::open`
//! is worse: its #61 sweep deletes expired drawers. So both reports copy the
//! file and open the copy.
//! What: [`with_store_copy`] copies `<data_dir>/kg.redb` into a fresh private
//! temp dir under a caller-chosen parent, opens the copy, hands the store to a
//! closure, and deletes the copy on return. It never opens, locks or renames
//! the live file, so it is safe while the daemon holds the palace open.
//! [`sweep_stale_copies`] removes copies an interrupted run left behind.
//! Test: `report_writes_nothing_to_the_palace`,
//! `incompatible_store_is_reported_not_recreated`,
//! `scan_leaves_palace_files_byte_identical_under_a_live_writer`,
//! `sweep_removes_only_stale_scratch_dirs`.

use std::panic::AssertUnwindSafe;
use std::path::Path;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use trusty_common::memory_core::store::kg_redb::KgStoreRedb;
use trusty_common::memory_core::store::OpenIntent;

/// Filename of a palace's KG store.
pub(crate) const KG_FILE: &str = "kg.redb";

/// Name prefix of every scratch dir holding a store copy.
pub(crate) const SCRATCH_PREFIX: &str = "trusty-memory-readonly-";

/// Age past which a scratch dir is treated as abandoned by a dead run.
pub(crate) const STALE_COPY_AGE: Duration = Duration::from_secs(60 * 60);

/// Open a private copy of a palace's KG store and run `read` against it.
///
/// Why: see the module doc — this is the one place the read-only guarantee is
/// made, and it is made by never handing the palace's own file to redb.
/// What: returns `Ok(None)` only when `kg.redb` is genuinely absent (an empty
/// palace). A stat that fails for any other reason — permission denied, an
/// I/O error — is an `Err`, never "no store" (#8645). Otherwise copies the
/// store into a `0700` temp dir created under `scratch_parent`, opens the copy
/// `ReadOnlyClient`, and returns `read`'s result. An incompatible-format copy
/// fails the open: `ReadOnlyClient` refuses it rather than recreating it
/// (#4911). The temp dir, the copy and anything redb wrote beside it are
/// deleted when this returns.
///
/// A copy taken while the daemon commits can be torn. redb then either
/// recovers the copy to an earlier commit or fails — or panics — opening or
/// reading it; the latter two surface as an error for that palace, never as a
/// write to the live file or an aborted scan. A panic in `read` itself is
/// caught the same way, so the panic error names both possible causes.
/// Test: `report_writes_nothing_to_the_palace`,
/// `incompatible_store_is_reported_not_recreated`,
/// `unstattable_palace_dir_is_an_error_row_not_an_absent_store`,
/// `truncated_store_copy_is_an_error_row`,
/// `read_closure_panic_is_not_reported_as_a_torn_store`.
pub(crate) fn with_store_copy<T>(
    data_dir: &Path,
    scratch_parent: &Path,
    read: impl FnOnce(KgStoreRedb) -> Result<T>,
) -> Result<Option<T>> {
    let live = data_dir.join(KG_FILE);
    // #8645: `exists()` reads a denied stat as "absent", which a scan would
    // then report as an empty palace. The io error names no stored bytes.
    let present = live
        .try_exists()
        .map_err(|e| anyhow::anyhow!("cannot stat {}: {e}", live.display()))?;
    if !present {
        return Ok(None);
    }
    let scratch = tempfile::TempDir::with_prefix_in(SCRATCH_PREFIX, scratch_parent)
        .context("create scratch dir for read-only palace copy")?;
    let copy = scratch.path().join(KG_FILE);
    std::fs::copy(&live, &copy)
        .with_context(|| format!("copy {} for read-only inspection", live.display()))?;

    // #8645: redb 4.1 `assert!`s that the file is at least as long as its
    // header's layout (page_manager.rs), so a truncated copy panics instead of
    // erroring. Contain that to this palace. `trusty_common::panic_hook` has
    // already logged the payload (redb's assert text names no stored bytes);
    // this function discards it.
    let opened = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let store = KgStoreRedb::open_with_intent(&copy, OpenIntent::ReadOnlyClient)
            .with_context(|| format!("open copy of KG store {}", live.display()))?;
        read(store)
    }));
    match opened {
        Ok(result) => result.map(Some),
        // #8645: a panic here is either redb on a torn copy or a bug in the
        // reader; nothing tells them apart, so the message claims neither.
        Err(_) => anyhow::bail!(
            "panic while opening or reading the copy of KG store {} \
             (torn copy or internal error)",
            live.display()
        ),
    }
}

/// Delete scratch dirs under `scratch_parent` older than `max_age`.
///
/// Why: #8645 — a copy holds a palace's drawers in plaintext. A run killed
/// before its `TempDir` drops (SIGKILL, a closed terminal) leaves that copy in
/// `$TMPDIR` indefinitely; the next run removes it.
/// What: removes every real directory (never a symlink) whose name starts with
/// [`SCRATCH_PREFIX`] and whose mtime is at least `max_age` old. A dir that
/// cannot be inspected or removed is logged at warn and skipped, so a sweep
/// failure never blocks the scan. Returns how many dirs were removed.
/// Test: `sweep_removes_only_stale_scratch_dirs`.
pub(crate) fn sweep_stale_copies(scratch_parent: &Path, max_age: Duration) -> usize {
    let entries = match std::fs::read_dir(scratch_parent) {
        Ok(entries) => entries,
        Err(e) => {
            tracing::warn!(dir = %scratch_parent.display(), "cannot sweep stale store copies: {e}");
            return 0;
        }
    };
    let now = SystemTime::now();
    let mut removed = 0;
    for entry in entries.flatten() {
        if !entry
            .file_name()
            .to_string_lossy()
            .starts_with(SCRATCH_PREFIX)
        {
            continue;
        }
        let path = entry.path();
        let stale = std::fs::symlink_metadata(&path).is_ok_and(|m| {
            m.is_dir()
                && m.modified()
                    .is_ok_and(|t| now.duration_since(t).is_ok_and(|age| age >= max_age))
        });
        if !stale {
            continue;
        }
        match std::fs::remove_dir_all(&path) {
            Ok(()) => removed += 1,
            Err(e) => {
                tracing::warn!(dir = %path.display(), "cannot remove stale store copy: {e}");
            }
        }
    }
    removed
}
