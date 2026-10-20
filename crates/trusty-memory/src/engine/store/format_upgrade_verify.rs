//! The read-only checks behind `trusty-memory palace migrate --verify` (#10094).
//!
//! Why: a user who converted a palace by hand, or whose migration was
//! interrupted, needs to see which markers are present, missing or
//! mismatched, without anything being written. The same checks run after a
//! migration, so `--verify` and the post-migration check cannot disagree.
//! What: [`verify`] reads the format status (`palace_format_status`), both
//! `kg_schema` markers, the `palace.json` mirror, the migration record, the
//! drawer and triple counts, and the newest format-migration backup, and
//! lists every gap. `kg.redb` is read through an `O_RDONLY` open, or through
//! a copy in the temp dir when another holder has it, so the palace
//! directory is never written.
//! Test: format_upgrade_tests.rs —
//! `verify_reports_the_gaps_of_a_hand_shaped_palace`,
//! `dry_run_reports_the_plan_and_writes_nothing`.

use super::concurrent_open::{OpenIntent, try_open_primary_or_snapshot};
use super::format_backup::{backup_data_root, backup_root, newest_complete_backup, verify_backup};
use super::kg_redb::KgStoreRedb;
use super::kg_store::{
    DRAWERS, KG_SCHEMA, KG_SCHEMA_TRIPLE_KEY, KG_TRIPLE_KEY_SCHEMA_VERSION, KgSchemaMarker,
    TRIPLES, decode_value,
};
use super::palace_format::{
    KG_FILE, KG_SCHEMA_PALACE_FORMAT, MIGRATION_RECORD, PALACE_FORMAT_SUPPORTED,
    PalaceFormatStatus, palace_format_status, read_mirror,
};
use anyhow::{Context, Result};
use redb::{ReadableDatabase, ReadableTableMetadata, TableDefinition};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// A palace's format state, as `palace migrate` reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FormatStatus {
    /// At this binary's format.
    Current,
    /// Behind; a migration would convert it.
    NeedsMigration,
    /// Behind, with a `format_migration.json` left by an interrupted run.
    Interrupted,
    /// `kg.redb` could not be read without writing (held, or crash repair).
    Unverified,
    /// Refused outright: newer, split markers, or unreadable.
    Refused,
}

impl FormatStatus {
    /// The status in the words `palace migrate` prints.
    pub fn label(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::NeedsMigration => "needs migration",
            Self::Interrupted => "interrupted",
            Self::Unverified => "unverified",
            Self::Refused => "refused",
        }
    }
}

/// One marker as found against the value this binary expects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum MarkerCheck {
    /// Present at the expected value.
    Present { value: u32 },
    /// Absent where one is expected.
    Missing,
    /// Present at another value.
    Mismatched { value: u32, want: u32 },
    /// Nothing to check: no `kg.redb`, or an empty KG stamped at creation.
    NotApplicable,
}

impl MarkerCheck {
    /// Classify `found` against `want`.
    fn of(found: Option<u32>, want: u32) -> Self {
        match found {
            Some(value) if value == want => Self::Present { value },
            Some(value) => Self::Mismatched { value, want },
            None => Self::Missing,
        }
    }

    /// Whether this check is a gap.
    fn is_gap(self) -> bool {
        matches!(self, Self::Missing | Self::Mismatched { .. })
    }
}

/// What `kg.redb` holds, read without writing it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct KgFacts {
    pub(crate) palace_format: Option<u32>,
    pub(crate) triple_key: Option<u32>,
    pub(crate) drawers: u64,
    pub(crate) triples: u64,
    /// Read from a copy because another holder has the live file.
    pub(crate) from_copy: bool,
}

/// The `--verify` report for one palace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifyReport {
    pub palace: String,
    pub dir: PathBuf,
    pub status: FormatStatus,
    /// Why the status is not `current`, when it is not.
    pub status_detail: Option<String>,
    /// `kg_schema.palace_format`.
    pub palace_format: MarkerCheck,
    /// `kg_schema.triple_key`.
    pub triple_key: MarkerCheck,
    /// `palace.json` `format_version`.
    pub mirror: MarkerCheck,
    /// Whether `format_migration.json` is present.
    pub migration_record: bool,
    pub drawers: u64,
    pub triples: u64,
    /// `kg.redb` was read from a temp copy (another holder has it).
    pub read_from_copy: bool,
    /// Where this palace's format-migration backups live.
    pub backup_root: PathBuf,
    /// The newest complete format-migration backup, if any.
    pub backup: Option<PathBuf>,
    /// Whether that backup re-hashes to its manifest; `None` with no backup.
    pub backup_verified: Option<bool>,
    /// Every failed check, in words; empty means the palace verifies.
    pub gaps: Vec<String>,
}

