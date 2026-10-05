//! Durable trail of the drawer deletions that maintenance paths make (#8732).
//!
//! Why: in #8729, 70 drawers left three live palaces within minutes and nothing
//! recorded why. The dream and purge summaries logged at `info`, below the
//! daemon's default `warn` filter, and none of them named a drawer. An operator
//! could not tell which drawer a removed one duplicated, or at what score.
//! What: every drawer a maintenance path deletes is appended as one JSON line to
//! `<palace data_dir>/maintenance_deletions.jsonl`: time, palace, drawer id,
//! reason, the surviving drawer id and cosine score where one exists, and the
//! pid of the deleting process. Each deleting pass also logs one summary at
//! `warn`. #8729 (owner ruling): every removal is also logged on its own `warn`
//! line naming the palace, drawer id and reason, so the daemon log alone shows
//! which drawers went and why. `trusty-memory palace deletions` reads the file
//! back.
//! User-initiated deletions (`memory_forget`, the HTTP drawer delete) call
//! [`PalaceHandle::forget`] and are not recorded, with one exception (#9172):
//! forgetting a drawer this journal names as a dedup survivor is recorded as
//! [`DeletionReason::ForgetOfMergedSurvivor`], because merged-in text leaves
//! with it.
//!
//! A failed append does not undo or block the deletion. The full record is
//! logged at `error` instead, which the default filter keeps. A palace with no
//! data dir (in-memory) logs the record at `warn`.
//! Test: `maintenance_log_tests::dream_dedup_records_the_removed_and_surviving_drawer`,
//! `maintenance_log_tests::every_maintenance_removal_logs_its_id_and_reason`,
//! `maintenance_log_tests::a_failed_record_write_logs_the_record_and_still_deletes`.

use crate::memory_core::palace::{Drawer, PalaceId};
use crate::memory_core::retrieval::{ForgetOutcome, PalaceHandle};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// File name of the per-palace deletion journal, inside the palace data dir.
pub const MAINTENANCE_LOG_FILENAME: &str = "maintenance_deletions.jsonl";
/// File name the journal is rotated to once it passes [`ROTATE_AT_BYTES`].
pub const MAINTENANCE_LOG_ROTATED_FILENAME: &str = "maintenance_deletions.1.jsonl";
/// Journal size that triggers one rotation. About 16k records at ~250 bytes.
pub(crate) const ROTATE_AT_BYTES: u64 = 4 * 1024 * 1024;

/// Which maintenance path deleted a drawer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeletionReason {
    /// Dream dedup merged the drawer into a near-duplicate and forgot it.
    DreamDedup,
    /// Dream content prune matched the blocklist or the word-count floor.
    DreamContentPrune,
    /// Dream prune: decayed importance at the floor and older than 30 days.
    DreamPrune,
    /// Room consolidation evicted an original a canonical drawer superseded.
    SemanticConsolidation,
    /// `PalaceHandle::purge_expired` reclaimed a drawer past its TTL.
    ExpiredPurge,
    /// The palace-open sweep reclaimed a drawer past its TTL.
    ExpiredPurgeAtOpen,
    /// #9172: a user forget removed a drawer a dedup merge had kept.
    ForgetOfMergedSurvivor,
}

impl DeletionReason {
    /// The snake_case name written to the journal.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DreamDedup => "dream_dedup",
            Self::DreamContentPrune => "dream_content_prune",
            Self::DreamPrune => "dream_prune",
            Self::SemanticConsolidation => "semantic_consolidation",
            Self::ExpiredPurge => "expired_purge",
            Self::ExpiredPurgeAtOpen => "expired_purge_at_open",
            Self::ForgetOfMergedSurvivor => "forget_of_merged_survivor",
        }
    }
}

impl std::fmt::Display for DeletionReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One journal line: a drawer a maintenance path deleted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MaintenanceDeletion {
    /// When the deletion was recorded (just after it committed).
    pub at: DateTime<Utc>,
    /// Palace id.
    pub palace: String,
    /// The deleted drawer.
    pub drawer_id: Uuid,
    /// The maintenance path that deleted it.
    pub reason: DeletionReason,
    /// The drawer that absorbed or superseded it (dedup, consolidation).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub survivor_id: Option<Uuid>,
    /// Cosine similarity between the two drawers (dedup only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<f32>,
    /// Pid of the process that deleted it; several processes may write one palace.
    pub pid: u32,
}

impl MaintenanceDeletion {
    /// A record stamped now with this process's pid and no survivor.
    pub fn new(palace: &PalaceId, drawer_id: Uuid, reason: DeletionReason) -> Self {
        Self {
            at: Utc::now(),
            palace: palace.as_str().to_string(),
            drawer_id,
            reason,
            survivor_id: None,
            score: None,
            pid: std::process::id(),
        }
    }

    /// Attach the surviving drawer and, for dedup, the similarity score.
    pub fn with_survivor(mut self, survivor_id: Uuid, score: Option<f32>) -> Self {
        self.survivor_id = Some(survivor_id);
        self.score = score;
        self
    }
}

