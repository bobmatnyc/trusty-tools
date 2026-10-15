//! The `kg.redb` steps of the palace format migration (#9274, #9275, #9918).
//!
//! Why: #9918 — a palace at any older format is migrated one step at a time,
//! so this binary carries every step. Format 1 fixes the TRIPLES key at
//! `(subject, predicate, object)` (#4810); format 2 permits `{uuid}#{n}`
//! chunk rows in `index.usearch.redb` and changes no table shape, so its step
//! is a stamp that fences the palace off from a 1.x binary (ADR-0067 D2).
//! What: [`STEPS`] holds step `k → k+1` at index `k`. Each step reads both
//! markers inside its write transaction, fails before any write unless they
//! name the format it moves from, and commits its table work and the new
//! markers together — that step's commit point (D3 rule 4).
//! Test: format_migration_v2_tests.rs —
//! `the_0_to_1_step_commits_nothing_unless_the_kg_is_an_intact_format_0`,
//! `the_1_to_2_step_commits_nothing_when_the_stored_format_is_not_1`.

use crate::engine::store::kg_store::{
    KG_SCHEMA, KG_SCHEMA_TRIPLE_KEY, KG_TRIPLE_KEY_SCHEMA_VERSION, KgSchemaMarker, TRIPLES,
    TripleValue, decode_legacy_triple_key, decode_value, encode_triple_key,
};
use crate::engine::store::palace_format::{KG_SCHEMA_PALACE_FORMAT, stamp_format};
use anyhow::{Context, Result, bail};
use redb::{Database, ReadableTable, WriteTransaction};

/// One `kg.redb` step: moves a KG from format `k` to `k + 1`.
type KgStep = fn(&Database) -> Result<()>;

/// Every step, indexed by the format it moves from (#9918).
///
/// Why: a step is never removed while a supported format needs it; the
/// length check keeps a format bump from shipping without its step.
/// Test: `the_step_table_has_one_step_per_older_format`.
pub(crate) const STEPS: &[KgStep] = &[step_0_to_1, step_1_to_2];

/// Run the `kg.redb` step that moves a palace to format `to`.
///
/// Why: the caller holds a verified backup and the migration record, and
/// runs one call per step in ascending order (#9918).
/// What: runs `STEPS[to - 1]`; a target with no step fails.
/// Test: `the_1_to_2_step_commits_nothing_when_the_stored_format_is_not_1`.
pub(crate) fn migrate_kg_to_format(db: &Database, to: u32) -> Result<()> {
    let step = to
        .checked_sub(1)
        .and_then(|from| STEPS.get(usize::try_from(from).ok()?));
    match step {
        Some(step) => step(db),
        None => bail!("this binary has no kg.redb step to format {to}"),
    }
}

/// Both `kg_schema` markers as stored: `(palace_format, triple_key)`.
///
/// What: an absent row is `None`; a row that does not decode fails.
fn read_markers(wtx: &WriteTransaction) -> Result<(Option<u32>, Option<u32>)> {
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
    Ok((read(KG_SCHEMA_PALACE_FORMAT)?, read(KG_SCHEMA_TRIPLE_KEY)?))
}

