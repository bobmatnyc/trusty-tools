//! Verified, byte-identical palace backup taken before a format migration.
//!
//! Why: ADR-0067 D3 rule 3 — a format migration may not write until every
//! primary file is copied and proven identical, and a failed copy aborts it.
//! The `.pre-4810.bak` sidecar (`kg_redb/migrate.rs`) checks length only and
//! reuses any older file of the same length, so it cannot serve (#9274).
//! What: [`ensure_format_backup`] copies the ADR-0067 D2 primary files into
//! `<data_root>/backups/format-migration/<palace>/<from>-to-<to>-<UTC>/`. It
//! needs 1.1x the source size free first. Each file is hashed while it is
//! copied, the copy is fsynced and re-read, and the source is hashed again; any
//! difference aborts. A `MANIFEST` naming each file's length and SHA-256 is
//! written last, inside a `.tmp` directory that is then renamed into place, so
//! a backup without a manifest is incomplete and never trusted. Two complete
//! backups per palace are kept (ADR-0067 D7): the one just written and the
//! newest other. This release calls it
//! from no migration; the 0 → 1 migration does (#9274 PR3).
//! Test: format_backup_tests.rs, starting with
//! `a_verified_backup_is_byte_identical_and_reused`.

use super::palace_store::PalaceStoreError;
use crate::memory_core::maintenance_log::{
    MAINTENANCE_LOG_FILENAME, MAINTENANCE_LOG_ROTATED_FILENAME,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

/// Complete backups kept per palace (ADR-0067 D7).
pub const BACKUP_KEEP: usize = 2;

/// The file whose presence marks a backup complete.
pub const MANIFEST: &str = "MANIFEST";

/// Suffix of a backup directory still being written.
const TMP_SUFFIX: &str = ".tmp";

/// ADR-0067 D2 primary files, by name inside the palace directory. Derived
/// files (vector index, BM25, caches) are rebuilt, never backed up.
const PRIMARY_FILES: [&str; 6] = [
    "palace.json",
    "identity.txt",
    "kg.redb",
    "chat_sessions.redb",
    MAINTENANCE_LOG_FILENAME,
    MAINTENANCE_LOG_ROTATED_FILENAME,
];

/// Copy buffer size; large enough that a multi-hundred-MB `kg.redb` streams.
const COPY_BUF: usize = 1 << 20;

type Result<T> = std::result::Result<T, PalaceStoreError>;

/// One backed-up file, as recorded in [`MANIFEST`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestEntry {
    /// File name inside the palace directory.
    pub name: String,
    /// Byte length.
    pub len: u64,
    /// Lower-case hex SHA-256 of the bytes.
    pub sha256: String,
}

/// The contents of [`MANIFEST`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupManifest {
    /// Palace id the backup belongs to.
    pub palace: String,
    /// Format the palace was at.
    pub from: u32,
    /// Format the migration was moving it to.
    pub to: u32,
    /// Every file copied, in [`PRIMARY_FILES`] order.
    pub files: Vec<ManifestEntry>,
}

/// Fault and environment seams; production uses [`Seams::default`].
#[derive(Clone, Copy)]
pub(crate) struct Seams {
    /// Free bytes on the filesystem holding a path; `None` when unknown.
    pub(crate) free_space: fn(&Path) -> io::Result<Option<u64>>,
    /// Runs on each backup file after it is fsynced, before it is re-read.
    pub(crate) after_copy: fn(&Path),
}

impl Default for Seams {
    fn default() -> Self {
        Self {
            free_space: free_space_at,
            after_copy: |_| {},
        }
    }
}

/// Where one palace's format-migration backups live.
pub fn backup_root(data_root: &Path, palace_id: &str) -> PathBuf {
    data_root
        .join("backups")
        .join("format-migration")
        .join(palace_id)
}

