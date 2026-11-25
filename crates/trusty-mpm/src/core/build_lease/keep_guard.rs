//! Give a pinned `CARGO_TARGET_DIR` the slot's stale-checkout guard (#11661).
//!
//! Why: a slot directory is guarded by [`super::stale_guard`], but a lease
//! whose plan keeps the caller's own directory ([`super::target_dir::TargetPlan::Keep`])
//! ran there unguarded. Two worktrees pinning one private directory get the
//! same unit names, cargo judges freshness by mtime, and the second worktree's
//! gate can run the first one's test binary and report green.
//! What: [`lock_for_checkout`] takes an exclusive `flock` on
//! [`CHECKOUT_LOCK`] inside the directory, bounded by the lease wait, then
//! runs [`invalidate_if_checkout_changed`] exactly as a slot does. The lock is
//! held by the returned [`CheckoutLock`] for the build's whole life, so no
//! other lease builds another checkout there meanwhile. A directory a build
//! outside any lease is compiling in (cargo's lock held) while it last built
//! another checkout is refused: its fingerprints cannot be cleared safely.
//! Test: `a_kept_dir_last_built_by_another_checkout_is_cleared`,
//! `a_kept_dir_held_by_another_lease_is_refused_naming_its_checkout`,
//! `a_kept_dir_compiled_by_an_unleased_build_of_another_checkout_is_refused`,
//! and `a_kept_target_dir_last_built_by_another_checkout_rebuilds_11661` in
//! `tests/tm_build_lease.rs`.

use std::fs::File;
use std::os::fd::AsRawFd;
use std::path::Path;
use std::time::{Duration, Instant};

use super::stale_guard::{
    Invalidation, LAST_CHECKOUT_MARKER, cargo_lock_held, invalidate_if_checkout_changed,
};

/// The lock file a lease holds inside a kept target directory.
pub const CHECKOUT_LOCK: &str = ".trusty-checkout.lock";

/// How often a held [`CHECKOUT_LOCK`] is retried.
const LOCK_POLL: Duration = Duration::from_millis(200);

/// The held [`CHECKOUT_LOCK`]; dropping it releases the `flock`.
#[derive(Debug)]
pub struct CheckoutLock {
    _file: File,
}

/// Lock `dir` for `checkout` and clear its fingerprints if another checkout
/// built there last.
///
/// What: see the module doc. `packages` lists the checkout's workspace
/// packages and runs only when a clear is needed.
///
/// # Errors
///
/// A description naming the directory and, when known, the checkout that
/// holds or last built it: the lock was not free within `wait`, cargo's lock
/// is held there by a build of another checkout, the directory or lock file
/// could not be made, or the clear failed. The caller must not build.
pub fn lock_for_checkout(
    dir: &Path,
    checkout: &Path,
    wait: Duration,
    packages: impl FnOnce() -> Result<Vec<String>, String>,
) -> Result<(CheckoutLock, Invalidation), String> {
    std::fs::create_dir_all(dir)
        .map_err(|err| format!("could not create {}: {err}", dir.display()))?;
    let path = dir.join(CHECKOUT_LOCK);
    let file = File::options()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|err| format!("could not open {}: {err}", path.display()))?;
    let deadline = Instant::now() + wait;
    loop {
        // SAFETY: `file` owns a valid descriptor for the call.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            break;
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "CARGO_TARGET_DIR={} is held by another lease, last for {}, for over {}s; \
                 a build here now could run that checkout's binaries (#11661). Re-run when \
                 it ends, or give this checkout its own CARGO_TARGET_DIR",
                dir.display(),
                last_checkout(dir).unwrap_or_else(|| "an unrecorded checkout".to_string()),
                wait.as_secs()
            ));
        }
        std::thread::sleep(LOCK_POLL);
    }
    let lock = CheckoutLock { _file: file };
    let current = checkout.display().to_string();
    let previous = last_checkout(dir);
    if previous.as_deref() != Some(current.as_str()) && cargo_lock_held(dir) {
        return Err(format!(
            "CARGO_TARGET_DIR={} is being compiled by a build outside the lease, and it last \
             built {}, not {current}; its fingerprints cannot be cleared while that build runs, \
             so this checkout's crates could read as Fresh and run another checkout's binaries \
             (#11661). Re-run when it ends",
            dir.display(),
            previous.as_deref().unwrap_or("an unrecorded checkout"),
        ));
    }
    let invalidation = invalidate_if_checkout_changed(dir, checkout, packages)?;
    Ok((lock, invalidation))
}

