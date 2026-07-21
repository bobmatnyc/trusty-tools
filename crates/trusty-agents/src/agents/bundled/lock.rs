//! Pass-level advisory lock for bundled-agent (re)provision (#3556 code-critic
//! follow-up, HIGH).
//!
//! Why: `reprovision_bundled_agents` runs its whole read-backup-write
//! sequence unguarded, and `delegate_to_agent` routinely spawns a fresh
//! `tagent` subprocess per delegation — so multiple processes can enter the
//! stale-stamp refresh path concurrently right after a binary upgrade.
//! Without mutual exclusion across the ENTIRE pass (not just per file), one
//! process's crash mid-write (or a genuinely torn on-disk file left by an
//! earlier interrupted pass) can be picked up by a second pass and archived
//! OVER a prior process's still-good `.stale.bak`, destroying the one
//! recovery copy a hand-edit depended on. `crates/trusty-agents/src/
//! state_writer.rs` already solves the identical class of problem (fs4
//! advisory lock + tmp-file + rename) for state files at PER-FILE
//! granularity; this reuses the same `fs4` primitive but at PASS granularity
//! — one lock held for the duration of the whole reprovision loop — because
//! the invariant that matters here ("don't let a second process observe a
//! half-finished pass") spans every file in the bundle, not just one.
//! What: [`acquire`] opens/creates `target_dir/.bundled-provision.lock` and
//! takes an exclusive `fs4` lock, returning an RAII [`ProvisionLock`] guard
//! that releases it on drop — so an early `?` return or a panic partway
//! through the pass can never leave the lock held forever.
//! Test: `pass_lock_serializes_concurrent_reprovision_calls` (tests.rs).

use std::fs::{File, OpenOptions};
use std::path::Path;

use anyhow::{Context, Result, anyhow};
use fs4::FileExt;

/// Fixed filename for the pass-level advisory lock, written as a sibling of
/// the deployed agent files inside the target agents directory.
const LOCK_FILE_NAME: &str = ".bundled-provision.lock";

/// RAII guard for the pass-level advisory lock.
///
/// Why: an early `?` return from anywhere inside a locked pass (a read
/// error, a write error, an embedded asset vanishing) must still release the
/// lock — tying release to `Drop` makes that automatic instead of relying on
/// every call site to remember to unlock on every exit path.
/// What: holds the open, locked `File`; `Drop` best-effort unlocks it (a
/// process exit / crash also releases an `fs4` advisory lock automatically,
/// so a failed explicit unlock here is not a correctness issue).
/// Test: covered indirectly by every test that calls `acquire` and lets the
/// guard drop at scope end (tests.rs); `pass_lock_serializes_concurrent_
/// reprovision_calls` pins the actual mutual-exclusion behavior.
pub(super) struct ProvisionLock {
    file: File,
}

impl Drop for ProvisionLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

/// Acquire the exclusive pass-level lock for `target_dir`, blocking until it
/// is available.
///
/// Why: called once at the top of `reprovision_bundled_agents`, before the
/// per-file loop, so the ENTIRE read-backup-write sequence for every bundled
/// file in one pass is atomic with respect to any other process's pass over
/// the same `target_dir`.
/// What: ensures `target_dir` exists, opens (creating if absent)
/// `target_dir/.bundled-provision.lock`, and takes an exclusive `fs4` lock —
/// which BLOCKS the calling thread until any other holder releases it,
/// rather than failing fast (a reprovision pass is not on any interactive
/// hot path, so waiting is preferable to racing).
/// Test: `pass_lock_serializes_concurrent_reprovision_calls` (tests.rs).
pub(super) fn acquire(target_dir: &Path) -> Result<ProvisionLock> {
    std::fs::create_dir_all(target_dir)
        .with_context(|| format!("creating directory {}", target_dir.display()))?;
    let path = target_dir.join(LOCK_FILE_NAME);
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .with_context(|| format!("opening lock file {}", path.display()))?;
    FileExt::lock(&file).map_err(|e| anyhow!("acquiring bundled-agent provision lock: {e}"))?;
    Ok(ProvisionLock { file })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Why (#3556 code-critic follow-up, HIGH): the whole point of the
    /// pass-level lock is that two concurrent `tagent` processes reprovisioning
    /// the SAME `target_dir` never interleave their read-backup-write
    /// decisions. `fs4`'s advisory lock blocks across independently-opened
    /// file descriptors (proven by `state_writer`'s own
    /// `concurrent_writes_no_corruption` test using the same primitive), so a
    /// multi-thread stand-in for multi-process is a faithful proxy here.
    /// What: 8 threads race to `acquire` the same `target_dir`; each records
    /// how many holders were concurrently active (via a shared counter and a
    /// short sleep while "holding" the lock) and tracks the observed maximum.
    /// Asserts the maximum never exceeded 1.
    /// Test: itself.
    #[test]
    fn pass_lock_serializes_concurrent_reprovision_calls() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = Arc::new(tmp.path().to_path_buf());
        let active = Arc::new(AtomicUsize::new(0));
        let max_concurrent = Arc::new(AtomicUsize::new(0));

        let handles: Vec<_> = (0..8)
            .map(|_| {
                let dir = Arc::clone(&dir);
                let active = Arc::clone(&active);
                let max_concurrent = Arc::clone(&max_concurrent);
                std::thread::spawn(move || {
                    let _guard = acquire(&dir).unwrap();
                    let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                    max_concurrent.fetch_max(now, Ordering::SeqCst);
                    std::thread::sleep(std::time::Duration::from_millis(20));
                    active.fetch_sub(1, Ordering::SeqCst);
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }

        assert_eq!(
            max_concurrent.load(Ordering::SeqCst),
            1,
            "pass-level lock must serialize concurrent acquisitions — \
             never more than 1 holder active at a time"
        );
    }
}
