//! Palace format marker, its `palace.json` mirror, and the open gate (#9274).
//!
//! Why: ADR-0067 D3 rule 1 — a palace written by a newer release is refused
//! under every open intent, before any byte under it changes. The triple-key
//! marker cannot do this job: it reads a newer version as "already migrated"
//! (`kg_redb/migrate.rs`), and ADR-0067 D1 freezes it.
//! What: the authoritative marker is the `palace_format` row of `kg.redb`'s
//! `kg_schema` table; the mirror is `palace.json`'s `format_version`. Either
//! one missing means format 0. A palace is refused when either names a format
//! above [`PALACE_FORMAT_SUPPORTED`], when they disagree, or when the marker
//! is present and cannot be read. Nothing here stamps a marker — the 0 → 1
//! migration does, and it is not in this release, so every existing palace
//! reads as format 0 and opens as before.
//! Test: palace_format_tests.rs — `n_plus_one_palace_is_refused_and_bytes_unchanged`,
//! `an_unstamped_palace_opens_read_write_and_read_only_as_before`, and the
//! other arms listed on each function below.

use super::kg_store::{KG_SCHEMA, KgSchemaMarker, decode_value};
use super::palace_store::PalaceStoreError;
use super::write_deadline::palace_label;
use redb::ReadableDatabase;
use serde::Deserialize;
use std::path::Path;

/// The palace format this binary reads and writes (ADR-0067 D1).
///
/// #9274: 0 until the 0 → 1 migration ships; a palace stamped 1 or higher is
/// refused by this binary.
pub const PALACE_FORMAT_SUPPORTED: u32 = 0;

/// The `kg_schema` key that holds the authoritative palace format.
pub const KG_SCHEMA_PALACE_FORMAT: &str = "palace_format";

/// The KG file the marker lives in, inside the palace directory.
const KG_FILE: &str = "kg.redb";

/// The metadata file that carries the mirror.
const PALACE_JSON: &str = "palace.json";

type Result<T> = std::result::Result<T, PalaceStoreError>;

/// The one `palace.json` field the gate reads; every other field is ignored.
#[derive(Deserialize)]
struct Mirror {
    #[serde(default)]
    format_version: Option<u32>,
}

/// Compare a palace's format with the one this binary supports.
///
/// Why: one rule set for every open path (ADR-0067 D3 rules 1 and 2).
/// What: newer → `FormatTooNew`; equal → `Ok`; one behind →
/// `FormatNeedsMigration`; further behind → `FormatTooOld`. One behind is
/// refused under both intents here because this release carries no migration;
/// the writer arm becomes the migration in the follow-up (#9274 PR3).
/// Test: `check_format_classifies_every_distance`,
/// `read_only_open_of_n_minus_1_is_refused_with_migration_hint`.
pub(crate) fn check_format(palace: &str, found: u32, supported: u32) -> Result<()> {
    let palace = palace.to_string();
    if found > supported {
        Err(PalaceStoreError::FormatTooNew {
            palace,
            found,
            supported,
        })
    } else if found == supported {
        Ok(())
    } else if found + 1 == supported {
        Err(PalaceStoreError::FormatNeedsMigration {
            palace,
            found,
            supported,
        })
    } else {
        Err(PalaceStoreError::FormatTooOld {
            palace,
            found,
            supported,
        })
    }
}

/// Read the `palace.json` format mirror; an absent file or field is format 0.
///
/// Why: the mirror is readable without opening redb, so a newer palace is
/// refused before any store takes a lock (ADR-0067 D1).
/// What: `Io` for a read that fails for any reason but absence, `Json` for
/// bytes that do not parse.
/// Test: `n_plus_one_in_palace_json_mirror_is_refused_before_redb_opens`.
pub(crate) fn read_mirror(data_dir: &Path) -> Result<Option<u32>> {
    let path = data_dir.join(PALACE_JSON);
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(PalaceStoreError::Io { path, source }),
    };
    let mirror: Mirror =
        serde_json::from_slice(&bytes).map_err(|source| PalaceStoreError::Json { path, source })?;
    Ok(mirror.format_version)
}

/// The mirror value `save_palace` must write back (#9274).
///
/// Why: `save_palace` rebuilds `palace.json` from a `Palace`, which carries no
/// format. Without this a rename would erase the mirror, and an older binary
/// would rewrite a newer palace's metadata.
/// What: the existing mirror, or `None` when there is none; `FormatTooNew`
/// when it is above [`PALACE_FORMAT_SUPPORTED`].
/// Test: `save_palace_keeps_the_format_mirror_and_refuses_a_newer_one`.
pub(crate) fn mirror_to_preserve(data_dir: &Path, palace: &str) -> Result<Option<u32>> {
    let mirror = read_mirror(data_dir)?;
    if let Some(found) = mirror
        && found > PALACE_FORMAT_SUPPORTED
    {
        return Err(PalaceStoreError::FormatTooNew {
            palace: palace.to_string(),
            found,
            supported: PALACE_FORMAT_SUPPORTED,
        });
    }
    Ok(mirror)
}

