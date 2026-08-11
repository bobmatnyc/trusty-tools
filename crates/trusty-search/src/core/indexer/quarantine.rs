//! Corpus-open write quarantine for [`CodeIndexer`] (issue #4122).
//!
//! Why: when an index's durable redb corpus FAILS to open at load time the
//! loader wires no `CorpusStore` at all, leaves the in-memory chunk map empty,
//! and sets [`CodeIndexer::corpus_open_failed`] — but the handle otherwise
//! stays fully live, watcher included. In the #4122 production incident that
//! combination turned a *recoverable* outage into permanent data loss: the
//! file watcher for `duettoresearch-hackathon-tm-hackathon-01` kept accepting
//! incremental writes from unrelated file saves, so `chunk_count` climbed
//! `0 → 68 → 1334` and a FRESH, PARTIAL corpus was persisted over the
//! never-opened original. The sibling index `cto-duetto` hit the identical
//! open failure in the same boot, took no watcher writes, and recovered all
//! 200,090 chunks on the next restart. Watcher-writes-during-failure was the
//! sole difference between full recovery and unrecoverable loss.
//!
//! What: this module gives `CodeIndexer` a small quarantine surface —
//! [`CodeIndexer::is_write_quarantined`] (the predicate),
//! [`CodeIndexer::refuse_incremental_write`] (enforcement + diagnostic for the
//! ingest family), [`CodeIndexer::refuse_durable_write`] (the same for the
//! snapshot family — see below, #4226),
//! [`CodeIndexer::refused_incremental_writes`] (the observable counter), and
//! [`CodeIndexer::clear_corpus_open_failure`] (the way back out, driven by
//! `set_corpus_store`). The quarantine is deliberately *asymmetric*: refusing
//! a write costs an un-indexed file save that the next reindex picks up
//! anyway, whereas accepting one destroys the corpus. So every corpus-open
//! failure that REACHES this module — transient I/O error, permission denial,
//! stale redb file lock, or an unresolvable storage path — quarantines, because
//! at the point of failure they are indistinguishable and only one of the two
//! possible mistakes is recoverable.
//!
//! ## Genuine corruption reaches here too, since #4227
//!
//! It did not always. `CorpusStore::open` (`core/corpus/store_impl.rs`)
//! delegates to `open_corpus_db_or_recreate`, and `core/corpus_recovery.rs`
//! used to classify `UpgradeRequired`, `RepairAborted`,
//! `Storage(Corrupted(_))` and `Storage(Io(InvalidData))` all as RECOVERABLE:
//! move the file aside with a `.v2-incompatible` suffix, return `Ok` with a
//! fresh EMPTY corpus. Because that was an `Ok`, the loader wired the empty
//! store and `corpus_open_failed` stayed FALSE — so a corrupt index came up
//! reporting HEALTHY with a live watcher, and the watcher then populated the
//! recreated corpus with a partial rebuild. The #4122 data-loss shape exactly,
//! with this quarantine and the #4087 query-surface guards both disarmed,
//! because every one of them keys on that flag.
//!
//! #4227 split the two buckets. `UpgradeRequired` — a KNOWN old on-disk format
//! with a data-preserving migration tool (`trusty-search migrate-redb`) — still
//! recreates silently; quarantining it would turn every redb major upgrade into
//! an outage. The three corruption variants now back the file aside and return
//! a typed `CorpusCorrupted` error, which reaches
//! `build_indexer_from_entry`'s failure arm, sets `corpus_open_failed` with
//! `CorpusOpenFailure::FormatIncompatible`, and wires NO corpus — so the
//! invariant below holds unchanged and the write quarantine engages.
//!
//! So the trigger list above is complete as written, and the quarantine
//! population is no longer transient-dominated. One consequence still worth
//! keeping in view: an in-process reopen retry would clear the transient part of
//! this population, which today needs a daemon restart (#4122 / PR #4220
//! HIGH-1). A corrupt index needs that restart too, but for a different reason —
//! the damaged file is already off the canonical path, so the next boot opens a
//! clean corpus and boot reconcile rebuilds it from source.
//!
//! Reads are NOT gated: a quarantined index still serves searches (returning
//! whatever its empty in-memory corpus holds). Making failed indexes stop
//! answering with silent empty results is issue #4087 and is deliberately out
//! of scope here.
//!
//! # The two write families, and why BOTH are gated (#4226)
//!
//! `refuse_incremental_write` covers the *ingest* family — the watcher path
//! that destroyed the #4122 index. It is not the whole story. `CodeIndexer`
//! also owns a *snapshot* family — [`CodeIndexer::save_chunks_to_disk`],
//! [`CodeIndexer::flush_corpus_to_disk`], [`CodeIndexer::save_vector_store`],
//! and `persist_hnsw::spawn_incremental_persist` — which writes the legacy
//! `chunks.json` corpus snapshot and the HNSW graph to plain files, NOT
//! through `self.corpus`. Those writers key on `self.corpus.is_none()`,
//! which on a quarantined index is `true`, so #4122 left them firing: the
//! shutdown flush wrote the (deliberately empty) in-memory corpus over
//! `chunks.json`, silently destroying a snapshot that
//! `indexer::migrations::JsonCorpusToRedbMigration` still reads at warm boot.
//! [`CodeIndexer::refuse_durable_write`] gates that family, so the ERROR
//! diagnostic's "the on-disk corpus is untouched" claim is now true of every
//! on-disk artifact and not merely of redb.
//!
//! # LOAD-BEARING INVARIANT: `corpus_open_failed == true` ⇒ `self.corpus == None`
//!
//! This — NOT "reindexes are operator-initiated" — is what makes the ungated
//! BULK path safe. Boot reconcile auto-fires full reindexes
//! (`service::reconcile`) with no `corpus_open_failed` check at all, so a
//! quarantined index CAN be reindexed without any human involved. That is
//! harmless only because every durable redb write funnels through
//! `self.corpus` and short-circuits when it is `None`
//! (`ingest::commit::commit_corpus_to_redb` early-returns on `None`), and
//! because `service::reindex::staging::should_stage(has_corpus_store())` is
//! `false` here, so such a reindex never even stages.
//!
//! The invariant is what makes the redb lane safe; it is emphatically NOT
//! what makes the snapshot lane safe. `self.corpus == None` is precisely the
//! condition that TURNS THE SNAPSHOT WRITERS ON — which is why they need
//! their own gate rather than inheriting this one (#4226).
//!
//! The invariant holds because the only producer of `corpus_open_failed ==
//! true` (`service::persistence_loader::build_indexer_from_entry`) is exactly
//! the path that wires NO corpus, and the only way to wire one
//! ([`CodeIndexer::set_corpus_store`]) clears the flag in the same call.
//!
//! **If you add an in-process corpus reopen/retry, you will break this.** A
//! retry that wires a corpus without clearing the flag — or that sets the flag
//! on an index that still holds one — re-opens the exact data-loss hole this
//! module closes, and every existing test stays green while it does. The two
//! `debug_assert!`s (in [`CodeIndexer::refuse_incremental_write`] and in
//! `commit_corpus_to_redb`) exist to make that failure loud instead of silent.
//!
//! Test: `crates/trusty-search/tests/corpus_open_quarantine_4122.rs` (ingest
//! family), `crates/trusty-search/tests/quarantine_durable_writes_4226.rs`
//! (snapshot family), and
//! `crates/trusty-search/tests/corpus_corruption_quarantine_4227.rs`
//! (corruption reaches the quarantine at all).

