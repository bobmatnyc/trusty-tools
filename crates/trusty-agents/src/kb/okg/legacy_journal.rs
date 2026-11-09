//! One-time migration of the in-tree `_sources/<id>.index.jsonl` journals
//! into this machine's trusty-okf index state (#10746 S12).
//!
//! Why: before S12 the agent recorded pushes in `_sources/<id>.index.jsonl`,
//! inside the synced tree. Those lines may describe ANOTHER machine's daemon
//! (SPEC-OKF-01 §4.9.1, r4 BLOCKER 5), so they can never become this
//! machine's Acknowledged or Searchable state. Dropping them is also wrong:
//! a journaled path whose file has since vanished must still be withdrawn
//! from the index, and only the journal remembers it was pushed.
//! What: [`migrate_legacy_journals`] reads each legacy journal once (last
//! line per entity wins, a torn line skipped) and appends one
//! `IndexRecord::PutFailed` per entity to this machine's journal, with the
//! entity's file name, its journaled byte hash and the
//! `legacy_journal_unconfirmed` reason. trusty-okf then reads every such
//! path as Pending, re-pushes a live file, and withdraws a vanished or
//! tombstoned one, all through this machine's daemon. The legacy files stay
//! in place, byte-identical (§4.9.1 rule 4, §2.9 item 8). A marker in the
//! state directory, never in the tree, records that a journal was migrated
//! for this state; a path the state journal already names is never
//! appended again, so an interrupted run re-runs without duplicates.
//! Test: `migrates_a_fixture_journal_as_pending_and_leaves_it_in_place`,
//! `interrupted_migration_reruns_without_duplicates`,
//! `migration_error_arms_record_nothing_as_indexed`.
//!
//! # Spec References
//! - SPEC-OKF-01 §4.9.1 and §5.4 F11, held in the trusty-okf project
//!   directory.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use trusty_okf::build::jsonl::read_journal;
use trusty_okf::format::id::ConceptId;
use trusty_okf::state::{IndexJournal, IndexRecord};

use crate::kb::okg::index_state::IndexStateRef;
use crate::kb::okg::registry::SOURCES_DIR;
use crate::kb::roots::confine;

/// The legacy journal file suffix under `_sources/`.
pub const LEGACY_SUFFIX: &str = ".index.jsonl";

/// The `last_error` every migrated path carries until this machine's
/// daemon confirms it.
pub const LEGACY_REASON: &str = "legacy_journal_unconfirmed";

/// The agent's marker directory inside the state directory. trusty-okf
/// owns only the `<sha256 of the bundle root>/` directories beside it.
const MARKER_DIR: &str = "agent-legacy-migrations";

/// The fields of a legacy journal line the migration needs.
#[derive(Debug, Deserialize)]
struct LegacyLine {
    entity: String,
    content_hash: String,
    state: LegacyState,
}

/// What a legacy line asserted.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum LegacyState {
    Indexed,
    Removed,
}

/// What one migration call did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct MigrationReport {
    /// Legacy journals read this call.
    pub journals: usize,
    /// Legacy journals already migrated for this state, skipped.
    pub already_migrated: usize,
    /// Paths appended to this machine's journal as Pending.
    pub migrated: usize,
    /// Paths this machine's journal already named; nothing appended.
    pub already_journaled: usize,
    /// Torn or unparseable legacy lines skipped (F11).
    pub malformed_lines: usize,
    /// Entities that are not a valid concept ID; nothing appended.
    pub skipped: Vec<String>,
}

/// Every legacy journal under `<root>/_sources/`, sorted by file name.
fn legacy_journals(root: &Path) -> anyhow::Result<Vec<(String, PathBuf)>> {
    let dir = confine(root, &[SOURCES_DIR])?;
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(anyhow::anyhow!("{}: {e}", dir.display())),
    };
    let mut out = Vec::new();
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.ends_with(LEGACY_SUFFIX) {
            out.push((name, entry.path()));
        }
    }
    out.sort();
    Ok(out)
}

/// The marker for one legacy journal migrated into one state journal.
fn marker_path(state: &IndexStateRef, journal: &IndexJournal, legacy: &str) -> PathBuf {
    let mut h = Sha256::new();
    h.update(journal.path().as_os_str().as_encoded_bytes());
    h.update(b"\n");
    h.update(legacy.as_bytes());
    let key: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
    state.path().join(MARKER_DIR).join(format!("{key}.json"))
}

/// Write `bytes` to `path` atomically: temp file, sync, rename.
fn write_marker(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    use std::io::Write;
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("marker {} has no parent", path.display()))?;
    std::fs::create_dir_all(parent)?;
    let tmp = path.with_extension("json.tmp");
    let mut f = std::fs::File::create(&tmp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Every path a state journal names.
fn journaled(records: &[IndexRecord]) -> BTreeSet<String> {
    records
        .iter()
        .filter_map(|r| match r {
            IndexRecord::PutAck { path, .. }
            | IndexRecord::PutFailed { path, .. }
            | IndexRecord::Probe { path, .. }
            | IndexRecord::RemoveAck { path, .. }
            | IndexRecord::WithdrawProbe { path, .. } => Some(path.clone()),
            _ => None,
        })
        .collect()
}

/// Migrate every in-tree legacy journal under `root` into `state` once.
///
/// Why/What: see the module doc. Order per journal: read the legacy file,
/// append the missing Pending records (each line synced), then write the
/// marker. Any error returns before the marker, so the next call retries;
/// no arm writes an acknowledgement, a probe or a remove.
/// Test: `migrates_a_fixture_journal_as_pending_and_leaves_it_in_place`,
/// `interrupted_migration_reruns_without_duplicates`,
/// `migration_error_arms_record_nothing_as_indexed`.
pub fn migrate_legacy_journals(
    root: &Path,
    state: &IndexStateRef,
) -> anyhow::Result<MigrationReport> {
    let journal = IndexJournal::open(&state.dir, root, &state.index_id)?;
    let mut report = MigrationReport::default();
    for (name, path) in legacy_journals(root)? {
        let marker = marker_path(state, &journal, &name);
        if marker.is_file() {
            report.already_migrated += 1;
            continue;
        }
        let load = read_journal::<LegacyLine>(&path)?;
        report.journals += 1;
        report.malformed_lines += load.malformed;
        let latest: BTreeMap<String, LegacyLine> = load
            .records
            .into_iter()
            .map(|l| (l.entity.clone(), l))
            .collect();
        let mut seen = journaled(&journal.read()?);
        let mut records = Vec::new();
        for (entity, line) in latest {
            let file = match ConceptId::parse(&entity) {
                Ok(id) => id.file_name(),
                Err(e) => {
                    report.skipped.push(format!("{name}: {entity}: {e}"));
                    continue;
                }
            };
            if !seen.insert(file.clone()) {
                report.already_journaled += 1;
                continue;
            }
            let was = match line.state {
                LegacyState::Indexed => "indexed",
                LegacyState::Removed => "removed",
            };
            records.push(IndexRecord::PutFailed {
                path: file,
                bytes: line.content_hash,
                error: format!(
                    "{LEGACY_REASON}: _sources/{name} recorded it {was}; this machine's \
                     search daemon has not confirmed it"
                ),
            });
        }
        journal.append(&records)?;
        report.migrated += records.len();
        let body = serde_json::json!({
            "legacy": format!("{SOURCES_DIR}/{name}"),
            "journal": journal.path(),
            "migrated": records.len(),
        });
        write_marker(&marker, body.to_string().as_bytes())?;
    }
    Ok(report)
}

#[cfg(test)]
#[path = "legacy_journal_tests.rs"]
mod tests;
