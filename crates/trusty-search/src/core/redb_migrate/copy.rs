//! Table-copy engine for the redb 2.x → 4.x corpus migration.
//!
//! Why: the actual row-by-row copy (reading with the redb 2.6 engine, writing
//! with the redb 4.x engine) is the bulk of the migration logic and is split
//! out here so the orchestration in [`super`] stays focused on detection,
//! backup, and atomic install (and so each file stays under the 500-line cap).
//! What: defines the fixed table catalogue ([`CORPUS_TABLES`]) and the
//! shape-specific copy helpers, plus the post-copy row-count verification and
//! `_meta` schema-version read. [`copy_all_tables`] is the single entry point
//! [`super::migrate_redb_corpus`] calls.
//! Test: covered by the round-trip tests in `super::tests` (which exercise all
//! three table shapes and the verification path) and the `#[ignore]`d
//! real-corpus smoke test.

use std::path::Path;

use anyhow::{Context, Result};
use redb::{ReadableDatabase, ReadableTableMetadata as _};
use redb2::ReadableTable as _;

// ── Table catalogue ─────────────────────────────────────────────────────────

/// Every redb table the trusty-search corpus persists, paired with its
/// key/value shape.
///
/// Why: redb enforces the stored `TypeName` on `open_table`, so a table cannot
/// be copied through a generic `&[u8]→&[u8]` view if it was declared `&str→…`.
/// Enumerating the fixed, known schema lets us open each side with the exact
/// matching typed `TableDefinition` on both the 2.x read and 4.x write engines.
/// The three shapes mirror the definitions in `core::corpus` and
/// `core::migration`: most tables are `&str → &[u8]`; the two legacy community
/// tables are `u64 → &[u8]` and `&str → u64`.
/// What: a `Shape` discriminant per table name. `migrate_redb_corpus` copies
/// every catalogued table that exists in the source; tables absent from the
/// source are silently skipped (an old corpus need not have every table).
/// Test: the round-trip test seeds tables of all three shapes and asserts they
/// all survive.
#[derive(Clone, Copy)]
enum Shape {
    /// `&str → &[u8]` — chunks, entities, kg_nodes, kg_edges, kg_edges_rev,
    /// file_hashes, _meta.
    StrBytes,
    /// `u64 → &[u8]` — kg_communities (legacy, migration-tolerance).
    U64Bytes,
    /// `&str → u64` — kg_symbol_community (legacy, migration-tolerance).
    StrU64,
}

/// The full set of `(table_name, shape)` pairs to copy, in a stable order.
///
/// Why: a single authoritative list keeps the migration in lock-step with the
/// corpus schema. If a new table is added to `core::corpus`, it must be added
/// here too or its rows would be silently dropped on upgrade.
/// What: matches the `TableDefinition::new(...)` names in `core::corpus`
/// (`chunks`, `entities`, `kg_nodes`, `kg_edges`, `kg_edges_rev`,
/// `file_hashes`, `kg_communities`, `kg_symbol_community`) plus `_meta` from
/// `core::migration`.
/// Test: the round-trip test exercises a representative subset
/// (`chunks`/`entities`/`kg_nodes`/`kg_edges`/`kg_communities`/
/// `kg_symbol_community`/`_meta`).
const CORPUS_TABLES: &[(&str, Shape)] = &[
    ("chunks", Shape::StrBytes),
    ("entities", Shape::StrBytes),
    ("kg_nodes", Shape::StrBytes),
    ("kg_edges", Shape::StrBytes),
    ("kg_edges_rev", Shape::StrBytes),
    ("file_hashes", Shape::StrBytes),
    ("kg_communities", Shape::U64Bytes),
    ("kg_symbol_community", Shape::StrU64),
    ("_meta", Shape::StrBytes),
];

// ── Copy engine ─────────────────────────────────────────────────────────────

