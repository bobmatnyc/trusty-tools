//! Chunked embed primitive shared by every drawer-vector writer (#9275).
//!
//! Why: a drawer longer than the embedder's input window lost everything past
//! the window from its vector. Every writer must split the same way, and the
//! health check must expect exactly what the writers produce.
//! What: [`ChunkPlanner`] holds the window and tokenizer one embedder chunks
//! with; [`plan_chunks`] splits a drawer's text; [`embed_chunks`] embeds the
//! chunks in one batch; [`expected_chunk_count`] is the row count a complete
//! drawer has.
//! Test: `embed_store_tests.rs`.

use trusty_common::embedder::{ChunkTokenizer, TextChunk, chunk_text};
use crate::engine::embed::Embedder;
use crate::engine::store::vector_key::MAX_CHUNKS_PER_DRAWER;
use crate::engine::timeouts;
use anyhow::{Context, Result};
use std::ops::Range;
use std::sync::Arc;
use std::time::Duration;

/// How one embedder splits drawer text: its window and its tokenizer.
///
/// Why: the health check splits every long drawer of a palace; resolving the
/// tokenizer once per check, not once per drawer, keeps that cheap.
/// What: built by [`ChunkPlanner::for_embedder`]; `None` there means the
/// embedder does not chunk (no window, or no tokenizer to count with).
pub(crate) struct ChunkPlanner {
    window: usize,
    tokenizer: Arc<ChunkTokenizer>,
}

impl ChunkPlanner {
    /// The planner for `embedder`, or `None` when it does not chunk.
    ///
    /// Architect ruling Q1: a window with a tokenizer that will not load is an
    /// error, never one silently truncated vector.
    /// Test: `unavailable_tokenizer_with_a_window_is_reported_not_swallowed`,
    /// `embedder_without_a_window_never_chunks`.
    pub(crate) fn for_embedder(embedder: &dyn Embedder) -> Result<Option<Self>> {
        let Some(window) = embedder.input_window() else {
            return Ok(None);
        };
        let tokenizer = embedder
            .chunking_tokenizer()
            .with_context(|| format!("load the tokenizer for the {window}-token embed window"))?;
        Ok(tokenizer.map(|tokenizer| Self { window, tokenizer }))
    }

    /// Whether `text` is one chunk without tokenizing it.
    ///
    /// A BERT-family tokenizer never makes more tokens than the text has bytes,
    /// so a text whose byte length fits the budget fits in tokens too, and
    /// `chunk_text` would return it whole.
    pub(crate) fn fits_one_chunk(&self, text: &str) -> bool {
        text.len() + self.tokenizer.special_overhead() <= self.window
    }

    /// `text` split at this planner's window, in order.
    /// Test: `chunk_count_above_the_cap_is_a_hard_error_before_any_embed`.
    pub(crate) fn split<'a>(&self, text: &'a str) -> Result<Vec<&'a str>> {
        let chunks = self.chunks(text)?;
        // Architect ruling (PR B Q4): a hard error, never truncation.
        // #9735: typed, so the backfill can split the drawer instead.
        if chunks.len() > MAX_CHUNKS_PER_DRAWER {
            return Err(TooManyChunks { n: chunks.len() }.into());
        }
        Ok(chunks.into_iter().map(|c| c.text).collect())
    }

    /// Byte ranges of `text`'s chunks with no chunk cap (#9735).
    /// Test: `every_part_replans_within_the_cap_and_completes`.
    pub(crate) fn chunk_ranges(&self, text: &str) -> Result<Vec<Range<usize>>> {
        Ok(self
            .chunks(text)?
            .into_iter()
            .map(|c| c.byte_range)
            .collect())
    }

    /// Every chunk of `text`, uncapped; one whole chunk when it fits.
    fn chunks<'a>(&self, text: &'a str) -> Result<Vec<TextChunk<'a>>> {
        if self.fits_one_chunk(text) {
            return Ok(vec![TextChunk {
                byte_range: 0..text.len(),
                text,
            }]);
        }
        chunk_text(&self.tokenizer, text, self.window)
            .context("split drawer text at the embed window")
    }
}

/// Drawer text that needs more than `MAX_CHUNKS_PER_DRAWER` chunks.
///
/// Why (#9735): the write paths refuse such text, but the backfill splits a
/// stored drawer instead; it needs to tell this error from a tokenizer fault.
/// What: the message text the untyped error had; downcast it from `anyhow`.
/// Test: `chunk_count_above_the_cap_is_a_hard_error_before_any_embed`,
/// `an_oversized_drawer_does_not_count_as_a_host_failure`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("drawer text needs {n} chunks; at most {MAX_CHUNKS_PER_DRAWER} are stored per drawer")]
pub(crate) struct TooManyChunks {
    /// Chunks the text needs.
    pub(crate) n: usize,
}

/// The chunk texts drawer `text` is embedded as, in order.
///
/// Why: one split for every writer and for the health check.
/// What: the whole text when the embedder does not chunk; otherwise
/// [`ChunkPlanner::split`]. A tokenizer that will not load, a split that
/// fails, or more than `MAX_CHUNKS_PER_DRAWER` chunks is an error.
/// Test: `short_text_is_one_chunk_and_one_embed_input`,
/// `embedder_without_a_window_never_chunks`.
pub(crate) fn plan_chunks<'a>(embedder: &dyn Embedder, text: &'a str) -> Result<Vec<&'a str>> {
    match ChunkPlanner::for_embedder(embedder)? {
        Some(planner) => planner.split(text),
        None => Ok(vec![text]),
    }
}

