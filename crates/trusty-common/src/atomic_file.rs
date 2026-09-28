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
//! cross-device rename is not atomic and not even possible) with a name unique
//! to the call, so two concurrent writers never share one (#8733); the target's
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
use std::sync::atomic::{AtomicU64, Ordering};

/// Replace `path`'s contents with `bytes`, atomically and durably.
///
/// Why: see the module docs — an interrupted direct write corrupts the target.
/// What: refuses a symlinked target, creates the parent directory, creates a
/// fresh `<name>.<pid>.<n>.tm-tmp` beside the target with `create_new` (never
/// truncating an existing file) and, on Unix, mode `0600`, so a crash-stranded
/// copy is owner-only (#8733). It writes and `fsync`s it, gives it the mode the
/// target will publish with (the target's own, or the plain-create default for
/// a new target), renames it over `path`, then `fsync`s the parent directory.
/// A failure after the temp file exists removes it and leaves `path`
/// byte-identical. Concurrent callers on one `path` each stage their own file,
/// so the last rename wins with a complete document.
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
/// `write_atomic_leaves_the_original_intact_when_staging_fails`,
/// `write_atomic_removes_its_temp_file_when_the_rename_fails`,
/// `write_atomic_leaves_no_temp_file_behind`,
/// `staging_file_is_owner_only_and_never_truncates`,
/// `write_atomic_gives_a_new_target_the_default_create_mode`,
/// `temp_sibling_is_unique_per_call_and_beside_the_target`,
/// `two_concurrent_dream_stats_writers_never_publish_a_partial_file`.
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
    // #8733: a failure here leaves nothing of ours on disk, so there is
    // nothing to clean.
    let mut file = create_staging_file(&tmp)?;

    let result = (|| -> io::Result<()> {
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

/// Staging path for one [`write_atomic`] call: `<name>.<pid>.<n>.tm-tmp`.
///
/// Why: a fixed staging name is shared by concurrent writers — one truncates
/// the file another is filling, the first rename publishes it partial, and the
/// second rename fails with `ENOENT` (#8733; `json_rmw::temp_path` is the
/// precedent). What: the target's own directory, so the rename stays on one
/// filesystem; the pid separates processes and a process-wide counter
/// separates calls within one.
/// Test: `temp_sibling_is_unique_per_call_and_beside_the_target`.
fn temp_sibling(path: &Path) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let mut name = path.as_os_str().to_owned();
    name.push(format!(".{}.{n}.tm-tmp", std::process::id()));
    PathBuf::from(name)
}

/// Create one call's staging file: `create_new`, and owner-only on Unix.
///
/// Why (#8733): the bytes sit in this file from the write until the rename. A
/// crash in that window strands it under a name nothing reuses, and a stray
/// copy of a scrubbed LaunchAgent plist must not be world-readable. What:
/// `create_new` never truncates an existing file; on Unix the mode is `0600`
/// (further narrowed by the umask). [`copy_mode`] sets the published mode.
/// Test: `staging_file_is_owner_only_and_never_truncates`.
fn create_staging_file(tmp: &Path) -> io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    // #8733: owner-only while it holds the payload; a crash may strand it.
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(tmp)
}

