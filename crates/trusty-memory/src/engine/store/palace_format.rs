//! Palace format marker, its `palace.json` mirror, and the open gate (#9274).
//!
//! Why: ADR-0067 D3 rule 1 — a palace written by a newer release is refused
//! under every open intent, before any byte under it changes. The triple-key
//! marker cannot do this job: it reads a newer version as "already migrated",
//! and ADR-0067 D1 freezes it.
//! What: the authoritative marker is the `palace_format` row of `kg.redb`'s
//! `kg_schema` table; the mirror is `palace.json`'s `format_version`. A KG with
//! no row and no data is new and is stamped at [`PALACE_FORMAT_SUPPORTED`] when
//! it is created; a KG with data and no row is format 0. [`classify_with`]
//! turns the two markers, plus the migration-in-progress record, into
//! "current" or "behind"; the migration itself (the 0 → 1 → 2 chain, #9918)
//! lives in `format_migration.rs`.
//! Test: palace_format_tests.rs — `n_plus_one_palace_is_refused_and_bytes_unchanged`,
//! and the other arms listed on each function below.

use super::chat_sessions::ChatSessionStoreError;
use super::hnsw_store::HnswStoreError;
use super::kg::{KgError, internal_of};
use super::kg_store::{
    DRAWERS, KG_SCHEMA, KG_SCHEMA_TRIPLE_KEY, KG_TRIPLE_KEY_SCHEMA_VERSION, KgSchemaMarker,
    TRIPLES, decode_value, encode_value,
};
use super::palace_store::PalaceStoreError;
use super::write_deadline::palace_label;
use crate::engine::EngineError;
use crate::engine::share::ShareError;
use redb::{ReadableDatabase, ReadableTable};
use serde::Deserialize;
use std::path::Path;

/// The palace format this binary reads and writes (ADR-0067 D1).
///
/// #9275: 2 permits `{uuid}#{n}` chunk rows in `index.usearch.redb`
/// (`vector_key.rs`); no table shape changes. A palace at format 0 or 1 is
/// migrated by a writer-intent open, step by step (#9918); a palace stamped 3
/// or higher is refused.
pub const PALACE_FORMAT_SUPPORTED: u32 = 2;

/// The oldest format this binary migrates; older is `FormatTooOld` (#9918).
pub(crate) const OLDEST_MIGRATABLE_FORMAT: u32 = 0;

/// The `kg_schema` key that holds the authoritative palace format.
pub(crate) const KG_SCHEMA_PALACE_FORMAT: &str = "palace_format";

/// The KG file the marker lives in, inside the palace directory.
pub(crate) const KG_FILE: &str = "kg.redb";

/// The metadata file that carries the mirror.
pub(crate) const PALACE_JSON: &str = "palace.json";

/// The migration-in-progress record, inside the palace directory (#9274).
pub const MIGRATION_RECORD: &str = "format_migration.json";

type Result<T> = std::result::Result<T, PalaceStoreError>;

/// The one `palace.json` field the gate reads; every other field is ignored.
#[derive(Deserialize)]
struct Mirror {
    #[serde(default)]
    format_version: Option<u32>,
}

/// What a read of `kg.redb`'s marker found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KgMarker {
    /// A `palace_format` row names this format.
    Stamped(u32),
    /// No row, and the KG holds drawers or triples: format 0.
    Unstamped,
    /// No file, or no row and no drawer or triple: a new KG, stamped when created.
    Fresh,
    /// The file could not be opened read-only; the post-open check reads it.
    Deferred,
}

/// Whether a palace opens as it is or needs the migration first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FormatState {
    /// At the supported format. `repair_mirror` is set when the mirror is
    /// absent or behind the marker, which a writer rewrites (ADR-0067 D1).
    Current { repair_mirror: bool },
    /// One format behind, or a migration record is present: a writer migrates
    /// (or resumes), a read-only open is refused.
    Behind { found: u32 },
    /// `kg.redb` could not be read without writing (held, or in need of crash
    /// repair). A writer reads it on an exclusive open before any store opens;
    /// a read-only open relies on the KG's post-open gate.
    Unverified,
}

/// The format state of a palace, for `trusty-memory doctor` (#9274).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
#[cfg_attr(not(feature = "server"), allow(dead_code))]
pub(crate) enum PalaceFormatStatus {
    /// At this binary's format.
    Current,
    /// Behind; the daemon migrates it at its next writer-intent open.
    /// `interrupted` is set when a migration record is present.
    NeedsMigration { found: u32, interrupted: bool },
    /// `kg.redb` cannot be read without writing — a running daemon holds it,
    /// or it needs crash repair — so the format is not known (#9274).
    Unverified,
}

