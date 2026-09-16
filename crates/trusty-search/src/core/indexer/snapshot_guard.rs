//! Overwrite guard for the legacy `chunks.json` snapshot (#7920).
//!
//! Why: the #1711 guard refuses to write an empty HNSW index over a populated
//! snapshot, but `chunks.json` had no equivalent. A shutdown flush resolves its
//! target path from disk at shutdown time, so an indexer whose in-memory corpus
//! was loaded from one location (or never loaded at all) could write that
//! empty or foreign state over a populated snapshot it had never read — the
//! 39-byte `{"version":1,"chunks":[],"entities":[]}` file in #7920.
//! What: [`SnapshotGuard`] remembers every snapshot path this indexer loaded or
//! wrote, and refuses a write when the file on disk holds chunks AND either the
//! in-memory corpus is empty or the path is not one this indexer owns. Every
//! refusal is counted and returned as [`SnapshotOverwriteRefused`].
//! Scope: `chunks.json` is written only while no redb corpus store is wired.
//! `refuse_durable_write` runs first and refuses a write-quarantined indexer
//! and one whose corpus a staged swap detached (#7920), so this guard sees
//! only indexers that never held a corpus. It inspects the one path the
//! caller names; it cannot see a snapshot at any other location.
//!
//! Residual, #7980: the UNREADABLE self-heal does not consult ownership. A
//! store-less writer whose corpus describes a different tree will still move an
//! unreadable file aside and write over it. That is deliberate — the bytes are
//! unrecoverable by every reader including the migration, and the alternative
//! is the permanent refusal #7980 exists to remove — but it is a real widening
//! of the foreign-write refusal, bounded to files nothing can parse and logged
//! with `owned = false` so the case is greppable.
//! Test: `shutdown_flush_refuses_empty_corpus_over_populated_chunks_json`,
//! `incremental_persist_refuses_empty_corpus_over_populated_chunks_json`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use super::CodeIndexer;

/// A `chunks.json` write refused because it would destroy a populated snapshot.
///
/// Why: callers must be able to tell a refusal from an I/O failure, and a test
/// must be able to assert the refusal arm without matching log text.
/// What: carries the target, what the file holds, the in-memory chunk count,
/// and the reason the write was refused.
#[derive(Debug, thiserror::Error)]
#[error(
    "refusing to overwrite populated chunk snapshot {} ({on_disk}) with {in_memory} \
     in-memory chunk(s): {reason}. The on-disk snapshot is preserved (#7920)",
    path.display()
)]
pub struct SnapshotOverwriteRefused {
    pub path: PathBuf,
    pub on_disk: String,
    pub in_memory: usize,
    pub reason: &'static str,
}

/// What the file at a snapshot path currently holds.
enum OnDisk {
    /// Missing or zero bytes — nothing to destroy.
    Empty,
    Chunks(usize),
    /// Present but not a readable snapshot; treated as populated. #7923 already
    /// ruled on this shape for the read side — a corrupt snapshot is a failure,
    /// never an empty corpus — and bytes this process cannot parse are bytes it
    /// must not destroy. The owning writer is unaffected: `check_overwrite`
    /// returns before it reads the file.
    Unreadable(u64),
}

/// Whether the writer's in-memory corpus is the authoritative one (#7980).
///
/// Why: the guard's empty/foreign refusal was written for an indexer whose
/// in-memory corpus is NOT authoritative — a write-quarantined one, whose
/// corpus is empty because redb never opened, or one a staged swap detached.
/// `refuse_durable_write` stops both before the guard runs, so the shape that
/// actually reaches it is a legacy, store-less, non-quarantined indexer, whose
/// `chunks.json` IS its durable corpus. Treating those two the same is what
/// made an unreadable file on an unowned path a permanent refusal — the writer
/// held real chunks and no reader could recover the bytes it was protecting.
/// What: a two-state marker the caller derives from the indexer, passed rather
/// than re-derived here so the guard stays a pure decision over its inputs.
/// Test: `unreadable_snapshot_no_longer_wedges_a_store_less_writer`,
/// `a_quarantined_writer_never_reaches_the_unreadable_self_heal`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WriterShape {
    /// Store-less and not quarantined: `chunks.json` is this indexer's corpus.
    CorpusIsAuthoritative,
    /// Anything else — the in-memory corpus may describe nothing at all.
    CorpusMayBeStandIn,
}

