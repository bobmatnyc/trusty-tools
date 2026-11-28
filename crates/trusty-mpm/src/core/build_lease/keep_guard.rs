//! Give a pinned `CARGO_TARGET_DIR` the slot's stale-checkout guard (#11661).
//!
//! Why: a slot directory is guarded by [`super::stale_guard`], but a lease
//! whose plan keeps the caller's own directory ([`super::target_dir::TargetPlan::Keep`])
//! ran there unguarded. Two worktrees pinning one private directory get the
//! same unit names, cargo judges freshness by mtime, and the second worktree's
//! gate can run the first one's test binary and report green.
//! What: [`lock_for_checkout`] holds a `flock` on [`CHECKOUT_LOCK`] inside
//! the directory for the build's whole life, retried until the lease wait
//! ends. A lease that finds no holder takes it EXCLUSIVE, runs
//! [`invalidate_if_checkout_changed`] exactly as a slot does (the marker's
//! seal, [`super::marker_seal`]), then downgrades to shared. A lease from the
//! checkout the marker names joins a live holder SHARED, so builds of one
//! checkout run side by side as before; any other lease waits for every
//! holder. A directory a build outside any lease is compiling in (cargo's
//! lock held) is refused: what it writes cannot be verified or cleared.
//! Test: `a_kept_dir_last_built_by_another_checkout_is_cleared`,
//! `a_kept_dir_held_by_another_lease_is_refused_naming_its_checkout`,
//! `two_leases_of_one_checkout_share_a_kept_dir`,
//! `a_kept_dir_compiled_by_an_unleased_build_of_another_checkout_is_refused`,
//! and `a_kept_target_dir_last_built_by_another_checkout_rebuilds_11661` in
//! `tests/tm_build_lease.rs`.

use std::fs::File;
use std::os::fd::AsRawFd;
use std::path::Path;
use std::time::{Duration, Instant};

use super::stale_guard::{Invalidation, cargo_lock_held, invalidate_if_checkout_changed};

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
    mut packages: impl FnMut() -> Result<Vec<String>, String>,
) -> Result<(CheckoutLock, Invalidation), String> {
    std::fs::create_dir_all(dir)
        .map_err(|err| format!("could not create {}: {err}", dir.display()))?;
    let path = dir.join(CHECKOUT_LOCK);
    let file = File::options()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|err| format!("could not open {}: {err}", path.display()))?;
    let current = checkout.display().to_string();
    let ours = |dir: &Path| last_checkout(dir).as_deref() == Some(current.as_str());
    let deadline = Instant::now() + wait;
    loop {
        if try_flock(&file, libc::LOCK_EX) {
            // #11661: no lease holds the directory, so it is verified — the
            // marker's seal — or cleared, never trusted on the marker alone.
            if cargo_lock_held(dir) {
                return Err(format!(
                    "CARGO_TARGET_DIR={} is being compiled by a build outside the lease (it last \
                     recorded {}); what that build writes cannot be verified or cleared while it \
                     runs, so this checkout's crates could read as Fresh and run another \
                     checkout's binaries (#11661). Re-run when it ends",
                    dir.display(),
                    last_checkout(dir).unwrap_or_else(|| "an unrecorded checkout".to_string()),
                ));
            }
            let invalidation = invalidate_if_checkout_changed(dir, checkout, &mut packages)?;
            // A failed downgrade lost the lock to another checkout: retry.
            if try_flock(&file, libc::LOCK_SH) && ours(dir) {
                return Ok((CheckoutLock { _file: file }, invalidation));
            }
            try_flock(&file, libc::LOCK_UN);
        } else if ours(dir) && try_flock(&file, libc::LOCK_SH) {
            // A lease of this checkout holds the directory and verified it at
            // its start; the marker only changes under the exclusive lock.
            if ours(dir) {
                return Ok((CheckoutLock { _file: file }, Invalidation::SameCheckout));
            }
            try_flock(&file, libc::LOCK_UN);
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "CARGO_TARGET_DIR={} is held by a lease of another checkout than {current} for \
                 over {}s (it last built {}); building here now could run another checkout's \
                 binaries (#11661). Re-run when it ends, or give this checkout its own \
                 CARGO_TARGET_DIR",
                dir.display(),
                wait.as_secs(),
                last_checkout(dir).unwrap_or_else(|| "an unrecorded checkout".to_string()),
            ));
        }
        std::thread::sleep(LOCK_POLL);
    }
}

