//! Count and warn on chunks the embedder truncates at its input window (#9284).
//!
//! Why: an embedder encodes at most its window of tokens and drops the rest
//! with no error, so the vector of a long chunk silently describes only its
//! head. Nothing counted it.
//! What: [`EmbedWindowStats`], the per-index counters `GET /indexes/:id/status`
//! reports. In memory only: nothing here is persisted.
//! Test: `core::indexer::tests::embed_window_9284`.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// Per-index embed-window counters since this indexer was built (#9284).
///
/// Why: an operator needs to see how much of the vector lane describes cut
/// text, without reading daemon logs.
/// What: `truncated` counts embedded chunks whose token count, special tokens
/// included, exceeded the window. `unmeasured` counts embedded chunks the
/// window applied to but whose length could not be counted. `window` is the
/// window of the latest embed pass, `0` when that embedder reports none.
/// Test: `a_chunk_over_the_window_counts_exactly_once`,
/// `a_chunk_within_the_window_does_not_count`.
#[derive(Debug, Default)]
pub struct EmbedWindowStats {
    truncated: AtomicU64,
    unmeasured: AtomicU64,
    window: AtomicUsize,
}

impl EmbedWindowStats {
    /// Embedded chunks cut at the window.
    pub fn truncated(&self) -> u64 {
        self.truncated.load(Ordering::Relaxed)
    }

    /// Embedded chunks whose length could not be counted against the window.
    pub fn unmeasured(&self) -> u64 {
        self.unmeasured.load(Ordering::Relaxed)
    }

    /// The latest pass's window; `None` when the embedder reports none, so
    /// the two counters cannot see truncation (#9284).
    pub fn window(&self) -> Option<usize> {
        match self.window.load(Ordering::Relaxed) {
            0 => None,
            n => Some(n),
        }
    }
}