/// Move an unreadable snapshot aside instead of destroying it (#7980).
///
/// Why: #7923 ruled that bytes this process cannot parse are bytes it must not
/// destroy, and that ruling is what makes admitting the write safe — the file
/// is preserved under a name nothing reads, so a later forensic pass still has
/// it while the live path is free for a corpus that is actually readable.
/// What: CLAIMS a free `<path>.corrupt[.N]` slot with
/// [`claim_sidecar_slot`], then renames `path` onto the slot it created.
/// The numbered form is the anti-clobber rule
/// [`trusty_common::redb_open::incompatible_backup_path`] already uses for the
/// same job, rather than a timestamp. A millisecond stamp is not a uniqueness
/// guarantee (two writers inside one tick collide, and a coarse-resolution
/// clock makes that likely), and it sorts by digits rather than by age.
/// Returns the new path.
/// Test: `unreadable_snapshot_no_longer_wedges_a_store_less_writer`,
/// `a_second_unreadable_snapshot_never_clobbers_the_first_sidecar`,
/// `a_failed_preservation_refuses_the_write_and_keeps_the_file`.
fn preserve_unreadable(path: &Path) -> std::io::Result<PathBuf> {
    let base = {
        let mut name = path.file_name().unwrap_or_default().to_os_string();
        name.push(CORRUPT_SUFFIX);
        path.with_file_name(name)
    };
    let kept = claim_sidecar_slot(&base)?;
    // The destination is the empty file this call just created, so the rename
    // replaces only our own claim.
    std::fs::rename(path, &kept)?;
    Ok(kept)
}

/// Most numbered sidecar slots tried before the preservation gives up.
///
/// Why: the loop must terminate. Exhaustion returns `Err`, which puts the
/// caller on the refusal — never on a clobbering fallback, which is what the
/// old `unwrap_or(base)` was.
const MAX_SIDECAR_SLOTS: u32 = 1_000;

/// Atomically claim a free `<base>[.N]` slot by creating it (#7980).
///
/// Why: choosing the name with `exists()` and renaming afterwards is a TOCTOU.
/// Two writers preserving the same snapshot both probe, both see the SAME free
/// name, and `rename(2)` replaces its destination without error — so the second
/// rename silently destroys the copy the first had just preserved, defeating the
/// "nothing is ever destroyed" guarantee this module exists to keep.
/// What: tries `base`, then `base.1`, `base.2`, … up to [`MAX_SIDECAR_SLOTS`],
/// returning the first one `create_new` actually created. `create_new` is the
/// atomic claim: the kernel picks the winner and every loser sees
/// `AlreadyExists` and advances. Any other I/O error — and exhaustion — returns
/// `Err`, so the caller refuses the write rather than destroying bytes it could
/// not preserve.
/// Test: `two_slot_claims_without_an_intervening_rename_never_collide`,
/// `a_second_unreadable_snapshot_never_clobbers_the_first_sidecar`.
pub(super) fn claim_sidecar_slot(base: &Path) -> std::io::Result<PathBuf> {
    for n in 0..MAX_SIDECAR_SLOTS {
        let candidate = if n == 0 {
            base.to_path_buf()
        } else {
            let mut s = base.as_os_str().to_os_string();
            s.push(format!(".{n}"));
            PathBuf::from(s)
        };
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(_) => return Ok(candidate),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        format!(
            "every sidecar slot up to {MAX_SIDECAR_SLOTS} beside {} is taken",
            base.display()
        ),
    ))
}

/// Suffix for a `chunks.json` this daemon could not parse (#7980).
///
/// Why: one well-known suffix keeps the recovery greppable, the same reason
/// `trusty_common::redb_open::INCOMPATIBLE_SUFFIX` exists for redb files.
///
/// Retention is deliberately the operator's: nothing prunes these. They are
/// produced only when a snapshot rots, which is rare and is evidence; a
/// self-pruning quarantine would delete the one copy of the bytes a diagnosis
/// needs. The numbered rule above bounds a burst to one file per occurrence.
pub(crate) const CORRUPT_SUFFIX: &str = ".corrupt";

/// Minimal parse shape: counts chunk entries without materialising them.
#[derive(serde::Deserialize)]
struct CountedSnapshot {
    #[serde(default)]
    chunks: Vec<serde::de::IgnoredAny>,
}