use std::sync::atomic::Ordering;

use super::CodeIndexer;

/// Emit the full ERROR diagnostic on the 1st refusal and then every Nth.
///
/// Why: a busy worktree can fire thousands of watcher events while an index is
/// quarantined. The first refusal must be loud (it is the operator's only
/// warning that saves are being dropped), but repeating it per-event would
/// flood `errors.jsonl` and drown every other captured error. Periodic
/// re-emission keeps the condition visible for an operator who starts reading
/// logs *after* the first event without turning the log into a firehose.
/// What: refusal `n` logs at ERROR when `n == 1 || n % INTERVAL == 0`;
/// every other refusal logs at DEBUG. The running total is carried in the
/// ERROR record's `refused_writes` field, so no refusal is unaccounted for.
/// Test: `quarantine_refusal_emits_error_level_diagnostic` asserts the first
/// refusal is captured at ERROR.
const QUARANTINE_ERROR_LOG_INTERVAL: u64 = 100;

impl CodeIndexer {
    /// True when this index's durable corpus failed to open and it must
    /// therefore refuse incremental writes (issue #4122).
    ///
    /// Why: gives the watcher glue (`service::watch_loop`) a cheap predicate
    /// so it can bail out *before* reading and chunking a saved file, and
    /// gives tests/diagnostics a name for the state.
    /// What: mirrors [`CodeIndexer::corpus_open_failed`], which is set by
    /// `service::persistence_loader::build_indexer_from_entry` on exactly two
    /// conditions — `open_corpus_with_retry` returned `Err`, or the redb
    /// corpus path could not be resolved at all (#2847). Since #4227 a
    /// genuinely corrupt corpus reaches the first of those instead of being
    /// recreated empty behind an `Ok`.
    /// Test: `quarantined_index_refuses_watcher_write_and_chunk_count_stays_zero`
    /// and `corrupt_corpus_refuses_watcher_writes_and_chunk_count_stays_zero`.
    pub fn is_write_quarantined(&self) -> bool {
        self.corpus_open_failed
    }

