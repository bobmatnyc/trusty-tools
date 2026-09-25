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
//! shares it rather than inventing a second one, plus four properties that
//! repair needs and the Codex writer did not: the temp file is a SIBLING (a
//! cross-device rename is not atomic and not even possible); the target's
//! existing permission bits are carried over (a `0600` plist must not silently
//! widen to the process umask); both the temp file and the parent directory are
//! `fsync`ed, in that order, so the atomicity survives a power loss and not
//! merely a process crash; and a symlinked target is REFUSED, because `rename`
//! replaces the link rather than writing through it.
//!
//! Test: `tests` below.
//!
//! [`write_atomic`]: crate::atomic_file::write_atomic

use std::io;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Replace `path`'s contents with `bytes`, atomically and durably.
///
/// Why: see the module docs — an interrupted direct write corrupts the target.
/// What: refuses a symlinked target, creates the parent directory, writes
/// `<path>.tm-tmp` beside the target, `fsync`s it, copies the target's
/// permission bits onto it when the target exists, renames it over `path`, then
/// `fsync`s the parent directory. A failure at any step removes the temp file
/// and leaves `path` byte-identical.
///
/// The two syncs are what make the atomicity survive a power loss rather than
/// only a process crash: the content has to be on disk before the rename that
/// publishes it, and the rename itself has to be on disk before the call
/// returns. Same ordering as [`crate::json_rmw`]'s `publish_atomic`.
///
/// Test: `write_atomic_replaces_the_contents`,
/// `write_atomic_preserves_the_targets_mode`,
/// `write_atomic_publishes_content_mode_and_no_temp_together`,
/// `write_atomic_refuses_a_symlinked_target`,
/// `write_atomic_leaves_the_original_intact_when_the_rename_fails`,
/// `write_atomic_leaves_no_temp_file_behind`.
///
/// # Errors
///
/// [`io::ErrorKind::InvalidInput`] when `path` is a symlink — `rename(2)` over
/// one replaces the LINK with a plain file, severing it silently (#8236), so a
/// repair that "succeeded" would have moved the operator's file out from under
/// them. Otherwise any I/O error from the directory creation, the write, the
/// sync, the permission copy, or the rename. The error is returned AFTER the
/// temp file is cleaned up.
///
/// # Code Contract
/// Postconditions:
/// - On `Ok`, `path` holds exactly `bytes`, synced, and is not a symlink.
/// - On `Err`, `path` is unchanged — absent, holding its prior bytes, or still
///   the symlink it was.
/// - No temp file is left behind in either case.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    refuse_symlink(path)?;
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty());
    if let Some(parent) = parent {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = temp_sibling(path);

    let result = (|| -> io::Result<()> {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(bytes)?;
        // Durability of the CONTENT has to precede the rename that publishes it.
        file.sync_all()?;
        drop(file);
        copy_mode(path, &tmp)?;
        std::fs::rename(&tmp, path)
    })();

    if result.is_err() {
        // Best effort: the caller already has the real error, and a failed
        // cleanup must not mask it.
        let _ = std::fs::remove_file(&tmp);
        return result;
    }

    // Durability of the rename itself. Unix-only: Windows has no directory
    // handle to sync. Best effort — the rename has already happened, and a
    // filesystem that refuses the handle has not made it un-happen.
    #[cfg(unix)]
    if let Some(parent) = parent
        && let Ok(dir) = std::fs::File::open(parent)
    {
        let _ = dir.sync_all();
    }
    Ok(())
}

/// Refuse a target that is a symlink.
///
/// Why: `rename(2)` resolves nothing — it replaces the link ITSELF, so a plist
/// (or a config) an operator symlinked into a dotfiles checkout would silently
/// become a plain file holding our bytes, and their real file would go stale
/// with no error anywhere (#8236). `symlink_metadata` is the one stat that does
/// not follow. A target that does not exist is not a symlink and is fine.
/// Test: `write_atomic_refuses_a_symlinked_target`.
fn refuse_symlink(path: &Path) -> io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "{} is a symlink — replacing it atomically would sever the link and leave \
                 its target stale; edit the target file directly",
                path.display()
            ),
        )),
        _ => Ok(()),
    }
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

    /// Why: the three postconditions the plist repair depends on hold TOGETHER
    /// on one published file — the content is the new content, the `0600` mode
    /// survived, and nothing is left beside it. A unit test cannot observe the
    /// `fsync` ordering itself (nothing in userspace can, short of pulling
    /// power), so this asserts everything downstream of it that is observable.
    /// Test: this test.
    #[cfg(unix)]
    #[test]
    fn write_atomic_publishes_content_mode_and_no_temp_together() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("com.trusty.mpm.plist");
        std::fs::write(&path, b"<plist>old</plist>").expect("seed");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("chmod");

        write_atomic(&path, b"<plist>new</plist>").expect("write");

        let meta = std::fs::symlink_metadata(&path).expect("stat");
        assert!(
            meta.file_type().is_file(),
            "the target stopped being a file"
        );
        assert_eq!(std::fs::read(&path).expect("read"), b"<plist>new</plist>");
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
        assert!(!temp_sibling(&path).exists(), "a temp file survived");
    }

    /// Why (#8236): `rename(2)` over a symlink replaces the LINK, so a repair
    /// that followed this path would sever an operator's dotfiles link and
    /// leave the real file stale, reporting success.
    /// Test: this test.
    #[cfg(unix)]
    #[test]
    fn write_atomic_refuses_a_symlinked_target() {
        let dir = tempfile::tempdir().expect("tempdir");
        let real = dir.path().join("real.plist");
        let link = dir.path().join("com.trusty.mpm.plist");
        std::fs::write(&real, b"original").expect("seed");
        std::os::unix::fs::symlink(&real, &link).expect("symlink");

        let err = write_atomic(&link, b"replacement").expect_err("must refuse");

        assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{err}");
        assert!(err.to_string().contains("symlink"), "{err}");
        assert!(
            std::fs::symlink_metadata(&link)
                .expect("stat")
                .file_type()
                .is_symlink(),
            "the link was replaced by a plain file"
        );
        assert_eq!(std::fs::read(&real).expect("read"), b"original");
        assert!(!temp_sibling(&link).exists());
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
