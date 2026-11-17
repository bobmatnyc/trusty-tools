//! The index identity's two `_meta` keys in the corpus (#10738).
//!
//! Why: the identity file lives under `<data_dir>/index-identity/`, which can
//! be unwritable while the corpus is not. A bump that fails there left the
//! pre-event file behind, and a restart read it back (SPEC-OKF-01 §5.6 D1).
//! The corpus records what it was attached under, so an attach can tell.
//! What: `identity_stamp` holds the identity (JSON) the corpus was attached
//! under, written only after a bump or attach succeeds;
//! `identity_bump_pending` is set before a bump for an event that keeps the
//! corpus, and removed with the stamp write once the bump succeeds.
//! Test: `a_corpus_loss_bump_that_failed_is_retried_after_a_restart`,
//! `a_pending_bump_that_failed_is_retried_after_a_restart`.

use anyhow::{Context, Result};
use redb::ReadableDatabase;

use super::store_impl::CorpusStore;
use crate::core::migration::META_TABLE;

/// The identity this corpus was attached under, as JSON.
pub(crate) const META_KEY_IDENTITY_STAMP: &str = "identity_stamp";
/// Present while a bump for an event that keeps the corpus has not succeeded.
pub(crate) const META_KEY_IDENTITY_BUMP_PENDING: &str = "identity_bump_pending";

impl CorpusStore {
    /// The stored stamp bytes, and whether a bump is pending.
    pub(crate) fn read_identity_meta_sync(&self) -> Result<(Option<Vec<u8>>, bool)> {
        let txn = self.db.begin_read().context("begin _meta read txn")?;
        let table = match txn.open_table(META_TABLE) {
            Ok(t) => t,
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok((None, false)),
            Err(e) => return Err(anyhow::anyhow!("open _meta table: {e}")),
        };
        let stamp = table
            .get(META_KEY_IDENTITY_STAMP)
            .context("read _meta[identity_stamp]")?
            .map(|v| v.value().to_vec());
        let pending = table
            .get(META_KEY_IDENTITY_BUMP_PENDING)
            .context("read _meta[identity_bump_pending]")?
            .is_some();
        Ok((stamp, pending))
    }

    /// Record, durably, that a bump for `reason` has started.
    pub(crate) fn mark_identity_bump_pending_sync(&self, reason: &str) -> Result<()> {
        let txn = self.db.begin_write().context("begin _meta write txn")?;
        {
            let mut table = txn.open_table(META_TABLE).context("open _meta table")?;
            table
                .insert(META_KEY_IDENTITY_BUMP_PENDING, reason.as_bytes())
                .context("write _meta[identity_bump_pending]")?;
        }
        txn.commit().context("commit _meta write txn")
    }

    /// Write `stamp` and clear any pending bump, in one transaction.
    pub(crate) fn write_identity_stamp_sync(&self, stamp: &[u8]) -> Result<()> {
        let txn = self.db.begin_write().context("begin _meta write txn")?;
        {
            let mut table = txn.open_table(META_TABLE).context("open _meta table")?;
            table
                .insert(META_KEY_IDENTITY_STAMP, stamp)
                .context("write _meta[identity_stamp]")?;
            table
                .remove(META_KEY_IDENTITY_BUMP_PENDING)
                .context("clear _meta[identity_bump_pending]")?;
        }
        txn.commit().context("commit _meta write txn")
    }
}
