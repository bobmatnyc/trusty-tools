//! File I/O helpers shared by the snapshot and its restore (#9282).
//!
//! Why: split from `snapshot.rs` for the 500-SLOC cap.
//! What: verified whole-file writes, fsync, a self-cleaning staging
//! directory, hashing and error mapping. Every failure is an `Err`.
//! Test: `snapshot_tests.rs`.

use super::{Result, SnapshotError, SnapshotFile, SnapshotHook, SnapshotStep};
use sha2::{Digest, Sha256};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};

/// Read buffer for hashing.
const BUF: usize = 1 << 20;

/// Write `bytes` to `dir/name`, fsync, re-read and compare the hash.
///
/// Test: `a_copy_altered_after_fsync_fails_verification`,
/// `an_fsync_failure_leaves_no_snapshot`.
pub(crate) fn write_verified(
    dir: &Path,
    name: &str,
    bytes: &[u8],
    hook: SnapshotHook,
) -> Result<SnapshotFile> {
    let path = dir.join(name);
    hook(SnapshotStep::WriteFile, &path).map_err(io_err("write", &path))?;
    let mut f = std::fs::File::create(&path).map_err(io_err("create", &path))?;
    f.write_all(bytes).map_err(io_err("write", &path))?;
    drop(f);
    sync_file(&path, hook)?;
    hook(SnapshotStep::Verify, &path).map_err(io_err("verify", &path))?;
    let expected = hex::encode(Sha256::digest(bytes));
    let (len, actual) = hash_file(&path)?;
    if actual != expected {
        return Err(SnapshotError::ChecksumMismatch {
            path,
            expected,
            actual,
        });
    }
    Ok(SnapshotFile {
        name: name.to_string(),
        len,
        sha256: expected,
    })
}

/// Stream `src` into `dir/name`, fsync, re-read and require `expected_sha256`.
///
/// Why: restore copies a `kg.redb` that can run to hundreds of MB; streaming
/// keeps it out of memory.
/// Test: `restore_into_an_empty_root_reopens_with_the_manifest_counts`,
/// `a_restore_write_failure_leaves_no_target`.
pub(crate) fn copy_verified(
    src: &Path,
    dir: &Path,
    name: &str,
    expected_sha256: &str,
    hook: SnapshotHook,
) -> Result<()> {
    let path = dir.join(name);
    hook(SnapshotStep::WriteFile, &path).map_err(io_err("write", &path))?;
    let mut input = std::fs::File::open(src).map_err(io_err("open", src))?;
    let mut output = std::fs::File::create(&path).map_err(io_err("create", &path))?;
    io::copy(&mut input, &mut output).map_err(io_err("copy into", &path))?;
    drop(output);
    sync_file(&path, hook)?;
    hook(SnapshotStep::Verify, &path).map_err(io_err("verify", &path))?;
    let (_, actual) = hash_file(&path)?;
    if actual != expected_sha256 {
        return Err(SnapshotError::ChecksumMismatch {
            path,
            expected: expected_sha256.to_string(),
            actual,
        });
    }
    Ok(())
}

/// fsync one file through the hook.
pub(crate) fn sync_file(path: &Path, hook: SnapshotHook) -> Result<()> {
    hook(SnapshotStep::Fsync, path).map_err(io_err("fsync", path))?;
    std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .and_then(|f| f.sync_all())
        .map_err(io_err("fsync", path))
}

/// A directory being filled; removed on drop unless renamed into place.
pub(crate) struct StagingDir {
    pub(crate) path: PathBuf,
    committed: bool,
}

