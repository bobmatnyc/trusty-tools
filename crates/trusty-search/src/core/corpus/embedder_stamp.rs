//! The embedder-identity stamp in a corpus's `_meta` table (#10664).
//!
//! Why: the stamp must travel with the vectors it describes. The corpus sits
//! beside the HNSW snapshot in both storage layouts and is copied by an
//! off-box delivery, while `indexes.toml` is not.
//! What: `embedder_identity` holds an [`EmbedderIdentity`] as JSON. An
//! incremental reindex carries it over with the rest of `_meta`; a forced
//! reindex starts from a fresh corpus and re-stamps on completion. A key an
//! older binary does not know is never read by it, so the format is unchanged.
//! Test: `a_completed_reindex_stamps_the_embedder_identity`,
//! `an_unreadable_stamp_reports_unreadable_not_match`.

use anyhow::{Context, Result};
use redb::ReadableDatabase;

use super::store_impl::CorpusStore;
use crate::core::embedder_identity::{EmbedderIdentity, StampRead};
use crate::core::migration::META_TABLE;

/// `_meta` key of the embedder-identity stamp (#10664).
pub(crate) const META_KEY_EMBEDDER_IDENTITY: &str = "embedder_identity";

impl CorpusStore {
    /// Read the stamp; a read or decode failure is `Unreadable`, never `Absent`.
    pub(crate) fn read_embedder_identity_sync(&self) -> StampRead {
        match self.read_embedder_identity_bytes() {
            Ok(None) => StampRead::Absent,
            Ok(Some(bytes)) => match serde_json::from_slice::<EmbedderIdentity>(&bytes) {
                Ok(identity) => StampRead::Stamped(identity),
                Err(e) => {
                    StampRead::Unreadable(format!("decode {META_KEY_EMBEDDER_IDENTITY}: {e}"))
                }
            },
            Err(e) => StampRead::Unreadable(format!("{e:#}")),
        }
    }

    /// Write `identity` as the stamp, replacing any earlier one.
    pub(crate) fn write_embedder_identity_sync(&self, identity: &EmbedderIdentity) -> Result<()> {
        self.write_embedder_identity_bytes(
            &serde_json::to_vec(identity).context("encode embedder identity")?,
        )
    }

    /// Write raw stamp bytes; used by [`Self::write_embedder_identity_sync`]
    /// and by the tests that plant an undecodable stamp.
    pub(crate) fn write_embedder_identity_bytes(&self, bytes: &[u8]) -> Result<()> {
        let txn = self.db.begin_write().context("begin _meta write txn")?;
        {
            let mut table = txn.open_table(META_TABLE).context("open _meta table")?;
            table
                .insert(META_KEY_EMBEDDER_IDENTITY, bytes)
                .context("insert embedder_identity")?;
        }
        txn.commit().context("commit _meta write txn")?;
        Ok(())
    }

    fn read_embedder_identity_bytes(&self) -> Result<Option<Vec<u8>>> {
        let txn = self.db.begin_read().context("begin _meta read txn")?;
        let table = match txn.open_table(META_TABLE) {
            Ok(t) => t,
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(None),
            Err(e) => return Err(anyhow::anyhow!("open _meta table: {e}")),
        };
        let value = table
            .get(META_KEY_EMBEDDER_IDENTITY)
            .context("read embedder_identity")?;
        Ok(value.map(|v| v.value().to_vec()))
    }
}