/// Rewrite legacy triple keys and stamp format 1, in one write transaction.
///
/// Why: restored from 0.33.0 (#9274) for the chain (#9918). Fail closed: a
/// `palace_format` row of any value means the file is not the format-0 KG
/// the gate classified, so the step refuses it (Fail-Open Check).
/// What: `palace_format` must be absent. Triple key v1 needs no rewrite; no
/// triple-key marker means the legacy key, rewritten with every legacy key
/// removed before any new one is inserted; any other version fails. A key or
/// value that does not decode fails the step.
/// Test: `the_0_to_1_step_rewrites_legacy_keys_and_stamps_format_1`,
/// `the_0_to_1_step_commits_nothing_unless_the_kg_is_an_intact_format_0`.
fn step_0_to_1(db: &Database) -> Result<()> {
    let wtx = db.begin_write().context("begin format-migration txn")?;
    {
        let (format, triple_key) = read_markers(&wtx)?;
        if let Some(format) = format {
            bail!("palace_format marker is {format}; the 0 -> 1 step reads an unstamped KG");
        }
        let plan = match triple_key {
            Some(KG_TRIPLE_KEY_SCHEMA_VERSION) => Vec::new(),
            // #9274: ADR-0067 D1 freezes this marker; a newer value is not
            // "already migrated".
            Some(other) => bail!("triple-key marker is {other}, which this binary does not read"),
            None => plan_rewrites(&wtx.open_table(TRIPLES).context("open triples for plan")?)?,
        };
        let mut triples = wtx.open_table(TRIPLES).context("open triples table")?;
        // A new key can equal another row's legacy key: remove all, then insert.
        for (old_key, _, _) in &plan {
            triples
                .remove(old_key.as_slice())
                .context("remove legacy triple key")?;
        }
        for (_, new_key, value) in &plan {
            triples
                .insert(new_key.as_slice(), value.as_slice())
                .context("insert migrated triple key")?;
        }
        drop(triples);
        stamp_format(&wtx, 1)?;
    }
    wtx.commit().context("commit format-migration txn")?;
    Ok(())
}

/// One TRIPLES row's rewrite: `(old_key, new_key, value)`.
type Rewrite = (Vec<u8>, Vec<u8>, Vec<u8>);

/// Build the rewrite plan for every legacy TRIPLES row.
///
/// What: splits each key into its `hist:` prefix, legacy core and 8-byte
/// timestamp suffix, and re-encodes the core with the object from the row's
/// value. #9274: a row that does not decode fails the plan; skipping it would
/// orphan it under a key no reader decodes once the marker says format 1.
fn plan_rewrites(
    triples: &impl ReadableTable<&'static [u8], &'static [u8]>,
) -> Result<Vec<Rewrite>> {
    let mut plan = Vec::new();
    for entry in triples.iter().context("iter triples for migration plan")? {
        let (k, v) = entry.context("read triples row for migration plan")?;
        let old_key = k.value().to_vec();
        let shown = hex::encode(&old_key);
        let value: TripleValue = decode_value(v.value())
            .with_context(|| format!("triple row {shown}: value does not decode"))?;
        let hist = old_key.strip_prefix(b"hist:".as_slice());
        let (core, suffix) = match hist {
            Some(rest) if rest.len() >= 8 => rest.split_at(rest.len() - 8),
            Some(_) => bail!("triple row {shown}: history key is truncated"),
            None => (old_key.as_slice(), &[][..]),
        };
        let Some((subject, predicate)) = decode_legacy_triple_key(core) else {
            bail!("triple row {shown}: legacy key does not decode");
        };
        let mut new_key = if hist.is_some() {
            b"hist:".to_vec()
        } else {
            Vec::new()
        };
        new_key.extend_from_slice(&encode_triple_key(&subject, &predicate, &value.object));
        new_key.extend_from_slice(suffix);
        if new_key != old_key {
            plan.push((old_key, new_key, v.value().to_vec()));
        }
    }
    Ok(plan)
}

/// Stamp format 2 over a format-1 KG, in one write transaction.
///
/// Why: fail closed (#9275, Fail-Open Check). A marker that is not 1 here
/// means the file changed under the migration; stamping over it would
/// advance a palace this step never read.
/// What: `palace_format` must be 1 and the triple key v1 (absent, other or
/// undecodable fails) before a byte is written.
/// Test: `the_1_to_2_step_commits_nothing_when_the_stored_format_is_not_1`.
fn step_1_to_2(db: &Database) -> Result<()> {
    let wtx = db.begin_write().context("begin format-migration txn")?;
    {
        let (format, triple_key) = read_markers(&wtx)?;
        if format != Some(1) {
            bail!("palace_format marker is {format:?}; the 1 -> 2 step stamps only format 1");
        }
        // #9274: ADR-0067 D1 freezes this marker; a newer value is not "already migrated".
        if triple_key != Some(KG_TRIPLE_KEY_SCHEMA_VERSION) {
            bail!("triple-key marker is {triple_key:?}, which this binary does not read");
        }
        stamp_format(&wtx, 2)?;
    }
    wtx.commit().context("commit format-migration txn")?;
    Ok(())
}