/// The checkout [`LAST_CHECKOUT_MARKER`] in `dir` names, if any.
fn last_checkout(dir: &Path) -> Option<String> {
    std::fs::read_to_string(dir.join(LAST_CHECKOUT_MARKER))
        .ok()
        .map(|s| s.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fingerprint(dir: &Path) -> std::path::PathBuf {
        let fp = dir.join("debug/.fingerprint/pkg-0123456789abcdef");
        std::fs::create_dir_all(&fp).expect("fingerprint");
        fp
    }

    fn pkgs() -> Result<Vec<String>, String> {
        Ok(vec!["pkg".to_string()])
    }

    #[test]
    fn a_kept_dir_last_built_by_another_checkout_is_cleared() {
        let tmp = tempfile::tempdir().expect("tmp");
        let dir = tmp.path().join("target");
        let fp = fingerprint(&dir);
        let a = Path::new("/work/a");
        let (lock, got) = lock_for_checkout(&dir, a, Duration::ZERO, pkgs).expect("first");
        assert!(matches!(got, Invalidation::Cleared { previous: None, .. }));
        assert!(!fp.exists(), "a missing marker clears");
        drop(lock);
        let fp = fingerprint(&dir);
        let (lock, got) = lock_for_checkout(&dir, a, Duration::ZERO, pkgs).expect("same");
        assert_eq!(got, Invalidation::SameCheckout);
        assert!(fp.exists(), "the same checkout keeps its fingerprints");
        drop(lock);
        let b = Path::new("/work/b");
        let (_lock, got) = lock_for_checkout(&dir, b, Duration::ZERO, pkgs).expect("other");
        assert!(
            matches!(&got, Invalidation::Cleared { previous: Some(p), removed: 1, .. } if p == "/work/a"),
            "{got:?}"
        );
        assert!(!fp.exists(), "another checkout clears");
    }

    #[test]
    fn a_kept_dir_held_by_another_lease_is_refused_naming_its_checkout() {
        let tmp = tempfile::tempdir().expect("tmp");
        let dir = tmp.path().join("target");
        let (held, _) =
            lock_for_checkout(&dir, Path::new("/work/a"), Duration::ZERO, pkgs).expect("first");
        let err = lock_for_checkout(&dir, Path::new("/work/b"), Duration::ZERO, pkgs)
            .expect_err("the lock is held");
        assert!(err.contains("/work/a") && err.contains("#11661"), "{err}");
        drop(held);
        assert!(lock_for_checkout(&dir, Path::new("/work/b"), Duration::ZERO, pkgs).is_ok());
    }

    #[test]
    fn a_kept_dir_compiled_by_an_unleased_build_of_another_checkout_is_refused() {
        let tmp = tempfile::tempdir().expect("tmp");
        let dir = tmp.path().join("target");
        std::fs::create_dir_all(dir.join("debug")).expect("profile");
        std::fs::write(dir.join(LAST_CHECKOUT_MARKER), "/work/a").expect("marker");
        let cargo_lock = File::create(dir.join("debug/.cargo-lock")).expect("lock");
        // SAFETY: valid descriptor; stands in for a running cargo.
        assert_eq!(
            unsafe { libc::flock(cargo_lock.as_raw_fd(), libc::LOCK_EX) },
            0
        );
        let fp = fingerprint(&dir);
        let err = lock_for_checkout(&dir, Path::new("/work/b"), Duration::ZERO, pkgs)
            .expect_err("cargo's lock is held");
        assert!(
            err.contains("outside the lease") && err.contains("/work/a"),
            "{err}"
        );
        assert!(fp.exists(), "nothing is cleared under a running build");
    }
}
