//! Give a pinned `CARGO_TARGET_DIR` the slot's stale-checkout guard (#11661).
//!
//! Why: a slot directory is guarded by [`super::stale_guard`], but a lease
//! whose plan keeps the caller's own directory ([`super::target_dir::TargetPlan::Keep`])
//! ran there unguarded. Two worktrees pinning one private directory get the
//! same unit names, cargo judges freshness by mtime, and the second worktree's
//! gate can run the first one's test binary and report green.
//! What: [`lock_for_checkout`] holds an EXCLUSIVE `flock` on
//! [`CHECKOUT_LOCK`] inside the directory for the build's whole life, retried
//! until the lease wait ends, and runs [`invalidate_if_checkout_changed`]
//! exactly as a slot does (the marker's seal, [`super::marker_seal`]).
//! #8692 (#9223): every other lease waits, a lease of the same checkout too,
//! so no second build replaces binaries the holder's tests or program run;
//! that ownership is what lets a kept-dir lease leave compile capacity once
//! cargo drops its lock ([`super::phase::flip_lock_dir`]). [`lock_dir`] is the
//! same exclusive lock without the checkout guard, for cargo's own
//! `target/` (the `Unset` plan). A lock file that cannot be opened or locked
//! (anything but "held") refuses at once, naming the file: the directory's
//! owner is then unknown (#8692 Q1, owner ruling: refuse). A directory a
//! build outside any lease is compiling in (cargo's lock held) is refused by
//! [`lock_for_checkout`]: what it writes cannot be verified or cleared.
//! Test: `a_kept_dir_last_built_by_another_checkout_is_cleared`,
//! `a_kept_dir_held_by_another_lease_is_refused_naming_its_checkout`,
//! `a_second_lease_of_one_checkout_waits_for_a_kept_dir_8692`,
//! `a_kept_dir_compiled_by_an_unleased_build_of_another_checkout_is_refused`,
//! `an_unopenable_dir_lock_refuses_naming_the_lock_file_8692`,
//! `a_held_dir_lock_refuses_naming_the_lock_file_8692`,
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
/// packages and runs only when a clear is needed. The lock is exclusive for
/// the build's whole life (#8692).
///
/// # Errors
///
/// A description naming the directory and, when known, the checkout that
/// holds or last built it: the lock was not free within `wait`, cargo's lock
/// is held there by a build outside the lease, the directory or lock file
/// could not be made, opened or locked, or the clear failed. The caller must
/// not build.
pub fn lock_for_checkout(
    dir: &Path,
    checkout: &Path,
    wait: Duration,
    mut packages: impl FnMut() -> Result<Vec<String>, String>,
) -> Result<(CheckoutLock, Invalidation), String> {
    let lock = lock_dir(dir, wait)?;
    // #11661: the lease holds the directory, so it is verified — the
    // marker's seal — or cleared, never trusted on the marker alone.
    if cargo_lock_held(dir) {
        return Err(format!(
            "CARGO_TARGET_DIR={} is being compiled by a build outside the lease (it last \
             recorded {}); what that build writes cannot be verified or cleared while it \
             runs, so this checkout's crates could read as Fresh and run another \
             checkout's binaries (#11661). Re-run when it ends",
            dir.display(),
            last_checkout(dir),
        ));
    }
    let invalidation = invalidate_if_checkout_changed(dir, checkout, &mut packages)?;
    Ok((lock, invalidation))
}

/// Hold `dir`'s [`CHECKOUT_LOCK`] exclusively, waiting up to `wait`.
///
/// Why (#8692, #9223): a second build in a directory whose holder runs tests
/// or a program would replace the binaries they use. What: creates `dir` and
/// the lock file, then retries a non-blocking exclusive `flock` every
/// [`LOCK_POLL`] until `wait` ends. Only "held by another" is retried; any
/// other open or `flock` error refuses at once (#8692 Q1).
///
/// # Errors
///
/// A description naming the lock file: it could not be created, opened or
/// locked, or another lease still held it when `wait` ended. The caller must
/// not build.
/// Test: `an_unopenable_dir_lock_refuses_naming_the_lock_file_8692`,
/// `a_held_dir_lock_refuses_naming_the_lock_file_8692`.
pub fn lock_dir(dir: &Path, wait: Duration) -> Result<CheckoutLock, String> {
    let path = dir.join(CHECKOUT_LOCK);
    // #8692 Q1: an unreadable lock never lets the build run on a guess.
    let unreadable = |err: std::io::Error| {
        format!(
            "the target-dir lock {} cannot be read ({err}), so whether another build owns \
             CARGO_TARGET_DIR={} is unknown (#8692). Repair or remove that file",
            path.display(),
            dir.display()
        )
    };
    std::fs::create_dir_all(dir).map_err(unreadable)?;
    let file = File::options()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(unreadable)?;
    let deadline = Instant::now() + wait;
    while !try_lock_exclusive(&file).map_err(unreadable)? {
        if Instant::now() >= deadline {
            return Err(format!(
                "CARGO_TARGET_DIR={} is held by another lease (lock {}) for over {}s (it last \
                 built {}); a build here now could replace binaries that lease's tests or \
                 program run, or run another checkout's (#8692, #11661). Re-run when it ends, \
                 or give this build its own CARGO_TARGET_DIR",
                dir.display(),
                path.display(),
                wait.as_secs(),
                last_checkout(dir),
            ));
        }
        std::thread::sleep(LOCK_POLL);
    }
    Ok(CheckoutLock { _file: file })
}

