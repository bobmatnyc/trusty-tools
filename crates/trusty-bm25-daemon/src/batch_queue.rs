//! Tokio-based write-coalescing queue in front of a single `PalaceBm25Index`.
//!
//! Why: every `index` (or `delete`) mutates the inverted index AND triggers a
//! snapshot flush, which would otherwise serialise across concurrent callers
//! and amplify disk I/O. Coalescing arrivals within a short window (default
//! 50 ms — disk I/O dominates here, so the window is 5× the
//! `trusty-embed-daemon`'s 10 ms) lets the worker apply the whole batch in
//! one shot and flush the snapshot once per batch.
//!
//! What: `BatchQueue::new` spawns a worker task that owns the
//! `PalaceBm25Index`. Public methods enqueue write operations and read-only
//! `search` calls. Reads also flow through the queue so the index never
//! needs an `Arc<Mutex<_>>` — the single-owner worker is the canonical
//! "mpsc channel is the lock" pattern. Search latency therefore equals one
//! channel round-trip (~microseconds) plus the score computation; that is
//! invisible at typical palace sizes (hundreds to low thousands of drawers).
//!
//! Test: `batch_queue_persists_indexed_doc`, `batch_queue_search_finds_match`,
//! `batch_queue_delete_removes_doc`, `batch_queue_rebuild_clears_index`.

use std::time::Duration;

use anyhow::{anyhow, Result};
use tokio::sync::{mpsc, oneshot};
use tokio::time::sleep;

use crate::index::PalaceBm25Index;
use crate::protocol::SearchHit;

/// Default write-coalescing window — how long the worker waits to coalesce
/// more write ops after seeing the first one.
///
/// Why: 50 ms is short enough that a human-driven `memory_remember` call
/// barely notices, but long enough to coalesce dozens of nearly-simultaneous
/// arrivals during a bulk-ingest run. Five times the embed daemon's 10 ms
/// window because BM25 writes do disk I/O (snapshot flush) whose latency
/// dominates the in-memory inverted-index update.
pub const DEFAULT_WRITE_WINDOW_MS: u64 = 50;

/// Default maximum batch size before forcing a flush.
///
/// Why: bounds the worst-case latency observed by any single caller. With the
/// 50 ms window the typical batch never reaches this, but a bulk import
/// could otherwise hold writers in the queue indefinitely.
pub const DEFAULT_MAX_BATCH_SIZE: usize = 64;

/// Channel capacity for pending operations.
///
/// Why: 1024 covers a bulk-ingest burst (tens of thousands of drawers
/// loaded back-to-back) with headroom. Above that the writer backpressures,
/// which surfaces overload to the JSON-RPC accept loop instead of silently
/// queueing.
const PENDING_CHANNEL_CAPACITY: usize = 1024;

/// One enqueued operation. The single-owner worker handles all four flavours
/// inline so the BM25 index never escapes the worker task.
enum Op {
    Index {
        doc_id: String,
        text: String,
        reply: oneshot::Sender<Result<bool>>,
    },
    Delete {
        doc_id: String,
        reply: oneshot::Sender<Result<bool>>,
    },
    Rebuild {
        reply: oneshot::Sender<Result<usize>>,
    },
    Search {
        query: String,
        top_k: usize,
        reply: oneshot::Sender<Result<Vec<SearchHit>>>,
    },
}

/// Configuration for the write-coalescing window and size.
///
/// Why: surfacing the two knobs lets the binary's CLI flags flow straight
/// through into worker behaviour without re-deriving defaults at each call
/// site.
/// What: small POD; `Default` returns the documented defaults.
/// Test: covered transitively by the worker's behaviour tests.
#[derive(Debug, Clone, Copy)]
pub struct BatchConfig {
    pub max_batch_size: usize,
    pub write_window: Duration,
}

impl Default for BatchConfig {
    fn default() -> Self {
        Self {
            max_batch_size: DEFAULT_MAX_BATCH_SIZE,
            write_window: Duration::from_millis(DEFAULT_WRITE_WINDOW_MS),
        }
    }
}

