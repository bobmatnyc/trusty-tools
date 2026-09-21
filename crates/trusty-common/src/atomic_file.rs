//! Crash-safe file replacement: temp file in the same directory, then rename.
//!
//! Why: #8236's `tm doctor --fix` rewrites a LIVE
//! `~/Library/LaunchAgents/com.trusty.*.plist`. A `std::fs::write` interrupted
//! partway through leaves a truncated plist, and launchd refuses to load a unit
//! it cannot parse — the repair would take the daemon down. `rename(2)` within
//! one filesystem is atomic, so an observer sees either the old file or the new
//! one and never a half-written one.
//!
//! What: [`write_atomic`] is the one helper. It is the pattern
//! `codex_config::write_atomic` already used, hoisted here so the plist repair
//! shares it rather than inventing a second one, plus two properties that
//! repair needs and the Codex writer did not: the temp file is a SIBLING (a
//! cross-device rename is not atomic and not even possible), and the target's
//! existing permission bits are carried over (a `0600` plist must not silently
//! widen to the process umask).
//!
//! Test: `tests` below.

use std::io;
use std::path::{Path, PathBuf};

/// Replace `path`'s contents with `bytes`, atomically.
///
/// Why: see the module docs — an interrupted direct write corrupts the target.
/// What: creates the parent directory, writes `<path>.tm-tmp` beside the
/// target, copies the target's permission bits onto it when the target exists,
/// flushes it to disk, then renames it over `path`. A failure at any step
/// removes the temp file and leaves `path` byte-identical.
///
/// # Errors
///
/// Any I/O error from the directory creation, the write, the flush, the
/// permission copy, or the rename. The error is returned AFTER the temp file is
/// cleaned up.
///
/// # Code Contract
/// Postconditions:
/// - On `Ok`, `path` holds exactly `bytes`.
/// - On `Err`, `path` is unchanged — either absent or holding its prior bytes.
/// - No temp file is left behind in either case.
///
/// Test: `write_atomic_replaces_the_contents`,
/// `write_atomic_preserves_the_targets_mode`,
/// `write_atomic_leaves_the_original_intact_when_the_rename_fails`,
/// `write_atomic_leaves_no_temp_file_behind`.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = temp_sibling(path);

    let result = (|| -> io::Result<()> {
        std::fs::write(&tmp, bytes)?;
        copy_mode(path, &tmp)?;
        std::fs::rename(&tmp, path)
    })();

    if result.is_err() {
        // Best effort: the caller already has the real error, and a failed
        // cleanup must not mask it.
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// `<path>.tm-tmp`, in the same directory so the rename stays intra-filesystem.
fn temp_sibling(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".tm-tmp");
    PathBuf::from(name)
}

/// Carry `from`'s permission bits onto `to`, when `from` exists.
///
/// Why: a `0600` plist that the repair widened to the umask default would
/// undo part of what #8236 is about. An absent target has no mode to copy, and
/// the umask default is then correct.
fn copy_mode(from: &Path, to: &Path) -> io::Result<()> {
    match std::fs::metadata(from) {
        Ok(meta) => std::fs::set_permissions(to, meta.permissions()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: the base case — the helper has to actually replace the file.
    #[test]
    fn write_atomic_replaces_the_contents() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("unit.plist");
        std::fs::write(&path, b"old").expect("seed");
        write_atomic(&path, b"new").expect("write");
        assert_eq!(std::fs::read(&path).expect("read"), b"new");
    }

    /// Why: #8236 — a `0600` plist must not widen to the umask default when the
    /// repair rewrites it.
    #[cfg(unix)]
    #[test]
    fn write_atomic_preserves_the_targets_mode() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("unit.plist");
        std::fs::write(&path, b"old").expect("seed");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("chmod");

        write_atomic(&path, b"new").expect("write");

        let mode = std::fs::metadata(&path).expect("stat").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "mode widened to {mode:o}");
    }

    /// Why: the whole point of the helper — a failed write must not corrupt the
    /// target. A directory standing where the temp file would go makes the
    /// write fail without touching the original.
    #[test]
    fn write_atomic_leaves_the_original_intact_when_the_rename_fails() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("unit.plist");
        std::fs::write(&path, b"original").expect("seed");
        std::fs::create_dir(temp_sibling(&path)).expect("block the temp path");

        let err = write_atomic(&path, b"replacement").expect_err("must fail");
        assert!(!matches!(err.kind(), io::ErrorKind::NotFound), "{err}");
        assert_eq!(std::fs::read(&path).expect("read"), b"original");
    }

    /// Why: a leftover `<path>.tm-tmp` beside a LaunchAgent is a second
    /// readable copy of whatever the plist held.
    #[test]
    fn write_atomic_leaves_no_temp_file_behind() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("unit.plist");
        write_atomic(&path, b"fresh").expect("write");
        assert!(!temp_sibling(&path).exists());
    }
}
