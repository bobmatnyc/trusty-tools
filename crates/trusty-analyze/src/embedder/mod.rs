//! Embedding backend for trusty-analyze concept clustering.
//!
//! Why: clustering needs a vector per chunk. Until #5067 there were two
//! backends — hashed bag-of-words and a fastembed/ONNX neural model — and the
//! neural one was constructed at every daemon boot even though nothing
//! selected it. That construction made an untimed Hugging Face request and
//! held up the whole daemon for as long as the request took (31m46s measured
//! in production). It is gone; BOW is the sole backend.
//!
//! What: the `Embedder` trait plus its one implementation, `BowEmbedder`
//! (wraps `crate::core::bow_embedding`). `EmbedderKind` is the wire label
//! carried on `/indexes/{id}/clusters` responses. The trait is kept as the
//! seam a future `trusty-embedderd` client would plug into — that daemon
//! already exists and already solves the warm-model problem, which is why the
//! in-process model load was not worth keeping.
//!
//! Test: `bow_embedder_produces_normalized_256d_vectors`,
//! `embedder_kind_has_only_bow`, and `analyze_declares_no_in_process_model_deps`.

pub mod bow;

pub use bow::BowEmbedder;

/// Which embedding backend produced a set of vectors.
///
/// Why: the `/clusters` response reports the embedder that actually ran, so a
/// caller never has to guess. Since #5067 there is exactly one, but the label
/// stays on the wire so the response shape is unchanged for existing clients.
/// What: a single-variant enum. Deserializing a `method` query parameter that
/// is not `bow` — notably the removed `neural` — fails, so a caller asking for
/// a backend that no longer exists gets an explicit 400 rather than hashed
/// vectors silently labelled as semantic ones.
/// Test: `embedder_kind_has_only_bow`, `rpc_clusters_reject_removed_neural_method`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EmbedderKind {
    /// Bag-of-words hashed embedding. Deterministic, fast, no model required.
    #[default]
    Bow,
}

impl EmbedderKind {
    /// Short label suitable for API responses (`"bow"`).
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Bow => "bow",
        }
    }
}

/// Common interface for all embedding backends.
///
/// Why: `/clusters` should not care which backend produced its vectors. Keeping
/// the trait after #5067 left one implementation behind, deliberately: it is
/// the insertion point for routing to `trusty-embedderd` over its UDS client
/// (the way `trusty-search` already does) if semantic clustering is ever wanted
/// back, without reintroducing an in-process model load on the boot path.
/// What: embed a batch of texts into a `Vec<Vec<f32>>` of consistent
/// dimension; expose `kind()` for response metadata and `dim()` for sanity
/// checks.
/// Test: see `bow.rs`.
pub trait Embedder: Send + Sync {
    /// Which backend this is — used in API responses for transparency.
    fn kind(&self) -> EmbedderKind;
    /// Embed a batch of texts. Returns one vector per input, all same dimension.
    fn embed_batch(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>>;
    /// Embedding dimension produced by this backend.
    fn dim(&self) -> usize;
}

#[cfg(test)]
mod tests;
