//! ONNX embedding helpers for the ingestion pipeline.
//!
//! Why: the CoreML/CUDA batching logic, RSS-tripwire, and in-flight pipelining
//! are dense enough that they deserve their own module rather than living inline
//! in the main ingest flow.
//! What: `embed_chunks_in_batches` (multi-flight pipelined embed) and the two
//! supporting helpers `resolve_embed_inflight` / `current_rss_mb`.
//! Test: `test_index_files_batch_*` and `tripwire_tests::test_current_rss_mb_is_plausible`.

use std::sync::Arc;

use anyhow::{Context, Result};

use crate::core::chunker::RawChunk;

use super::super::{embed_batch_size, CodeIndexer};
use super::PROGRESS_CHUNK_INTERVAL;

/// `(chunk_start_index, expected_count, embed_result)` — alias to satisfy clippy.
type WaveResult = (usize, usize, Result<Vec<Vec<f32>>>);

/// Concurrent in-flight sub-batches (issue #753).
///
/// Why: a single serial `embed_batch` loop left ANE ~78% idle. Overlapping
/// `TRUSTY_EMBED_INFLIGHT` sub-batches via ordered `buffered` fills the ANE
/// dispatch queue.
/// What: reads `TRUSTY_EMBED_INFLIGHT`, clamps to [1, 4], defaults to 2.
/// Test: `embed_chunks_in_batches` (indirect).
pub(super) fn resolve_embed_inflight() -> usize {
    static CACHED: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *CACHED.get_or_init(|| {
        std::env::var("TRUSTY_EMBED_INFLIGHT")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .map(|n| n.clamp(1, 4))
            .unwrap_or(2)
    })
}

/// Resident-set-size of the current process, in megabytes.
///
/// Why: used by the CoreML memory tripwire to measure the RSS delta a single
/// `embed_batch` call produced. CoreML on Apple Silicon can spike RSS by tens
/// of GB within one call (the per-batch buffer is not released until the call
/// returns), so the inter-batch RSS poller fires too late to prevent the
/// spike — the tripwire instead measures the damage after the fact and halves
/// the batch size for subsequent calls.
/// What: reads resident set size for `std::process::id()`. On macOS, shells
/// out to `ps -o rss= -p <pid>` (KiB). On Linux, parses `VmRSS` from
/// `/proc/self/status` (KiB). Returns 0 on any error — the tripwire then
/// degrades gracefully (a 0 reading just means the tripwire never fires).
/// Test: `tripwire_tests::test_current_rss_mb_is_plausible` asserts the value
/// is in a plausible range.
pub(super) fn current_rss_mb() -> usize {
    #[cfg(target_os = "macos")]
    {
        use std::process::Command;
        let pid = std::process::id();
        Command::new("ps")
            .args(["-o", "rss=", "-p", &pid.to_string()])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .and_then(|s| s.trim().parse::<usize>().ok())
            .map(|kb| kb / 1024)
            .unwrap_or(0)
    }
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string("/proc/self/status")
            .ok()
            .and_then(|text| {
                text.lines()
                    .find_map(|line| line.strip_prefix("VmRSS:"))
                    .and_then(|rest| rest.split_whitespace().next().map(str::to_string))
            })
            .and_then(|kb| kb.parse::<usize>().ok())
            .map(|kb| kb / 1024)
            .unwrap_or(0)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        0
    }
}