/// `flock(op | LOCK_NB)` on `file`; `true` when it succeeded.
fn try_flock(file: &File, op: libc::c_int) -> bool {
    // SAFETY: `file` owns a valid descriptor for the call.
    unsafe { libc::flock(file.as_raw_fd(), op | libc::LOCK_NB) == 0 }
}

/// The checkout [`super::stale_guard::LAST_CHECKOUT_MARKER`] in `dir` names, if any.
fn last_checkout(dir: &Path) -> Option<String> {
    super::marker_seal::read_marker(dir).0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::build_lease::marker_seal::seal_marker;
    use crate::core::build_lease::stale_guard::LAST_CHECKOUT_MARKER;

    // #10429: a parallel test's fork can hold a just-dropped lease's open file
    // description for a moment, so a call that expects to acquire polls up to
    // this bound; calls that expect a refusal keep `Duration::ZERO`.
    const ACQUIRE_WAIT: Duration = Duration::from_secs(5);

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
        let (lock, got) = lock_for_checkout(&dir, a, ACQUIRE_WAIT, pkgs).expect("first");
        assert!(matches!(got, Invalidation::Cleared { previous: None, .. }));
        assert!(!fp.exists(), "a missing marker clears");
        // The lease's own build, then its seal (#11661).
        let fp = fingerprint(&dir);
        assert_eq!(seal_marker(&dir, a), Ok(true));
        drop(lock);
        let (lock, got) = lock_for_checkout(&dir, a, ACQUIRE_WAIT, pkgs).expect("same");
        assert_eq!(got, Invalidation::SameCheckout);
        assert!(fp.exists(), "the same checkout keeps its fingerprints");
        drop(lock);
        let b = Path::new("/work/b");
        let (_lock, got) = lock_for_checkout(&dir, b, ACQUIRE_WAIT, pkgs).expect("other");
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
        assert!(lock_for_checkout(&dir, Path::new("/work/b"), ACQUIRE_WAIT, pkgs).is_ok());
    }

    #[test]
    fn two_leases_of_one_checkout_share_a_kept_dir() {
        let tmp = tempfile::tempdir().expect("tmp");
        let dir = tmp.path().join("target");
        let a = Path::new("/work/a");
        let (first, _) = lock_for_checkout(&dir, a, ACQUIRE_WAIT, pkgs).expect("first");
        let fp = fingerprint(&dir);
        let (second, got) = lock_for_checkout(&dir, a, ACQUIRE_WAIT, pkgs)
            .expect("a second lease of the same checkout shares the directory");
        assert_eq!(got, Invalidation::SameCheckout);
        assert!(fp.exists(), "nothing is cleared for the same checkout");
        let err = lock_for_checkout(&dir, Path::new("/work/b"), Duration::ZERO, pkgs)
            .expect_err("another checkout waits for both holders");
        assert!(err.contains("/work/a"), "{err}");
        drop(first);
        assert!(
            lock_for_checkout(&dir, Path::new("/work/b"), Duration::ZERO, pkgs).is_err(),
            "one holder still builds /work/a"
        );
        drop(second);
        assert!(lock_for_checkout(&dir, Path::new("/work/b"), ACQUIRE_WAIT, pkgs).is_ok());
        assert!(!fp.exists(), "/work/b clears /work/a's fingerprints");
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