/// Summary returned by [`copy_all_tables`]: `(per_table_counts, total_rows,
/// schema_version)`.
///
/// Why: a named alias keeps the copy entry point's signature readable (and
/// satisfies clippy's `type_complexity` lint) without leaking a one-off struct.
/// What: the per-table `(name, rows)` vector, the summed row count, and the
/// preserved `_meta` `schema_version`.
/// Test: the round-trip test destructures this triple.
pub(super) type CopySummary = (Vec<(&'static str, u64)>, u64, u32);

/// Open `source` with redb 2.6 and copy every catalogued table into a fresh
/// redb 4.x database at `staging`, verifying per-table row counts.
///
/// Why: this is the load-bearing data-preservation step. Each table is copied
/// row-for-row with its exact key/value shape so the new 4.x corpus is byte-
/// identical at the payload level — no re-embedding, no re-parsing.
/// What: returns `(per_table_counts, total_rows, schema_version)`. After
/// copying, re-reads the staging 4.x table and asserts its row count equals the
/// source's; a mismatch is a hard error (the staging file is left for the
/// caller to discard). `schema_version` is decoded from the copied `_meta` row
/// (`0` if absent) so the caller can log/return it.
/// Test: `tests::round_trip_v2_to_v4` verifies counts and schema version.
pub(super) fn copy_all_tables(source: &Path, staging: &Path) -> Result<CopySummary> {
    let mut per_table: Vec<(&'static str, u64)> = Vec::with_capacity(CORPUS_TABLES.len());
    let mut total_rows = 0u64;

    // Scope the two `Database` handles so both are dropped — releasing redb's
    // exclusive file locks — before we re-open the staging file for
    // verification. redb permits only one open handle per file at a time.
    {
        let src = redb2::Database::open(source)
            .with_context(|| format!("open redb 2.x source {}", source.display()))?;
        let dst = redb::Database::create(staging)
            .with_context(|| format!("create staging redb 4.x corpus {}", staging.display()))?;

        let read = src
            .begin_read()
            .context("begin redb 2.x read transaction")?;
        let write = dst
            .begin_write()
            .context("begin redb 4.x write transaction")?;

        for (name, shape) in CORPUS_TABLES {
            let copied = match shape {
                Shape::StrBytes => copy_str_bytes(&read, &write, name)?,
                Shape::U64Bytes => copy_u64_bytes(&read, &write, name)?,
                Shape::StrU64 => copy_str_u64(&read, &write, name)?,
            };
            if copied > 0 {
                tracing::debug!(table = name, rows = copied, "copied redb table");
            }
            per_table.push((name, copied));
            total_rows += copied;
        }

        write
            .commit()
            .context("commit redb 4.x write transaction")?;
    }

    // Verify: re-open the staging 4.x corpus and confirm each table's row count
    // matches what we copied. This catches a silent partial write before we
    // touch the original and rename the staging file into place.
    verify_counts(staging, &per_table)
        .context("verify migrated staging corpus row counts match the source")?;

    let schema_version = read_schema_version(staging).unwrap_or(0);
    Ok((per_table, total_rows, schema_version))
}

/// Copy a `&str → &[u8]` table from the 2.x read txn into the 4.x write txn.
///
/// Why: the majority of corpus tables (chunks, entities, KG adjacency, file
/// hashes, `_meta`) share this shape; one helper avoids duplicating the
/// open/iterate/insert dance.
/// What: opens the named table on both engines, streams every `(key, value)`
/// pair across, and returns the number of rows copied. A source table that does
/// not exist yields `0` (nothing to copy) rather than an error.
/// Test: exercised by the round-trip test's chunks/entities/_meta rows.
fn copy_str_bytes(
    read: &redb2::ReadTransaction,
    write: &redb::WriteTransaction,
    name: &str,
) -> Result<u64> {
    let def2: redb2::TableDefinition<&str, &[u8]> = redb2::TableDefinition::new(name);
    let src = match read.open_table(def2) {
        Ok(t) => t,
        Err(redb2::TableError::TableDoesNotExist(_)) => return Ok(0),
        Err(e) => return Err(anyhow::anyhow!("open 2.x table '{name}': {e}")),
    };
    let def4: redb::TableDefinition<&str, &[u8]> = redb::TableDefinition::new(name);
    let mut dst = write
        .open_table(def4)
        .with_context(|| format!("open 4.x table '{name}'"))?;

    let mut n = 0u64;
    for entry in src.iter().with_context(|| format!("iterate '{name}'"))? {
        let (k, v) = entry.with_context(|| format!("read row in '{name}'"))?;
        dst.insert(k.value(), v.value())
            .with_context(|| format!("insert row into '{name}'"))?;
        n += 1;
    }
    Ok(n)
}

/// Copy a `u64 → &[u8]` table (legacy `kg_communities`).
///
/// Why: the community-record table is keyed by a `u64` id; its rows must be
/// preserved verbatim for backward-compat even though the active search path no
/// longer reads them.
/// What: same stream-copy as [`copy_str_bytes`] but with the `u64` key type.
/// Test: the round-trip test seeds a `kg_communities` row and asserts it
/// survives.
fn copy_u64_bytes(
    read: &redb2::ReadTransaction,
    write: &redb::WriteTransaction,
    name: &str,
) -> Result<u64> {
    let def2: redb2::TableDefinition<u64, &[u8]> = redb2::TableDefinition::new(name);
    let src = match read.open_table(def2) {
        Ok(t) => t,
        Err(redb2::TableError::TableDoesNotExist(_)) => return Ok(0),
        Err(e) => return Err(anyhow::anyhow!("open 2.x table '{name}': {e}")),
    };
    let def4: redb::TableDefinition<u64, &[u8]> = redb::TableDefinition::new(name);
    let mut dst = write
        .open_table(def4)
        .with_context(|| format!("open 4.x table '{name}'"))?;

    let mut n = 0u64;
    for entry in src.iter().with_context(|| format!("iterate '{name}'"))? {
        let (k, v) = entry.with_context(|| format!("read row in '{name}'"))?;
        dst.insert(k.value(), v.value())
            .with_context(|| format!("insert row into '{name}'"))?;
        n += 1;
    }
    Ok(n)
}

/// Copy a `&str → u64` table (legacy `kg_symbol_community`).
///
/// Why: the symbol→community mapping is keyed by string with a `u64` value; it
/// must round-trip verbatim for backward-compat.
/// What: same stream-copy with the `(&str, u64)` shape.
/// Test: the round-trip test seeds a `kg_symbol_community` row and asserts it
/// survives.
fn copy_str_u64(
    read: &redb2::ReadTransaction,
    write: &redb::WriteTransaction,
    name: &str,
) -> Result<u64> {
    let def2: redb2::TableDefinition<&str, u64> = redb2::TableDefinition::new(name);
    let src = match read.open_table(def2) {
        Ok(t) => t,
        Err(redb2::TableError::TableDoesNotExist(_)) => return Ok(0),
        Err(e) => return Err(anyhow::anyhow!("open 2.x table '{name}': {e}")),
    };
    let def4: redb::TableDefinition<&str, u64> = redb::TableDefinition::new(name);
    let mut dst = write
        .open_table(def4)
        .with_context(|| format!("open 4.x table '{name}'"))?;

    let mut n = 0u64;
    for entry in src.iter().with_context(|| format!("iterate '{name}'"))? {
        let (k, v) = entry.with_context(|| format!("read row in '{name}'"))?;
        dst.insert(k.value(), v.value())
            .with_context(|| format!("insert row into '{name}'"))?;
        n += 1;
    }
    Ok(n)
}

// ── Verification ────────────────────────────────────────────────────────────

/// Re-open the staging 4.x corpus and assert each table's row count matches
/// what the copy step reported.
///
/// Why: a silent partial write (e.g. a disk-full mid-copy that did not surface
/// as an insert error) must be caught *before* we destroy the original. Re-
/// reading the committed staging file is the cheapest end-to-end integrity
/// check.
/// What: opens `staging` with redb 4.x, reads each catalogued table's `len()`,
/// and errors on the first mismatch. Tables absent from the source (`expected
/// == 0`) are skipped because they were never created in the staging file.
/// Test: covered by the round-trip test (passes) — a mismatch path is a defensive
/// invariant, not normally reachable.
fn verify_counts(staging: &Path, per_table: &[(&'static str, u64)]) -> Result<()> {
    let db = redb::Database::open(staging).with_context(|| {
        format!(
            "re-open staging corpus {} for verification",
            staging.display()
        )
    })?;
    let read = db.begin_read().context("begin verify read txn")?;

    for (name, expected) in per_table {
        if *expected == 0 {
            continue;
        }
        let actual = verify_table_len(&read, name, expected)?;
        if actual != *expected {
            anyhow::bail!(
                "row-count mismatch after migration: table '{name}' has {actual} rows in the \
                 migrated corpus but {expected} were copied from the source"
            );
        }
    }
    Ok(())
}

/// Read one table's row count from the staging 4.x read transaction, trying
/// each shape until one opens.
///
/// Why: `verify_counts` does not track the shape per table, and `len()` is
/// shape-agnostic; opening with the right typed definition is all that is
/// needed. Probing the three shapes in turn keeps the verifier table-shape-
/// agnostic.
/// What: tries the `&str→&[u8]`, then `u64→&[u8]`, then `&str→u64`
/// definitions; returns the `len()` of whichever opens. Errors only if none
/// opens (which would contradict the copy step having written rows).
/// Test: exercised by `verify_counts` in the round-trip test.
fn verify_table_len(read: &redb::ReadTransaction, name: &str, expected: &u64) -> Result<u64> {
    if let Ok(t) = read.open_table::<&str, &[u8]>(redb::TableDefinition::new(name)) {
        return t.len().with_context(|| format!("len of '{name}'"));
    }
    if let Ok(t) = read.open_table::<u64, &[u8]>(redb::TableDefinition::new(name)) {
        return t.len().with_context(|| format!("len of '{name}'"));
    }
    if let Ok(t) = read.open_table::<&str, u64>(redb::TableDefinition::new(name)) {
        return t.len().with_context(|| format!("len of '{name}'"));
    }
    anyhow::bail!(
        "verification could not open migrated table '{name}' (expected {expected} rows) under any \
         known shape"
    )
}

/// Decode the `schema_version` from a freshly migrated 4.x corpus's `_meta`.
///
/// Why: the migration must preserve the source's app-schema version so the
/// in-app migration chain (M001…) runs from the correct starting point rather
/// than treating the corpus as brand-new. We read it back here purely to log /
/// return it for operator confidence.
/// What: opens `_meta` in the 4.x corpus, reads `schema_version`, decodes the
/// 4-byte little-endian `u32`. Returns `None` when the table or key is absent
/// (a legacy pre-migration-framework corpus → treated as version 0 by the
/// runner).
/// Test: the round-trip test seeds a `_meta` `schema_version` and asserts it is
/// preserved.
fn read_schema_version(staging: &Path) -> Option<u32> {
    let db = redb::Database::open(staging).ok()?;
    let read = db.begin_read().ok()?;
    let table = read
        .open_table::<&str, &[u8]>(redb::TableDefinition::new("_meta"))
        .ok()?;
    let v = table.get("schema_version").ok()??;
    let bytes = v.value();
    if bytes.len() == 4 {
        Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    } else {
        None
    }
}
