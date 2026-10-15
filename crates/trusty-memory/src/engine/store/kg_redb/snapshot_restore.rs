//! Offline restore of a palace snapshot into an empty palace root (#9282).
//!
//! Why: a backup is only as good as its restore, and a restore that writes
//! over live data, or writes before it has proved the snapshot whole, turns a
//! bad backup into a lost palace.
//! What: [`restore_palace_snapshot`] reads and checks the manifest, re-hashes
//! every file, re-counts the copied databases, and only then stages the files
//! in a hidden sibling of the target and renames it into place.
//! Test: `restore_into_an_empty_root_reopens_with_the_manifest_counts`,
//! `restore_refuses_a_non_empty_target_and_leaves_it_unchanged`,
//! `restore_with_a_checksum_mismatch_writes_nothing`.

use super::{
    CHAT_FILE, DRAWER_COUNTS_FILE, JOURNAL_FILES, KG_FILE, PLAIN_FILES, Result,
    SNAPSHOT_FORMAT_VERSION, SNAPSHOT_KIND, SNAPSHOT_MANIFEST, SnapshotError, SnapshotHook,
    SnapshotManifest, StagingDir, check_counts, copy_verified, hash_file, io_err, no_fault,
    redb_err, table_counts,
};
use crate::engine::store::palace_format::PALACE_FORMAT_SUPPORTED;
use redb::ReadableDatabase;
use std::collections::BTreeMap;
use std::path::Path;

/// Restore the snapshot at `snapshot_dir` into the empty palace root `target`.
///
/// Why: see the module doc. Offline only: the target is a palace directory no
/// process has open, and this function never touches a running daemon.
/// What: in order — parse the manifest and refuse a newer snapshot or palace
/// format; refuse a non-empty target with [`SnapshotError::TargetNotEmpty`];
/// re-hash every file the manifest names and re-count both databases against
/// it. Nothing is written until all of that passes. Then every palace file is
/// copied into `.<target name>.restore.tmp`, fsynced and re-hashed, and the
/// directory is renamed onto `target` (an empty `target` directory is removed
/// first). `drawer_counts.jsonl` stays in the snapshot: it belongs to the data
/// root, outside the palace root. Derived files are not in a snapshot; the
/// daemon re-embeds the restored drawers. Returns the verified manifest.
/// Test: `restore_into_an_empty_root_reopens_with_the_manifest_counts`,
/// `restore_refuses_a_non_empty_target_and_leaves_it_unchanged`,
/// `restore_with_a_checksum_mismatch_writes_nothing`,
/// `restore_refuses_a_newer_palace_format`.
pub fn restore_palace_snapshot(snapshot_dir: &Path, target: &Path) -> Result<SnapshotManifest> {
    restore_with(snapshot_dir, target, no_fault)
}

/// [`restore_palace_snapshot`] with an explicit fault hook.
pub(crate) fn restore_with(
    snapshot_dir: &Path,
    target: &Path,
    hook: SnapshotHook,
) -> Result<SnapshotManifest> {
    let manifest = read_manifest(snapshot_dir)?;
    refuse_non_empty(target)?;
    verify_snapshot(snapshot_dir, &manifest)?;

    let name = target
        .file_name()
        .ok_or_else(|| SnapshotError::InvalidManifest {
            path: target.to_path_buf(),
            reason: "restore target has no directory name".to_string(),
        })?
        .to_string_lossy()
        .into_owned();
    let parent = target.parent().unwrap_or(Path::new("."));
    let staging = StagingDir::create(parent.join(format!(".{name}.restore.tmp")))?;
    for entry in &manifest.files {
        if entry.name == DRAWER_COUNTS_FILE {
            continue;
        }
        let src = snapshot_dir.join(&entry.name);
        copy_verified(&src, &staging.path, &entry.name, &entry.sha256, hook)?;
    }
    // #9282: `refuse_non_empty` passed, so an existing target is an empty
    // directory; `remove_dir` fails rather than delete anything written since.
    if target.try_exists().map_err(io_err("stat", target))? {
        std::fs::remove_dir(target).map_err(io_err("remove empty", target))?;
    }
    staging.commit_to(target)?;
    Ok(manifest)
}

