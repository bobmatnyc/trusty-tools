//! At-open, per-palace migration of the TRIPLES key from `(subject,
//! predicate)` to `(subject, predicate, object)` (#4810).
//!
//! Why: the old key made the object invisible to storage, so a second object
//! under one `(subject, predicate)` closed the first. Fixing the encoder alone
//! would leave every existing palace's rows unreadable at their new key —
//! `query_active` would return nothing and the next assert would insert a
//! duplicate alongside the orphan. The rows have to be rewritten, once, and the
//! only place that reliably happens for every palace on every machine is at
//! open.
//! What: gated on the [`KG_SCHEMA`] marker row so it runs at most once per
//! palace; backs the database file up first and verifies the copy; rewrites
//! every TRIPLES key (active and `hist:`) and stamps the marker — the rewrite
//! and the stamp inside ONE redb write transaction, so a failure part-way
//! through rolls both back and the next open retries from the original rows.
//! `TRIPLES_BY_OBJECT` is untouched: its key already carried the object.
//! #6652 retired the `TRIPLES_BY_PREDICATE` rebuild this migration used to
//! carry: the index has no reader, so the write maintenance is gone and the
//! table itself is dropped by not being copied during the
//! [`super::copy_swap`] compaction — off the request path, size-gated, and
//! behind a verified backup.
//! Fail-open: any error is logged at `warn!` and the palace opens un-migrated,
//! matching how `PalaceHandle::open_with_intent` degrades when `load_drawers`
//! fails.
//! Test: `migration_stamps_schema_and_is_idempotent`,
//! `migration_rewrites_legacy_keys_and_preserves_history`,
//! `migration_failure_leaves_the_palace_openable_and_retries`.

use crate::memory_core::store::kg_store::{
    KG_SCHEMA, KG_SCHEMA_TRIPLE_KEY, KG_TRIPLE_KEY_SCHEMA_VERSION, KgSchemaMarker, TRIPLES,
    TripleValue, decode_legacy_triple_key, decode_value, encode_triple_key, encode_value,
};
use anyhow::{Context, Result, bail};
use redb::{Database, ReadableDatabase, ReadableTable};
use std::path::{Path, PathBuf};

/// Suffix appended to the redb file name for the pre-migration backup.
const BACKUP_SUFFIX: &str = ".pre-4810.bak";

/// Run the #4810 migration, logging rather than propagating any failure.
///
/// Why: a palace that cannot be migrated must still open. Refusing would take
/// the whole memory service down over a condition — a full disk, an
/// unwritable directory — that clears on its own and that the next open
/// retries. Nothing is lost either: the rows stay on disk in their original
/// form, and the failed attempt commits nothing.
/// What: calls [`migrate_triple_keys`] and maps its outcome onto tracing. The
/// warning states the consequence plainly, because it is not a small one: the
/// readers decode the new three-component key, so until the migration
/// succeeds an un-migrated palace's triples do not appear in `query_active` /
/// `list_active` at all. They are not gone — the next successful open makes
/// them visible again.
/// Test: `migration_failure_leaves_the_palace_openable_and_retries`.
pub(super) fn migrate_triple_keys_fail_open(db: &Database, path: &Path) {
    match migrate_triple_keys(db, path) {
        Ok(MigrationOutcome::AlreadyMigrated) => {}
        Ok(MigrationOutcome::Stamped) => {
            tracing::debug!(path = %path.display(), "#4810: stamped triple-key schema on an empty KG");
        }
        Ok(MigrationOutcome::Migrated { rows, backup }) => {
            tracing::info!(
                path = %path.display(),
                rows,
                backup = %backup.display(),
                "#4810: rewrote KG triple keys to (subject, predicate, object)"
            );
        }
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                "#4810: KG triple-key migration failed; opening un-migrated. The rows are intact on disk and the next open retries, but until it succeeds this palace's triples will not appear in queries: {e:#}"
            );
        }
    }
}

/// What a migration attempt did.
#[derive(Debug)]
pub(super) enum MigrationOutcome {
    /// The marker row already names this version or newer — nothing to do.
    AlreadyMigrated,
    /// No legacy rows existed (a fresh or empty palace); only the marker was
    /// written.
    Stamped,
    /// `rows` TRIPLES rows were rewritten; the pre-migration file is at
    /// `backup`.
    Migrated { rows: usize, backup: PathBuf },
}

