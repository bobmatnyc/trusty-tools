//! The reindex-commit stamp in a corpus's `_meta` table (#9169).
//!
//! Why: `search.project.resolve` ranks a repo's indexes by "most recently
//! indexed". The `index.redb` mtime cannot answer that, because redb rewrites
//! the file each time it is opened, so a cold-loaded stale index looked newer
//! than a freshly reindexed one. Any committed write stamps it: a full
//! reindex, and an incremental write or delete (#9230).
//! What: read and write `META_KEY_REINDEXED_UNIX` through a held
//! [`CorpusStore`], plus [`read_reindexed_unix_at`], which reads a corpus this
//! process does not hold through a read-only redb open.
//! Test: `the_stamp_round_trips_and_a_staging_copy_carries_it`,
//! `a_read_only_read_leaves_the_corpus_file_untouched`.

use std::path::Path;

use anyhow::{anyhow, Context, Result};
use redb::ReadableDatabase;

use super::store_impl::CorpusStore;
use crate::core::migration::{META_KEY_REINDEXED_UNIX, META_TABLE};

/// Page cache for a read-only stamp read: one `_meta` lookup needs a few pages.
const READ_ONLY_CACHE_BYTES: usize = 1 << 20;

impl CorpusStore {
    /// When a write last committed this corpus; `None` when never stamped.
    pub(crate) fn read_reindexed_unix_sync(&self) -> Result<Option<u64>> {
        read_stamp(&self.db)
    }

    /// Record that a write committed this corpus at `unix` seconds: a full
    /// reindex (#9169) or any committed incremental write or delete (#9230).
    ///
    /// Why: the resolver's recency must change only when a commit lands.
    /// What: one write transaction upserting the 8-byte little-endian value.
    /// Production writes go through [`Self::write_reindexed_now_sync`].
    /// Test: `the_stamp_round_trips_and_a_staging_copy_carries_it`.
    pub(crate) fn write_reindexed_unix_sync(&self, unix: u64) -> Result<()> {
        let txn = self.db.begin_write().context("begin _meta write txn")?;
        {
            let mut table = txn.open_table(META_TABLE).context("open _meta table")?;
            table
                .insert(META_KEY_REINDEXED_UNIX, unix.to_le_bytes().as_slice())
                .context("insert reindexed_unix")?;
        }
        txn.commit().context("commit _meta write txn")?;
        Ok(())
    }

    /// Stamp the current unix time; returns the value written.
    ///
    /// Why: a full reindex commit (#9169) and an incremental commit (#9230)
    /// both stamp "now"; a clock before the epoch is an error, not a 0 stamp.
    /// What: reads `SystemTime::now()`, then [`Self::write_reindexed_unix_sync`].
    /// Test: `index_file_stamps_the_corpus_it_commits`.
    pub(crate) fn write_reindexed_now_sync(&self) -> Result<u64> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .context("system clock is before the unix epoch")?
            .as_secs();
        self.write_reindexed_unix_sync(now)?;
        Ok(now)
    }
}

/// Read the reindex stamp of a corpus file this process does not hold open.
///
/// Why: the resolver reads cold-parked indexes too, and must not change what
/// it reads next time. A read-write open rewrites the redb header and so the
/// file's mtime.
/// What: `redb::Builder::open_read_only` opens the file read-only under a
/// shared lock, which fails with `DatabaseAlreadyOpen` while any writer holds
/// it. The database is dropped before returning, so no handle outlives the
/// call. Callers run it inside `open_serialized` so a daemon load of the same
/// file waits for it instead of failing.
/// Test: `a_read_only_read_leaves_the_corpus_file_untouched`.
pub(crate) fn read_reindexed_unix_at(path: &Path) -> Result<Option<u64>> {
    let db = redb::Builder::new()
        .set_cache_size(READ_ONLY_CACHE_BYTES)
        .open_read_only(path)
        .with_context(|| format!("open {} read-only", path.display()))?;
    read_stamp(&db)
}

/// Decode the stamp from any open database; `Ok(None)` when absent.
fn read_stamp(db: &impl ReadableDatabase) -> Result<Option<u64>> {
    let txn = db.begin_read().context("begin _meta read txn")?;
    let table = match txn.open_table(META_TABLE) {
        Ok(t) => t,
        Err(redb::TableError::TableDoesNotExist(_)) => return Ok(None),
        Err(e) => return Err(anyhow!("open _meta table: {e}")),
    };
    let Some(value) = table
        .get(META_KEY_REINDEXED_UNIX)
        .context("read reindexed_unix")?
    else {
        return Ok(None);
    };
    let bytes: [u8; 8] = value
        .value()
        .try_into()
        .map_err(|_| anyhow!("reindexed_unix is {} bytes, not 8", value.value().len()))?;
    Ok(Some(u64::from_le_bytes(bytes)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: #9169 — the stamp must survive a reopen and an incremental
    /// staging copy, so a resident index mid-reindex still has its stamp.
    /// Test: this test.
    #[test]
    fn the_stamp_round_trips_and_a_staging_copy_carries_it() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let live_path = tmp.path().join("index.redb");
        let live = CorpusStore::open(&live_path).expect("open");
        assert_eq!(live.read_reindexed_unix_sync().expect("read"), None);
        live.write_reindexed_unix_sync(1_700_000_000)
            .expect("write");
        let staging = CorpusStore::open_fresh(&tmp.path().join("staging.redb")).expect("staging");
        staging.copy_all_from(&live).expect("copy");
        assert_eq!(
            staging.read_reindexed_unix_sync().expect("read"),
            Some(1_700_000_000)
        );
        drop(live);
        let reopened = CorpusStore::open(&live_path).expect("reopen");
        assert_eq!(
            reopened.read_reindexed_unix_sync().expect("read"),
            Some(1_700_000_000)
        );
    }

    /// Why: #9169 — reading the stamp of a cold corpus must not change the
    /// file, or the read itself would refresh the mtime fallback; and it must
    /// refuse, not wait, while a writer holds the file.
    /// Test: this test.
    #[test]
    fn a_read_only_read_leaves_the_corpus_file_untouched() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("index.redb");
        let store = CorpusStore::open(&path).expect("open");
        store.write_reindexed_unix_sync(42).expect("write");
        assert!(read_reindexed_unix_at(&path).is_err(), "a writer holds it");
        drop(store);
        let before = std::fs::read(&path).expect("bytes");
        let modified = std::fs::metadata(&path).and_then(|m| m.modified());
        assert_eq!(read_reindexed_unix_at(&path).expect("read"), Some(42));
        assert_eq!(std::fs::read(&path).expect("bytes"), before);
        assert_eq!(
            std::fs::metadata(&path).and_then(|m| m.modified()).ok(),
            modified.ok()
        );
    }
}
