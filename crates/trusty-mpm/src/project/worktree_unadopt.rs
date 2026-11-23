//! Ledger-only removal of adopted worktrees: `tm project unadopt` (#10592).
//!
//! Why: `tm project init` on a legacy checkout adopts every pre-existing
//! worktree into `project-registry/worktrees.json` (#7357, #7588), and there
//! was no way to take a record back out short of hand-editing the JSON.
//! What: [`unadopt_paths`] removes the records for the named paths from the
//! adoption store and reports what it removed and what it did not find. It
//! touches the store file only — never a worktree directory, never git
//! metadata. `dry_run` reads the store and reports without writing.
//!
//! NEVER FAIL-OPEN: a store that cannot be read, parsed or published returns
//! `Err`, so a caller can never report a removal that did not reach disk. A
//! dry run reads the store strictly for the same reason: a corrupt store must
//! not preview as "nothing adopted".
//! Test: `unadopt_removes_only_the_named_record_and_keeps_the_folder`,
//! `unadopt_refuses_a_corrupt_store_and_reports_no_removal`,
//! `project_unadopt_removes_the_record_and_keeps_the_folder_10592`.

use std::path::{Path, PathBuf};

use serde::Serialize;
use trusty_common::json_rmw::JsonRmwError;

use super::worktree_adoption::{ADOPTED_WORKTREES_FILE, AdoptedWorktree, AdoptionStore};

/// What one unadopt pass removed, or would remove under `dry_run` (#10592).
///
/// Test: `unadopt_removes_only_the_named_record_and_keeps_the_folder`,
/// `unadopt_dry_run_reports_and_writes_nothing`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct UnadoptReport {
    /// `true` when nothing was written.
    pub dry_run: bool,
    /// The records removed (or that a real run would remove).
    pub removed: Vec<AdoptedWorktree>,
    /// Requested paths that matched no record.
    pub not_adopted: Vec<PathBuf>,
}

/// The paths one requested path may be stored under: as given, and canonical.
///
/// Why: backfill stores canonical paths, while an operator may type a
/// relative or symlinked one; a folder already deleted cannot canonicalize,
/// so the given form is always kept too.
fn candidates_for(path: &Path) -> Vec<PathBuf> {
    let mut out = vec![path.to_path_buf()];
    if let Ok(canonical) = std::fs::canonicalize(path)
        && canonical != path
    {
        out.push(canonical);
    }
    out
}

/// Remove `paths` from the adoption store in `store_dir`, ledger only (#10592).
///
/// Why: the acceptance contract of #10592 — forget worktrees by path from the
/// ledger, with a dry run, and never edit or delete a worktree folder.
/// What: for each path, removes the record stored under that path or its
/// canonical form. A real run does the whole pass inside
/// [`AdoptionStore::update`] (locked read-modify-write, atomic publish); a dry
/// run strictly reads the file and writes nothing. A path with no record lands
/// in [`UnadoptReport::not_adopted`]. A later `tm project init` on the same
/// checkout adopts the worktree again.
///
/// # Errors
///
/// [`JsonRmwError`] when the store cannot be read, parsed or published. The
/// store is left unchanged and no removal is reported.
///
/// Test: `unadopt_removes_only_the_named_record_and_keeps_the_folder`,
/// `unadopt_dry_run_reports_and_writes_nothing`,
/// `unadopt_matches_a_non_canonical_path`,
/// `unadopt_refuses_a_corrupt_store_and_reports_no_removal`.
pub fn unadopt_paths(
    store_dir: &Path,
    paths: &[PathBuf],
    dry_run: bool,
) -> Result<UnadoptReport, JsonRmwError> {
    let pass = |store: &mut AdoptionStore| {
        let mut report = UnadoptReport {
            dry_run,
            ..UnadoptReport::default()
        };
        for path in paths {
            match store.remove_path(&candidates_for(path)) {
                Some(entry) => report.removed.push(entry),
                None => report.not_adopted.push(path.clone()),
            }
        }
        report
    };
    if dry_run {
        let mut store = read_strict(store_dir)?;
        return Ok(pass(&mut store));
    }
    // #10592: no removal is reported unless the publish succeeded.
    AdoptionStore::update(store_dir, pass)
}

/// Read the store, failing on a present-but-unparseable file (#10592).
///
/// Why: [`AdoptionStore::load`] is fail-open (corrupt reads as empty), which
/// would make a dry run over a corrupt store say "not adopted" for every path.
/// What: absent file → empty store; read or parse failure → `Err`.
fn read_strict(store_dir: &Path) -> Result<AdoptionStore, JsonRmwError> {
    let file_path = store_dir.join(ADOPTED_WORKTREES_FILE);
    let raw = match std::fs::read_to_string(&file_path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(AdoptionStore::default());
        }
        Err(source) => {
            return Err(JsonRmwError::Io {
                path: file_path,
                source,
            });
        }
    };
    let entries: Vec<AdoptedWorktree> =
        serde_json::from_str(&raw).map_err(|e| JsonRmwError::Serialize {
            path: file_path.clone(),
            message: e.to_string(),
        })?;
    let mut store = AdoptionStore::default();
    for entry in entries {
        store.record(entry);
    }
    Ok(store)
}

#[cfg(test)]
#[path = "worktree_unadopt_tests.rs"]
mod tests;