/// Rows a fully embedded drawer with this text has in the vector index.
/// Test: `expected_chunk_count_equals_rows_the_pipeline_writes`.
pub(crate) fn expected_chunk_count(embedder: &dyn Embedder, text: &str) -> Result<usize> {
    Ok(plan_chunks(embedder, text)?.len())
}

/// Embed drawer `text` as one vector per chunk, in chunk order.
///
/// Why: the one embed every drawer-vector writer calls, so a drawer written
/// by remember, import, rebuild, kuzu import or repair has the same rows.
/// Store the result with `UsearchStore::upsert_chunks`, which replaces the
/// drawer's whole group.
/// What: [`plan_chunks`], then one `embed_batch` over all chunks (the
/// embedder bounds its own inference batches). Callers keep their own timeout.
/// Test: `long_text_embeds_every_chunk_in_one_batch`,
/// `needle_in_the_last_chunk_is_recalled_at_rank_1`.
pub async fn embed_chunks(embedder: &dyn Embedder, text: &str) -> Result<Vec<Vec<f32>>> {
    let chunks: Vec<String> = plan_chunks(embedder, text)?
        .into_iter()
        .map(str::to_string)
        .collect();
    embed_planned(embedder, &chunks).await
}

/// The embed ceiling for `chunks` inputs: `per_chunk` once per chunk.
///
/// Why (#9275 fix round): `TRUSTY_EMBED_BATCH_TIMEOUT_SECS` was sized for one
/// input. Applied once to a whole drawer's chunks, it timed out a long drawer
/// that got one vector before chunking, and it then got none.
/// What: `per_chunk × max(chunks, 1)`, saturating, clamped by
/// [`clamp_chunked_timeout`] at `TRUSTY_EMBED_CHUNKED_CAP_SECS` (#9735 Q3).
/// Test: `a_multi_chunk_drawer_inside_the_scaled_embed_timeout_gets_every_vector`,
/// `a_wedged_chunked_embed_ends_at_the_scaled_bound`,
/// `chunked_embed_timeout_honours_the_cap_env`.
pub(crate) fn chunked_embed_timeout(per_chunk: Duration, chunks: usize) -> Duration {
    clamp_chunked_timeout(per_chunk, chunks, timeouts::chunked_embed_cap())
}

/// `min(per_chunk × chunks, max(per_chunk, cap))`.
///
/// Why (#9735 Q3): the scaled bound needs a ceiling, but an operator who
/// raised `TRUSTY_EMBED_BATCH_TIMEOUT_SECS` above the cap must keep at least
/// one chunk's worth.
/// What: with `per_chunk` above `cap` and more than one chunk, the ceiling is
/// `per_chunk`; otherwise it is the scaled bound, capped at `cap`.
/// Test: `chunked_embed_timeout_is_clamped_at_the_cap`,
/// `chunked_embed_timeout_never_drops_below_the_operators_per_chunk`.
pub(crate) fn clamp_chunked_timeout(per_chunk: Duration, chunks: usize, cap: Duration) -> Duration {
    let scaled = per_chunk.saturating_mul(u32::try_from(chunks.max(1)).unwrap_or(u32::MAX));
    // #9735: the operator's per-chunk value is the floor of the ceiling.
    scaled.min(per_chunk.max(cap))
}

/// [`embed_chunks`] under [`chunked_embed_timeout`]: the one bounded chunked
/// embed the remember path and the dream merge share.
/// Test: `a_multi_chunk_drawer_inside_the_scaled_embed_timeout_gets_every_vector`.
pub(crate) async fn embed_chunks_within(
    embedder: &dyn Embedder,
    text: &str,
    per_chunk: Duration,
) -> Result<Vec<Vec<f32>>> {
    let chunks: Vec<String> = plan_chunks(embedder, text)?
        .into_iter()
        .map(str::to_string)
        .collect();
    embed_planned_within(embedder, &chunks, per_chunk).await
}

/// [`embed_planned`] under [`chunked_embed_timeout`] for `chunks`.
/// Test: `a_wedged_chunked_embed_ends_at_the_scaled_bound`.
pub(crate) async fn embed_planned_within(
    embedder: &dyn Embedder,
    chunks: &[String],
    per_chunk: Duration,
) -> Result<Vec<Vec<f32>>> {
    // #9275: the ceiling scales with the chunk count.
    let bound = chunked_embed_timeout(per_chunk, chunks.len());
    tokio::time::timeout(bound, embed_planned(embedder, chunks))
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "embedding {} chunks timed out after {bound:?} ({per_chunk:?} per chunk); \
                 increase TRUSTY_EMBED_BATCH_TIMEOUT_SECS if batches legitimately take \
                 longer on this host",
                chunks.len()
            )
        })?
}

/// One `embed_batch` over `chunks`; a vector count other than the chunk count
/// is an error, never a partial group.
/// Test: `embed_batch_returning_the_wrong_count_is_an_error_and_writes_nothing`.
pub(crate) async fn embed_planned(
    embedder: &dyn Embedder,
    chunks: &[String],
) -> Result<Vec<Vec<f32>>> {
    let vectors = embedder
        .embed_batch(chunks)
        .await
        .context("embed drawer chunks")?;
    anyhow::ensure!(
        vectors.len() == chunks.len(),
        "embedder returned {} vectors for {} chunks",
        vectors.len(),
        chunks.len()
    );
    Ok(vectors)
}