/// Compare a palace's format with the one this binary supports.
///
/// Why: one rule set for every open path (ADR-0067 D3 rules 1 and 2).
/// What: newer → `FormatTooNew`; equal → `Ok`; behind, from
/// [`OLDEST_MIGRATABLE_FORMAT`] up → `FormatNeedsMigration` (#9918: the chain
/// migrates it); older → `FormatTooOld`.
/// Test: `check_format_classifies_every_distance`.
pub(crate) fn check_format(palace: &str, found: u32, supported: u32) -> Result<()> {
    check_format_with(palace, found, supported, OLDEST_MIGRATABLE_FORMAT)
}

/// [`check_format`] with an explicit oldest migratable format, for its test.
fn check_format_with(palace: &str, found: u32, supported: u32, oldest: u32) -> Result<()> {
    let palace = palace.to_string();
    if found > supported {
        Err(PalaceStoreError::FormatTooNew {
            palace,
            found,
            supported,
        })
    } else if found == supported {
        Ok(())
    } else if found >= oldest {
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

/// Read the `palace.json` format mirror; an absent file or field is `None`.
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

/// Read the marker from an open database (never [`KgMarker::Deferred`]).
///
/// Why: fail closed — a marker that is present and cannot be decoded is not
/// format 0, so it refuses the palace (supervisor ruling Q2, #9274). A KG with
/// no row is format 0 only when it holds data; an empty one is new.
/// Test: `an_undecodable_marker_refuses_the_palace`,
/// `a_new_palace_is_stamped_at_the_supported_format`.
pub(crate) fn read_marker(db: &impl ReadableDatabase, kg_path: &Path) -> Result<KgMarker> {
    let unreadable = |reason: String| PalaceStoreError::FormatMarkerUnreadable {
        palace: palace_label(kg_path).to_string(),
        path: kg_path.to_path_buf(),
        reason,
    };
    let rtx = db
        .begin_read()
        .map_err(|e| unreadable(format!("begin read: {e}")))?;
    match rtx.open_table(KG_SCHEMA) {
        Ok(table) => {
            if let Some(guard) = table
                .get(KG_SCHEMA_PALACE_FORMAT)
                .map_err(|e| unreadable(format!("read {KG_SCHEMA_PALACE_FORMAT}: {e}")))?
            {
                let marker: KgSchemaMarker = decode_value(guard.value())
                    .map_err(|e| unreadable(format!("decode {KG_SCHEMA_PALACE_FORMAT}: {e}")))?;
                return Ok(KgMarker::Stamped(marker.schema_version));
            }
        }
        Err(redb::TableError::TableDoesNotExist(_)) => {}
        Err(e) => return Err(unreadable(format!("open kg_schema: {e}"))),
    }
    // #9274: an empty KG has nothing to migrate.
    for (name, def) in [("drawers", DRAWERS), ("triples", TRIPLES)] {
        let has_rows = match rtx.open_table(def) {
            Ok(t) => t
                .first()
                .map_err(|e| unreadable(format!("probe {name}: {e}")))?
                .is_some(),
            Err(redb::TableError::TableDoesNotExist(_)) => false,
            Err(e) => return Err(unreadable(format!("open {name}: {e}"))),
        };
        if has_rows {
            return Ok(KgMarker::Unstamped);
        }
    }
    Ok(KgMarker::Fresh)
}

/// Read the marker without writing a byte of `kg_path`, when that is possible.
///
/// Why: `Database::create` writes redb's header on open, so the marker of a
/// palace that is about to be refused must be read through an `O_RDONLY`
/// handle (ADR-0067 D3 rule 1).
/// What: [`KgMarker::Fresh`] for an absent file; the marker from a read-only
/// open; [`KgMarker::Deferred`] when the file cannot be opened read-only — a
/// holder has the lock, it needs crash repair, or it is a redb 2 file.
/// Test: `n_plus_one_palace_is_refused_and_bytes_unchanged`,
/// `a_held_kg_is_checked_after_open_and_refused`.
pub(crate) fn read_marker_without_writing(kg_path: &Path) -> Result<KgMarker> {
    if let Ok(false) = kg_path.try_exists() {
        return Ok(KgMarker::Fresh);
    }
    match trusty_common::redb_cache::open_palace_db_read_only(kg_path) {
        Ok(db) => read_marker(&db, kg_path),
        Err(e) => {
            tracing::debug!(path = %kg_path.display(), error = %e,
                "#9274: palace format marker deferred to the post-open check");
            Ok(KgMarker::Deferred)
        }
    }
}

/// Classify a whole palace directory before any of its stores opens.
///
/// Why: `PalaceHandle::open_with_intent_purging` opens `index.usearch.redb`
/// before `kg.redb`, so the gate runs on the directory first.
/// What: a mirror or marker above `supported` → `FormatTooNew`. A migration
/// record → `Behind` (resume), unless it names a move this binary does not
/// run ([`refuse_foreign_record`]). A new KG → `Current`; a deferred marker read →
/// `Unverified`. Otherwise the
/// marker wins over a lower mirror (ADR-0067 D1); a mirror above the marker
/// with no record is `FormatMarkerMismatch` (fail closed); the marker is then
/// classified by [`check_format`], where behind is `Behind`; a record's
/// `Behind` names the format its move started at (#9918).
/// Test: `n_plus_one_in_palace_json_mirror_is_refused_before_redb_opens`,
/// `markers_that_disagree_refuse_the_palace`,
/// `a_lower_mirror_is_repaired_by_a_writer_and_the_marker_wins`,
/// `a_stale_0_to_1_record_is_resumed_by_the_chain`.
pub(crate) fn classify_with(data_dir: &Path, palace: &str, supported: u32) -> Result<FormatState> {
    let mirror = read_mirror(data_dir)?;
    let m = mirror.unwrap_or(0);
    if m > supported {
        check_format(palace, m, supported)?;
    }
    let marker = read_marker_without_writing(&data_dir.join(KG_FILE))?;
    if let KgMarker::Stamped(v) = marker
        && v > supported
    {
        check_format(palace, v, supported)?;
    }
    if let Some(found) = refuse_foreign_record(data_dir, palace, supported)? {
        return Ok(FormatState::Behind { found });
    }
    let repair_mirror = mirror != Some(supported);
    let found = match marker {
        KgMarker::Fresh => return Ok(FormatState::Current { repair_mirror }),
        KgMarker::Deferred => return Ok(FormatState::Unverified),
        KgMarker::Stamped(v) => v,
        KgMarker::Unstamped => 0,
    };
    if m > found {
        return Err(PalaceStoreError::FormatMarkerMismatch {
            palace: palace.to_string(),
            marker: found,
            mirror: m,
        });
    }
    match check_format(palace, found, supported) {
        Ok(()) => Ok(FormatState::Current { repair_mirror }),
        Err(PalaceStoreError::FormatNeedsMigration { found, .. }) => {
            Ok(FormatState::Behind { found })
        }
        Err(e) => Err(e),
    }
}

/// The format a present migration record moves from, refusing one of a move
/// this binary does not run.
///
/// Why: without this it read as `Behind` and failed later as a generic
/// `FormatMigrationFailed` (#9275, plan Q7). #9918: an older release's
/// interrupted 0 → 1 is resumed by the chain, so it is not foreign.
/// What: `Ok(None)` with no record. A record whose `to` is above `supported`
/// → `FormatTooNew`; whose `from` [`check_format`] refuses → that refusal.
/// Otherwise `Ok(Some(from))`. A record that does not parse is
/// `Ok(Some(supported - 1))`: the writer's resume fails closed on it.
/// Test: `a_stale_0_to_1_record_is_resumed_by_the_chain`.
pub(crate) fn refuse_foreign_record(
    data_dir: &Path,
    palace: &str,
    supported: u32,
) -> Result<Option<u32>> {
    let record = data_dir.join(MIGRATION_RECORD);
    if !record.try_exists().map_err(|source| PalaceStoreError::Io {
        path: record,
        source,
    })? {
        return Ok(None);
    }
    if let Ok(Some(rec)) = super::format_migration::read_record(data_dir) {
        if rec.to > supported {
            check_format(palace, rec.to, supported)?;
        }
        return match check_format(palace, rec.from, supported) {
            Ok(()) | Err(PalaceStoreError::FormatNeedsMigration { .. }) => Ok(Some(rec.from)),
            Err(e) => Err(e),
        };
    }
    Ok(Some(supported.saturating_sub(1)))
}

/// Refuse a palace a read-only caller cannot use: newer, too old, split, or
/// behind (ADR-0067 D3 rule 2), at an explicit supported format.
///
/// Test: `read_only_open_of_n_minus_1_is_refused_with_migration_hint`.
#[cfg(test)]
pub(crate) fn gate_read_only_with(data_dir: &Path, palace: &str, supported: u32) -> Result<()> {
    match classify_with(data_dir, palace, supported)? {
        FormatState::Current { .. } | FormatState::Unverified => Ok(()),
        FormatState::Behind { found } => Err(PalaceStoreError::FormatNeedsMigration {
            palace: palace.to_string(),
            found,
            supported,
        }),
    }
}

/// Gate a palace directory for a store whose own bytes no migration changes.
///
/// Why: `chat_sessions.redb` has one shape in formats 0 and 1, and the chat
/// tools open it without a palace handle, so a format-0 palace keeps its chat
/// store while the daemon has yet to migrate it. A newer, split or unreadable
/// palace is still refused before redb opens.
/// What: [`classify_with`] at [`PALACE_FORMAT_SUPPORTED`]; `Behind` passes.
/// Test: `chat_session_store_refuses_a_newer_palace_with_bytes_unchanged`.
pub(crate) fn gate_palace(data_dir: &Path, palace: &str) -> Result<()> {
    classify_with(data_dir, palace, PALACE_FORMAT_SUPPORTED).map(|_| ())
}

/// The format status of one palace directory, without writing a byte.
///
/// Why: `trusty-memory doctor` reports a palace the daemon has yet to migrate
/// as its own state (#9274), apart from one it refuses.
/// What: [`classify_with`] at [`PALACE_FORMAT_SUPPORTED`]. A refusal is the
/// `Err` arm, so the caller can print its message. A deferred marker read is
/// `Unverified`, never `Current`: the daemon may refuse that palace.
/// Test: `status_reports_needs_migration_for_a_format_1_palace`,
/// `status_reports_a_held_kg_as_unverified`.
#[cfg_attr(not(feature = "server"), allow(dead_code))]
pub(crate) fn palace_format_status(data_dir: &Path) -> Result<PalaceFormatStatus> {
    let label = data_dir.file_name().map_or_else(
        || data_dir.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    Ok(
        match classify_with(data_dir, &label, PALACE_FORMAT_SUPPORTED)? {
            FormatState::Current { .. } => PalaceFormatStatus::Current,
            // #9274: a held or crash-repair KG is not known to be current.
            FormatState::Unverified => PalaceFormatStatus::Unverified,
            FormatState::Behind { found } => PalaceFormatStatus::NeedsMigration {
                found,
                interrupted: data_dir.join(MIGRATION_RECORD).exists(),
            },
        },
    )
}

/// Gate one `kg.redb` file before `KgStoreRedb` opens it.
///
/// Why: `kg_rebuild`, `kg_twin_merge` and the store-snapshot reader open the
/// KG directly, without a palace handle, so a direct open must refuse a newer
/// or an unmigrated file before `Database::create` writes its header.
/// What: a stamped marker goes through [`check_format`]; a KG with data and no
/// marker is format 0 and refused as behind; a new or deferred KG passes, and
/// [`gate_kg_post_open`] reads it after the open.
/// Test: `a_direct_kg_open_of_an_n_plus_one_file_is_refused`,
/// `a_direct_kg_open_of_a_format_0_file_is_refused`.
pub(crate) fn gate_kg_pre_open(kg_path: &Path) -> Result<()> {
    let label = palace_label(kg_path);
    match read_marker_without_writing(kg_path)? {
        KgMarker::Stamped(v) => check_format(&label, v, PALACE_FORMAT_SUPPORTED),
        KgMarker::Unstamped => check_format(&label, 0, PALACE_FORMAT_SUPPORTED),
        KgMarker::Fresh | KgMarker::Deferred => Ok(()),
    }
}

/// Gate an opened `kg.redb` before its first write transaction.
///
/// Why: the pre-open read can be deferred, and the file can change between it
/// and the open, so every fresh open re-reads the marker on the database it
/// will write (PR2 critic MEDIUM, #9274).
/// What: `Ok(true)` for a new KG, which the caller stamps in its init
/// transaction; `Ok(false)` at the supported format; otherwise the refusal.
/// Test: `a_held_kg_is_checked_after_open_and_refused`,
/// `the_post_open_gate_reads_the_opened_file_not_the_pre_open_marker`.
pub(crate) fn gate_kg_post_open(db: &impl ReadableDatabase, kg_path: &Path) -> Result<bool> {
    let label = palace_label(kg_path);
    match read_marker(db, kg_path)? {
        KgMarker::Fresh => Ok(true),
        KgMarker::Stamped(v) => check_format(&label, v, PALACE_FORMAT_SUPPORTED).map(|()| false),
        KgMarker::Unstamped | KgMarker::Deferred => {
            check_format(&label, 0, PALACE_FORMAT_SUPPORTED).map(|()| false)
        }
    }
}

/// Write the `palace_format` and triple-key markers inside `wtx`.
///
/// Why: both the creation of a new KG and each migration step (#9918)
/// commit the format in the same transaction as their KG change
/// (ADR-0067 D3 rule 4).
/// What: overwrites both `kg_schema` rows; format 1 implies triple key v1.
/// Test: `a_new_palace_is_stamped_at_the_supported_format`,
/// `a_format_1_palace_migrates_to_2_at_first_writer_open`.
pub(crate) fn stamp_format(wtx: &redb::WriteTransaction, format: u32) -> anyhow::Result<()> {
    use anyhow::Context;
    let mut schema = wtx.open_table(KG_SCHEMA).context("open kg_schema")?;
    for (key, version) in [
        (KG_SCHEMA_TRIPLE_KEY, KG_TRIPLE_KEY_SCHEMA_VERSION),
        (KG_SCHEMA_PALACE_FORMAT, format),
    ] {
        let bytes = encode_value(&KgSchemaMarker {
            schema_version: version,
        })
        .context("encode KgSchemaMarker")?;
        schema
            .insert(key, bytes.as_slice())
            .with_context(|| format!("stamp {key}"))?;
    }
    Ok(())
}

/// Whether an open error is a format refusal, through any context layers.
///
/// Why: a refusal never resolves by waiting, so the open retry loops stop on
/// it (#9274), as they do for `is_incompatible_format_refusal`.
/// Test: `format_errors_are_not_retried`,
/// `format_refusal_is_seen_through_kg_error`.
pub(crate) fn is_format_refusal(err: &anyhow::Error) -> bool {
    palace_store_errors(err).any(PalaceStoreError::is_format_refusal)
}

/// Every `PalaceStoreError` in an error's chain, bare or inside a `KgError`,
/// a `ChatSessionStoreError`, a `HnswStoreError`, a `ShareError` or an
/// `EngineError`, and in the internal error any of them boxes.
///
/// Why (#9278 Q4, T2): each `Palace` variant is transparent, so `chain()`
/// steps past its payload; only a match on the wrapper reaches it. A boxed
/// variant hides its whole internal error the same way (M1, #9914 MEDIUM).
/// Test: `format_refusal_is_seen_through_kg_error`,
/// `count_refusal_sees_a_kg_error_palace`,
/// `chat_session_store_refuses_a_newer_palace_with_bytes_unchanged`,
/// `share_error_keeps_typed_sources_visible_to_the_classifiers`,
/// `a_refusal_boxed_in_any_wrapper_other_is_seen_by_both_classifiers`,
/// `classifiers_see_a_refusal_through_kg_and_hnsw_variants`.
pub(crate) fn palace_store_errors(
    err: &anyhow::Error,
) -> Box<dyn Iterator<Item = &PalaceStoreError> + '_> {
    fn hnsw(h: &HnswStoreError) -> Option<&PalaceStoreError> {
        match h {
            HnswStoreError::Palace(p) => Some(p),
            _ => None,
        }
    }
    Box::new(err.chain().flat_map(|e| {
        let nested = internal_of(e).into_iter().flat_map(palace_store_errors);
        e.downcast_ref::<PalaceStoreError>()
            .or_else(|| e.downcast_ref::<KgError>()?.as_palace_store())
            .or_else(|| match e.downcast_ref::<ChatSessionStoreError>()? {
                ChatSessionStoreError::Palace(p) => Some(p),
                _ => None,
            })
            .or_else(|| hnsw(e.downcast_ref::<HnswStoreError>()?))
            // #9278 T3a: a share path carries both wrappers.
            .or_else(|| match e.downcast_ref::<ShareError>()? {
                ShareError::Palace(p) => Some(p),
                ShareError::Hnsw(h) => hnsw(h),
                _ => None,
            })
            .or_else(|| match e.downcast_ref::<EngineError>()? {
                EngineError::Palace(p) => Some(p),
                // #9278 T3c-1: the write paths carry both wrappers too.
                EngineError::Kg(k) => k.as_palace_store(),
                EngineError::Hnsw(h) => hnsw(h),
                _ => None,
            })
            .into_iter()
            .chain(nested)
    }))
}

#[cfg(test)]
#[path = "palace_format_tests.rs"]
mod tests;
