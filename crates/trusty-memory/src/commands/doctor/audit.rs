//! Palace audit helpers for `trusty-memory doctor --fix-palaces`.
//!
//! Why: factored out of the monolithic `doctor.rs` so the audit data types
//! and logic live in their own focused file, separate from the check
//! functions and the command entry-points.
//! What: exports [`PalaceAuditStatus`], [`PalaceAuditEntry`], and
//! [`audit_palaces`] used by the presenter in `mod.rs`.
//! Test: `find_orphaned_palaces_lists_non_matching_and_empty` and
//! `audit_palaces_ok_when_pin_file_claims_it` in the parent `mod.rs`.

use std::path::{Path, PathBuf};

use crate::project_root::{project_slug_at, read_project_pin, PERSONAL_PALACE, PIN_FILE_REL};

/// Outcome of a single palace audit entry for `doctor --fix-palaces`.
///
/// Why: separating the palace audit result from the standard `CheckResult`
/// makes the presentation logic cleaner — palace audit output is tabular
/// (one row per palace) rather than the pass/warn/fail traffic-light model
/// used by the daemon health checks.
/// What: encodes whether a palace is `Ok` (name matches a detectable project
/// slug, is the `personal` sentinel, or is claimed by a pin file in any
/// scanned project directory), `Orphaned` (name does not correspond to any
/// project directory we can find on disk), or `Empty` (the palace directory
/// exists but has no `palace.json`).
/// Test: `find_orphaned_palaces_lists_non_matching_and_empty`,
///       `audit_palaces_ok_when_pin_file_claims_it`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PalaceAuditStatus {
    /// Palace name matches the current project slug, is `personal`, or is
    /// claimed by a `.trusty-tools/trusty-memory.yaml` pin file found in
    /// any of the standard project search directories.
    Ok,
    /// Palace name does not match any project directory or pin file found by
    /// the bounded scan. Advisory: existing data is intact; no mutation made.
    Orphaned,
    /// The palace directory exists under the data root but contains no
    /// `palace.json` (was never fully initialised, or was manually deleted).
    Empty,
}

/// One row in the `doctor --fix-palaces` audit table.
///
/// Why: bundles the palace id, its on-disk data directory, and the audit
/// status into a single struct so the presenter and the audit logic can be
/// separated cleanly.
/// What: carries `id` (the palace name used as the on-disk dir name),
/// `data_dir` (absolute path), and `status`.
/// Test: `find_orphaned_palaces_lists_non_matching_and_empty`.
#[derive(Debug, Clone)]
pub struct PalaceAuditEntry {
    pub id: String,
    pub data_dir: PathBuf,
    pub status: PalaceAuditStatus,
}

