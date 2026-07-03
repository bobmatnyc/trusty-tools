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
//! [`stale_base_dir_error`] turns the "the base path is occupied by a broken,
//! non-bare directory" state into an actionable operator error (issue #1937
//! item 1), and is shared by BOTH backends; [`fake_is_established_bare_checkout`]
//! / [`write_fake_bare_checkout`] let `FakeGitBackend` mirror the real backend's
//! `rev-parse`-based validity semantics rather than a superficial file-exists
//! probe (issue #1937 item 3).
//! Test: `ensure_base_checkout_recovers_from_concurrent_race`,
//! `ensure_base_checkout_rejects_stale_non_bare_directory`,
//! `base_checkout_lock_recovers_stale_lock_marker`,
//! `fake_ensure_base_checkout_rejects_stale_non_bare_directory`, all in
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

/// Build an actionable error for a base-checkout path occupied by a broken,
/// non-bare directory — or return `None` when the path is safe to clone into.
///
/// Why: `ensure_base_checkout` correctly refuses to reuse a `<project_dir>/.base`
/// that is not a valid bare checkout (a leftover from a crashed mid-clone, or a
/// stale non-bare directory), but the resulting failure used to be an opaque
/// `git clone --bare` "destination path already exists" message that gives the
/// operator no recovery path (issue #1937 item 1). We deliberately do NOT
/// auto-remediate (delete + re-clone): silently `rm -rf`-ing a directory the
/// daemon did not create is too destructive for an advisory recovery path.
/// Instead we surface the exact path and the exact command to clear it, and let
/// the human decide.
/// What: returns `None` when `base_dir` is absent or empty (either is safe —
/// `git clone --bare` accepts a missing or empty target); otherwise returns
/// `Some(ProvisionError::Git(..))` whose message names the exact path and the
/// literal `rm -rf <path>` command to run before retrying. Callers MUST invoke
/// this only AFTER confirming the path is not already a valid bare checkout
/// (via [`is_established_bare_checkout`]) so a healthy base is never flagged.
/// Test: `ensure_base_checkout_rejects_stale_non_bare_directory` and
/// `fake_ensure_base_checkout_rejects_stale_non_bare_directory` (both assert
/// the returned message contains the path and the `rm -rf` hint).
pub(super) fn stale_base_dir_error(base_dir: &Path) -> Option<ProvisionError> {
    // A missing dir (read_dir errors) or an empty dir (no entries) is fine:
    // `git clone --bare` clones cleanly into either. Only a NON-empty dir that
    // is not a valid bare checkout is the stale/broken state worth reporting.
    let non_empty = std::fs::read_dir(base_dir)
        .map(|mut entries| entries.next().is_some())
        .unwrap_or(false);
    if !non_empty {
        return None;
    }
    Some(ProvisionError::Git(format!(
        "base checkout path {path} exists but is not a valid bare git checkout \
         (likely a leftover from a crashed clone, or a stale non-bare directory). \
         trusty-mpm will not delete it automatically. To allow re-provisioning, \
         remove it manually and retry:\n    rm -rf {path}",
        path = base_dir.display()
    )))
}

/// Determine whether `dir` is an already-established (fake) BARE checkout,
/// mirroring [`is_established_bare_checkout`]'s intent for `FakeGitBackend`.
///
/// Why: `FakeGitBackend::ensure_base_checkout` used to treat a lone
/// `dir.join("HEAD").is_file()` as proof of an established base — the exact
/// superficial file-existence probe the real backend abandoned in favour of
/// `git rev-parse --is-bare-repository` (issue #1937 item 3). A future test
/// author using the fake to simulate a stale `.base` (a stray `HEAD` file with
/// no repository structure) would have gotten a false-positive "already
/// established" pass, hiding the very stale-directory bug a real-backend test
/// would catch. This check restores that fidelity in the filesystem-light fake.
/// What: returns `true` only when BOTH a root-level `HEAD` file exists AND a
/// root-level `config` file marked `bare = true` exists — the structural
/// markers [`write_fake_bare_checkout`] writes to simulate a real bare clone. A
/// directory containing only a stray `HEAD` (the stale-mid-clone shape) fails
/// the `config` check and reads as NOT established, matching the real backend.
/// Test: `fake_ensure_base_checkout_rejects_stale_non_bare_directory`,
/// `provision_reuses_base_checkout_across_sessions`.
pub(super) fn fake_is_established_bare_checkout(dir: &Path) -> bool {
    dir.join("HEAD").is_file()
        && std::fs::read_to_string(dir.join("config"))
            .map(|c| c.contains("bare = true"))
            .unwrap_or(false)
}

/// Write the minimal structural markers that make a directory read as an
/// established (fake) bare checkout.
///
/// Why: `FakeGitBackend::ensure_base_checkout` must leave behind enough state
/// that [`fake_is_established_bare_checkout`] recognises it on the next call
/// (idempotent reuse across sessions) while still being distinguishable from a
/// stale directory that merely holds a stray `HEAD` file (issue #1937 item 3).
/// What: creates `dir` (and parents) and writes a `HEAD` ref pointer plus a
/// `config` file carrying the `bare = true` marker — the two files
/// [`fake_is_established_bare_checkout`] requires. Returns any I/O error as
/// [`ProvisionError::Io`].
/// Test: `provision_reuses_base_checkout_across_sessions` (second provision is a
/// no-op reuse), `fake_ensure_base_checkout_rejects_stale_non_bare_directory`.
pub(super) fn write_fake_bare_checkout(dir: &Path) -> Result<(), ProvisionError> {
    std::fs::create_dir_all(dir)?;
    std::fs::write(dir.join("HEAD"), "ref: refs/heads/main\n")?;
    std::fs::write(dir.join("config"), "[core]\n\tbare = true\n")?;
    Ok(())
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
                // KNOWN, ACCEPTED RACE (issue #1937 item 2): staleness is judged
                // purely by the marker file's mtime crossing `LOCK_STALE_AFTER`.
                // On a filesystem with low-resolution timestamps, or under
                // significant wall-clock skew (e.g. NTP step, VM clock drift),
                // it is theoretically possible for this caller to compute a
                // still-live holder's marker as "stale" and force-remove it,
                // after which BOTH callers could hold the lock at once. We
                // accept this window deliberately: this is an ADVISORY lock, not
                // a correctness-critical mutex — the only thing it guards is a
                // rare first-provision `git clone --bare` collision (finding #1),
                // and `LOCK_STALE_AFTER` (5 min) is set comfortably longer than
                // `LOCK_ACQUIRE_TIMEOUT` (60 s) precisely so a live,
                // slow-but-progressing clone is never mistaken for an abandoned
                // one under normal conditions. A dependency-free, kernel-tracked
                // alternative (e.g. `flock`) or a PID-liveness check would close
                // the window, but the added complexity is not justified for this
                // low-probability, non-destructive edge case (worst outcome: the
                // git-clone collision this lock exists to prevent, which was
                // already tolerable before the lock landed).
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
