//! Reap orphaned HNSW staging files left behind by dead processes (issue #2936).
//!
//! Why: [`super::usearch_store::save`] writes its snapshot to a staging file and
//! renames it into place, so a reader never sees a half-written snapshot. Every
//! failure path in `save` deletes its own staging file, but a process that is
//! SIGKILLed between the write and the rename runs no cleanup at all, and since
//! #4395 made staging names process-scoped (`hnsw.usearch.<pid>.tmp`) no later
//! process overwrites the leftover either. `staging_path`'s own doc states that
//! residual plainly. Nothing in `core/store/` ever called `read_dir`, so nothing
//! could reap them: one file per (index, killed pid) accumulated forever beside
//! the live snapshot.
//!
//! What: a single pass over the snapshot's parent directory, run from
//! [`super::usearch_store::UsearchStore::load_from`] before it reads the
//! sidecar. A staging file is deleted when its embedded pid names a process that
//! is gone, and pre-#4395 bare `.tmp` names are deleted unconditionally (no pid
//! to check, and no live writer produces that name any more). A staging file
//! belonging to a LIVE process is left alone — that is a concurrent save by
//! another daemon on a colocated snapshot, and deleting it would recreate
//! exactly the cross-process corruption #4395 fixed.
//!
//! Test: `super::tests_2936::reap_removes_dead_pid_and_bare_staging_files`,
//! `super::tests_2936::reap_leaves_live_pid_staging_and_the_live_snapshot_alone`.

use std::path::Path;

use crate::service::daemon::pid_alive;

/// Decide what to do with a directory entry named `name`, given the basename of
/// a live artifact (`live`) that staging files for it are derived from.
///
/// Why: separating the naming decision from the filesystem walk is what makes
/// the rules testable without planting files, and keeps the two shapes
/// (`<live>.tmp` and `<live>.<pid>.tmp`) written down in exactly one place.
/// What: returns `true` when `name` is a reapable staging file for `live`.
/// `<live>.tmp` is the pre-#4395 deterministic name — always reapable, since no
/// current writer emits it. `<live>.<pid>.tmp` is reapable only when `pid`
/// parses and names a dead process. Anything else (including `live` itself)
/// returns `false`.
/// Test: `super::tests_2936::staging_is_reapable_classifies_each_name_shape`.
pub(super) fn is_reapable_staging(name: &str, live: &str) -> bool {
    let Some(rest) = name.strip_prefix(live) else {
        return false;
    };
    // Pre-#4395 bare staging name. No pid to interrogate and nothing writes it
    // today, so a survivor is unambiguously abandoned.
    if rest == ".tmp" {
        return true;
    }
    let Some(pid) = rest
        .strip_prefix('.')
        .and_then(|r| r.strip_suffix(".tmp"))
        .and_then(|p| p.parse::<u32>().ok())
    else {
        return false;
    };
    // A live pid means another daemon is mid-save on a colocated snapshot.
    // Deleting that file is the cross-process corruption #4395 removed.
    !pid_alive(pid)
}

/// Delete abandoned staging files beside the snapshot at `hnsw_path`.
///
/// Why: see the module docs — this is the only thing that ever removes a
/// staging file left by a SIGKILLed process. Called from `load_from`, which is
/// the one moment the process is provably not mid-save on this path.
/// What: scans `hnsw_path`'s parent for staging names derived from either live
/// artifact (the snapshot itself and its `keys.json` sidecar) and unlinks the
/// reapable ones. Every step is best-effort: an unreadable directory, an
/// unreadable entry, or a failed unlink is logged at `debug` and skipped, never
/// propagated — a snapshot that loads fine must not be refused because a
/// leftover file could not be tidied. Returns the number of files removed.
/// Test: see the module docs.
pub(super) fn reap_orphan_staging_files(hnsw_path: &Path) -> usize {
    let Some(parent) = hnsw_path.parent() else {
        return 0;
    };
    let Some(snapshot_name) = hnsw_path.file_name().and_then(|n| n.to_str()) else {
        return 0;
    };
    // The two artifacts `save` stages: `<stem>.usearch` and `<stem>.keys.json`.
    let sidecar = hnsw_path.with_extension("keys.json");
    let sidecar_name = sidecar.file_name().and_then(|n| n.to_str()).unwrap_or("");

    let entries = match std::fs::read_dir(parent) {
        Ok(e) => e,
        Err(e) => {
            tracing::debug!(
                "usearch: could not scan {} for orphaned staging files ({e}) — skipping \
                 reap (issue #2936)",
                parent.display()
            );
            return 0;
        }
    };

    let mut reaped = 0usize;
    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };
        if !is_reapable_staging(name, snapshot_name) && !is_reapable_staging(name, sidecar_name) {
            continue;
        }
        let path = entry.path();
        match std::fs::remove_file(&path) {
            Ok(()) => {
                reaped += 1;
                tracing::info!(
                    "usearch: reaped orphaned staging file {} left by a dead process \
                     (issue #2936)",
                    path.display()
                );
            }
            Err(e) => tracing::debug!(
                "usearch: could not remove orphaned staging file {} ({e}) — leaving it \
                 (issue #2936)",
                path.display()
            ),
        }
    }
    reaped
}