    /// Number of writes refused since the index entered quarantine (issue
    /// #4122; widened to cover snapshot writes by #4226).
    ///
    /// Why: the refusal must be observable from something other than a log
    /// line — tests need a deterministic condition to wait on instead of a
    /// sleep, and operators need a count rather than "some writes were
    /// dropped".
    /// What: monotonic counter covering BOTH refusal families — incremental
    /// ingest ([`Self::refuse_incremental_write`]) and durable snapshot
    /// flushes ([`Self::refuse_durable_write`]). Reset to 0 by
    /// [`Self::clear_corpus_open_failure`] when the index recovers. The name
    /// is retained for API compatibility.
    /// Test: `quarantined_index_refuses_watcher_write_and_chunk_count_stays_zero`.
    pub fn refused_incremental_writes(&self) -> u64 {
        self.incremental_writes_refused.load(Ordering::Relaxed)
    }

    /// Bump the refusal counter and decide whether this refusal is loud.
    ///
    /// Why: both refusal families ([`Self::refuse_incremental_write`] and
    /// [`Self::refuse_durable_write`]) share one counter and one log-rate
    /// policy; duplicating the `fetch_add` + modulo in each would let them
    /// drift apart.
    /// What: returns the post-increment count and `true` when the caller
    /// should log at ERROR rather than DEBUG.
    /// Test: `quarantine_refusal_emits_error_level_diagnostic` (first refusal
    /// is loud); `quarantined_shutdown_flush_does_not_destroy_chunks_json`
    /// (the durable-write family shares the counter).
    fn count_refusal(&self) -> (u64, bool) {
        let refused = self
            .incremental_writes_refused
            .fetch_add(1, Ordering::Relaxed)
            + 1;
        (
            refused,
            refused == 1 || refused.is_multiple_of(QUARANTINE_ERROR_LOG_INTERVAL),
        )
    }