/// Give `to` the mode `from` will publish with: `from`'s own, or — when `from`
/// does not exist yet — the mode a plain create would give it.
///
/// Why: a `0600` plist that the repair widened to the umask default would
/// undo part of what #8236 is about. A new target keeps the umask default
/// (`0666 & !umask`, `0644` under the usual `022`) that it had before the
/// staging file became `0600` (#8733).
/// Test: `write_atomic_preserves_the_targets_mode`,
/// `write_atomic_gives_a_new_target_the_default_create_mode`.
fn copy_mode(from: &Path, to: &Path) -> io::Result<()> {
    match std::fs::metadata(from) {
        Ok(meta) => std::fs::set_permissions(to, meta.permissions()),
        #[cfg(unix)]
        // #8733: undo the `0600` staging mode for a new target.
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            std::fs::set_permissions(to, default_create_mode(from)?)
        }
        #[cfg(not(unix))]
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// The mode a plain create beside `target` gets, read without touching the
/// process-global umask.
///
/// Why (#8733): `umask(2)` can only be read by setting it, which races every
/// other thread's creates. What: creates an EMPTY probe sibling with the
/// default mode, reads its permissions, removes it. Runs only for a new target.
#[cfg(unix)]
fn default_create_mode(target: &Path) -> io::Result<std::fs::Permissions> {
    let probe = temp_sibling(target);
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)?;
    let perms = std::fs::metadata(&probe).map(|m| m.permissions());
    let _ = std::fs::remove_file(&probe);
    perms
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

    /// Sorted file names in `dir` — proves no staging file survived, whatever
    /// its unique name was.
    fn entries(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .expect("read_dir")
            .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// Why: the whole point of the helper — a failed write must not corrupt the
    /// target. A read-only parent refuses the staging file, which no unique
    /// name can dodge (#8733).
    #[cfg(unix)]
    #[test]
    fn write_atomic_leaves_the_original_intact_when_staging_fails() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("unit.plist");
        std::fs::write(&path, b"original").expect("seed");
        let Some(_ro) = test_support::ReadOnlyDir::new(dir.path()) else {
            return;
        };

        let err = write_atomic(&path, b"replacement").expect_err("must fail");
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied, "{err}");
        assert_eq!(std::fs::read(&path).expect("read"), b"original");
        assert_eq!(entries(dir.path()), ["unit.plist"]);
    }

    /// Why: a failure AFTER the staging file exists must remove it. A
    /// non-empty directory at the target makes the rename itself fail.
    #[test]
    fn write_atomic_removes_its_temp_file_when_the_rename_fails() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("unit.plist");
        std::fs::create_dir(&path).expect("directory at the target");
        std::fs::write(path.join("keep"), b"kept").expect("seed");

        write_atomic(&path, b"replacement").expect_err("rename over a directory must fail");
        assert_eq!(entries(dir.path()), ["unit.plist"]);
        assert_eq!(std::fs::read(path.join("keep")).expect("read"), b"kept");
    }

    /// Why (#8733): the staging file holds the full payload until the rename;
    /// a crash strands it, so it must be owner-only from creation, and it
    /// must never open (and truncate) a file that already exists.
    #[cfg(unix)]
    #[test]
    fn staging_file_is_owner_only_and_never_truncates() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let tmp = temp_sibling(&dir.path().join("unit.plist"));

        let mut file = create_staging_file(&tmp).expect("create");
        file.write_all(b"secret").expect("write");
        let mode = std::fs::metadata(&tmp).expect("stat").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "staging file created at {mode:o}");

        let err = create_staging_file(&tmp).expect_err("an existing file must not be reopened");
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists, "{err}");
        assert_eq!(std::fs::read(&tmp).expect("read"), b"secret");
    }

    /// Why (#8733): a `0600` staging file must not make a NEW target `0600` —
    /// it keeps the mode a plain create gives it, as before.
    #[cfg(unix)]
    #[test]
    fn write_atomic_gives_a_new_target_the_default_create_mode() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let reference = dir.path().join("reference");
        std::fs::File::create(&reference).expect("plain create");
        let expected = std::fs::metadata(&reference)
            .expect("stat")
            .permissions()
            .mode();
        let path = dir.path().join("fresh.json");

        write_atomic(&path, b"{}").expect("write");

        let mode = std::fs::metadata(&path).expect("stat").permissions().mode();
        assert_eq!(mode & 0o777, expected & 0o777, "new target got {mode:o}");
        assert_eq!(entries(dir.path()), ["fresh.json", "reference"]);
    }

    /// Why (#8733): a shared staging name let concurrent writers truncate
    /// each other's file. Every call must get its own name, in the target's
    /// directory, carrying this process's pid.
    #[test]
    fn temp_sibling_is_unique_per_call_and_beside_the_target() {
        let path = Path::new("/state/dream_stats.json");
        let (a, b) = (temp_sibling(path), temp_sibling(path));
        assert_ne!(a, b);
        for tmp in [&a, &b] {
            assert_eq!(tmp.parent(), path.parent());
            let name = tmp.file_name().expect("name").to_string_lossy();
            let prefix = format!("dream_stats.json.{}.", std::process::id());
            assert!(
                name.starts_with(&prefix) && name.ends_with(".tm-tmp"),
                "{name}"
            );
        }
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
        let name = path.file_name().expect("name").to_string_lossy();
        assert_eq!(entries(dir.path()), [name], "a temp file survived");
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
        assert_eq!(entries(dir.path()).len(), 2, "only the link and its target");
    }

    /// Why: a leftover `*.tm-tmp` beside a LaunchAgent is a second
    /// readable copy of whatever the plist held.
    #[test]
    fn write_atomic_leaves_no_temp_file_behind() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("unit.plist");
        write_atomic(&path, b"fresh").expect("write");
        assert_eq!(entries(dir.path()), ["unit.plist"]);
    }
}

/// Test-only fixtures shared with callers' error-arm tests.
#[cfg(all(test, unix))]
pub(crate) mod test_support {
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};

    /// Makes a directory read-only for its lifetime, restoring `0755` on drop.
    ///
    /// Why (#8733): with a unique staging name per call, pre-blocking the
    /// temp path no longer forces a failure; a read-only parent does, for any
    /// name. What: `None` (with a skip message) when this process can still
    /// create a file there — root ignores the mode bits.
    pub(crate) struct ReadOnlyDir(PathBuf);

    impl ReadOnlyDir {
        pub(crate) fn new(dir: &Path) -> Option<Self> {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o555))
                .expect("chmod 0555");
            let guard = Self(dir.to_path_buf());
            let probe = dir.join(".read-only-probe");
            if std::fs::File::create(&probe).is_ok() {
                let _ = std::fs::remove_file(&probe);
                eprintln!(
                    "skipping: {} stays writable at mode 0555 (running as root?)",
                    dir.display()
                );
                return None;
            }
            Some(guard)
        }
    }

    impl Drop for ReadOnlyDir {
        fn drop(&mut self) {
            let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
        }
    }
}
