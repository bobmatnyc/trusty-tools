//! Query-side embed failure handling: degrade to lexical, or refuse a pinned
//! semantic query with a typed error (#8348).
//!
//! Why: a query embed that failed — the classic case is an embedder sidecar
//! that cannot spawn — used to propagate out of `search_with_outcome` as a bare
//! `anyhow::Error`, which the HTTP layer answered with `500 internal search
//! error`. The corpus and BM25 were intact the whole time, so a hybrid query
//! had a correct lexical answer it never gave.
//! What: [`CodeIndexer::embed_query_or_degrade`] turns an embed failure into
//! "no query vector, plus the failure text" for an unpinned query, and into
//! [`EmbedderUnavailable`] for a query that pinned the semantic lane — lexical
//! rows would answer a different question than that caller asked (#5068).
//! Test: `a_failed_query_embed_degrades_to_lexical_hits_flagged_vector_unavailable`,
//! `a_pinned_semantic_query_with_a_failed_embed_is_503_not_500`.

use crate::core::indexer::CodeIndexer;

/// The query embed failed for a query that pinned the semantic lane (#8348).
///
/// Why: the search handler must answer `503 vector_unavailable`, which it can
/// only do if it can recognise this failure among every other `anyhow::Error`
/// the pipeline raises.
/// What: carries the index and the embedder's own error text.
/// Test: `a_pinned_semantic_query_with_a_failed_embed_is_503_not_500`.
#[derive(Debug, thiserror::Error)]
#[error(
    "index '{index_id}': the embedder could not embed the query ({detail}) — a pinned \
     semantic search cannot be answered from the lexical lane (#8348, #5068)"
)]
pub struct EmbedderUnavailable {
    /// The index the query was sent to.
    pub index_id: String,
    /// The embedder's error, rendered with its full context chain.
    pub detail: String,
}

/// A query vector the caller already computed, or the reason it could not
/// (#9027).
///
/// Why: the all-index fan-out embeds once and hands the result to every
/// per-index search. A failed embed travels too, so each index degrades to
/// lexical at once rather than retrying the same failing embedder per index.
/// Test: `global_search_embeds_the_query_once_for_every_index`,
/// `a_precomputed_embed_failure_degrades_without_re_embedding`.
#[derive(Debug, Clone, Copy)]
pub enum PrecomputedQueryVector<'a> {
    /// The embedded query text.
    Embedded(&'a [f32]),
    /// The embedder's error, rendered with its full context chain.
    Failed(&'a str),
}

impl CodeIndexer {
    /// Resolve a [`PrecomputedQueryVector`] exactly as
    /// [`Self::embed_query_or_degrade`] resolves a fresh embed.
    ///
    /// Why: one rule for a failed embed, whoever ran it (#8348, #9027).
    /// What: `Embedded` is the vector; `Failed` degrades an unpinned query to
    /// lexical and refuses a pinned semantic one with [`EmbedderUnavailable`].
    /// Test: `a_precomputed_embed_failure_degrades_without_re_embedding`.
    pub(crate) fn precomputed_or_degrade(
        &self,
        pre: PrecomputedQueryVector<'_>,
        pinned_semantic: bool,
    ) -> anyhow::Result<(Option<Vec<f32>>, Option<String>)> {
        match pre {
            PrecomputedQueryVector::Embedded(v) => Ok((Some(v.to_vec()), None)),
            PrecomputedQueryVector::Failed(detail) if pinned_semantic => {
                Err(anyhow::Error::new(EmbedderUnavailable {
                    index_id: self.index_id.clone(),
                    detail: detail.to_string(),
                }))
            }
            PrecomputedQueryVector::Failed(detail) => Ok((None, Some(detail.to_string()))),
        }
    }

    /// Embed `text` for a query, degrading an embed failure instead of raising
    /// it unless the caller pinned the semantic lane (#8348).
    ///
    /// Why: see the module doc.
    /// What: `Ok((vector, None))` on success (`vector` is `None` when no
    /// embedder is wired). On failure with `pinned_semantic == false` returns
    /// `Ok((None, Some(reason)))` and logs a warning; with `pinned_semantic ==
    /// true` returns `Err(EmbedderUnavailable)`.
    /// Test: `a_failed_query_embed_degrades_to_lexical_hits_flagged_vector_unavailable`.
    pub(crate) async fn embed_query_or_degrade(
        &self,
        text: &str,
        pinned_semantic: bool,
    ) -> anyhow::Result<(Option<Vec<f32>>, Option<String>)> {
        match self.embed_query(text).await {
            Ok(vector) => Ok((vector, None)),
            Err(e) => {
                let detail = format!("{e:#}");
                if pinned_semantic {
                    return Err(anyhow::Error::new(EmbedderUnavailable {
                        index_id: self.index_id.clone(),
                        detail,
                    }));
                }
                tracing::warn!(
                    index_id = %self.index_id,
                    error = %detail,
                    "search: query embed failed — answering from the lexical lane (#8348)"
                );
                Ok((None, Some(detail)))
            }
        }
    }
}
