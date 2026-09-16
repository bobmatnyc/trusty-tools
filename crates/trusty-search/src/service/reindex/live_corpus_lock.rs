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
//! Known limit, stated rather than papered over, because the dangerous shape is
//! NOT the one that errors. A filesystem that cannot lock at all reports
//! `Unsupported`, and this module refuses — that case is safe. The case this
//! gate does NOT catch is flock SUCCEEDING client-locally: an NFS mount with
//! `-o nolock` (or `local_lock=flock`, or `local_lock=all`), and some FUSE
//! layers (sshfs without `-o workaround`, several S3/object-storage mounts),
//! satisfy the lock against the local kernel only. Two hosts then each take the
//! "exclusive" lock, `try_lock` returns `Ok` on both, and the gate ADMITS the
//! promotion that #7991 is about. There is no local test that distinguishes
//! that mount from a healthy one, so it is an accepted, documented residual:
//! run a shared-FS deployment with `local_lock=none` (the NFSv4 default, which
//! maps flock onto a server-side POSIX lock) for this gate to mean anything
//! across hosts.
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

/// One-line operator-facing reason for a refused promotion (#7991).
///
/// Why: `commit_staged_corpus_swap` records the refusal on the indexer so
/// `GET /indexes/:id/status` can report it, and the text a caller reads must
/// not drift from the ERROR line the arms above log.
/// What: names the live path and what a reader should do. Deliberately does not
/// re-probe — the probe already ran and its arms logged the specific cause.
/// Test: `a_deferred_promotion_is_reported_in_status_and_is_not_complete`.
pub(super) fn deferral_reason(live_path: &Path) -> String {
    format!(
        "the live corpus {} could not be exclusively locked for promotion — another opener \
         holds it, or this filesystem cannot prove otherwise. The live corpus is unchanged \
         and still serves; this reindex's staged work was discarded and must be re-run once \
         the other opener is gone (#7991). The daemon log names the specific arm.",
        live_path.display()
    )
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
                 inode. The LIVE corpus is untouched and still serves; this run's staged \
                 work is discarded by the next reindex, which must redo it. Reported as \
                 `promotion_deferred` on GET /indexes/:id/status (#7991).",
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
