//! The "drawer table is authoritative" marker (#9722).
//!
//! Why: palace open gap-fills the drawer table from `l1_cache.json` so a
//! palace from before drawer persistence (#32) keeps its drawers. A stale
//! second handle can write a forgotten drawer back into that snapshot, and the
//! gap-fill then revives it. Once the snapshot's drawers are durable in
//! [`DRAWERS`], the gap-fill has nothing left to do and only revives.
//! What: one `kg_schema` row, [`KG_SCHEMA_DRAWERS_AUTHORITATIVE`]. It is
//! written in the same transaction that adopts the gap-filled rows, so a
//! stamped palace never depends on the snapshot for a drawer.
//! Test: `retrieval::l1_live_tests`.

use super::drawer_ops::batch_upsert_drawer;
use super::store::KgStoreRedb;
use crate::memory_core::palace::Drawer;
use crate::memory_core::store::kg_store::{
    DRAWERS, DRAWERS_BY_FACT_KEY, KG_SCHEMA, KgSchemaMarker, encode_value,
};
use anyhow::{Context, Result};
use redb::ReadableDatabase;

/// The `kg_schema` key whose presence retires the L1 snapshot gap-fill.
pub const KG_SCHEMA_DRAWERS_AUTHORITATIVE: &str = "drawers_authoritative";

impl KgStoreRedb {
    /// Whether [`KG_SCHEMA_DRAWERS_AUTHORITATIVE`] is stamped.
    ///
    /// Why/What: see the module doc. Presence alone counts; a missing
    /// `kg_schema` table reads as unstamped.
    /// Test: `a_stale_second_handle_does_not_revive_a_forgotten_drawer_on_reopen`.
    pub fn drawers_authoritative(&self) -> Result<bool> {
        let rtx = self
            .db()
            .begin_read()
            .context("begin drawers_authoritative read")?;
        match rtx.open_table(KG_SCHEMA) {
            Ok(t) => Ok(t
                .get(KG_SCHEMA_DRAWERS_AUTHORITATIVE)
                .context("read drawers_authoritative")?
                .is_some()),
            Err(redb::TableError::TableDoesNotExist(_)) => Ok(false),
            Err(e) => Err(e).context("open kg_schema"),
        }
    }

    /// Persist `drawers` into [`DRAWERS`] and stamp the marker, atomically.
    ///
    /// Why: completes the #32 migration durably, so later opens can skip the
    /// gap-fill without losing a pre-persistence drawer.
    /// What: one write transaction; any error aborts it, so the marker is
    /// never stamped without every row. `may_commit` runs once the write lock
    /// is held; `false` aborts with nothing written, so a caller that has
    /// given up on this write can stop it landing behind a later writer.
    /// Test: `a_legacy_snapshot_drawer_is_adopted_and_kept_when_adoption_is_skipped`,
    /// `an_adoption_that_overran_its_budget_never_commits`.
    pub fn adopt_snapshot_drawers(
        &self,
        drawers: &[Drawer],
        may_commit: impl FnOnce() -> bool,
    ) -> Result<()> {
        self.check_writable()?;
        let gw = self
            .begin_write_guarded()
            .context("begin adopt_snapshot_drawers txn")?;
        if !may_commit() {
            // #9722: dropping `gw` aborts the transaction.
            anyhow::bail!("adopt_snapshot_drawers cancelled: the open's budget ran out");
        }
        {
            let wtx = &gw.txn;
            let mut table = wtx.open_table(DRAWERS).context("open drawers table")?;
            let mut by_fact_key = wtx
                .open_table(DRAWERS_BY_FACT_KEY)
                .context("open drawers_by_fact_key table")?;
            for d in drawers {
                batch_upsert_drawer(&mut table, &mut by_fact_key, d)?;
            }
            let mut schema = wtx.open_table(KG_SCHEMA).context("open kg_schema")?;
            let bytes = encode_value(&KgSchemaMarker { schema_version: 1 })
                .context("encode drawers_authoritative marker")?;
            schema
                .insert(KG_SCHEMA_DRAWERS_AUTHORITATIVE, bytes.as_slice())
                .context("stamp drawers_authoritative")?;
        }
        gw.commit().context("commit adopt_snapshot_drawers txn")?;
        Ok(())
    }
}