impl StagingDir {
    /// Create the directory; an existing one is an error, never reused.
    pub(crate) fn create(path: PathBuf) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(io_err("create", parent))?;
        }
        std::fs::create_dir(&path).map_err(io_err("create", &path))?;
        Ok(Self {
            path,
            committed: false,
        })
    }

    /// fsync the directory, rename it to `target`, fsync the parent.
    pub(crate) fn commit_to(mut self, target: &Path) -> Result<()> {
        sync_dir(&self.path)?;
        std::fs::rename(&self.path, target).map_err(io_err("rename", target))?;
        self.committed = true;
        if let Some(parent) = target.parent() {
            sync_dir(parent)?;
        }
        Ok(())
    }
}

impl Drop for StagingDir {
    fn drop(&mut self) {
        if !self.committed
            && let Err(e) = std::fs::remove_dir_all(&self.path)
        {
            tracing::warn!(path = %self.path.display(),
                "#9282: could not remove an abandoned snapshot staging dir: {e}");
        }
    }
}

/// fsync a directory so a rename inside it is durable (no-op off unix).
fn sync_dir(dir: &Path) -> Result<()> {
    if cfg!(unix) {
        std::fs::File::open(dir)
            .and_then(|f| f.sync_all())
            .map_err(io_err("fsync", dir))?;
    }
    Ok(())
}

/// Length and lower-case hex SHA-256 of a file.
pub(crate) fn hash_file(path: &Path) -> Result<(u64, String)> {
    let mut f = std::fs::File::open(path).map_err(io_err("open", path))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; BUF];
    let mut len: u64 = 0;
    loop {
        let n = f.read(&mut buf).map_err(io_err("read", path))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        len += n as u64;
    }
    Ok((len, hex::encode(hasher.finalize())))
}

/// `path` made absolute and resolved the way a later create would see it.
///
/// Why: the registry guard (#9282) must compare where a backup would really
/// land, through symlinks and `..`, including for a path not created yet.
/// What: walks the components from the root, keeping the prefix canonical:
/// an existing component is canonicalised (symlinks followed), a missing one
/// is appended as is, `..` pops one level. A dangling symlink counts as
/// missing: it is never followed, since mkdir on it fails with EEXIST and the
/// create refuses. Any error other than "not found" is returned, so the guard
/// fails closed; a symlink loop is such an error (`ELOOP`).
/// Test: `a_destination_in_the_registry_is_refused_and_writes_nothing`,
/// `a_flat_layout_dest_under_palaces_is_refused_and_creates_nothing`,
/// `an_unresolvable_dest_prefix_is_an_error_and_writes_nothing`,
/// `a_self_referencing_dangling_symlink_dest_is_an_error_not_a_crash`.
pub(crate) fn resolve_existing_prefix(path: &Path) -> io::Result<PathBuf> {
    let abs = std::path::absolute(path)?;
    let mut base = PathBuf::new();
    for comp in abs.components() {
        match comp {
            Component::Prefix(_) | Component::RootDir => base.push(comp.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                base.pop();
            }
            Component::Normal(name) => {
                let next = base.join(name);
                base = match next.canonicalize() {
                    Ok(real) => real,
                    // #9282: a dangling link is appended, never followed; following
                    // `evil -> missing/../evil` recursed until the stack overflowed.
                    Err(e) if e.kind() == io::ErrorKind::NotFound => next,
                    Err(e) => return Err(e),
                };
            }
        }
    }
    Ok(base)
}

/// `<path><suffix>` in the same directory, so a rename stays atomic.
pub(crate) fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

/// Map an `io::Error` at `path` into [`SnapshotError::Io`].
pub(crate) fn io_err(op: &'static str, path: &Path) -> impl FnOnce(io::Error) -> SnapshotError {
    let path = path.to_path_buf();
    move |source| SnapshotError::Io { op, path, source }
}

/// Map any displayable redb or anyhow error into [`SnapshotError::Redb`].
pub(crate) fn redb_err<E: std::fmt::Display>(
    op: &'static str,
    path: &Path,
) -> impl FnOnce(E) -> SnapshotError {
    let path = path.to_path_buf();
    move |e| SnapshotError::Redb {
        op,
        path,
        reason: format!("{e:#}"),
    }
}