/// Client handle for the BM25 batch worker.
///
/// Why: the daemon hands one of these to every accepted connection so
/// per-connection handlers can submit writes and run searches without ever
/// touching the index directly.
/// What: clones cheaply (`Arc` + `mpsc::Sender`). All operations — read
/// AND write — flow through the worker, which is the single owner of the
/// `PalaceBm25Index`. Search is dispatched immediately by the worker (not
/// blocked behind the write window) so reads stay snappy even during a
/// bulk-write storm.
/// Test: `batch_queue_search_finds_match` exercises read and write paths.
#[derive(Clone)]
pub struct BatchQueue {
    tx: mpsc::Sender<Op>,
}

impl BatchQueue {
    /// Spawn the batch worker and return a client handle.
    ///
    /// Why: the worker is the sole owner of the `PalaceBm25Index`; transferring
    /// ownership at construction time means no other task ever holds it
    /// mutably. No `Arc<Mutex<_>>` necessary.
    /// What: creates the bounded mpsc channel, spawns [`batch_worker`] with
    /// the supplied index + config, returns a handle holding the sender.
    /// Test: covered by every worker test in this module.
    pub fn new(index: PalaceBm25Index, config: BatchConfig) -> Self {
        let (tx, rx) = mpsc::channel(PENDING_CHANNEL_CAPACITY);
        tokio::spawn(batch_worker(rx, index, config));
        Self { tx }
    }

    /// Enqueue an `index` op and await the ack.
    ///
    /// Why: callers (the JSON-RPC dispatch) need a per-request future so
    /// they can return a typed response when the write lands.
    /// What: sends a `Op::Index` on the channel, awaits the worker's
    /// oneshot reply. Returns `true` when the doc was inserted/updated.
    /// Test: `batch_queue_persists_indexed_doc`.
    pub async fn index_doc(&self, doc_id: String, text: String) -> Result<bool> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(Op::Index {
                doc_id,
                text,
                reply: reply_tx,
            })
            .await
            .map_err(|_| anyhow!("bm25 batch worker channel closed"))?;
        reply_rx
            .await
            .map_err(|_| anyhow!("bm25 batch worker dropped reply channel"))?
    }

    /// Enqueue a `delete` op and await the ack.
    ///
    /// Why: same motivation as [`Self::index_doc`] — reserved for the dream
    /// subprocess, but exposed on the queue so the dispatch code is uniform.
    /// What: sends a `Op::Delete` on the channel. Returns `true` iff the id
    /// was present beforehand.
    /// Test: `batch_queue_delete_removes_doc`.
    pub async fn delete(&self, doc_id: String) -> Result<bool> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(Op::Delete {
                doc_id,
                reply: reply_tx,
            })
            .await
            .map_err(|_| anyhow!("bm25 batch worker channel closed"))?;
        reply_rx
            .await
            .map_err(|_| anyhow!("bm25 batch worker dropped reply channel"))?
    }

    /// Drop every indexed document and flush the empty snapshot.
    ///
    /// Why: reserved for the dream subprocess (full reindex from sources).
    /// What: sends a `Op::Rebuild`; the worker handles it as a fast-path
    /// (no need to coalesce — there is only ever one rebuild in flight).
    /// Returns the post-rebuild doc count (always `0`).
    /// Test: `batch_queue_rebuild_clears_index`.
    pub async fn rebuild(&self) -> Result<usize> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(Op::Rebuild { reply: reply_tx })
            .await
            .map_err(|_| anyhow!("bm25 batch worker channel closed"))?;
        reply_rx
            .await
            .map_err(|_| anyhow!("bm25 batch worker dropped reply channel"))?
    }

    /// Run a BM25 search against the live index.
    ///
    /// Why: search is read-only but still flows through the worker so the
    /// index lives behind a single owner. Search latency is one channel
    /// round-trip plus the score computation — microseconds in steady state.
    /// What: sends a `Op::Search` on the channel; the worker handles search
    /// immediately, before any write batching, so reads are never blocked by
    /// the write window.
    /// Test: `batch_queue_search_finds_match`.
    pub async fn search(&self, query: String, top_k: usize) -> Result<Vec<SearchHit>> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(Op::Search {
                query,
                top_k,
                reply: reply_tx,
            })
            .await
            .map_err(|_| anyhow!("bm25 batch worker channel closed"))?;
        reply_rx
            .await
            .map_err(|_| anyhow!("bm25 batch worker dropped reply channel"))?
    }
}