/// A non-blocking exclusive `flock` on `file`: `Ok(true)` taken, `Ok(false)`
/// held by another, `Err` for every other failure.
fn try_lock_exclusive(file: &File) -> std::io::Result<bool> {
    // SAFETY: `file` owns a valid descriptor for the call.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        return Ok(true);
    }
    let err = std::io::Error::last_os_error();
    if err.raw_os_error() == Some(libc::EWOULDBLOCK) {
        Ok(false)
    } else {
        Err(err)
    }
}

/// The checkout [`super::stale_guard::LAST_CHECKOUT_MARKER`] in `dir` names,
/// or `an unrecorded checkout`.
fn last_checkout(dir: &Path) -> String {
    super::marker_seal::read_marker(dir)
        .0
        .unwrap_or_else(|| "an unrecorded checkout".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::build_lease::marker_seal::seal_marker;
    use crate::core::build_lease::stale_guard::LAST_CHECKOUT_MARKER;

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
        // The lease's own build, then its seal (#11661).
        let fp = fingerprint(&dir);
        assert_eq!(seal_marker(&dir, a), Ok(true));
        drop(lock);
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

    /// #8692 (#9223): a kept directory is no longer shared by two leases of
    /// one checkout — the second could replace binaries the first's tests run.
    #[test]
    fn a_second_lease_of_one_checkout_waits_for_a_kept_dir_8692() {
        let tmp = tempfile::tempdir().expect("tmp");
        let dir = tmp.path().join("target");
        let a = Path::new("/work/a");
        let (first, _) = lock_for_checkout(&dir, a, Duration::ZERO, pkgs).expect("first");
        // The first lease's own build, then its seal (#11661).
        let fp = fingerprint(&dir);
        assert_eq!(seal_marker(&dir, a), Ok(true));
        let err = lock_for_checkout(&dir, a, Duration::ZERO, pkgs)
            .expect_err("a second lease of the same checkout waits for the holder");
        assert!(
            err.contains(CHECKOUT_LOCK) && err.contains("/work/a") && err.contains("#8692"),
            "{err}"
        );
        assert!(fp.exists(), "a refused lease clears nothing");
        drop(first);
        let (_second, got) = lock_for_checkout(&dir, a, Duration::ZERO, pkgs)
            .expect("free once the first lease ends");
        assert!(
            fp.exists(),
            "the same checkout keeps its fingerprints: {got:?}"
        );
    }

    /// #8692 Q1 (owner ruling: refuse): a lock file that cannot be opened —
    /// here a directory in its place — refuses at once, naming the file.
    #[test]
    fn an_unopenable_dir_lock_refuses_naming_the_lock_file_8692() {
        let tmp = tempfile::tempdir().expect("tmp");
        let dir = tmp.path().join("target");
        std::fs::create_dir_all(dir.join(CHECKOUT_LOCK)).expect("a directory as the lock");
        let started = Instant::now();
        let err = lock_dir(&dir, Duration::from_secs(5)).expect_err("unreadable");
        assert!(
            err.contains(&dir.join(CHECKOUT_LOCK).display().to_string())
                && err.contains("cannot be read"),
            "{err}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "an unreadable lock refuses at once, not after the wait"
        );
        let err = lock_for_checkout(&dir, Path::new("/work/a"), Duration::ZERO, pkgs)
            .expect_err("a kept dir refuses the same way");
        assert!(err.contains("cannot be read"), "{err}");
    }

    /// #8692: [`lock_dir`] is exclusive and names its lock file when held.
    #[test]
    fn a_held_dir_lock_refuses_naming_the_lock_file_8692() {
        let tmp = tempfile::tempdir().expect("tmp");
        let dir = tmp.path().join("target");
        let held = lock_dir(&dir, Duration::ZERO).expect("first");
        let err = lock_dir(&dir, Duration::ZERO).expect_err("held");
        assert!(
            err.contains(&dir.join(CHECKOUT_LOCK).display().to_string())
                && err.contains("held by another lease"),
            "{err}"
        );
        drop(held);
        assert!(lock_dir(&dir, Duration::ZERO).is_ok());
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