/// Ensure a verified backup of `palace_dir` exists for a `from` → `to` move.
///
/// Why: see the module doc; the caller must not begin a write until this
/// returns `Ok`.
/// What: reuses a complete backup for the same move whose files still match
/// its manifest (a re-run after a crash, ADR-0067 D3 rule 4); removes an
/// incomplete one; otherwise writes a new backup and prunes to
/// [`BACKUP_KEEP`]. Returns the backup directory.
/// Test: `a_verified_backup_is_byte_identical_and_reused`,
/// `incomplete_backup_without_manifest_is_not_trusted`.
pub fn ensure_format_backup(
    data_root: &Path,
    palace_id: &str,
    palace_dir: &Path,
    from: u32,
    to: u32,
) -> Result<PathBuf> {
    ensure_with(data_root, palace_id, palace_dir, from, to, Seams::default())
}

/// [`ensure_format_backup`] with explicit [`Seams`].
pub(crate) fn ensure_with(
    data_root: &Path,
    palace_id: &str,
    palace_dir: &Path,
    from: u32,
    to: u32,
    seams: Seams,
) -> Result<PathBuf> {
    let root = backup_root(data_root, palace_id);
    std::fs::create_dir_all(&root).map_err(|e| failed(&root, e))?;
    let prefix = format!("{from}-to-{to}-");
    for dir in list_dirs(&root)? {
        let name = file_name(&dir);
        if !name.starts_with(&prefix) {
            continue;
        }
        if name.ends_with(TMP_SUFFIX) || !has_manifest(&dir)? {
            // #9274: a backup without a manifest never finished; replace it.
            std::fs::remove_dir_all(&dir).map_err(|e| failed(&dir, e))?;
        } else if verify_backup(&dir).is_ok() {
            return Ok(dir);
        }
    }

    let sources = present_sources(palace_dir)?;
    check_space(&root, &sources, seams)?;

    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%S%.3fZ");
    let final_dir = root.join(format!("{prefix}{stamp}"));
    let tmp_dir = root.join(format!("{prefix}{stamp}{TMP_SUFFIX}"));
    std::fs::create_dir(&tmp_dir).map_err(|e| failed(&tmp_dir, e))?;

    let mut files = Vec::with_capacity(sources.len());
    for (name, src) in &sources {
        files.push(copy_verified(name, src, &tmp_dir.join(name), seams)?);
    }
    let manifest = BackupManifest {
        palace: palace_id.to_string(),
        from,
        to,
        files,
    };
    write_manifest(&tmp_dir, &manifest)?;
    sync_dir(&tmp_dir)?;
    std::fs::rename(&tmp_dir, &final_dir).map_err(|e| failed(&final_dir, e))?;
    sync_dir(&root)?;
    // #9274: the stamp order can put this backup last (a clock moved back);
    // prune must never delete the backup this call returns.
    prune(&root, &final_dir)?;
    Ok(final_dir)
}

/// Re-hash every file a backup's manifest names and compare.
///
/// Why: a backup is trusted only while it still matches what was verified.
/// Test: `a_verified_backup_is_byte_identical_and_reused`.
pub fn verify_backup(dir: &Path) -> Result<BackupManifest> {
    let path = dir.join(MANIFEST);
    let bytes = std::fs::read(&path).map_err(|e| failed(&path, e))?;
    let manifest: BackupManifest =
        serde_json::from_slice(&bytes).map_err(|source| PalaceStoreError::Json { path, source })?;
    for entry in &manifest.files {
        let file = dir.join(&entry.name);
        let (len, sha) = hash_file(&file)?;
        if len != entry.len || sha != entry.sha256 {
            return Err(mismatch(&file, &entry.sha256, &sha));
        }
    }
    Ok(manifest)
}

/// The primary files that exist in `palace_dir`, with their paths.
fn present_sources(palace_dir: &Path) -> Result<Vec<(&'static str, PathBuf)>> {
    let mut out = Vec::new();
    for name in PRIMARY_FILES {
        let path = palace_dir.join(name);
        if path.try_exists().map_err(|e| failed(&path, e))? {
            out.push((name, path));
        }
    }
    Ok(out)
}