/// Where `id`'s format-migration backups live, for the palace at `dir`.
pub(crate) fn backup_root_of(dir: &Path, id: &str) -> PathBuf {
    backup_root(&backup_data_root(dir), id)
}

/// The format status of `dir`, the format found, and a note on how it was read.
///
/// Test: `dry_run_reports_the_plan_and_writes_nothing`,
/// `a_palace_this_process_holds_reads_as_current_through_a_copy`.
pub(crate) fn status_of(dir: &Path) -> (FormatStatus, Option<u32>, Option<String>) {
    match palace_format_status(dir) {
        Ok(PalaceFormatStatus::Current) => {
            (FormatStatus::Current, Some(PALACE_FORMAT_SUPPORTED), None)
        }
        Ok(PalaceFormatStatus::NeedsMigration { found, interrupted }) => {
            let status = if interrupted {
                FormatStatus::Interrupted
            } else {
                FormatStatus::NeedsMigration
            };
            (status, Some(found), None)
        }
        // #10094: on the daemon route the daemon holds the KG it opened, and
        // its writer gate admitted it at that open (as `writer_must_migrate`).
        Ok(PalaceFormatStatus::Unverified) if KgStoreRedb::is_cached(&dir.join(KG_FILE)) => (
            FormatStatus::Current,
            Some(PALACE_FORMAT_SUPPORTED),
            Some("held open by this daemon, which checked its format at open".into()),
        ),
        Ok(PalaceFormatStatus::Unverified) => (
            FormatStatus::Unverified,
            None,
            Some("kg.redb is held by another process or needs crash repair".into()),
        ),
        Err(e) => (FormatStatus::Refused, None, Some(e.to_string())),
    }
}

/// Read `kg.redb`'s markers and counts without writing the palace directory.
///
/// Why: #10094 — `--dry-run` and `--verify` write nothing, and the daemon
/// route runs while the daemon holds the file.
/// What: `None` with no file. An `O_RDONLY` open first; when that fails (a
/// holder, or crash repair) a copy in the temp dir, removed on return.
/// Test: `verify_reports_the_gaps_of_a_hand_shaped_palace`.
pub(crate) fn read_kg_facts(dir: &Path) -> Result<Option<KgFacts>> {
    let kg = dir.join(KG_FILE);
    if !kg
        .try_exists()
        .with_context(|| format!("stat {}", kg.display()))?
    {
        return Ok(None);
    }
    match trusty_common::redb_cache::open_palace_db_read_only(&kg) {
        Ok(db) => facts_of(&db, false).map(Some),
        Err(e) => {
            tracing::debug!(path = %kg.display(), error = %e,
                "#10094: kg.redb not readable in place; reading a copy");
            let (db, _guard, _) = try_open_primary_or_snapshot(&kg, OpenIntent::Observer)
                .with_context(|| format!("read a copy of {}", kg.display()))?;
            facts_of(&*db, true).map(Some)
        }
    }
}

/// The markers and row counts of an open database.
fn facts_of(db: &impl ReadableDatabase, from_copy: bool) -> Result<KgFacts> {
    let rtx = db.begin_read().context("begin read of kg.redb")?;
    let schema_row = |key: &str| -> Result<Option<u32>> {
        let table = match rtx.open_table(KG_SCHEMA) {
            Ok(t) => t,
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(None),
            Err(e) => return Err(e).context("open kg_schema"),
        };
        let Some(guard) = table.get(key).with_context(|| format!("read {key}"))? else {
            return Ok(None);
        };
        let marker: KgSchemaMarker =
            decode_value(guard.value()).with_context(|| format!("decode {key}"))?;
        Ok(Some(marker.schema_version))
    };
    let rows = |def: TableDefinition<&[u8], &[u8]>| -> Result<u64> {
        match rtx.open_table(def) {
            Ok(t) => t.len().context("count rows"),
            Err(redb::TableError::TableDoesNotExist(_)) => Ok(0),
            Err(e) => Err(e).context("open table"),
        }
    };
    Ok(KgFacts {
        palace_format: schema_row(KG_SCHEMA_PALACE_FORMAT)?,
        triple_key: schema_row(KG_SCHEMA_TRIPLE_KEY)?,
        drawers: rows(DRAWERS)?,
        triples: rows(TRIPLES)?,
        from_copy,
    })
}

