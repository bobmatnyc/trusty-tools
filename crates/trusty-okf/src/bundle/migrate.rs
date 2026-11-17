//! Lossless trusty-kb to OKF v0.2 migration (SPEC-OKF-01 §2.8, §2.9,
//! §4.11).
//!
//! Why: an existing trusty-kb tree must reach the v0.2 form without losing an
//! ID, a body byte, a key, a source, a verification event or a log line, and
//! a migration that fails its audit or is interrupted must leave the tree it
//! found, byte for byte.
//! What: [`Bundle::plan_migration`] reads every concept and returns the
//! files the §2.8 rules (`normalize_v02`) would change, writing nothing.
//! [`Bundle::migrate`] re-checks each file's bytes against the plan, stores
//! a content-addressed backup of every file it will change at
//! `_sources/migrations/<run-id>/blobs/sha256-<hex>` and the manifest
//! `_sources/migrations/<run-id>.jsonl` (one line per file: path, old hash,
//! blob, new hash), then runs one journaled `Migrate` operation: the
//! revision journal (one `migrate` line per concept, §4.5 ownership) and
//! each concept file. The §2.9 audit runs before the completion record; a
//! failure restores every file from its blob, checks each restored hash and
//! returns `MigrationLoss`. Recovery rolls an interrupted migration back
//! from the same blobs. After a passed audit the blobs are removed and the
//! manifest stays as history. A migration writes no `log.md` entry: §2.9
//! item 8 allows only appends to logs, and `log.md` inserts at the top.
//! Slice S6 of #10746.
//! Test: `c23_migration_keeps_ids_and_bodies_on_the_trusty_kb_tree`,
//! `c25_second_migration_plans_nothing_and_writes_nothing`,
//! `c26_logs_and_journals_are_equal_or_appended`,
//! `c67_migrate_keeps_verification_and_moves_changed_files_to_pending`,
//! `c24_audit_restores_every_file_when_a_sources_entry_is_dropped`,
//! `c120_body_byte_change_restores_and_a_corrupt_blob_is_restore_failed`,
//! `c106_supersede_and_migrate_crash_at_every_boundary`.

use chrono::{DateTime, FixedOffset};
use serde::Serialize;

use crate::OkfError;
use crate::bundle::Bundle;
use crate::bundle::audit::{Inventory, MIGRATIONS_DIR};
use crate::bundle::ops::{self, OpKind, OpPlan, disk_hash, mutate_seam, put_file};
use crate::bundle::paths::{confined_file, io_err, read_rel};
use crate::bundle::record::instant_string;
use crate::bundle::walk::scan;
use crate::format::concept::parse_concept;
use crate::format::hash::{bytes_hash, content_hash};
use crate::format::id::{ConceptId, ConceptRef};
use crate::format::v02::normalize_v02;
use crate::format::validate::{Finding, FindingKind, Severity};
use crate::format::write::render_checked;
use crate::state::revisions::{REVISIONS_REL, RevisionLine, run_id};

/// The actor of a migration's operation-journal records.
const MIGRATE_ACTOR: &str = "process:trusty-okf-migrate";

/// One planned file change (SPEC-OKF-01 §4.2 `FileChange`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChange {
    /// The concept.
    pub id: ConceptId,
    /// `sha256:<hex>` of the bytes the plan read.
    pub old_hash: String,
    /// The bytes the migration writes.
    pub new_bytes: Vec<u8>,
}

/// What a migration would change; built by [`Bundle::plan_migration`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MigrationPlan {
    /// One entry per file whose bytes change, in path order.
    pub changes: Vec<FileChange>,
    /// §2.8 warnings (an `updated` that cannot be mapped) and the files the
    /// plan could not read or patch; such a file is not changed.
    pub findings: Vec<Finding>,
}

/// What [`Bundle::migrate`] did (SPEC-OKF-01 §4.2).
#[derive(Debug, Clone, PartialEq)]
pub struct MigrationReport {
    /// The operation ID; names the manifest. Empty when nothing changed.
    pub run_id: String,
    /// Files changed.
    pub changed: usize,
    /// The plan's findings and the operation's.
    pub findings: Vec<Finding>,
}

/// One manifest line (§2.9 step 2).
#[derive(Serialize)]
struct ManifestLine<'a> {
    path: &'a str,
    old: &'a str,
    blob: String,
    new: String,
}