/// Refuse to start a copy the filesystem cannot hold with 10% headroom.
///
/// Test: `insufficient_space_aborts_before_copy`.
fn check_space(root: &Path, sources: &[(&str, PathBuf)], seams: Seams) -> Result<()> {
    let mut total: u64 = 0;
    for (_, src) in sources {
        let len = std::fs::metadata(src).map_err(|e| failed(src, e))?.len();
        total = total.saturating_add(len);
    }
    let needed = total.saturating_add(total.div_ceil(10));
    match (seams.free_space)(root).map_err(|e| failed(root, e))? {
        Some(available) if available < needed => Err(PalaceStoreError::InsufficientSpace {
            path: root.to_path_buf(),
            needed,
            available,
        }),
        Some(_) => Ok(()),
        None => {
            // #9274: no statvfs here; the verified copy still fails on a full disk.
            tracing::warn!(path = %root.display(), needed,
                "free-space check unavailable on this platform; copying without it");
            Ok(())
        }
    }
}

/// Copy one file, then prove the copy and the source both match its bytes.
///
/// What: the source is hashed while it is copied; the copy is fsynced, the
/// seam runs, the copy is re-read and the source re-hashed. A copy that
/// differs, or a source that changed under the copy, is `BackupVerifyMismatch`.
/// Test: `backup_hash_mismatch_aborts_with_verify_error`,
/// `a_source_changed_during_the_copy_aborts`.
fn copy_verified(name: &str, src: &Path, dst: &Path, seams: Seams) -> Result<ManifestEntry> {
    let mut input = std::fs::File::open(src).map_err(|e| failed(src, e))?;
    let mut output = std::fs::File::create(dst).map_err(|e| failed(dst, e))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; COPY_BUF];
    let mut len: u64 = 0;
    loop {
        let n = input.read(&mut buf).map_err(|e| failed(src, e))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        output.write_all(&buf[..n]).map_err(|e| failed(dst, e))?;
        len += n as u64;
    }
    output.sync_all().map_err(|e| failed(dst, e))?;
    drop(output);
    let expected = hex::encode(hasher.finalize());

    (seams.after_copy)(dst);
    let (_, copied) = hash_file(dst)?;
    if copied != expected {
        return Err(mismatch(dst, &expected, &copied));
    }
    let (_, after) = hash_file(src)?;
    if after != expected {
        return Err(mismatch(src, &expected, &after));
    }
    Ok(ManifestEntry {
        name: name.to_string(),
        len,
        sha256: expected,
    })
}

/// Write [`MANIFEST`] through a temp file, fsynced, then renamed.
///
/// Test: `a_failed_manifest_write_leaves_no_complete_backup`.
fn write_manifest(dir: &Path, manifest: &BackupManifest) -> Result<()> {
    let path = dir.join(MANIFEST);
    let tmp = dir.join(format!("{MANIFEST}{TMP_SUFFIX}"));
    let bytes = serde_json::to_vec_pretty(manifest).map_err(|source| PalaceStoreError::Json {
        path: path.clone(),
        source,
    })?;
    let mut f = std::fs::File::create(&tmp).map_err(|e| failed(&tmp, e))?;
    f.write_all(&bytes).map_err(|e| failed(&tmp, e))?;
    f.sync_all().map_err(|e| failed(&tmp, e))?;
    std::fs::rename(&tmp, &path).map_err(|e| failed(&path, e))
}