/// Parse the manifest and refuse anything this build cannot restore.
fn read_manifest(snapshot_dir: &Path) -> Result<SnapshotManifest> {
    let path = snapshot_dir.join(SNAPSHOT_MANIFEST);
    let bytes = std::fs::read(&path).map_err(io_err("read", &path))?;
    let manifest: SnapshotManifest =
        serde_json::from_slice(&bytes).map_err(|source| SnapshotError::ManifestJson {
            path: path.clone(),
            source,
        })?;
    let invalid = |reason: String| SnapshotError::InvalidManifest {
        path: path.clone(),
        reason,
    };
    if manifest.kind != SNAPSHOT_KIND {
        return Err(invalid(format!("kind is {:?}", manifest.kind)));
    }
    if manifest.format_version > SNAPSHOT_FORMAT_VERSION {
        return Err(invalid(format!(
            "snapshot format {} is newer than this build reads ({SNAPSHOT_FORMAT_VERSION})",
            manifest.format_version
        )));
    }
    if let Some(found) = manifest.palace_format
        && found > PALACE_FORMAT_SUPPORTED
    {
        return Err(SnapshotError::PalaceFormatTooNew {
            found,
            supported: PALACE_FORMAT_SUPPORTED,
        });
    }
    let allowed = [KG_FILE, CHAT_FILE, DRAWER_COUNTS_FILE]
        .into_iter()
        .chain(PLAIN_FILES)
        .chain(JOURNAL_FILES);
    let allowed: Vec<&str> = allowed.collect();
    for entry in &manifest.files {
        if !allowed.contains(&entry.name.as_str()) {
            return Err(invalid(format!("unexpected file {:?}", entry.name)));
        }
    }
    Ok(manifest)
}

/// A target that exists must be an empty directory.
fn refuse_non_empty(target: &Path) -> Result<()> {
    if !target.try_exists().map_err(io_err("stat", target))? {
        return Ok(());
    }
    let refused = || SnapshotError::TargetNotEmpty {
        path: target.to_path_buf(),
    };
    if !target.is_dir() {
        return Err(refused());
    }
    let mut entries = std::fs::read_dir(target).map_err(io_err("list", target))?;
    match entries.next() {
        None => Ok(()),
        Some(Ok(_)) => Err(refused()),
        Some(Err(e)) => Err(io_err("list", target)(e)),
    }
}

/// Re-hash every file and re-count both databases against the manifest.
///
/// Test: `restore_with_a_checksum_mismatch_writes_nothing`.
fn verify_snapshot(dir: &Path, manifest: &SnapshotManifest) -> Result<()> {
    for entry in &manifest.files {
        let path = dir.join(&entry.name);
        let (len, sha256) = hash_file(&path)?;
        if len != entry.len || sha256 != entry.sha256 {
            return Err(SnapshotError::ChecksumMismatch {
                path,
                expected: entry.sha256.clone(),
                actual: sha256,
            });
        }
    }
    let has = |name: &str| manifest.files.iter().any(|f| f.name == name);
    if has(KG_FILE) {
        recount(&dir.join(KG_FILE), &manifest.kg_tables)?;
    }
    if has(CHAT_FILE) {
        recount(&dir.join(CHAT_FILE), &manifest.chat_tables)?;
    }
    Ok(())
}

/// Open a snapshot database read-only and compare its counts with `expected`.
fn recount(path: &Path, expected: &BTreeMap<String, u64>) -> Result<()> {
    let db = trusty_common::redb_cache::open_palace_db_read_only(path)
        .map_err(redb_err("open read-only", path))?;
    let rtx = db.begin_read().map_err(redb_err("begin read", path))?;
    let names: Vec<&str> = expected.keys().map(String::as_str).collect();
    let actual = table_counts(&rtx, path, &names)?;
    check_counts(path, expected, &actual)
}
