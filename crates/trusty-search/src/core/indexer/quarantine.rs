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
//! [`CodeIndexer::refuse_incremental_write`] (the enforcement + diagnostic),
//! [`CodeIndexer::refused_incremental_writes`] (the observable counter), and
//! [`CodeIndexer::clear_corpus_open_failure`] (the way back out, driven by
//! `set_corpus_store`). The quarantine is deliberately *asymmetric*: refusing
//! a write costs an un-indexed file save that the next reindex picks up
//! anyway, whereas accepting one destroys the corpus. So every corpus-open
//! failure — transient I/O error, permission denial, stale redb file lock, an
//! unresolvable storage path, or genuine corruption — quarantines, because at
//! the point of failure they are indistinguishable and only one of the two
//! possible mistakes is recoverable.
//!
//! Reads are NOT gated: a quarantined index still serves searches (returning
//! whatever its empty in-memory corpus holds). Making failed indexes stop
//! answering with silent empty results is issue #4087 and is deliberately out
//! of scope here.
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
//! Test: `crates/trusty-search/tests/corpus_open_quarantine_4122.rs`.

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
    /// corpus path could not be resolved at all (#2847).
    /// Test: `quarantined_index_refuses_watcher_write_and_chunk_count_stays_zero`.
    pub fn is_write_quarantined(&self) -> bool {
        self.corpus_open_failed
    }

    /// Number of incremental writes refused since the index entered
    /// quarantine (issue #4122).
    ///
    /// Why: the refusal must be observable from something other than a log
    /// line — tests need a deterministic condition to wait on instead of a
    /// sleep, and operators need a count rather than "some writes were
    /// dropped".
    /// What: monotonic counter, reset to 0 by
    /// [`Self::clear_corpus_open_failure`] when the index recovers.
    /// Test: `quarantined_index_refuses_watcher_write_and_chunk_count_stays_zero`.
    pub fn refused_incremental_writes(&self) -> u64 {
        self.incremental_writes_refused.load(Ordering::Relaxed)
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
        let refused = self
            .incremental_writes_refused
            .fetch_add(1, Ordering::Relaxed)
            + 1;
        let index_id = &self.index_id;
        if refused == 1 || refused.is_multiple_of(QUARANTINE_ERROR_LOG_INTERVAL) {
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
                 #4122). {refused} write(s) refused so far; the on-disk corpus is \
                 untouched and still recoverable. TO RECOVER: fix the underlying redb \
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