/// Run every post-migration check on one palace, read-only.
///
/// Why: see the module doc; the post-migration check is this function.
/// What: gaps are a status other than current, a missing or mismatched
/// marker or mirror, a marker and mirror that disagree, and a migration
/// record left on disk. A KG with no marker row and no data is new, and its
/// markers are not applicable. When `kg.redb` is held, the status comes from
/// the copy: current only if the marker, the mirror and the absence of a
/// record all say so. The backup is reported, never counted as a gap.
/// Test: `verify_reports_the_gaps_of_a_hand_shaped_palace`,
/// `palace_migrate_takes_a_backup_and_reports_it`,
/// `a_palace_this_process_holds_reads_as_current_through_a_copy`.
pub(crate) fn verify(dir: &Path, id: &str) -> Result<VerifyReport> {
    let want = PALACE_FORMAT_SUPPORTED;
    let (mut status, _, mut status_detail) = status_of(dir);
    let mirror_found = read_mirror(dir).ok().flatten();
    let facts = read_kg_facts(dir)?;
    let migration_record = dir
        .join(MIGRATION_RECORD)
        .try_exists()
        .with_context(|| format!("stat {}", dir.join(MIGRATION_RECORD).display()))?;
    let empty = facts.is_none_or(|f| f.palace_format.is_none() && f.drawers + f.triples == 0);
    let (palace_format, triple_key) = match facts {
        _ if empty => (MarkerCheck::NotApplicable, MarkerCheck::NotApplicable),
        Some(f) => (
            MarkerCheck::of(f.palace_format, want),
            MarkerCheck::of(f.triple_key, KG_TRIPLE_KEY_SCHEMA_VERSION),
        ),
        None => (MarkerCheck::NotApplicable, MarkerCheck::NotApplicable),
    };
    let mirror = MarkerCheck::of(mirror_found, want);
    if status == FormatStatus::Unverified
        && let Some(f) = facts
        && f.palace_format == Some(want)
        && mirror_found == Some(want)
        && !migration_record
    {
        status = FormatStatus::Current;
        status_detail = Some("read from a copy: another process holds kg.redb".into());
    }

    let mut gaps = Vec::new();
    if status != FormatStatus::Current {
        let why = status_detail.as_deref().unwrap_or("behind this binary");
        gaps.push(format!(
            "format status is {}, not current ({why})",
            status.label()
        ));
    }
    for (name, check) in [
        ("kg_schema palace_format marker", palace_format),
        ("kg_schema triple_key marker", triple_key),
        ("palace.json format_version mirror", mirror),
    ] {
        if check.is_gap() {
            gaps.push(format!("{name} is {}", describe(check)));
        }
    }
    if let Some(f) = facts
        && !empty
        && f.palace_format != mirror_found
    {
        gaps.push(format!(
            "the kg_schema marker ({}) and the palace.json mirror ({}) disagree",
            shown(f.palace_format),
            shown(mirror_found)
        ));
    }
    if migration_record {
        gaps.push(format!(
            "{MIGRATION_RECORD} is present: a migration was interrupted"
        ));
    }

    let root = backup_root_of(dir, id);
    let backup = newest_complete_backup(&root)?;
    let backup_verified = backup.as_deref().map(|b| verify_backup(b).is_ok());
    Ok(VerifyReport {
        palace: id.to_string(),
        dir: dir.to_path_buf(),
        status,
        status_detail,
        palace_format,
        triple_key,
        mirror,
        migration_record,
        drawers: facts.map_or(0, |f| f.drawers),
        triples: facts.map_or(0, |f| f.triples),
        read_from_copy: facts.is_some_and(|f| f.from_copy),
        backup_root: root,
        backup,
        backup_verified,
        gaps,
    })
}

/// A marker check in words: `present (2)`, `missing`, `mismatched (1, want 2)`.
pub fn describe(check: MarkerCheck) -> String {
    match check {
        MarkerCheck::Present { value } => format!("present ({value})"),
        MarkerCheck::Missing => "missing".into(),
        MarkerCheck::Mismatched { value, want } => format!("mismatched ({value}, want {want})"),
        MarkerCheck::NotApplicable => "not applicable (new, empty KG)".into(),
    }
}

/// An optional format in words.
fn shown(v: Option<u32>) -> String {
    v.map_or_else(|| "absent".into(), |v| v.to_string())
}