fn inspect(path: &Path) -> OnDisk {
    let len = match std::fs::metadata(path) {
        Ok(m) => m.len(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return OnDisk::Empty,
        Err(_) => return OnDisk::Unreadable(0),
    };
    if len == 0 {
        return OnDisk::Empty;
    }
    match std::fs::read(path) {
        Ok(bytes) => match serde_json::from_slice::<CountedSnapshot>(&bytes) {
            Ok(c) if c.chunks.is_empty() => OnDisk::Empty,
            Ok(c) => OnDisk::Chunks(c.chunks.len()),
            Err(_) => OnDisk::Unreadable(len),
        },
        Err(_) => OnDisk::Unreadable(len),
    }
}

/// Per-indexer ownership record and refusal counter for `chunks.json` writes.
///
/// Why: see the module doc. Shared behind an `Arc` so the detached
/// incremental-persist task applies the same decision as the shutdown flush.
/// What: `owned` holds paths this indexer loaded or wrote; `refused` counts
/// refusals for the lifetime of the indexer.
#[derive(Debug, Default)]
pub(crate) struct SnapshotGuard {
    owned: Mutex<HashSet<PathBuf>>,
    refused: AtomicU64,
    /// #7980: the live [`WriterShape`], readable by the detached incremental
    /// persister. `true` means the in-memory corpus may be a stand-in.
    ///
    /// Why it lives here rather than being captured at spawn: the persister's
    /// dirty loop outlives one decision, and a quarantine landing mid-loop
    /// would leave a captured `CorpusIsAuthoritative` stale — the task holds
    /// only `Arc` clones and cannot re-read the indexer's plain `bool` fields.
    /// The guard is already one of those clones, so it is the one place both
    /// sides can see.
    stand_in: std::sync::atomic::AtomicBool,
}

impl SnapshotGuard {
    /// Record that this indexer's in-memory corpus now describes `path`.
    pub(crate) fn mark_owned(&self, path: &Path) {
        self.owned
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(path.to_path_buf());
    }

    /// Publish the writer's current shape for every later `check_overwrite`.
    ///
    /// Why: see [`Self::stand_in`]. Called from `&self` sites that CAN read the
    /// indexer's fields, so the detached persister never has to.
    /// Test: `a_quarantine_mid_persist_loop_is_seen_by_the_detached_writer`.
    pub(crate) fn set_shape(&self, shape: WriterShape) {
        self.stand_in
            .store(shape == WriterShape::CorpusMayBeStandIn, Ordering::Release);
    }

    /// The shape last published by [`Self::set_shape`].
    pub(crate) fn shape(&self) -> WriterShape {
        if self.stand_in.load(Ordering::Acquire) {
            WriterShape::CorpusMayBeStandIn
        } else {
            WriterShape::CorpusIsAuthoritative
        }
    }

    fn owns(&self, path: &Path) -> bool {
        self.owned
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(path)
    }

    /// Decide whether `in_memory` chunks may replace the snapshot at `path`.
    ///
    /// Why: see the module doc.
    /// What: allows the write when this indexer owns `path` and holds at least
    /// one chunk, or when the file holds nothing. #7980: also allows it when
    /// the file is UNREADABLE, the writer holds at least one chunk, and
    /// `writer` says its corpus is authoritative — the unreadable bytes are
    /// renamed aside first, so nothing is destroyed. Otherwise counts the
    /// refusal, logs it at ERROR, and returns [`SnapshotOverwriteRefused`].
    /// Reads the file only on the refusal-candidate path.
    ///
    /// The read and the caller's rename are not atomic, which narrows the #7920
    /// window rather than closing it. What passes through the remaining window
    /// is bounded: an empty or foreign corpus is refused whenever the file holds
    /// anything, so reaching the write means the file held nothing at the read,
    /// and the only writers of that path are this indexer's own two call sites
    /// — which write the same corpus — plus a second daemon, which the PID
    /// lockfile excludes. Closing it outright needs an exclusive lock spanning
    /// read-decide-write, on a path written only while no redb corpus is wired.
    ///
    /// Test: `shutdown_flush_refuses_empty_corpus_over_populated_chunks_json`,
    /// `shutdown_flush_refuses_foreign_partial_corpus`,
    /// `chunks_added_after_load_are_persisted`,
    /// `corrupt_snapshot_does_not_block_the_owning_writer`,
    /// `unreadable_snapshot_no_longer_wedges_a_store_less_writer`,
    /// `a_quarantined_writer_never_reaches_the_unreadable_self_heal`.
    pub(crate) fn check_overwrite(
        &self,
        index_id: &str,
        path: &Path,
        in_memory: usize,
        writer: WriterShape,
    ) -> Result<(), SnapshotOverwriteRefused> {
        let owned = self.owns(path);
        if owned && in_memory > 0 {
            return Ok(());
        }
        let on_disk = match inspect(path) {
            OnDisk::Empty => return Ok(()),
            OnDisk::Chunks(n) => format!("{n} chunk(s) on disk"),
            // #7980: an unreadable file has no chunks any reader can recover —
            // not the migration, not this daemon. Refusing it forever wedged a
            // store-less indexer that holds a real corpus out of ever
            // persisting, a self-heal the pre-#7920 reindex had. Preserve the
            // bytes under a sidecar name and let the write land.
            OnDisk::Unreadable(bytes) => {
                if in_memory > 0 && writer == WriterShape::CorpusIsAuthoritative {
                    match preserve_unreadable(path) {
                        Ok(kept) => {
                            // #7980: the unowned case is the one with residual
                            // risk — this indexer never read that path, so its
                            // corpus may describe a different tree. It is still
                            // admitted (the bytes are unreadable by anyone, and
                            // the alternative is a permanent refusal), but it is
                            // logged as its own shape rather than folded in.
                            let provenance = if owned {
                                "a file this indexer owns"
                            } else {
                                "a file this indexer never loaded — its corpus may describe a \
                                 different location, so verify the replacement"
                            };
                            tracing::warn!(
                                index_id = %index_id,
                                owned = owned,
                                "index '{index_id}': {} held {bytes} unreadable byte(s) in \
                                 {provenance}; moved it to {} so this indexer's {in_memory} \
                                 in-memory chunk(s) can be persisted (#7980). Nothing was \
                                 destroyed.",
                                path.display(),
                                kept.display(),
                            );
                            return Ok(());
                        }
                        // Could not move it aside — fall through to the refusal
                        // rather than write over bytes we failed to preserve.
                        Err(e) => tracing::error!(
                            index_id = %index_id,
                            "index '{index_id}': could not preserve the unreadable snapshot at \
                             {} ({e}) — refusing the write instead (#7980)",
                            path.display(),
                        ),
                    }
                }
                format!("unreadable, {bytes} bytes on disk")
            }
        };
        let reason = if in_memory == 0 {
            "the in-memory corpus is empty"
        } else {
            "this indexer never loaded or wrote that file, so its in-memory corpus \
             describes a different location"
        };
        let refused = self.refused.fetch_add(1, Ordering::Relaxed) + 1;
        let err = SnapshotOverwriteRefused {
            path: path.to_path_buf(),
            on_disk,
            in_memory,
            reason,
        };
        tracing::error!(
            index_id = %index_id,
            refused_snapshot_overwrites = refused,
            "index '{index_id}': {err}. Move the file aside if it is genuinely stale."
        );
        Err(err)
    }

    pub(crate) fn refused(&self) -> u64 {
        self.refused.load(Ordering::Relaxed)
    }
}

impl CodeIndexer {
    /// Number of `chunks.json` writes refused by the #7920 overwrite guard.
    pub fn refused_snapshot_overwrites(&self) -> u64 {
        self.snapshot_guard.refused()
    }

    /// Whether this indexer's in-memory corpus is its own durable state (#7980).
    ///
    /// Why: see [`WriterShape`]. Derived here rather than inside the guard
    /// because these three flags are the indexer's, not the guard's.
    /// What: authoritative only for a store-less indexer that never had a
    /// corpus wired and is not write-quarantined — a legacy `chunks.json`
    /// index. A quarantined indexer, or one whose corpus a staged swap
    /// detached, is a stand-in whose empty corpus proves nothing.
    /// Test: `a_quarantined_writer_never_reaches_the_unreadable_self_heal`.
    pub(crate) fn snapshot_writer_shape(&self) -> WriterShape {
        let shape = if !self.corpus_open_failed && !self.corpus_ever_wired && self.corpus.is_none()
        {
            WriterShape::CorpusIsAuthoritative
        } else {
            WriterShape::CorpusMayBeStandIn
        };
        // #7980: publish on every read so the detached persister, which cannot
        // see these fields, decides on the same value this call just computed.
        self.snapshot_guard.set_shape(shape);
        shape
    }
}
