//! The durable-corpus read fault: one shared record of the last failed read of
//! an index's redb corpus, and the error every reader raises from it (#5917).
//!
//! Why: a corpus that OPENS cleanly and then goes bad — redb's "Previous I/O
//! error occurred" state, which fails every later read for the process's
//! lifetime — passes #4087's open-time guard. Each reader then absorbed the
//! failure on its own: the detached rehydrate logged a `warn` and left the
//! in-memory maps empty, and `fetch_chunks_for_ids` logged a `warn` and fell
//! back to those same empty maps. A search over an index holding 85,269 chunks
//! therefore returned `results: []` at HTTP 200 with `bm25_lane_degraded: true`
//! — a total outage rendered as "nothing matched".
//!
//! What: [`CorpusReadFault`], an `Arc`-shared cell every corpus reader writes
//! to (the detached rehydrate task holds only `Arc` clones of the index's
//! state, so a plain field on `CodeIndexer` cannot reach it), plus
//! [`CorpusReadUnavailable`], the typed error the search path raises so
//! `service::server::search` can render it as `503 index_corpus_unavailable`
//! rather than a bare 500. The record is self-healing: any successful durable
//! read clears it, so a single transient failure cannot wedge an index into
//! permanent refusal.
//!
//! Test: `core::indexer::tests::corpus_fault`.

use std::sync::Mutex;

/// The durable corpus opened, then failed a read (#5917).
///
/// Why: the search path must raise something the HTTP layer can recognise. A
/// bare `anyhow::Error` reached `search_handler` as `500 internal search
/// error`, which names neither the index nor the fault; a typed error lets the
/// handler downcast and answer with the same `503 index_corpus_unavailable`
/// contract `GET /indexes/:id/chunks` already uses for this state.
/// What: carries the index it is about and the underlying fault text. The
/// `Display` body is the message the caller reads, so it states both.
/// Test: `search_over_an_unreadable_corpus_is_an_error_not_an_empty_result_set`.
#[derive(Debug, thiserror::Error)]
#[error(
    "index '{index_id}': the durable corpus could not be read ({detail}) — answering \
     from the in-memory view would report an unreadable corpus as an empty one \
     (#5917). Retry once the read recovers; redb's \"Previous I/O error occurred\" \
     state clears on daemon restart."
)]
pub struct CorpusReadUnavailable {
    /// The index whose corpus could not be read.
    pub index_id: String,
    /// The underlying fault, as reported by the failing read.
    pub detail: String,
}

/// Shared record of the most recent durable-corpus read failure for one index.
///
/// Why: the failure is discovered in three places that cannot see each other —
/// the detached rehydrate task, the query-time point-read, and the cursor
/// enumeration — and is consumed in a fourth (the tail of
/// `CodeIndexer::search_with_drops`, which must refuse rather than return the
/// empty lanes that failure produced). One `Arc`-shared cell is what connects
/// them; `lane_degraded` is the same pattern for the "still rehydrating" state
/// this one deliberately does NOT cover.
/// What: a `Mutex<Option<String>>` holding the last fault text, or `None` when
/// the corpus last read cleanly. Every critical section is a single move, never
/// held across an await.
/// Test: `core::indexer::tests::corpus_fault`.
#[derive(Debug, Default)]
pub(crate) struct CorpusReadFault {
    last: Mutex<Option<String>>,
}

impl CorpusReadFault {
    /// Record a failed durable read, replacing any earlier fault.
    ///
    /// `detail` is the full `{err:#}` chain of the failing read; the caller
    /// formats it so this type never depends on `anyhow`.
    pub(crate) fn record(&self, detail: impl Into<String>) {
        *self.lock() = Some(detail.into());
    }

    /// Clear the fault after a durable read succeeded.
    ///
    /// Why: without this a single transient failure would refuse every later
    /// search forever, because the recorded fault outlives the condition that
    /// caused it. Any successful read is proof the corpus answers again.
    pub(crate) fn clear(&self) {
        *self.lock() = None;
    }

    /// The last recorded fault, or `None` when the corpus last read cleanly.
    pub(crate) fn detail(&self) -> Option<String> {
        self.lock().clone()
    }

    /// The typed error for `index_id`, or `None` when there is no fault.
    pub(crate) fn error(&self, index_id: &str) -> Option<CorpusReadUnavailable> {
        self.detail().map(|detail| CorpusReadUnavailable {
            index_id: index_id.to_string(),
            detail,
        })
    }

    /// Poison-tolerant lock: a panic in one of the single-move critical
    /// sections above cannot leave the record inconsistent, so recovering the
    /// inner value is correct and keeps a poisoned mutex from turning a
    /// corpus fault into a daemon-wide one.
    fn lock(&self) -> std::sync::MutexGuard<'_, Option<String>> {
        self.last.lock().unwrap_or_else(|e| e.into_inner())
    }
}