/// Keep `keep` plus the newest complete backups, [`BACKUP_KEEP`] in all;
/// never touch an incomplete one.
///
/// Why: the stamp in a directory name is wall-clock time. After the clock
/// moves back, older backups can carry later stamps than the one just
/// written, and a plain newest-first cut would delete it (#9274).
/// What: `keep` always survives and counts toward [`BACKUP_KEEP`]; the other
/// complete backups are ranked by stamp and the oldest are removed.
/// Test: `retention_keeps_the_two_newest_complete_backups`,
/// `prune_never_deletes_the_backup_it_just_wrote`.
fn prune(root: &Path, keep: &Path) -> Result<()> {
    let mut complete: Vec<(String, PathBuf)> = Vec::new();
    for dir in list_dirs(root)? {
        let name = file_name(&dir);
        if dir != keep && !name.ends_with(TMP_SUFFIX) && has_manifest(&dir)? {
            complete.push((stamp_of(&name).to_string(), dir));
        }
    }
    complete.sort_by(|a, b| b.0.cmp(&a.0));
    for (_, dir) in complete.into_iter().skip(BACKUP_KEEP.saturating_sub(1)) {
        std::fs::remove_dir_all(&dir).map_err(|e| failed(&dir, e))?;
    }
    Ok(())
}

/// Whether `dir` holds a [`MANIFEST`]; a stat that fails is an error, never
/// "absent" (ADR-0045), because "absent" leads to a delete.
fn has_manifest(dir: &Path) -> Result<bool> {
    let path = dir.join(MANIFEST);
    path.try_exists().map_err(|e| failed(&path, e))
}

/// The UTC stamp of `<from>-to-<to>-<stamp>`; the whole name if malformed.
fn stamp_of(name: &str) -> &str {
    name.rsplit_once('-').map_or(name, |(_, s)| s)
}

/// Immediate child directories of `root`.
fn list_dirs(root: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(root).map_err(|e| failed(root, e))? {
        let entry = entry.map_err(|e| failed(root, e))?;
        if entry
            .file_type()
            .map_err(|e| failed(&entry.path(), e))?
            .is_dir()
        {
            out.push(entry.path());
        }
    }
    Ok(out)
}

/// Length and lower-case hex SHA-256 of a file.
fn hash_file(path: &Path) -> Result<(u64, String)> {
    let mut f = std::fs::File::open(path).map_err(|e| failed(path, e))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; COPY_BUF];
    let mut len: u64 = 0;
    loop {
        let n = f.read(&mut buf).map_err(|e| failed(path, e))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        len += n as u64;
    }
    Ok((len, hex::encode(hasher.finalize())))
}

/// fsync a directory so a rename inside it is durable (no-op off unix).
fn sync_dir(dir: &Path) -> Result<()> {
    if cfg!(unix) {
        std::fs::File::open(dir)
            .and_then(|f| f.sync_all())
            .map_err(|e| failed(dir, e))?;
    }
    Ok(())
}

/// Free bytes available to this user on the filesystem holding `path`.
///
/// Why: supervisor ruling Q3 (#9274) — `statvfs`, no new crate.
#[cfg(unix)]
#[allow(clippy::unnecessary_cast)] // field widths differ between Linux and macOS
fn free_space_at(path: &Path) -> io::Result<Option<u64>> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    // SAFETY: `statvfs` is plain data; all-zero is a valid value to overwrite.
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `c` is NUL-terminated and outlives the call; `st` is writable.
    if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(Some(
        (st.f_bavail as u64).saturating_mul(st.f_frsize as u64),
    ))
}

/// No free-space probe off unix; the caller logs and continues.
#[cfg(not(unix))]
fn free_space_at(_path: &Path) -> io::Result<Option<u64>> {
    Ok(None)
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn failed(path: &Path, source: io::Error) -> PalaceStoreError {
    PalaceStoreError::BackupFailed {
        path: path.to_path_buf(),
        source,
    }
}

fn mismatch(file: &Path, expected: &str, actual: &str) -> PalaceStoreError {
    PalaceStoreError::BackupVerifyMismatch {
        file: file.to_path_buf(),
        expected: expected.to_string(),
        actual: actual.to_string(),
    }
}

#[cfg(test)]
#[path = "format_backup_tests.rs"]
mod tests;
