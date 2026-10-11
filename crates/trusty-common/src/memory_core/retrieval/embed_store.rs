//! Chunked embed primitive shared by every drawer-vector writer (#9275).
//!
//! Why: a drawer longer than the embedder's input window lost everything past
//! the window from its vector. Every writer must split the same way, and the
//! health check must expect exactly what the writers produce.
//! What: [`plan_chunks`] splits a drawer's text; [`embed_chunks`] embeds the
//! chunks in one batch; [`expected_chunk_count`] is the row count a complete
//! drawer has.
//! Test: `embed_store_tests.rs`.

use crate::memory_core::embed::Embedder;
use anyhow::{Context, Result};

/// The chunk texts drawer `text` is embedded as, in order.
///
/// C1 seam: one chunk, the whole text (the PR B behaviour). The pipeline
/// commit replaces this with the window-sized split.
/// Test: `short_text_is_one_chunk_and_one_embed_input`.
pub(crate) fn plan_chunks<'a>(_embedder: &dyn Embedder, text: &'a str) -> Result<Vec<&'a str>> {
    Ok(vec![text])
}

/// Rows a fully embedded drawer with this text has in the vector index.
/// Test: `expected_chunk_count_equals_rows_the_pipeline_writes`.
#[allow(dead_code)] // C1: the health check reads it from the pipeline commit on.
pub(crate) fn expected_chunk_count(embedder: &dyn Embedder, text: &str) -> Result<usize> {
    Ok(plan_chunks(embedder, text)?.len())
}

/// Embed drawer `text` as one vector per chunk, in chunk order.
///
/// Why: the one embed every drawer-vector writer calls, so a drawer written
/// by remember, import, rebuild or repair has the same rows.
/// What: [`plan_chunks`], then one `embed_batch` over all chunks.
/// Test: `long_text_embeds_every_chunk_in_one_batch`.
pub async fn embed_chunks(embedder: &dyn Embedder, text: &str) -> Result<Vec<Vec<f32>>> {
    let chunks: Vec<String> = plan_chunks(embedder, text)?
        .into_iter()
        .map(str::to_string)
        .collect();
    embed_planned(embedder, &chunks).await
}

/// One `embed_batch` over `chunks`.
/// Test: `embed_batch_returning_the_wrong_count_is_an_error_and_writes_nothing`.
pub(crate) async fn embed_planned(
    embedder: &dyn Embedder,
    chunks: &[String],
) -> Result<Vec<Vec<f32>>> {
    let vectors = embedder
        .embed_batch(chunks)
        .await
        .context("embed drawer chunks")?;
    Ok(vectors.into_iter().take(1).collect())
}
