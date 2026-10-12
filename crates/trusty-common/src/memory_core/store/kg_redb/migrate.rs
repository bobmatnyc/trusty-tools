//! The `kg.redb` step of the palace format 1 → 2 migration (#9275).
//!
//! Why: format 2 permits `{uuid}#{n}` chunk rows in `index.usearch.redb`
//! (`vector_key.rs`). No table shape and no existing row changes, so the step
//! is a stamp: it fences the palace off from a 1.x binary that would misread
//! or rewrite chunk rows (ADR-0067 D2 bump rule). ADR-0067 D3 rule 2 migrates
//! N-1 only, so this binary carries one step; the 0 → 1 triple-key rewrite
//! (#9274) shipped in 0.33.x and a format-0 palace is refused `FormatTooOld`.
//! What: [`migrate_kg_to_format`] dispatches on the target format. The
//! 1 → 2 step reads both markers inside its write transaction, fails unless
//! the palace is at format 1 with triple key v1, stamps 2 and commits — the
//! commit point of the migration. A failure commits nothing (D3 rule 4).
//! Test: format_migration_v2_tests.rs —
//! `the_1_to_2_step_commits_nothing_when_the_stored_format_is_not_1`,
//! `a_format_1_palace_migrates_to_2_at_first_writer_open`.

use crate::memory_core::store::kg_store::{
    KG_SCHEMA, KG_SCHEMA_TRIPLE_KEY, KG_TRIPLE_KEY_SCHEMA_VERSION, KgSchemaMarker, decode_value,
};
use crate::memory_core::store::palace_format::{KG_SCHEMA_PALACE_FORMAT, stamp_format};
use anyhow::{Context, Result, bail};
use redb::{Database, ReadableTable};

/// Run the `kg.redb` step that moves a palace to format `to`.
///
/// Why: the caller holds a verified backup and the migration record; the next
/// format bump adds an arm here, never a chain (ADR-0067 D7).
/// What: `to == 2` runs the 1 → 2 stamp; any other target fails.
/// Test: `the_1_to_2_step_commits_nothing_when_the_stored_format_is_not_1`.
pub(crate) fn migrate_kg_to_format(db: &Database, to: u32) -> Result<()> {
    match to {
        2 => step_1_to_2(db),
        _ => bail!("this binary has no kg.redb step to format {to}"),
    }
}

/// Stamp format 2 over a format-1 KG, in one write transaction.
///
/// Why: fail closed (#9275, Fail-Open Check). The gate classified the palace
/// before the exclusive open, so a marker that is not 1 here means the file
/// changed under the migration; stamping over it would advance a palace this
/// step never read.
/// What: reads `palace_format` and the triple-key marker; anything but 1 and
/// v1 (absent, other, undecodable) fails before a byte is written.
/// Test: `the_1_to_2_step_commits_nothing_when_the_stored_format_is_not_1`,
/// `a_failed_step_does_not_advance_the_stored_format_or_remove_the_record`.
fn step_1_to_2(db: &Database) -> Result<()> {
    let wtx = db.begin_write().context("begin format-migration txn")?;
    {
        let schema = wtx
            .open_table(KG_SCHEMA)
            .context("open kg_schema for migration")?;
        let read = |key: &str| -> Result<Option<u32>> {
            schema
                .get(key)
                .with_context(|| format!("read {key} marker"))?
                .map(|g| decode_value::<KgSchemaMarker>(g.value()))
                .transpose()
                .with_context(|| format!("decode {key} marker"))
                .map(|m| m.map(|m| m.schema_version))
        };
        let format = read(KG_SCHEMA_PALACE_FORMAT)?;
        let triple_key = read(KG_SCHEMA_TRIPLE_KEY)?;
        if format != Some(1) {
            bail!("palace_format marker is {format:?}; the 1 -> 2 step stamps only format 1");
        }
        // #9274: ADR-0067 D1 freezes this marker; a newer value is not "already migrated".
        if triple_key != Some(KG_TRIPLE_KEY_SCHEMA_VERSION) {
            bail!("triple-key marker is {triple_key:?}, which this binary does not read");
        }
        drop(schema);
        stamp_format(&wtx, 2)?;
    }
    wtx.commit().context("commit format-migration txn")?;
    Ok(())
}
