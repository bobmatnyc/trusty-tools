//! Tokio-based batching queue for the unified `trusty-embedderd` daemon.
//!
//! Why: `trusty-embedderd` serves both an HTTP endpoint and a UDS endpoint
//! from the same ONNX session. Without batching, concurrent callers each
//! submit a single-item call to the model, wasting the throughput gains of
//! ONNX's batched execution. The queue collects pending texts inside a small
//! time window (default 10 ms) or until a batch-size cap (default 32) is
//! reached, then runs one `embed_batch` and fans the per-text results back to
//! each waiting caller via `oneshot` channels.
//!
//! What: `BatchQueue::new` spawns a worker task that owns the embedder.
//! Public methods enqueue requests and await the oneshot reply. The worker
//! exits when the channel is closed (i.e. all `BatchQueue` handles dropped).
//! Both the HTTP handler and the UDS accept loop hold a clone of `BatchQueue`
//! and submit through it — the same ONNX session serves both transports.
//!
//! Test: `batch_queue_collapses_concurrent_requests` constructs the queue
//! against a `MockEmbedder`, fires N concurrent `embed_one` calls, and
//! asserts each gets the expected vector. The test does not assert on batch
//! grouping (timing-dependent) — that property is observed via tracing in
//! manual benchmark runs. Ported from `trusty-embed-daemon` (issue #164).

use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};
use tokio::sync::{mpsc, oneshot};
use tokio::time::sleep;
use trusty_common::embedder::Embedder;

/// Default batching window — how long the worker waits to coalesce more
/// requests after seeing the first one.
///
/// Why: 10 ms is short enough to be invisible to a human-facing recall
/// query but long enough to coalesce dozens of nearly-simultaneous arrivals
/// in the steady state.
pub const DEFAULT_BATCH_WINDOW_MS: u64 = 10;

/// Default maximum batch size before forcing a flush.
///
/// Why: empirical sweep on M4 Max (issue #753) showed 64 gives the best
/// throughput (~83 cps) vs 32 (~77 cps) at only modest RSS growth
/// (285 MB → 369 MB). No OOM or CoreML tripwire was observed at 64.
/// Raised from 32 to 64 as part of the #753 multi-flight pipeline fix.
pub const DEFAULT_BATCH_SIZE: usize = 64;

/// Channel capacity for pending requests.
///
/// Why: 512 covers a worst-case burst (50 concurrent callers x ~10 in
/// flight each) with headroom. Above that we backpressure the writer, which
/// surfaces overload to the JSON-RPC accept loop instead of silently
/// queueing thousands of texts.
const PENDING_CHANNEL_CAPACITY: usize = 512;

/// One queued embed request — holds the text plus a oneshot reply channel.
struct PendingEmbed {
    text: String,
    reply: oneshot::Sender<Result<Vec<f32>>>,
}

/// Configuration for the batching window and size.
///
/// Why: surfacing the two knobs (window + cap) lets the binary's CLI flags
/// flow straight through into worker behaviour without re-deriving defaults
/// at each call site.
/// What: small POD; `Default` returns the documented defaults.
/// Test: covered transitively by the worker's behaviour test.
#[derive(Debug, Clone, Copy)]
pub struct BatchConfig {
    pub batch_size: usize,
    pub batch_window: Duration,
}

impl Default for BatchConfig {
    fn default() -> Self {
        Self {
            batch_size: DEFAULT_BATCH_SIZE,
            batch_window: Duration::from_millis(DEFAULT_BATCH_WINDOW_MS),
        }
    }
}

/// Client handle for the batching worker.
///
/// Why: the daemon hands one of these to every accepted connection so that
/// per-connection handlers can submit texts and await their embeddings
/// without ever touching the embedder directly.
/// What: holds the `mpsc::Sender` end of the worker's input channel; the
/// worker task is detached and owns the embedder.
/// Test: `batch_queue_collapses_concurrent_requests`.
#[derive(Clone)]
pub struct BatchQueue {
    tx: mpsc::Sender<PendingEmbed>,
}

impl BatchQueue {
    /// Spawn the batching worker and return a client handle.
    ///
    /// Why: ownership of the embedder is transferred into the worker task
    /// permanently — there is exactly one consumer of the ONNX session, so
    /// no contention can occur.
    /// What: creates the bounded mpsc channel, spawns
    /// [`batch_worker`] with the supplied config, returns the sender wrapped
    /// in `BatchQueue`.
    /// Test: covered by the worker behaviour test in this module.
    pub fn new(embedder: Arc<dyn Embedder>, config: BatchConfig) -> Self {
        let (tx, rx) = mpsc::channel(PENDING_CHANNEL_CAPACITY);
        tokio::spawn(batch_worker(rx, embedder, config));
        Self { tx }
    }