/// Audit every palace subdirectory under `registry_dir` and classify each.
///
/// Why: the classification logic is factored out of the presenter so it can
/// be unit-tested without touching the terminal.
/// What: walks `registry_dir` one level deep; for each subdirectory, checks
/// for a `palace.json` (marks `Empty` when absent), then applies the following
/// resolution order:
///
///   1. `personal` is always `Ok`.
///   2. Any ancestor of `registry_dir` is a project root whose slug matches
///      the palace id → `Ok` (daemon running inside a project tree).
///   3. A standard project search directory (`~/Projects/<id>`,
///      `~/Developer/<id>`, etc.) exists on disk with a matching basename →
///      `Ok` (heuristic, covers common single-project-dir layout).
///   4. Change 3: a `.trusty-tools/trusty-memory.yaml` pin file is found in
///      any immediate subdirectory of the standard search dirs whose `palace`
///      field equals this palace id → `Ok`. This is the key improvement: a
///      repo that has been moved or renamed but still has its pin file
///      committed will NOT be flagged as orphaned even if its directory name
///      no longer matches the palace id.
///   5. None of the above → `Orphaned` (advisory, no mutation).
///
/// The scan is bounded: we only look one level deep inside the standard
/// search dirs (no recursive filesystem walk).
/// Test: `find_orphaned_palaces_lists_non_matching_and_empty`,
///       `audit_palaces_ok_when_pin_file_claims_it`.
pub fn audit_palaces(registry_dir: &Path) -> Vec<PalaceAuditEntry> {
    let Ok(entries) = std::fs::read_dir(registry_dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let id = match path.file_name().and_then(|n| n.to_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };
        // `Empty` takes priority — no palace.json means it was never
        // initialised or was partially deleted.
        if !path.join("palace.json").exists() {
            out.push(PalaceAuditEntry {
                id,
                data_dir: path,
                status: PalaceAuditStatus::Empty,
            });
            continue;
        }
        // `personal` is always Ok.
        if id == PERSONAL_PALACE {
            out.push(PalaceAuditEntry {
                id,
                data_dir: path,
                status: PalaceAuditStatus::Ok,
            });
            continue;
        }
        // Check whether any ancestor of `registry_dir` is a project root
        // whose slug matches the palace id. This catches the case where the
        // user runs the daemon from inside a project tree.
        let matches_ancestor = project_slug_at(registry_dir)
            .map(|slug| slug == id)
            .unwrap_or(false);
        if matches_ancestor {
            out.push(PalaceAuditEntry {
                id,
                data_dir: path,
                status: PalaceAuditStatus::Ok,
            });
            continue;
        }
        // Step 3: heuristic — look for a directory named after the slug in the
        // user's common project locations. We check `$HOME/Projects/<id>`,
        // `$HOME/Developer/<id>`, `$HOME/Code/<id>`, and `$HOME/<id>` as
        // plausible locations. This keeps the check lightweight (no recursive
        // scan) while catching the most common single-project-dir layout.
        let home = dirs::home_dir();
        let found_on_disk = home
            .as_ref()
            .map(|home| {
                let candidates = [
                    home.join("Projects").join(&id),
                    home.join("Developer").join(&id),
                    home.join("Code").join(&id),
                    home.join(&id),
                ];
                candidates.iter().any(|c| c.is_dir())
            })
            .unwrap_or(false);
        if found_on_disk {
            out.push(PalaceAuditEntry {
                id,
                data_dir: path,
                status: PalaceAuditStatus::Ok,
            });
            continue;
        }

        // Step 4 (Change 3): scan one level inside the standard search dirs
        // for a `.trusty-tools/trusty-memory.yaml` pin file that claims this
        // palace id. This lets renamed/moved repos avoid the Orphaned
        // classification as long as they committed their pin file.
        let claimed_by_pin = home
            .as_ref()
            .map(|home| {
                scan_project_dirs_for_pin(
                    &[
                        home.join("Projects"),
                        home.join("Developer"),
                        home.join("Code"),
                        home.clone(),
                    ],
                    &id,
                )
            })
            .unwrap_or(false);
        let status = if claimed_by_pin {
            PalaceAuditStatus::Ok
        } else {
            PalaceAuditStatus::Orphaned
        };
        out.push(PalaceAuditEntry {
            id,
            data_dir: path,
            status,
        });
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

/// Scan one level inside each of `search_dirs` for a committed pin file
/// (`PIN_FILE_REL`) whose `palace` field equals `palace_id`.
///
/// Why: Change 3 — a palace created from a project that was later moved or
/// renamed should not be flagged `Orphaned` as long as the project's pin file
/// is still present somewhere on the standard project search path. A bounded
/// one-level scan keeps the check cheap (no recursive walk) while catching the
/// common case where projects live directly under `~/Projects/` or `~/Code/`.
/// What: for each dir in `search_dirs` that exists, `read_dir` one level and
/// for each entry attempt to read `<entry>/PIN_FILE_REL`; if the parse
/// succeeds and `pin.palace == palace_id`, returns `true` immediately.
/// Returns `false` if no match is found. All I/O errors are silently ignored
/// (advisory check, read-only).
/// Test: `audit_palaces_ok_when_pin_file_claims_it`.
pub fn scan_project_dirs_for_pin(search_dirs: &[PathBuf], palace_id: &str) -> bool {
    for search_dir in search_dirs {
        let Ok(entries) = std::fs::read_dir(search_dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let candidate = entry.path();
            if !candidate.is_dir() {
                continue;
            }
            // Read the pin file if present.
            if let Ok(Some(pin)) = read_project_pin(&candidate) {
                if pin.palace == palace_id {
                    tracing::debug!(
                        palace_id = %palace_id,
                        pin_path = %candidate.join(PIN_FILE_REL).display(),
                        "audit: palace claimed by pin file — classifying as Ok"
                    );
                    return true;
                }
            }
        }
    }
    false
}