    /// Refuse a durable *snapshot* write (`chunks.json`, the HNSW graph) while
    /// this index is quarantined; returns `true` when the caller must abandon
    /// the write (issue #4226).
    ///
    /// Why: the #4122 quarantine gated the ingest family only, and the
    /// snapshot family is keyed on `self.corpus.is_none()` — which quarantine
    /// makes `true`. So the one lane the quarantine was meant to close stayed
    /// open on the shutdown path: `flush_corpus_to_disk` fell through to
    /// `save_chunks_to_disk` and wrote the deliberately-empty in-memory corpus
    /// over the legacy `chunks.json` snapshot, which
    /// `indexer::migrations::JsonCorpusToRedbMigration` still reads at warm
    /// boot. Gating the *family* rather than the single `chunks.json` call
    /// site also covers the HNSW snapshot, whose own shrink guards
    /// (`core::store::usearch_store::SHRINK_GUARD_MIN_ON_DISK_VECTORS`) are
    /// inert below 1 000 on-disk vectors and above the 50 % ratio.
    /// What: no-op returning `false` when not quarantined. Otherwise bumps
    /// [`Self::refused_incremental_writes`] and logs — ERROR on the first
    /// refusal and every [`QUARANTINE_ERROR_LOG_INTERVAL`]th thereafter,
    /// DEBUG in between — then returns `true`.
    /// Test: `quarantined_shutdown_flush_does_not_destroy_chunks_json` and
    /// `quarantined_index_refuses_hnsw_snapshot_write` in
    /// `tests/quarantine_durable_writes_4226.rs`.
    pub(crate) fn refuse_durable_write(&self, op: &str, target: &str) -> bool {
        if !self.corpus_open_failed {
            return false;
        }
        let (refused, loud) = self.count_refusal();
        let index_id = &self.index_id;
        if loud {
            tracing::error!(
                index_id = %index_id,
                op = %op,
                target = %target,
                refused_writes = refused,
                "index '{index_id}': REFUSING durable snapshot write ({op} {target}) — \
                 this index's durable redb corpus failed to open at load time \
                 (corpus_open_failed), so the index is write-quarantined and its \
                 in-memory corpus is EMPTY. Writing that empty state out would \
                 overwrite the on-disk snapshot with nothing — the legacy \
                 chunks.json snapshot is still read at warm boot by the \
                 chunks.json → index.redb migration, so destroying it destroys a \
                 recovery source (issue #4226). {refused} write(s) refused so far. \
                 TO RECOVER: fix the underlying redb file (permissions, stale lock, \
                 corruption), then RESTART THE DAEMON — only a successful \
                 CorpusStore::open lifts the quarantine."
            );
        } else {
            tracing::debug!(
                index_id = %index_id,
                op = %op,
                target = %target,
                refused_writes = refused,
                "write-quarantined index refused a durable snapshot write (issue #4226)"
            );
        }
        true
    }