impl CodeIndexer {
    /// Batched ONNX embed — multi-flight pipelined (issue #753).
    ///
    /// Why: serial loop left ANE ~78% idle; `TRUSTY_EMBED_INFLIGHT` (default 2)
    /// sub-batches now run concurrently via ordered `buffered`, filling the ANE
    /// queue. CoreML tripwire fires between waves.
    /// What: returns `Vec<Option<Vec<f32>>>` 1:1 with `chunks` (None = BM25-only
    /// mode). `progress_tx`: when `Some`, a `(chunks_in_wave, wave_embed_ms)` pair
    /// is sent after each wave of ≥ `PROGRESS_CHUNK_INTERVAL` chunks so callers
    /// can emit fine-grained progress events without polling.
    /// Test: `test_index_files_batch_*`. Order: `tests/multiflight.rs`.
    pub(crate) async fn embed_chunks_in_batches(
        &self,
        chunks: &[RawChunk],
        progress_tx: Option<&tokio::sync::mpsc::UnboundedSender<(usize, u64)>>,
    ) -> Result<Vec<Option<Vec<f32>>>> {
        use futures::StreamExt as _;

        let mut embeddings: Vec<Option<Vec<f32>>> = vec![None; chunks.len()];
        let (Some(embedder), Some(_store)) = (&self.embedder, &self.store) else {
            return Ok(embeddings);
        };
        let chunk_total = chunks.len();
        // CoreML pre-allocates ANE buffers; oversized batches stack until jetsam
        // kills the daemon.
        let is_coreml = matches!(
            embedder.provider(),
            trusty_common::embedder::ExecutionProvider::CoreML
                | trusty_common::embedder::ExecutionProvider::CoreMLAne
        );
        let mut batch_size = if is_coreml {
            let bs = crate::core::resolve_coreml_batch_size();
            tracing::debug!(
                "embed_chunks_in_batches: CoreML ({:?}) — TRUSTY_COREML_BATCH_SIZE={bs}",
                embedder.provider()
            );
            bs
        } else {
            embed_batch_size()
        };

        let tripwire_mb = if is_coreml {
            crate::core::resolve_coreml_tripwire_mb()
        } else {
            0
        };
        let mut tripwire_fired = false;
        let inflight = resolve_embed_inflight();
        tracing::debug!(chunk_total, batch_size, inflight, "embed_chunks_in_batches");
        let mut batch_start = 0usize;
        while batch_start < chunk_total {
            let wave_start = batch_start;
            let mut wave_sub_batches: Vec<(usize, Vec<String>)> = Vec::with_capacity(inflight);
            let mut wave_pos = batch_start;
            while wave_sub_batches.len() < inflight && wave_pos < chunk_total {
                let end = (wave_pos + batch_size).min(chunk_total);
                let sub: Vec<String> = chunks[wave_pos..end]
                    .iter()
                    .map(|c| c.content.clone())
                    .collect();
                wave_sub_batches.push((wave_pos, sub));
                wave_pos = end;
            }

            let rss_before = if is_coreml { current_rss_mb() } else { 0 };
            let wave_embed_start = std::time::Instant::now();
            // Dispatch concurrently — `buffered` preserves order.
            let wave_results: Vec<WaveResult> = {
                let iter = wave_sub_batches.into_iter().map(|(start_pos, texts)| {
                    let emb = Arc::clone(embedder);
                    let n = texts.len();
                    async move {
                        let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
                        (start_pos, n, emb.embed_batch(&refs).await)
                    }
                });
                futures::stream::iter(iter)
                    .buffered(inflight)
                    .collect()
                    .await
            };

            for (start_pos, expected_n, vecs) in wave_results {
                let batch_vecs = vecs.context("batch embed_batch failed")?;
                if batch_vecs.len() != expected_n {
                    anyhow::bail!(
                        "embed_batch returned {} vectors, expected {}",
                        batch_vecs.len(),
                        expected_n
                    );
                }
                for (offset, vec) in batch_vecs.into_iter().enumerate() {
                    embeddings[start_pos + offset] = Some(vec);
                }
            }

            // Fine-grained progress notification: fire once per wave when
            // ≥ PROGRESS_CHUNK_INTERVAL chunks were embedded.
            let chunks_in_wave = wave_pos - wave_start;
            if let Some(tx) = progress_tx {
                if chunks_in_wave >= PROGRESS_CHUNK_INTERVAL {
                    let wave_ms = wave_embed_start.elapsed().as_millis() as u64;
                    let _ = tx.send((chunks_in_wave, wave_ms));
                }
            }

            // CoreML RSS tripwire: halve batch_size if spike detected.
            if is_coreml && !tripwire_fired && rss_before > 0 {
                let rss_after = current_rss_mb();
                let delta_mb = rss_after.saturating_sub(rss_before);
                if delta_mb > tripwire_mb {
                    let new_size =
                        (batch_size / 2).max(crate::core::memory_policy::COREML_BATCH_SIZE_MIN);
                    tracing::warn!(
                        "embed_chunks_in_batches: CoreML RSS delta {}MB exceeds tripwire \
                         {}MB after wave of {} chunks (inflight={}) — halving batch size \
                         {} → {} for remaining waves (non-fatal, reindex continues)",
                        delta_mb,
                        tripwire_mb,
                        wave_pos - wave_start,
                        inflight,
                        batch_size,
                        new_size,
                    );
                    batch_size = new_size;
                    tripwire_fired = true;
                }
            }

            batch_start = wave_pos;
        }
        Ok(embeddings)
    }
}

#[cfg(test)]
mod tripwire_tests {
    use super::current_rss_mb;

    /// `current_rss_mb` must never panic and must return a plausible value.
    ///
    /// Why: the CoreML tripwire depends on this probe. It is intentionally
    /// non-fatal — a failed probe returns 0 and the tripwire simply never
    /// fires — so the only hard guarantee is "no panic, plausible range".
    /// What: on macOS/Linux the live test process has a non-zero RSS, so the
    /// reading should be > 0 and below an implausible 1 TB ceiling. On other
    /// platforms the function returns 0 by design.
    /// Test: this test.
    #[test]
    fn test_current_rss_mb_is_plausible() {
        let rss = current_rss_mb();
        // Sanity ceiling: no test host has a 1 TB resident set.
        assert!(rss < 1024 * 1024, "current_rss_mb implausibly large: {rss}");
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            // The running test process must occupy some resident memory.
            assert!(rss > 0, "current_rss_mb should be > 0 on macOS/Linux");
        }
    }
}