/// Where a record ended up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordOutcome {
    /// Appended to the palace's journal file.
    Journaled,
    /// The journal was unavailable; the record went to the log only.
    LoggedOnly,
}

/// Path of the live journal for a palace data dir.
pub fn journal_path(data_dir: &Path) -> PathBuf {
    data_dir.join(MAINTENANCE_LOG_FILENAME)
}

/// How long an append waits for the journal lock before appending unlocked.
const JOURNAL_LOCK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// Append `rec` to the journal in `data_dir`, rotating once past `rotate_at`.
///
/// Why: up to ~16 processes append to one palace's journal (#8733). Two that
/// both saw the full size each renamed the live file over `.1`, and the second
/// rename discarded the whole previous generation.
/// What: the size check, the rename over the single `.1` generation, and the
/// append all run under the `file_lock` sidecar lock
/// (`maintenance_deletions.jsonl.lock`), a cross-process `flock`. The size is
/// read after the lock is held, so a writer that waited does not rotate a file
/// another writer just started. Each record is one `write_all` on an
/// `O_APPEND` handle. A lock that cannot be taken within
/// [`JOURNAL_LOCK_TIMEOUT`], or a failed rotation, falls back to a plain append
/// to the live file, so the record is kept and the journal only grows past its
/// bound.
/// Test: `maintenance_log_tests::concurrent_appends_across_the_rotation_boundary_lose_no_record`,
/// `maintenance_log_tests::a_lock_or_rotation_failure_still_appends_the_record`.
pub(crate) fn append(data_dir: &Path, rec: &MaintenanceDeletion, rotate_at: u64) -> Result<()> {
    let path = journal_path(data_dir);
    let mut line = serde_json::to_string(rec).context("serialize maintenance deletion")?;
    line.push('\n');
    let locked = crate::file_lock::with_exclusive_lock_timeout(&path, JOURNAL_LOCK_TIMEOUT, || {
        if let Err(e) = rotate_if_due(data_dir, &path, rotate_at) {
            tracing::warn!(palace = %rec.palace, "#8732: journal rotation failed; appending to the live file: {e:#}");
        }
        append_line(&path, &line)
    });
    match locked {
        Ok(appended) => appended,
        Err(e) => {
            tracing::warn!(palace = %rec.palace, "#8732: journal lock unavailable; appending without rotation: {e}");
            append_line(&path, &line)
        }
    }
}

/// Rename the live journal over `.1` when it has reached `rotate_at`.
fn rotate_if_due(data_dir: &Path, path: &Path, rotate_at: u64) -> Result<()> {
    if let Ok(meta) = std::fs::metadata(path)
        && meta.is_file()
        && meta.len() >= rotate_at
    {
        std::fs::rename(path, data_dir.join(MAINTENANCE_LOG_ROTATED_FILENAME))
            .with_context(|| format!("rotate {}", path.display()))?;
    }
    Ok(())
}

/// Write `line` to `path` in one `O_APPEND` write, creating the file if needed.
fn append_line(path: &Path, line: &str) -> Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("open {}", path.display()))?;
    file.write_all(line.as_bytes())
        .with_context(|| format!("append {}", path.display()))
}

/// Record one maintenance deletion; never drops it silently.
///
/// Why/What: see the module doc. Appends to the journal when the palace has a
/// data dir, and logs the removal at `warn` (#8729). When the append fails, the
/// whole record goes to the log at `error`; with no data dir it goes at `warn`.
/// Every arm reaches the daemon's log at its default filter.
/// Test: `maintenance_log_tests::every_maintenance_removal_logs_its_id_and_reason`,
/// `maintenance_log_tests::a_failed_record_write_logs_the_record_and_still_deletes`.
pub fn record(data_dir: Option<&Path>, rec: &MaintenanceDeletion) -> RecordOutcome {
    let Some(dir) = data_dir else {
        tracing::warn!(
            palace = %rec.palace, drawer_id = %rec.drawer_id, reason = %rec.reason,
            survivor_id = ?rec.survivor_id, score = ?rec.score,
            "#8732: maintenance deletion (palace has no data dir; this line is the record)"
        );
        return RecordOutcome::LoggedOnly;
    };
    match append(dir, rec, ROTATE_AT_BYTES) {
        Ok(()) => {
            // #8729: each removal is logged with its id and reason, not only
            // counted in the pass summary.
            tracing::warn!(
                palace = %rec.palace, drawer_id = %rec.drawer_id, reason = %rec.reason,
                survivor_id = ?rec.survivor_id, score = ?rec.score,
                "#8729: maintenance removed drawer {} ({})", rec.drawer_id, rec.reason
            );
            RecordOutcome::Journaled
        }
        Err(e) => {
            tracing::error!(
                palace = %rec.palace, drawer_id = %rec.drawer_id, reason = %rec.reason,
                survivor_id = ?rec.survivor_id, score = ?rec.score,
                "#8732: maintenance deletion journal write failed; the drawer is \
                 deleted and this line is the record: {e:#}"
            );
            RecordOutcome::LoggedOnly
        }
    }
}