/// One TRIPLES row's rewrite: where it is now, where it belongs, and its value.
struct Rewrite {
    old_key: Vec<u8>,
    new_key: Vec<u8>,
    value: Vec<u8>,
}

/// Migrate this database's triple keys if it has not been migrated already.
///
/// Why/What: see the module doc. Returns what it did so the caller can log at
/// the right level.
/// Test: `migration_rewrites_legacy_keys_and_preserves_history`.
fn migrate_triple_keys(db: &Database, path: &Path) -> Result<MigrationOutcome> {
    if read_schema_version(db)?.is_some_and(|v| v >= KG_TRIPLE_KEY_SCHEMA_VERSION) {
        return Ok(MigrationOutcome::AlreadyMigrated);
    }

    let plan = plan_rewrites(db)?;
    if plan.is_empty() {
        stamp_only(db)?;
        return Ok(MigrationOutcome::Stamped);
    }

    // Back up BEFORE the write transaction. A verified copy of the original is
    // the only thing that makes a bad rewrite recoverable, and a truncated copy
    // is worse than none — it looks like a safety net and is not one.
    let backup = ensure_verified_backup(path)?;

    let rows = plan.len();
    let wtx = db.begin_write().context("begin #4810 migration txn")?;
    {
        // The marker write and the row rewrite ride the SAME transaction:
        // redb commits it atomically, so a failure anywhere below leaves both
        // the old rows and the absent marker in place and the next open
        // retries from scratch. A marker stamped in a separate transaction
        // could land while the rewrite did not, permanently skipping it.
        let mut schema = wtx
            .open_table(KG_SCHEMA)
            .context("open kg_schema table for migration")?;
        if let Some(v) = schema
            .get(KG_SCHEMA_TRIPLE_KEY)
            .context("re-read schema marker inside migration txn")?
            .map(|g| decode_value::<KgSchemaMarker>(g.value()))
            .transpose()
            .context("decode schema marker inside migration txn")?
            && v.schema_version >= KG_TRIPLE_KEY_SCHEMA_VERSION
        {
            return Ok(MigrationOutcome::AlreadyMigrated);
        }

        let mut triples = wtx
            .open_table(TRIPLES)
            .context("open triples table for migration")?;
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

        let marker = encode_value(&KgSchemaMarker {
            schema_version: KG_TRIPLE_KEY_SCHEMA_VERSION,
        })
        .context("encode KgSchemaMarker")?;
        schema
            .insert(KG_SCHEMA_TRIPLE_KEY, marker.as_slice())
            .context("stamp triple-key schema marker")?;
    }
    wtx.commit().context("commit #4810 migration txn")?;
    Ok(MigrationOutcome::Migrated { rows, backup })
}

/// The stored triple-key schema version, if any.
///
/// Why/What: a palace with no [`KG_SCHEMA`] row predates #4810 and needs
/// migrating; one whose row names this version or newer does not. A missing
/// table is reported the same as a missing row — that is a palace whose file
/// was opened read-only or created before the table existed.
/// Test: `migration_stamps_schema_and_is_idempotent`.
fn read_schema_version(db: &Database) -> Result<Option<u32>> {
    let rtx = db.begin_read().context("begin schema-marker read")?;
    let table = match rtx.open_table(KG_SCHEMA) {
        Ok(t) => t,
        Err(redb::TableError::TableDoesNotExist(_)) => return Ok(None),
        Err(e) => return Err(anyhow::Error::new(e).context("open kg_schema table")),
    };
    let Some(guard) = table
        .get(KG_SCHEMA_TRIPLE_KEY)
        .context("read schema marker")?
    else {
        return Ok(None);
    };
    let marker: KgSchemaMarker =
        decode_value(guard.value()).context("decode triple-key schema marker")?;
    Ok(Some(marker.schema_version))
}