/// The backup blob of the bytes hashing to `hash` under migration `op`.
pub(crate) fn blob_rel(op: &str, hash: &str) -> String {
    format!(
        "{MIGRATIONS_DIR}/{op}/blobs/{}",
        hash.replacen("sha256:", "sha256-", 1)
    )
}

/// Restore `path` to the bytes hashing to `pre` from migration `op`'s
/// backup, then check the hash (§2.9 step 4). `false` when the blob is
/// missing or the restored bytes hash otherwise; the blob stays either way.
pub(crate) fn restore(
    b: &Bundle,
    op: &str,
    path: &str,
    pre: Option<&str>,
) -> Result<bool, OkfError> {
    match pre {
        None => put_file(b, path, None)?,
        Some(h) => match read_rel(b.root(), &blob_rel(op, h))? {
            Some(bytes) => put_file(b, path, Some(&bytes))?,
            None => return Ok(false),
        },
    }
    Ok(disk_hash(b, path)?.as_deref() == pre)
}

fn plan_finding(rel: &str, message: String) -> Finding {
    Finding::new(
        Severity::Warning,
        FindingKind::UnmappedLegacyValue,
        rel.to_string(),
        message,
        "SPEC-OKF-01 §2.9",
    )
}

impl Bundle {
    /// The per-file changes a migration would make (SPEC-OKF-01 §2.9 step 1).
    ///
    /// Why: a migration is planned before it writes, and a second migration
    /// over a migrated tree must plan nothing (criterion 25).
    /// What: reads every concept and applies `normalize_v02` through the
    /// raw-preserving render; a file whose render differs becomes a
    /// [`FileChange`]. An unreadable, unparseable or unpatchable file is a
    /// finding and is not planned. Writes nothing.
    /// Test: `c25_second_migration_plans_nothing_and_writes_nothing`.
    pub fn plan_migration(&self) -> Result<MigrationPlan, OkfError> {
        let mut plan = MigrationPlan::default();
        for entry in scan(self.root())?.files {
            let ConceptRef::Id(id) = entry.r else {
                continue;
            };
            let Some(bytes) = read_rel(self.root(), &entry.rel)? else {
                continue;
            };
            let parsed = std::str::from_utf8(&bytes)
                .map_err(|e| e.to_string())
                .and_then(|t| parse_concept(id.clone(), t).map_err(|e| e.to_string()));
            let mut c = match parsed {
                Ok((c, _)) => c,
                Err(e) => {
                    plan.findings
                        .push(plan_finding(&entry.rel, format!("not migrated: {e}")));
                    continue;
                }
            };
            plan.findings.extend(normalize_v02(&mut c));
            let rendered = match render_checked(&c) {
                Ok(r) => r.into_bytes(),
                Err(e) => {
                    plan.findings
                        .push(plan_finding(&entry.rel, format!("not migrated: {e}")));
                    continue;
                }
            };
            if rendered != bytes {
                plan.changes.push(FileChange {
                    id,
                    old_hash: bytes_hash(&bytes),
                    new_bytes: rendered,
                });
            }
        }
        Ok(plan)
    }