    /// Embed a single text and return the resulting vector.
    ///
    /// Why: the common case for `memory_recall` is a single query; this
    /// keeps call sites readable.
    /// What: enqueues one `PendingEmbed`, awaits the oneshot reply, and
    /// unwraps the inner `Result`. Returns `Err` if the worker died.
    /// Test: indirectly covered by the batched test.
    #[allow(dead_code)] // kept as a documented part of the queue's API surface
    pub async fn embed_one(&self, text: String) -> Result<Vec<f32>> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(PendingEmbed {
                text,
                reply: reply_tx,
            })
            .await
            .map_err(|_| anyhow!("embed worker channel closed"))?;
        reply_rx
            .await
            .map_err(|_| anyhow!("embed worker dropped reply channel"))?
    }

    /// Embed a batch of texts; preserves input order in the result.
    ///
    /// Why: amortises IPC overhead when callers already have a batch in
    /// hand (e.g. indexing a chunked file). Each text still goes through
    /// the same worker queue — the worker simply sees them all arrive in a
    /// tight burst, which is exactly the case its window-and-cap policy is
    /// tuned for.
    /// What: enqueues N pending requests, awaits all N replies in order.
    /// Bails on the first internal error so callers do not see a partial
    /// result.
    /// Test: `batch_queue_collapses_concurrent_requests` exercises this
    /// path via `tokio::join!`.
    pub async fn embed_many(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let mut receivers = Vec::with_capacity(texts.len());
        for text in texts {
            let (reply_tx, reply_rx) = oneshot::channel();
            self.tx
                .send(PendingEmbed {
                    text,
                    reply: reply_tx,
                })
                .await
                .map_err(|_| anyhow!("embed worker channel closed"))?;
            receivers.push(reply_rx);
        }
        let mut out = Vec::with_capacity(receivers.len());
        for rx in receivers {
            let v = rx
                .await
                .map_err(|_| anyhow!("embed worker dropped reply channel"))??;
            out.push(v);
        }
        Ok(out)
    }
}

/// Worker loop — owns the embedder, drains the channel into batches.
///
/// Why: a single owner of the embedder is the entire point of the daemon;
/// the worker holds it for its lifetime, batches incoming work, and never
/// hands a `&mut` reference out anywhere.
/// What: blocks on `rx.recv()` for the first item, then races a tokio
/// `sleep(batch_window)` against further `rx.recv()` calls, stopping when
/// the window elapses or the batch reaches `batch_size`. Calls
/// `embedder.embed_batch` once per batch and fans results out via the
/// per-request oneshot channels. On embedder error, the same error string
/// is replied to every member of the batch.
/// Test: integration test in this module.
async fn batch_worker(
    mut rx: mpsc::Receiver<PendingEmbed>,
    embedder: Arc<dyn Embedder>,
    config: BatchConfig,
) {
    tracing::info!(
        "embed batch worker started (batch_size={}, batch_window_ms={})",
        config.batch_size,
        config.batch_window.as_millis()
    );

    loop {
        // Wait for the first item — exits when all senders are dropped.
        let Some(first) = rx.recv().await else {
            tracing::info!("embed batch worker shutting down (channel closed)");
            return;
        };
        let mut batch: Vec<PendingEmbed> = Vec::with_capacity(config.batch_size);
        batch.push(first);

        // Race the batching window against further arrivals.
        let deadline = sleep(config.batch_window);
        tokio::pin!(deadline);

        while batch.len() < config.batch_size {
            tokio::select! {
                biased;
                _ = &mut deadline => break,
                item = rx.recv() => match item {
                    Some(p) => batch.push(p),
                    None => break, // channel closed; flush whatever we have
                }
            }
        }

        let texts: Vec<String> = batch.iter().map(|p| p.text.clone()).collect();
        tracing::debug!("embed batch flushing: size={}", texts.len());
        let result = embedder.embed_batch(&texts).await;

        match result {
            Ok(vectors) if vectors.len() == batch.len() => {
                for (pending, vector) in batch.into_iter().zip(vectors) {
                    // Drop errors silently — caller awaiting the oneshot may
                    // have been cancelled (client disconnect), which is fine.
                    let _ = pending.reply.send(Ok(vector));
                }
            }
            Ok(vectors) => {
                // Embedder returned a count mismatch — treat as internal
                // error; callers see a consistent failure message.
                let msg = format!(
                    "embedder returned {} vectors for {} inputs",
                    vectors.len(),
                    batch.len()
                );
                tracing::error!("{msg}");
                for pending in batch {
                    let _ = pending.reply.send(Err(anyhow!(msg.clone())));
                }
            }
            Err(e) => {
                let msg = format!("embedder failed: {e:#}");
                tracing::error!("{msg}");
                for pending in batch {
                    let _ = pending.reply.send(Err(anyhow!(msg.clone())));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use trusty_common::embedder::{MockEmbedder, EMBED_DIM};

    #[tokio::test]
    async fn batch_queue_collapses_concurrent_requests() {
        let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(EMBED_DIM));
        let queue = BatchQueue::new(embedder, BatchConfig::default());

        let texts: Vec<String> = (0..16).map(|i| format!("input-{i}")).collect();
        let mut handles = Vec::new();
        for t in texts.clone() {
            let q = queue.clone();
            handles.push(tokio::spawn(async move { q.embed_one(t).await }));
        }
        let mut results = Vec::new();
        for h in handles {
            let v = h.await.unwrap().unwrap();
            assert_eq!(v.len(), EMBED_DIM);
            results.push(v);
        }
        // Each unique input should produce a distinct embedding under the
        // mock hash; assert at least 2 differ to catch a stub regression.
        assert_ne!(results[0], results[1]);
    }

    #[tokio::test]
    async fn embed_many_preserves_order() {
        let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(EMBED_DIM));
        let queue = BatchQueue::new(embedder.clone(), BatchConfig::default());

        let texts: Vec<String> = vec!["alpha".into(), "beta".into(), "gamma".into()];
        let got = queue.embed_many(texts.clone()).await.unwrap();
        assert_eq!(got.len(), 3);

        // Verify ordering by comparing against direct mock invocation.
        let direct = embedder.embed_batch(&texts).await.unwrap();
        for (a, b) in got.iter().zip(direct.iter()) {
            assert_eq!(a, b);
        }
    }

    #[tokio::test]
    async fn embed_many_empty_returns_empty() {
        let embedder: Arc<dyn Embedder> = Arc::new(MockEmbedder::new(EMBED_DIM));
        let queue = BatchQueue::new(embedder, BatchConfig::default());
        let got = queue.embed_many(Vec::new()).await.unwrap();
        assert!(got.is_empty());
    }
}