    /// Refuse `op` against `target` when this index is quarantined, logging a
    /// diagnostic that actually persists; returns `true` when the caller must
    /// abandon the write (issue #4122).
    ///
    /// Why: a *silent* no-op would reproduce the original sin of this bug
    /// class — the system reporting a state it does not have. In this
    /// workspace only ERROR-level events reach `errors.jsonl` /
    /// `list_recent_errors` / `tm doctor` (`trusty_common::error_capture`'s
    /// `BugCaptureLayer` drops anything that is not `Level::ERROR`, and the
    /// daemon installs it via `init_tracing_with_buffer_and_capture` in
    /// `commands::start::daemon`), so `warn!` here would be invisible to
    /// every diagnostic surface an operator actually reads. Hence ERROR.
    /// What: no-op returning `false` when not quarantined. Otherwise bumps
    /// [`Self::refused_incremental_writes`] and logs — ERROR on the first
    /// refusal and every [`QUARANTINE_ERROR_LOG_INTERVAL`]th thereafter,
    /// DEBUG in between — then returns `true`.
    /// Test: `quarantine_refusal_emits_error_level_diagnostic`.
    pub(crate) fn refuse_incremental_write(&self, op: &str, target: &str) -> bool {
        if !self.corpus_open_failed {
            return false;
        }
        // Issue #4122, see the module-level invariant: a quarantined index must
        // never hold a wired corpus. If it ever does, the ungated BULK reindex
        // path (auto-fired by boot reconcile) would write durably to a corpus
        // this module has declared unsafe — silently, with CI green.
        debug_assert!(
            self.corpus.is_none(),
            "index '{}': INVARIANT VIOLATED — corpus_open_failed is set while a \
             CorpusStore is wired. The ungated bulk-reindex path is safe only \
             because a quarantined index has no corpus to write through. Did you \
             add an in-process corpus reopen that skips set_corpus_store (issue \
             #4122)?",
            self.index_id
        );
        let (refused, loud) = self.count_refusal();
        let index_id = &self.index_id;
        if loud {
            tracing::error!(
                index_id = %index_id,
                op = %op,
                target = %target,
                refused_writes = refused,
                "index '{index_id}': REFUSING incremental write ({op} {target}) — this \
                 index's durable redb corpus failed to open at load time \
                 (corpus_open_failed), so the index is write-quarantined. Accepting \
                 watcher/incremental writes against an unopened corpus builds a fresh \
                 PARTIAL corpus over the original and destroys it permanently (issue \
                 #4122). {refused} write(s) refused so far; NO on-disk artifact of \
                 this index is written while quarantined — not index.redb, not the \
                 legacy chunks.json snapshot, not the HNSW graph (issue #4226) — so \
                 all of them stay recoverable. TO RECOVER: fix the underlying redb \
                 file (permissions, stale lock, corruption), then RESTART THE DAEMON — \
                 only a successful CorpusStore::open lifts the quarantine, and it is \
                 attempted solely at load time (there is no in-process reopen retry). \
                 A reindex will NOT clear this state and will NOT persist anything: \
                 with no corpus wired it skips staging entirely, so its results are \
                 discarded at the next restart. Saves dropped while quarantined are \
                 picked up by the first reindex AFTER a successful restart."
            );
        } else {
            tracing::debug!(
                index_id = %index_id,
                op = %op,
                target = %target,
                refused_writes = refused,
                "write-quarantined index refused an incremental write (issue #4122)"
            );
        }
        true
    }

    /// Lift the write quarantine after a corpus open succeeds (issue #4122).
    ///
    /// Why: quarantining forever on one transient failure would be a new
    /// outage, not a fix — the #4122 contrast case (`cto-duetto`) recovered
    /// cleanly on the next boot and MUST keep doing so. A successful
    /// `CorpusStore::open` is precisely the event that makes writes safe
    /// again, so [`CodeIndexer::set_corpus_store`] — the single sink every
    /// successful open funnels through — calls this. There is no separate
    /// "unquarantine" lever to forget to pull.
    /// What: no-op when not quarantined (the overwhelmingly common
    /// healthy-boot call). Otherwise clears `corpus_open_failed`, resets the
    /// refusal counter, and logs at WARN — the daemon's default stderr filter
    /// is `warn`, so a recovery notice at INFO would not be printed at all.
    /// Note `swap_corpus_store` deliberately does NOT call this: it replaces
    /// an already-wired corpus (the reindex staging swap), which by
    /// construction cannot happen on a quarantined index because
    /// `has_corpus_store()` is `false` there.
    /// Test: `successful_reopen_lifts_quarantine_and_leaves_corpus_intact`.
    pub(crate) fn clear_corpus_open_failure(&mut self) {
        if !self.corpus_open_failed {
            return;
        }
        self.corpus_open_failed = false;
        // #4333: the classification is cleared in the SAME call that clears the
        // flag, so `corpus_open_failure.is_some() == corpus_open_failed` holds
        // by construction and the two surfaces can never disagree.
        self.corpus_open_failure = None;
        let refused = self.incremental_writes_refused.swap(0, Ordering::Relaxed);
        tracing::warn!(
            index_id = %self.index_id,
            refused_writes = refused,
            "index '{}': durable corpus opened successfully — lifting the #4122 write \
             quarantine; incremental/watcher writes are accepted again ({} write(s) were \
             refused while quarantined; run a reindex to pick them up)",
            self.index_id,
            refused
        );
    }
}