/// Read the marker from an open database; a missing table or row is format 0.
///
/// Why: fail closed — a marker that is present and cannot be decoded is not
/// format 0, so it refuses the palace (supervisor ruling Q2, #9274).
/// Test: `an_undecodable_marker_refuses_the_palace`.
pub(crate) fn read_marker(db: &impl ReadableDatabase, kg_path: &Path) -> Result<u32> {
    let unreadable = |reason: String| PalaceStoreError::FormatMarkerUnreadable {
        palace: palace_label(kg_path).to_string(),
        path: kg_path.to_path_buf(),
        reason,
    };
    let rtx = db
        .begin_read()
        .map_err(|e| unreadable(format!("begin read: {e}")))?;
    let table = match rtx.open_table(KG_SCHEMA) {
        Ok(t) => t,
        Err(redb::TableError::TableDoesNotExist(_)) => return Ok(0),
        Err(e) => return Err(unreadable(format!("open kg_schema: {e}"))),
    };
    let Some(guard) = table
        .get(KG_SCHEMA_PALACE_FORMAT)
        .map_err(|e| unreadable(format!("read {KG_SCHEMA_PALACE_FORMAT}: {e}")))?
    else {
        return Ok(0);
    };
    let marker: KgSchemaMarker = decode_value(guard.value())
        .map_err(|e| unreadable(format!("decode {KG_SCHEMA_PALACE_FORMAT}: {e}")))?;
    Ok(marker.schema_version)
}

/// Read the marker without writing a byte of `kg_path`, when that is possible.
///
/// Why: `Database::create` writes redb's header on open, so the marker of a
/// palace that is about to be refused must be read through an `O_RDONLY`
/// handle (ADR-0067 D3 rule 1).
/// What: `Some(0)` for an absent file; `Some(v)` from a read-only open.
/// `None` when the file cannot be opened read-only — a holder has the lock,
/// it needs crash repair, or it is a redb 2 file — so the caller reads the
/// marker after its normal open instead (`gate_kg_post_open`).
/// Test: `n_plus_one_palace_is_refused_and_bytes_unchanged`,
/// `a_held_kg_is_checked_after_open_and_refused`.
pub(crate) fn read_marker_without_writing(kg_path: &Path) -> Result<Option<u32>> {
    if let Ok(false) = kg_path.try_exists() {
        return Ok(Some(0));
    }
    match crate::redb_cache::open_palace_db_read_only(kg_path) {
        Ok(db) => read_marker(&db, kg_path).map(Some),
        Err(e) => {
            tracing::debug!(path = %kg_path.display(), error = %e,
                "#9274: palace format marker deferred to the post-open check");
            Ok(None)
        }
    }
}

/// Gate a whole palace directory before any of its stores opens.
///
/// Why: `PalaceHandle::open_with_intent_purging` opens `index.usearch.redb`
/// for writing before `kg.redb`, so a gate inside the KG alone would let the
/// vector store write first.
/// What: [`gate_palace_with`] at [`PALACE_FORMAT_SUPPORTED`].
/// Test: `n_plus_one_palace_is_refused_and_bytes_unchanged`.
pub(crate) fn gate_palace(data_dir: &Path, palace: &str) -> Result<()> {
    gate_palace_with(data_dir, palace, PALACE_FORMAT_SUPPORTED)
}

/// [`gate_palace`] against an explicit supported format (the test seam).
///
/// What: mirror above `supported` → refused with no redb open. Then the
/// marker, read without writing: above `supported` → `FormatTooNew`; differing
/// from the mirror → `FormatMarkerMismatch`; otherwise [`check_format`]. When
/// the marker read is deferred, the mirror is classified here and the KG open
/// checks the marker itself.
/// Test: `n_plus_one_in_palace_json_mirror_is_refused_before_redb_opens`,
/// `markers_that_disagree_refuse_the_palace`,
/// `read_only_open_of_n_minus_1_is_refused_with_migration_hint`.
pub(crate) fn gate_palace_with(data_dir: &Path, palace: &str, supported: u32) -> Result<()> {
    let mirror = read_mirror(data_dir)?.unwrap_or(0);
    if mirror > supported {
        return check_format(palace, mirror, supported);
    }
    let Some(marker) = read_marker_without_writing(&data_dir.join(KG_FILE))? else {
        return check_format(palace, mirror, supported);
    };
    if marker != mirror && marker <= supported {
        return Err(PalaceStoreError::FormatMarkerMismatch {
            palace: palace.to_string(),
            marker,
            mirror,
        });
    }
    check_format(palace, marker, supported)
}

/// Gate one `kg.redb` file before `KgStoreRedb` opens it for writing.
///
/// Why: `kg_rebuild`, `kg_twin_merge` and the store-snapshot reader open the
/// KG directly, without a palace handle.
/// What: `Ok(true)` when the marker was read and accepted; `Ok(false)` when
/// the read was deferred and [`gate_kg_post_open`] must run.
/// Test: `a_direct_kg_open_of_an_n_plus_one_file_is_refused`.
pub(crate) fn gate_kg_pre_open(kg_path: &Path) -> Result<bool> {
    let Some(found) = read_marker_without_writing(kg_path)? else {
        return Ok(false);
    };
    check_format(&palace_label(kg_path), found, PALACE_FORMAT_SUPPORTED)?;
    Ok(true)
}

/// Gate an opened `kg.redb` before its first write transaction.
///
/// Test: `a_held_kg_is_checked_after_open_and_refused`.
pub(crate) fn gate_kg_post_open(db: &impl ReadableDatabase, kg_path: &Path) -> Result<()> {
    let found = read_marker(db, kg_path)?;
    check_format(&palace_label(kg_path), found, PALACE_FORMAT_SUPPORTED)
}

/// Whether an open error is a format refusal, through any context layers.
///
/// Why: a refusal never resolves by waiting, so the open retry loops stop on
/// it (#9274), as they do for `is_incompatible_format_refusal`.
/// Test: `format_errors_are_not_retried`.
pub fn is_format_refusal(err: &anyhow::Error) -> bool {
    err.chain()
        .filter_map(|e| e.downcast_ref::<PalaceStoreError>())
        .any(PalaceStoreError::is_format_refusal)
}

#[cfg(test)]
#[path = "palace_format_tests.rs"]
mod tests;