/// Build the full rewrite plan from a read snapshot.
///
/// Why: computing the plan outside the write transaction keeps the exclusive
/// write window short and lets the caller decide whether a backup is even
/// needed (an empty palace does not earn one).
/// What: walks TRIPLES, splits each key into its `hist:` prefix / legacy core /
/// 8-byte timestamp suffix, decodes the core with
/// [`decode_legacy_triple_key`], and re-encodes it with the object taken from
/// the row's value. Rows whose key or value will not decode are skipped with a
/// warning rather than failing the migration — one unreadable row must not
/// block the rest of the palace.
/// Test: `migration_rewrites_legacy_keys_and_preserves_history`.
fn plan_rewrites(db: &Database) -> Result<Vec<Rewrite>> {
    let rtx = db.begin_read().context("begin migration plan read")?;
    let triples = match rtx.open_table(TRIPLES) {
        Ok(t) => t,
        Err(redb::TableError::TableDoesNotExist(_)) => return Ok(Vec::new()),
        Err(e) => return Err(anyhow::Error::new(e).context("open triples table for plan")),
    };
    let mut plan = Vec::new();
    for entry in triples.iter().context("iter triples for migration plan")? {
        let (k, v) = entry.context("read triples row for migration plan")?;
        let old_key = k.value().to_vec();
        let value_bytes = v.value().to_vec();
        let value: TripleValue = match decode_value(&value_bytes) {
            Ok(val) => val,
            Err(e) => {
                tracing::warn!("#4810: skipping undecodable triple value during migration: {e}");
                continue;
            }
        };

        let is_hist = old_key.starts_with(b"hist:");
        let (core, suffix): (&[u8], &[u8]) = if is_hist {
            let stripped = &old_key[b"hist:".len()..];
            if stripped.len() < 8 {
                tracing::warn!("#4810: skipping truncated history key during migration");
                continue;
            }
            stripped.split_at(stripped.len() - 8)
        } else {
            (old_key.as_slice(), &[])
        };

        let Some((subject, predicate)) = decode_legacy_triple_key(core) else {
            tracing::warn!("#4810: skipping undecodable triple key during migration");
            continue;
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

/// Stamp the marker on a palace with nothing to rewrite.
///
/// Why: a fresh palace must still record which key shape it uses, or every
/// subsequent open would re-run the plan scan.
/// Test: `migration_stamps_schema_and_is_idempotent`.
fn stamp_only(db: &Database) -> Result<()> {
    let marker = encode_value(&KgSchemaMarker {
        schema_version: KG_TRIPLE_KEY_SCHEMA_VERSION,
    })
    .context("encode KgSchemaMarker")?;
    let wtx = db.begin_write().context("begin schema-stamp txn")?;
    {
        let mut schema = wtx
            .open_table(KG_SCHEMA)
            .context("open kg_schema table for stamp")?;
        schema
            .insert(KG_SCHEMA_TRIPLE_KEY, marker.as_slice())
            .context("stamp triple-key schema marker")?;
    }
    wtx.commit().context("commit schema-stamp txn")?;
    Ok(())
}

/// Produce (or reuse) a size-verified copy of the database file.
///
/// Why: the rewrite touches every triple row in the palace. If it goes wrong in
/// a way redb's transaction cannot undo — a disk filling mid-commit, a bug in
/// this file — the original bytes are the only recovery path. A backup that was
/// itself truncated is worse than no backup at all, because an operator will
/// trust it, so the copy is verified before the rewrite is allowed to start.
/// What: copies `path` to `<path>.pre-4810.bak` and re-stats the result,
/// failing when the byte counts disagree. An existing backup whose size already
/// matches the current file is left alone: a previous attempt failed, rolled
/// back, and left the source unchanged, so that copy is still a good one and
/// re-copying only risks replacing it with a worse one.
/// Test: `migration_failure_leaves_the_palace_openable_and_retries`.
fn ensure_verified_backup(path: &Path) -> Result<PathBuf> {
    let backup = PathBuf::from(format!("{}{BACKUP_SUFFIX}", path.display()));
    let src_len = std::fs::metadata(path)
        .with_context(|| format!("stat {} for backup", path.display()))?
        .len();

    if let Ok(meta) = std::fs::metadata(&backup)
        && meta.len() == src_len
    {
        return Ok(backup);
    }

    std::fs::copy(path, &backup)
        .with_context(|| format!("copy {} to {}", path.display(), backup.display()))?;
    let copied = std::fs::metadata(&backup)
        .with_context(|| format!("stat backup {}", backup.display()))?
        .len();
    if copied != src_len {
        bail!(
            "backup {} is {copied} bytes but {} is {src_len}; refusing to migrate behind a truncated backup",
            backup.display(),
            path.display()
        );
    }
    Ok(backup)
}
