//! Advisory locking + bare-repo detection for `RealGitBackend::ensure_base_checkout`.
//!
//! Why: split out of `workspace.rs` (which grew past the 500-SLOC production
//! cap once trusty-review's PR #1936 / #1935 findings were fixed) to keep
//! both files under the mechanical `scripts/check_line_cap.sh` gate. Grouping
//! these items together here — rather than trimming their Why/What/Test
//! documentation — keeps the fix's rationale fully documented in place.
//! What: [`is_established_bare_checkout`] replaces the fragile
//! `HEAD.is_file()` idempotency probe (review finding #2) with a real
//! `git rev-parse --is-bare-repository` check; [`acquire_base_checkout_lock`]
//! (backed by [`BaseCheckoutLock`], a dependency-free marker-file mutex) plus
//! [`lock_is_stale`] serialize the check-and-clone window across concurrent
//! callers to fix the TOCTOU provisioning race (review finding #1).
//! Test: `ensure_base_checkout_recovers_from_concurrent_race`,
//! `ensure_base_checkout_rejects_stale_non_bare_directory`,
//! `base_checkout_lock_recovers_stale_lock_marker`, all in
//! `provisioner/workspace/tests.rs`.

use std::path::{Path, PathBuf};

use super::ProvisionError;

/// Determine whether `dir` is an already-established BARE git checkout.
///
/// Why: the previous idempotency guard for [`super::RealGitBackend`]'s
/// `ensure_base_checkout` (`base_dir.join("HEAD").is_file()`) only checks for
/// a file literally named `HEAD` at the root of `base_dir` — it never
/// confirms that directory is actually a valid, complete git repository. A
/// directory that merely contains a stray `HEAD` file (e.g. left behind by a
/// `git clone --bare` that crashed mid-clone — git writes `HEAD` early,
/// before the rest of the object database/refs — or any other stale/corrupt
/// artifact) would pass the old check and be silently mistaken for an
/// established shared base (trusty-review finding #2 on PR #1936 / #1935).
/// `git rev-parse --is-bare-repository` is the canonical way git itself
/// answers this question, so it cannot be fooled by directory layout alone.
/// This helper is also reused to resolve the TOCTOU provisioning race
/// (finding #1): after acquiring [`acquire_base_checkout_lock`], re-running
/// this check tells the caller whether a concurrent caller already finished
/// establishing a valid base while this one was waiting.
/// What: shells out to `git -C dir rev-parse --is-bare-repository` and
/// returns `true` only when the command exits successfully AND stdout trims
/// to exactly `"true"`. Returns `false` (never panics) if the subprocess
/// fails to spawn, exits non-zero (not a git repository at all), or prints
/// anything else (e.g. `"false"` for a non-bare repo).
/// Test: `ensure_base_checkout_recovers_from_concurrent_race`,
/// `ensure_base_checkout_rejects_stale_non_bare_directory`.
pub(super) fn is_established_bare_checkout(dir: &Path) -> bool {
    use std::process::Command;
    let dir_s = dir.to_string_lossy();
    match Command::new("git")
        .args(["-C", &dir_s, "rev-parse", "--is-bare-repository"])
        .output()
    {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).trim() == "true",
        _ => false,
    }
}

/// Maximum time a caller will wait to acquire the base-checkout lock before
/// giving up.
///
/// Why: a real clone of a large repository can legitimately take a while;
/// this must be generous enough not to spuriously fail a normal (non-racing)
/// first provision on a slow network, while still bounding how long a
/// session-spawn request can block.
/// What: 60 seconds.
/// Test: `ensure_base_checkout_recovers_from_concurrent_race` completes well
/// within this window against a local, near-instant clone.
pub(super) const LOCK_ACQUIRE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// Interval between retries while polling for the base-checkout lock.
///
/// Why: short enough that a waiting caller notices the lock's release
/// quickly, long enough not to busy-spin the CPU.
/// What: 50 milliseconds.
/// Test: covered indirectly by every test that exercises the lock.
const LOCK_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);

/// Age after which an existing lock marker file is treated as abandoned.
///
/// Why: the lock marker is a plain file, not a kernel-tracked `flock` — if
/// the process holding it crashes (killed, panicked, power loss) mid-clone,
/// the marker file is never cleaned up by a `Drop` impl that never ran. A
/// purely time-bound marker would otherwise deadlock every future
/// provisioning attempt for that project forever.
/// What: 5 minutes — comfortably longer than [`LOCK_ACQUIRE_TIMEOUT`] so a
/// live, slow-but-progressing clone is never mistaken for an abandoned one.
/// Test: `base_checkout_lock_recovers_stale_lock_marker`.
pub(super) const LOCK_STALE_AFTER: std::time::Duration = std::time::Duration::from_secs(300);