/// Worker loop — drains the channel into time-bounded write batches.
///
/// Why: the single owner of the index is the whole point of the queue. The
/// worker batches incoming mutations (50 ms window or 64 ops), applies them
/// in a tight inner loop, then flushes once. Search and rebuild are handled
/// out-of-band — they don't participate in the write batching so they never
/// pay the 50 ms latency tax.
/// What: blocks on `rx.recv()` for the first op. If the first op is a
/// `Search` or `Rebuild`, handles it immediately and continues. If it is a
/// write op (`Index` / `Delete`), enters batching mode: races a tokio
/// `sleep(write_window)` against further `rx.recv()` calls, applying writes
/// inline and forwarding reads / rebuilds immediately. Flushes the snapshot
/// once when the window expires.
/// Test: integration tests in this module.
async fn batch_worker(mut rx: mpsc::Receiver<Op>, mut index: PalaceBm25Index, config: BatchConfig) {
    tracing::info!(
        "bm25 batch worker started (max_batch_size={}, write_window_ms={})",
        config.max_batch_size,
        config.write_window.as_millis()
    );

    loop {
        // Wait for the first op — exits when all senders are dropped.
        let Some(first) = rx.recv().await else {
            tracing::info!("bm25 batch worker shutting down (channel closed)");
            // Best-effort final flush so any unwritten state hits disk.
            if let Err(e) = index.flush() {
                tracing::warn!("final BM25 snapshot flush failed: {e:#}");
            }
            return;
        };

        // Read / rebuild ops never participate in write batching — handle
        // them inline and loop back for the next op.
        let writes_started_with = match first {
            Op::Search {
                query,
                top_k,
                reply,
            } => {
                let hits = index.search(&query, top_k);
                let _ = reply.send(Ok(hits));
                continue;
            }
            Op::Rebuild { reply } => {
                let new_count = index.rebuild();
                let flush_result = index
                    .flush()
                    .map(|_| new_count)
                    .map_err(|e| anyhow::anyhow!("BM25 rebuild flush failed: {e:#}"));
                let _ = reply.send(flush_result);
                continue;
            }
            write_op @ (Op::Index { .. } | Op::Delete { .. }) => write_op,
        };

        // We have a write op — enter batching mode.
        let mut write_count: usize = 0;
        apply_write_op(&mut index, writes_started_with);
        write_count += 1;

        let deadline = sleep(config.write_window);
        tokio::pin!(deadline);

        loop {
            if write_count >= config.max_batch_size {
                break;
            }
            tokio::select! {
                biased;
                _ = &mut deadline => break,
                next = rx.recv() => match next {
                    Some(Op::Search { query, top_k, reply }) => {
                        // Read interleaved with writes — the index already
                        // reflects every applied write so a search here is
                        // race-free.
                        let hits = index.search(&query, top_k);
                        let _ = reply.send(Ok(hits));
                    }
                    Some(Op::Rebuild { reply }) => {
                        // Honour the rebuild atomically. Flush the pending
                        // write batch first so the new empty state is the
                        // last thing on disk.
                        let flush_pre = index.flush();
                        if let Err(e) = flush_pre {
                            // Surface the flush error to whoever issued the
                            // rebuild — they care about durability.
                            let _ = reply.send(Err(anyhow::anyhow!(
                                "BM25 pre-rebuild flush failed: {e:#}"
                            )));
                            continue;
                        }
                        let new_count = index.rebuild();
                        let flush_result = index
                            .flush()
                            .map(|_| new_count)
                            .map_err(|e| anyhow::anyhow!("BM25 rebuild flush failed: {e:#}"));
                        let _ = reply.send(flush_result);
                        // Reset the write-batch counter since we already
                        // flushed; let the outer loop pick up the next op.
                        write_count = 0;
                        break;
                    }
                    Some(write_op @ (Op::Index { .. } | Op::Delete { .. })) => {
                        apply_write_op(&mut index, write_op);
                        write_count += 1;
                    }
                    None => {
                        // Channel closed mid-batch — flush below and exit.
                        if let Err(e) = index.flush() {
                            tracing::warn!("BM25 snapshot flush at shutdown failed: {e:#}");
                        }
                        return;
                    }
                }
            }
        }

        if write_count > 0 {
            tracing::debug!(
                write_count,
                doc_count = index.doc_count(),
                "bm25 write batch flushing snapshot"
            );
            if let Err(e) = index.flush() {
                tracing::error!("BM25 snapshot flush failed: {e:#}");
            }
        }
    }
}