/// Log one `warn` summary for a pass that deleted `count` drawers.
pub fn warn_removed(palace: &PalaceId, pass: &str, count: usize) {
    if count == 0 {
        return;
    }
    tracing::warn!(
        palace = %palace, pass, count,
        "#8732: {pass} removed {count} drawer(s); per-drawer record: \
         `trusty-memory palace deletions {palace}`"
    );
}

/// The journal read back, oldest first.
#[derive(Debug, Default)]
pub struct JournalContents {
    /// Every parseable record, the rotated generation before the live one.
    pub records: Vec<MaintenanceDeletion>,
    /// Lines that did not parse (a torn write, or a newer schema).
    pub malformed: usize,
}

/// Read the rotated and live journals in `data_dir`. A missing file is empty.
///
/// Test: `maintenance_log_tests::the_journal_rotates_and_reads_back_oldest_first`.
pub fn read_journal(data_dir: &Path) -> Result<JournalContents> {
    let mut out = JournalContents::default();
    for name in [MAINTENANCE_LOG_ROTATED_FILENAME, MAINTENANCE_LOG_FILENAME] {
        let path = data_dir.join(name);
        let file = match std::fs::File::open(&path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e).with_context(|| format!("open {}", path.display())),
        };
        for line in std::io::BufReader::new(file).lines() {
            let line = line.with_context(|| format!("read {}", path.display()))?;
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<MaintenanceDeletion>(&line) {
                Ok(rec) => out.records.push(rec),
                Err(_) => out.malformed += 1,
            }
        }
    }
    Ok(out)
}

impl PalaceHandle {
    /// Forget `id` on behalf of a maintenance pass, then record the deletion.
    ///
    /// Why: a maintenance deletion must leave a trail (#8732); a user's
    /// `forget` must not be reclassified as one, so the two stay separate
    /// entry points.
    /// What: runs [`PalaceHandle::forget`]. Only a real delete
    /// ([`ForgetOutcome::Deleted`]) is recorded; `survivor` carries the
    /// surviving drawer id and, for dedup, the score.
    /// Test: `maintenance_log_tests::dream_dedup_records_the_removed_and_surviving_drawer`,
    /// `maintenance_log_tests::user_forget_writes_no_maintenance_record`.
    pub async fn forget_for_maintenance(
        &self,
        id: Uuid,
        reason: DeletionReason,
        survivor: Option<(Uuid, Option<f32>)>,
    ) -> Result<ForgetOutcome> {
        // #9172: `forget_removing`, not `forget`, so a maintenance deletion of
        // a survivor writes this one record rather than two.
        let Some(_removed) = self.forget_removing(id).await? else {
            return Ok(ForgetOutcome::NotFound);
        };
        let mut rec = MaintenanceDeletion::new(&self.id, id, reason);
        if let Some((survivor_id, score)) = survivor {
            rec = rec.with_survivor(survivor_id, score);
        }
        record(self.data_dir.as_deref(), &rec);
        Ok(ForgetOutcome::Deleted)
    }
}

/// Record a user forget of `drawer` when the journal names it as a survivor.
///
/// Why (#9172): two dedup survivors were later removed with no record, so the
/// text merged into them could not be traced.
/// What: a no-op without a data dir or when [`names_survivor`] says no;
/// otherwise records [`DeletionReason::ForgetOfMergedSurvivor`].
/// Test: `dedup_survivor_tests::forgetting_a_dedup_survivor_writes_a_journal_record`,
/// `dedup_survivor_tests::forgetting_an_unmerged_drawer_writes_no_record`.
pub(crate) fn record_survivor_forget(handle: &PalaceHandle, drawer: &Drawer) {
    let Some(dir) = handle.data_dir.as_deref() else {
        return;
    };
    if names_survivor(dir, drawer) {
        let rec = MaintenanceDeletion::new(
            &handle.id,
            drawer.id,
            DeletionReason::ForgetOfMergedSurvivor,
        );
        record(Some(dir), &rec);
    }
}

/// Whether a journal record in `dir` names `drawer` as its survivor.
///
/// What: answers `false` without reading when neither journal file was written
/// after `drawer` was created — no record can name a drawer younger than the
/// file. An unreadable journal answers `true`: a spare record costs less than
/// a missing one.
fn names_survivor(dir: &Path, drawer: &Drawer) -> bool {
    let created = std::time::SystemTime::from(drawer.created_at);
    let written_since = [MAINTENANCE_LOG_ROTATED_FILENAME, MAINTENANCE_LOG_FILENAME]
        .iter()
        .filter_map(|name| std::fs::metadata(dir.join(name)).ok()?.modified().ok())
        .any(|modified| modified >= created);
    if !written_since {
        return false;
    }
    match read_journal(dir) {
        Ok(journal) => journal
            .records
            .iter()
            .any(|r| r.survivor_id == Some(drawer.id)),
        Err(e) => {
            tracing::warn!(drawer_id = %drawer.id, "#9172: journal unreadable; recording the forget: {e:#}");
            true
        }
    }
}