/// Compute the sibling marker-file path used to serialize concurrent
/// `ensure_base_checkout` calls for the same `base_dir`.
///
/// Why: the lock must live NEXT TO `base_dir` (not inside it), since
/// `base_dir` itself may not exist yet when the lock is first acquired.
/// What: `<base_dir's parent>/<base_dir's file name>.lock`, e.g.
/// `<project_dir>/.base.lock` for the standard `.base` base-checkout name.
/// Test: exercised transitively by every `ensure_base_checkout` test.
pub(super) fn base_checkout_lock_path(base_dir: &Path) -> PathBuf {
    match (base_dir.parent(), base_dir.file_name()) {
        (Some(parent), Some(name)) => {
            let mut lock_name = name.to_os_string();
            lock_name.push(".lock");
            parent.join(lock_name)
        }
        _ => base_dir.with_extension("lock"),
    }
}

/// RAII guard holding the base-checkout provisioning lock.
///
/// Why: guarantees the marker file is removed when `ensure_base_checkout`
/// returns via ANY path (success, error, or early return) — a manual
/// remove-on-every-branch approach is error-prone and easy to miss on a new
/// return path added later.
/// What: wraps the locked path; `Drop` best-effort removes the marker file
/// (a failure to remove is not actionable here and must not panic in a
/// destructor).
/// Test: `ensure_base_checkout_recovers_from_concurrent_race` (the lock is
/// released promptly enough for every racing thread to eventually proceed).
pub(super) struct BaseCheckoutLock {
    path: PathBuf,
}

impl Drop for BaseCheckoutLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Acquire the advisory base-checkout provisioning lock at `lock_path`.
///
/// Why: resolves the TOCTOU race (#1935 review finding #1) at its root by
/// serializing the whole check-and-clone window, rather than trying to
/// detect and recover from corruption after the fact — real `git clone
/// --bare` is not safe against a concurrent writer into the same target
/// directory (empirically observed failure mode without this lock: two
/// interleaved clones can corrupt each other's partial checkout, e.g.
/// colliding on copying `hooks/commit-msg.sample`, rather than cleanly
/// failing with "already exists").
/// What: retries `OpenOptions::new().write(true).create_new(true)` — atomic
/// at the filesystem level, failing with `AlreadyExists` iff another caller
/// currently holds the lock — for up to [`LOCK_ACQUIRE_TIMEOUT`], sleeping
/// [`LOCK_POLL_INTERVAL`] between attempts. A lock file older than
/// [`LOCK_STALE_AFTER`] is treated as abandoned (left behind by a crashed
/// holder) and force-removed before retrying, so a crash mid-clone can never
/// permanently deadlock future provisioning attempts for the same project.
/// Returns a [`BaseCheckoutLock`] guard that releases the lock on drop.
/// Test: `ensure_base_checkout_recovers_from_concurrent_race`,
/// `base_checkout_lock_recovers_stale_lock_marker`.
pub(super) fn acquire_base_checkout_lock(
    lock_path: &Path,
) -> Result<BaseCheckoutLock, ProvisionError> {
    let deadline = std::time::Instant::now() + LOCK_ACQUIRE_TIMEOUT;
    loop {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(lock_path)
        {
            Ok(_) => {
                return Ok(BaseCheckoutLock {
                    path: lock_path.to_owned(),
                });
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                if lock_is_stale(lock_path) {
                    // Best-effort: if the remove races with the true holder's
                    // own release, the next loop iteration's create_new simply
                    // retries — no harm done either way.
                    let _ = std::fs::remove_file(lock_path);
                    continue;
                }
                if std::time::Instant::now() >= deadline {
                    return Err(ProvisionError::Git(format!(
                        "timed out after {:?} waiting for base-checkout lock at {}",
                        LOCK_ACQUIRE_TIMEOUT,
                        lock_path.display()
                    )));
                }
                std::thread::sleep(LOCK_POLL_INTERVAL);
            }
            Err(e) => return Err(ProvisionError::Io(e)),
        }
    }
}

/// Determine whether the lock marker at `lock_path` is old enough to be
/// treated as abandoned by a crashed holder.
///
/// Why: split out of [`acquire_base_checkout_lock`] so the staleness policy
/// is independently testable and readable in isolation.
/// What: returns `true` iff the file's modified time is more than
/// [`LOCK_STALE_AFTER`] in the past. Returns `false` (never panics) if the
/// file vanished, its metadata can't be read, or the clock query fails —
/// treating an unreadable lock as NOT stale is the conservative, safe default
/// (it just means this caller waits and retries rather than force-clearing
/// state it can't reason about).
/// Test: `base_checkout_lock_recovers_stale_lock_marker`.
pub(super) fn lock_is_stale(lock_path: &Path) -> bool {
    std::fs::metadata(lock_path)
        .and_then(|meta| meta.modified())
        .and_then(|modified| {
            modified
                .elapsed()
                .map_err(|e| std::io::Error::other(e.to_string()))
        })
        .map(|age| age > LOCK_STALE_AFTER)
        .unwrap_or(false)
}
