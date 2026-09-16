//! Promotion gate: never rename a staged corpus over a live `index.redb` that
//! another opener still holds (#7991).
//!
//! Why: `begin_staged_corpus_swap` drops this daemon's handle on the live
//! corpus for the whole reindex. On a shared filesystem — the two-host setup
//! #7920 describes — a second daemon can open that live `index.redb` inside the
//! window. `commit_staged_corpus_swap` then renamed the staged file over it,
//! and the other process kept reading and writing an inode that no longer has a
//! name. Its later writes land nowhere, its reads answer from a file this host
//! believes it replaced, and the existing quarantine only fires on a failed
//! re-open, which happens AFTER the rename.
//!
//! What: [`acquire_for_promotion`] takes the same advisory lock redb itself
//! takes, on the live file, and HOLDS it across the rename. Holding it rather
//! than probing and releasing is what closes the window instead of narrowing
//! it: any opener arriving between the check and the rename now fails its own
//! open with `DatabaseAlreadyOpen` rather than ending up on the doomed inode.
//!
//! Detection mechanism and why it is sound on macOS and Linux: redb's
//! `FileBackend::new_internal` calls `std::fs::File::try_lock()` on the
//! database file and maps `WouldBlock` to `DatabaseError::DatabaseAlreadyOpen`.
//! `File::try_lock` is `flock(2)` with `LOCK_EX | LOCK_NB` on both macOS (BSD
//! flock) and Linux, so this module contends with the exact primitive every
//! redb opener uses — there is no second, private lock to keep in step. flock
//! state lives on the open file description and the kernel releases it when the
//! descriptor closes or the process dies, so a crashed host leaves no stale
//! lock to clear by hand, which a PID lock file would.
//!
//! Known limit, stated rather than papered over: flock is advisory and, over
//! NFS, is local-to-the-client on Linux kernels that do not map it onto a POSIX
//! lock. Where the filesystem cannot lock at all, `try_lock` reports
//! `Unsupported` and this module REFUSES the promotion — the reindex keeps its
//! staged corpus and the live one is untouched, which is the safe answer when
//! the hazard cannot be excluded.
//!
//! Test: `super::live_corpus_lock_tests`.

use std::fs::{File, OpenOptions, TryLockError};
use std::path::Path;

use crate::core::registry::IndexId;

/// An exclusive advisory lock held on the live corpus across its promotion.
///
/// Why: the lock must outlive the check. Returning a guard rather than a `bool`
/// makes "checked" and "still held" the same fact, so no caller can drop the
/// protection between the two by rearranging statements.
/// What: owns the locked `File`. Dropping it closes the descriptor, which
/// releases the flock. After the rename that descriptor refers to the replaced
/// inode, so the release affects nothing the daemon still serves. The field is
/// `None` in exactly one case — there was no live file to lock — so that
/// `Some(guard)` means "may promote" on both arms and the caller has one test.
/// Test: `a_held_live_corpus_refuses_promotion`.
#[derive(Debug)]
pub(super) struct LiveCorpusLock {
    _file: Option<File>,
}

/// Take the live corpus's advisory lock, or refuse the promotion (#7991).
///
/// Why: see the module doc. This is the one place that decides whether the
/// rename may proceed, and it is deliberately called BEFORE the staging handle
/// is released, so a refusal leaves the reindex exactly where it was — staging
/// attached, checkpoint intact, live corpus untouched — instead of in the
/// no-corpus state the post-release failure arms quarantine.
/// What: opens the live file read-write without creating it and takes
/// `File::try_lock`. Every outcome other than "the file does not exist" or "the
/// lock is ours" refuses:
///
/// - live file absent — nothing can be holding it, so promotion proceeds;
/// - lock acquired — no other opener, promotion proceeds holding it;
/// - `WouldBlock` — another opener holds it, REFUSE;
/// - `Unsupported` — the filesystem cannot lock, so the hazard cannot be
///   excluded, REFUSE;
/// - any other lock error, or any open error other than `NotFound`
///   (permissions, `EISDIR`, I/O), REFUSE.
///
/// Test: `a_held_live_corpus_refuses_promotion`,
/// `an_unheld_live_corpus_is_promotable`,
/// `a_missing_live_corpus_is_promotable`,
/// `an_unopenable_live_path_refuses_promotion`.
pub(super) async fn acquire_for_promotion(
    live_path: &Path,
    index_id: &IndexId,
) -> Option<LiveCorpusLock> {
    let live = live_path.to_path_buf();
    let id = index_id.0.clone();
    match tokio::task::spawn_blocking(move || acquire_blocking(&live, &id)).await {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(
                index_id = %index_id.0,
                "staged corpus swap: the live-corpus lock probe panicked for '{}' ({e}) — \
                 REFUSING to promote over {} (#7991)",
                index_id.0,
                live_path.display()
            );
            None
        }
    }
}

/// The blocking half of [`acquire_for_promotion`]; see its doc for the arms.
fn acquire_blocking(live_path: &Path, index_id: &str) -> Option<LiveCorpusLock> {
    let file = match OpenOptions::new().read(true).write(true).open(live_path) {
        Ok(f) => f,
        // A live corpus that does not exist yet cannot be held open: this is a
        // first promotion, and there is nothing to rename over.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Some(LiveCorpusLock { _file: None })
        }
        Err(e) => {
            tracing::error!(
                index_id = %index_id,
                "staged corpus swap: cannot open the live corpus {} for '{index_id}' ({e}) — \
                 REFUSING to promote, because a file this host cannot open is one it cannot \
                 prove is unheld (#7991)",
                live_path.display()
            );
            return None;
        }
    };
    match file.try_lock() {
        Ok(()) => Some(LiveCorpusLock { _file: Some(file) }),
        Err(TryLockError::WouldBlock) => {
            tracing::error!(
                index_id = %index_id,
                "index '{index_id}': REFUSING to promote the staged corpus over {} — another \
                 process still holds that file open (redb's own advisory lock is taken). \
                 Renaming over it would leave that opener reading and writing an unlinked \
                 inode. The staged corpus is kept and the live corpus is untouched; the next \
                 reindex resumes from it once the other opener is gone (#7991).",
                live_path.display()
            );
            None
        }
        Err(TryLockError::Error(e)) if e.kind() == std::io::ErrorKind::Unsupported => {
            tracing::error!(
                index_id = %index_id,
                "index '{index_id}': REFUSING to promote the staged corpus over {} — this \
                 filesystem does not support the advisory lock redb uses, so a concurrent \
                 opener on another host cannot be excluded (#7991).",
                live_path.display()
            );
            None
        }
        Err(TryLockError::Error(e)) => {
            tracing::error!(
                index_id = %index_id,
                "index '{index_id}': REFUSING to promote the staged corpus over {} — the \
                 advisory lock could not be taken ({e}) (#7991).",
                live_path.display()
            );
            None
        }
    }
}
