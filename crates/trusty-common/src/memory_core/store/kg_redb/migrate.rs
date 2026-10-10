//! The `kg.redb` step of the palace format 0 → 1 migration (#9274).
//!
//! Why: format 1 fixes the TRIPLES key at `(subject, predicate, object)`
//! (#4810). A format-0 palace may still carry the old `(subject, predicate)`
//! key, under which a second object closed the first. The #4810 rewrite used
//! to run at every KG open and fail open: an error was logged and the palace
//! opened un-migrated, and an undecodable row was skipped while the marker was
//! still stamped. ADR-0067 D3 rule 4 makes the migration fail closed, so this
//! step now runs only inside the format migration, behind its verified backup.
//! What: [`migrate_kg_to_format`] rewrites every legacy TRIPLES key (active and
//! `hist:`) and stamps the triple-key and `palace_format` markers, all in ONE
//! redb write transaction — the commit point of the migration. A failure
//! anywhere commits nothing. A row that does not decode fails the step.
//! `TRIPLES_BY_OBJECT` is untouched: its key already carried the object.
//! Test: format_migration_tests.rs — `a_format_0_palace_migrates_at_first_writer_open`,
//! `an_undecodable_legacy_row_fails_the_migration_and_commits_nothing`.

use crate::memory_core::store::kg_store::{
    KG_SCHEMA, KG_SCHEMA_TRIPLE_KEY, KG_TRIPLE_KEY_SCHEMA_VERSION, KgSchemaMarker, TRIPLES,
    TripleValue, decode_legacy_triple_key, decode_value, encode_triple_key,
};
use crate::memory_core::store::palace_format::stamp_format;
use anyhow::{Context, Result, bail};
use redb::{Database, ReadableTable};

/// One TRIPLES row's rewrite: where it is now, where it belongs, and its value.
struct Rewrite {
    old_key: Vec<u8>,
    new_key: Vec<u8>,
    value: Vec<u8>,
}

/// Rewrite legacy triple keys and stamp `to`, in one write transaction.
///
/// Why: see the module doc; the caller holds a verified backup and the
/// migration record before it calls this.
/// What: reads the triple-key marker inside the transaction. Version 1 needs
/// no rewrite; no marker means the legacy key, which is planned and rewritten;
/// any other version is refused. Then [`stamp_format`] and commit. Returns the
/// number of rows rewritten.
/// Test: `a_format_0_palace_migrates_at_first_writer_open`,
/// `an_undecodable_legacy_row_fails_the_migration_and_commits_nothing`.
pub(crate) fn migrate_kg_to_format(db: &Database, to: u32) -> Result<usize> {
    let wtx = db.begin_write().context("begin format-migration txn")?;
    let rows;
    {
        let triple_key = {
            let schema = wtx
                .open_table(KG_SCHEMA)
                .context("open kg_schema for migration")?;
            let marker = schema
                .get(KG_SCHEMA_TRIPLE_KEY)
                .context("read triple-key marker")?
                .map(|g| decode_value::<KgSchemaMarker>(g.value()))
                .transpose()
                .context("decode triple-key marker")?;
            marker.map(|m| m.schema_version)
        };
        let plan = match triple_key {
            Some(KG_TRIPLE_KEY_SCHEMA_VERSION) => Vec::new(),
            // #9274: ADR-0067 D1 freezes this marker; a newer value is not
            // "already migrated".
            Some(other) => bail!("triple-key marker is {other}, which this binary does not read"),
            None => {
                let triples = wtx.open_table(TRIPLES).context("open triples for plan")?;
                plan_rewrites(&triples)?
            }
        };
        rows = plan.len();
        let mut triples = wtx.open_table(TRIPLES).context("open triples table")?;
        // Remove every legacy key before inserting any new one: a new key can
        // equal some other row's legacy key, and interleaving the two would let
        // an insert land under a key a later remove then deletes.
        for r in &plan {
            triples
                .remove(r.old_key.as_slice())
                .context("remove legacy triple key")?;
        }
        for r in &plan {
            triples
                .insert(r.new_key.as_slice(), r.value.as_slice())
                .context("insert migrated triple key")?;
        }
        drop(triples);
        stamp_format(&wtx, to)?;
    }
    wtx.commit().context("commit format-migration txn")?;
    Ok(rows)
}

/// Build the full rewrite plan for every legacy TRIPLES row.
///
/// What: splits each key into its `hist:` prefix / legacy core / 8-byte
/// timestamp suffix, decodes the core with [`decode_legacy_triple_key`], and
/// re-encodes it with the object taken from the row's value. #9274: a key or
/// value that does not decode fails the plan — skipping it would orphan the
/// row under a key no reader decodes while the marker says it was migrated.
/// Test: `an_undecodable_legacy_row_fails_the_migration_and_commits_nothing`.
fn plan_rewrites(
    triples: &impl ReadableTable<&'static [u8], &'static [u8]>,
) -> Result<Vec<Rewrite>> {
    let mut plan = Vec::new();
    for entry in triples.iter().context("iter triples for migration plan")? {
        let (k, v) = entry.context("read triples row for migration plan")?;
        let old_key = k.value().to_vec();
        let value_bytes = v.value().to_vec();
        let shown = hex::encode(&old_key);
        let value: TripleValue = decode_value(&value_bytes)
            .with_context(|| format!("triple row {shown}: value does not decode"))?;

        let is_hist = old_key.starts_with(b"hist:");
        let (core, suffix): (&[u8], &[u8]) = if is_hist {
            let stripped = &old_key[b"hist:".len()..];
            if stripped.len() < 8 {
                bail!("triple row {shown}: history key is truncated");
            }
            stripped.split_at(stripped.len() - 8)
        } else {
            (old_key.as_slice(), &[])
        };

        let Some((subject, predicate)) = decode_legacy_triple_key(core) else {
            bail!("triple row {shown}: legacy key does not decode");
        };

        let mut new_key = Vec::new();
        if is_hist {
            new_key.extend_from_slice(b"hist:");
        }
        new_key.extend_from_slice(&encode_triple_key(&subject, &predicate, &value.object));
        new_key.extend_from_slice(suffix);
        if new_key == old_key {
            continue;
        }

        plan.push(Rewrite {
            old_key,
            new_key,
            value: value_bytes,
        });
    }
    Ok(plan)
}
