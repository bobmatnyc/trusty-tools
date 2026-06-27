//! System-scope advisory lock for `trusty-installer up` (DOC-12 §3.4 / DOC-3 §4).
//!
//! Why: STAGE 0 must serialise concurrent `trusty-installer up` invocations (e.g. a launchd
//! boot trigger racing a manual run) so the always-on core is never
//! double-started. DOC-3 §4 specifies a filesystem advisory lock (`ensure.lock`)
//! around the system-mutating stages; this is the controller's flock(2)-based
//! implementation.
//!
//! What: `BootLock` is an RAII guard that holds an exclusive `flock` on
//! `~/.trusty-tools/trusty-installer/ensure.lock` for its lifetime and releases
//! it (and the fd) on drop. `acquire()` takes the lock (blocking, so the loser
//! waits then proceeds to re-CHECK and no-op per DOC-3 §4 loser behavior);
//! `try_acquire_at()` is a non-blocking variant used by the tests.
//!
//! Test: `super::tests::lock_is_exclusive` asserts a second non-blocking acquire
//! on the same path fails while the first guard is held, and succeeds after drop.

use std::fs::{File, OpenOptions};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

/// RAII guard holding the system-scope boot advisory lock.
///
/// Why: Tying the lock lifetime to a value means it is always released on scope
/// exit (including on early-return/error paths), avoiding a stuck lock.
///
/// What: Owns the open lock `File`; `Drop` releases the `flock` via `LOCK_UN`
/// then closes the fd.
///
/// Test: `super::tests::lock_is_exclusive`.
#[derive(Debug)]
pub struct BootLock {
    file: File,
    path: PathBuf,
}

/// Resolve the default boot lock path: `~/.trusty-tools/trusty-installer/ensure.lock`.
///
/// Why: Co-locates the lock with the #1220 installer config dir so all
/// installer state lives under one root.
///
/// What: Joins `dirs::home_dir()` with `.trusty-tools/trusty-installer/ensure.lock`.
/// Returns `None` if the home dir is unresolvable.
///
/// Test: `super::tests::lock_path_shape`.
pub fn lock_path() -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    Some(
        home.join(".trusty-tools")
            .join("trusty-installer")
            .join("ensure.lock"),
    )
}

impl BootLock {
    /// Acquire the boot lock at the default path, blocking until available.
    ///
    /// Why: STAGE 0 blocks on the lock so the loser of a race waits, then
    /// re-CHECKs and no-ops (DOC-3 §4) rather than double-starting the core.
    ///
    /// What: Resolves `lock_path()` and delegates to `acquire_at(.., blocking=true)`.
    ///
    /// Test: Path-resolving wrapper; covered via `acquire_at` in `super::tests`.
    pub fn acquire() -> anyhow::Result<Self> {
        let path = lock_path()
            .ok_or_else(|| anyhow::anyhow!("cannot resolve home dir for the boot lock"))?;
        Self::acquire_at(&path, true)
    }

    /// Acquire the lock at a specific path, optionally non-blocking.
    ///
    /// Why: Tests need a deterministic, non-blocking variant on a temp path;
    /// production uses the blocking default-path wrapper above.
    ///
    /// What: Creates the parent dir, opens (create) the lock file, and calls
    /// `flock(LOCK_EX[|LOCK_NB])`. On a non-blocking conflict returns an error.
    ///
    /// Test: `super::tests::lock_is_exclusive`.
    pub fn acquire_at(path: &Path, blocking: bool) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(path)?;
        let mut op = libc::LOCK_EX;
        if !blocking {
            op |= libc::LOCK_NB;
        }
        // SAFETY: `file` owns a valid open fd for the duration of this call.
        let rc = unsafe { libc::flock(file.as_raw_fd(), op) };
        if rc != 0 {
            let err = std::io::Error::last_os_error();
            anyhow::bail!("could not acquire boot lock at {}: {err}", path.display());
        }
        Ok(Self {
            file,
            path: path.to_path_buf(),
        })
    }
}

impl Drop for BootLock {
    fn drop(&mut self) {
        // Best-effort release; the OS also drops the lock when the fd closes.
        // SAFETY: `self.file` still owns a valid open fd here.
        unsafe {
            libc::flock(self.file.as_raw_fd(), libc::LOCK_UN);
        }
        tracing::debug!(path = %self.path.display(), "released boot lock");
    }
}