    /// Apply a migration plan (SPEC-OKF-01 §2.9 steps 2-6, §4.11).
    ///
    /// Why/What: see the module doc. Refusals, with no file changed: a path
    /// of a conflicted operation (`RecoveryPending`), and a file whose bytes
    /// no longer match the plan (`Conflict { reason: "stale_plan" }`). An
    /// empty plan writes nothing.
    /// Test: `c24_audit_restores_every_file_when_a_sources_entry_is_dropped`,
    /// `c67_migrate_keeps_verification_and_moves_changed_files_to_pending`,
    /// `c127_migrate_crash_after_two_file_steps_rolls_back`.
    pub fn migrate(
        &mut self,
        plan: &MigrationPlan,
        now: DateTime<FixedOffset>,
    ) -> Result<MigrationReport, OkfError> {
        let mut report = MigrationReport {
            run_id: String::new(),
            changed: 0,
            findings: plan.findings.clone(),
        };
        if plan.changes.is_empty() {
            return Ok(report);
        }
        // #10746 S6: one writer for the whole migration (§4.4).
        let _lock = self.write_lock()?;
        let mut paths: Vec<String> = plan.changes.iter().map(|c| c.id.file_name()).collect();
        paths.push(REVISIONS_REL.to_string());
        self.check_recovery(&paths)?;
        let stamp = instant_string(&now);
        let run = run_id("migrate", &[&stamp, &paths.join("\n")]);
        let rev_pre = read_rel(self.root(), REVISIONS_REL)?;
        let mut rev_post = rev_pre.clone().unwrap_or_default();
        if rev_post.last().is_some_and(|b| *b != b'\n') {
            rev_post.push(b'\n');
        }
        let mut files = Vec::new();
        for change in &plan.changes {
            let rel = change.id.file_name();
            let old = read_rel(self.root(), &rel)?.unwrap_or_default();
            if bytes_hash(&old) != change.old_hash {
                return Err(crate::bundle::put::conflict(&change.id, "stale_plan"));
            }
            let mut new = change.new_bytes.clone();
            mutate_seam(&rel, &mut new);
            // #10746 S6: a post-image that does not parse refuses the whole
            // migration before any write; no default content hash.
            let text = String::from_utf8_lossy(&new);
            let content = content_hash(&parse_concept(change.id.clone(), &text)?.0)
                .as_str()
                .to_string();
            let mut line = RevisionLine {
                id: change.id.as_str().to_string(),
                content,
                bytes: bytes_hash(&new),
                op_kind: "migrate".to_string(),
                now: stamp.clone(),
                run: run.clone(),
                op: None,
                step: None,
                proven: None,
                origin: None,
            };
            // §4.5: a migration over unproven bytes records `proven: false`.
            line.stamp(self.root(), Some(&old), false)?;
            rev_post.extend_from_slice(line.json()?.as_bytes());
            rev_post.push(b'\n');
            files.push((rel, old, new));
        }
        let ids: Vec<ConceptId> = plan.changes.iter().map(|c| c.id.clone()).collect();
        let inventory = Inventory::take(self, &ids)?;
        let mut op = OpPlan::new(OpKind::Migrate, &stamp, MIGRATE_ACTOR);
        op.file(REVISIONS_REL, rev_pre.clone(), Some(rev_post.clone()));
        for (rel, old, new) in &files {
            op.file(rel, Some(old.clone()), Some(new.clone()));
        }
        let op_id = op.op_id();
        // §2.9 step 2: backups and manifest, synced, before the intent.
        let mut manifest = String::new();
        let mut backed = vec![(REVISIONS_REL.to_string(), rev_pre, rev_post)];
        backed.extend(files.into_iter().map(|(r, o, n)| (r, Some(o), n)));
        for (rel, old, new) in &backed {
            let old_hash = old.as_deref().map(bytes_hash).unwrap_or_default();
            if let Some(old) = old {
                put_file(self, &blob_rel(&op_id, &old_hash), Some(old))?;
            }
            let line = ManifestLine {
                path: rel,
                old: &old_hash,
                blob: blob_rel(&op_id, &old_hash),
                new: bytes_hash(new),
            };
            manifest.push_str(&serde_json::to_string(&line).map_err(|e| OkfError::Parse {
                path: MIGRATIONS_DIR.into(),
                line: None,
                message: e.to_string(),
            })?);
            manifest.push('\n');
        }
        put_file(
            self,
            &format!("{MIGRATIONS_DIR}/{op_id}.jsonl"),
            Some(manifest.as_bytes()),
        )?;
        let check = |b: &Bundle| match inventory.check(b)? {
            loss if loss.is_empty() => Ok(()),
            loss => Err(OkfError::MigrationLoss(loss)),
        };
        report.findings.extend(ops::run_checked(self, op, &check)?);
        // §2.9 step 6: the blobs go; the manifest stays as history. #10746
        // S6: a failed removal fails the call (the tree is migrated, and a
        // re-run plans nothing); it is never a warning beside success.
        ops::io_seam(&format!("remove-backups:{op_id}"))?;
        let blobs = confined_file(self.root(), &format!("{MIGRATIONS_DIR}/{op_id}"))?;
        let removed = std::fs::remove_dir_all(&blobs); // okf: temp-cleanup (migration backups, §2.9 step 6)
        match removed {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(io_err(&blobs, e)),
        }
        report.run_id = op_id;
        report.changed = plan.changes.len();
        Ok(report)
    }
}

#[cfg(test)]
#[path = "s6_fault_tests.rs"]
mod s6_fault_tests;