/// Apply one write op to the index, forwarding the typed reply.
///
/// Why: factored out so the worker loop reads top-to-bottom and the per-op
/// reply shape stays in one place.
/// What: handles `Index` (returns `true` on success) and `Delete` (returns
/// the prior-presence bool). The receiver awaiting the oneshot may have been
/// cancelled (client disconnect); ignore that case so the worker keeps
/// draining the rest of the batch.
fn apply_write_op(index: &mut PalaceBm25Index, op: Op) {
    match op {
        Op::Index {
            doc_id,
            text,
            reply,
        } => {
            index.index_doc(&doc_id, &text);
            let _ = reply.send(Ok(true));
        }
        Op::Delete { doc_id, reply } => {
            let was_present = index.delete_doc(&doc_id);
            let _ = reply.send(Ok(was_present));
        }
        // Caller guarantees only write ops reach here; treat anything else
        // as a programmer error in this module.
        Op::Search { .. } | Op::Rebuild { .. } => {
            tracing::error!("apply_write_op called with non-write op — this is a bug");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_queue() -> (BatchQueue, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let index = PalaceBm25Index::load_or_create(dir.path()).expect("load palace index");
        (BatchQueue::new(index, BatchConfig::default()), dir)
    }

    #[tokio::test]
    async fn batch_queue_persists_indexed_doc() {
        let (q, _dir) = fresh_queue();
        q.index_doc("d1".into(), "hello world".into())
            .await
            .unwrap();
        let hits = q.search("hello".into(), 5).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].doc_id, "d1");
    }

    #[tokio::test]
    async fn batch_queue_search_finds_match() {
        let (q, _dir) = fresh_queue();
        q.index_doc("d1".into(), "the quick brown fox".into())
            .await
            .unwrap();
        q.index_doc("d2".into(), "lazy dog napping".into())
            .await
            .unwrap();

        let hits = q.search("fox".into(), 10).await.unwrap();
        assert!(!hits.is_empty(), "expected at least one hit for 'fox'");
        assert_eq!(hits[0].doc_id, "d1");
        assert!(hits[0].score > 0.0);
    }

    #[tokio::test]
    async fn batch_queue_delete_removes_doc() {
        let (q, _dir) = fresh_queue();
        q.index_doc("d1".into(), "alpha beta gamma".into())
            .await
            .unwrap();
        assert!(!q.search("alpha".into(), 10).await.unwrap().is_empty());
        let was_present = q.delete("d1".into()).await.unwrap();
        assert!(was_present);
        let hits = q.search("alpha".into(), 10).await.unwrap();
        assert!(hits.is_empty(), "expected no hits after delete");
    }

    #[tokio::test]
    async fn batch_queue_rebuild_clears_index() {
        let (q, _dir) = fresh_queue();
        q.index_doc("d1".into(), "alpha".into()).await.unwrap();
        q.index_doc("d2".into(), "beta".into()).await.unwrap();
        let count = q.rebuild().await.unwrap();
        assert_eq!(count, 0);
        assert!(q.search("alpha".into(), 10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn batch_queue_search_respects_top_k() {
        let (q, _dir) = fresh_queue();
        for i in 0..5 {
            q.index_doc(format!("d{i}"), format!("token{i} shared"))
                .await
                .unwrap();
        }
        let hits = q.search("shared".into(), 2).await.unwrap();
        assert!(hits.len() <= 2);
    }

    #[tokio::test]
    async fn batch_queue_snapshot_survives_restart() {
        let dir = tempfile::tempdir().expect("tempdir");
        {
            let index = PalaceBm25Index::load_or_create(dir.path()).expect("load palace index");
            let q = BatchQueue::new(index, BatchConfig::default());
            q.index_doc("a".into(), "phoenix".into()).await.unwrap();
            // Ensure the write batch flushes by giving it past the window.
            tokio::time::sleep(Duration::from_millis(120)).await;
        }
        // Reopen — the snapshot must rehydrate the corpus.
        let reopened = PalaceBm25Index::load_or_create(dir.path()).unwrap();
        let q2 = BatchQueue::new(reopened, BatchConfig::default());
        let hits = q2.search("phoenix".into(), 10).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].doc_id, "a");
    }
}
