//! RPC types and client implementations for the trusty-embedderd standalone
//! process (issue #110, Phase 1).
//!
//! Why: The existing `FastEmbedder` runs ONNX inside the trusty-search
//! process, which means a jetsam/OOM kill of the search daemon also kills
//! the model state. Extracting the embedder into a dedicated process
//! (`trusty-embedderd`) lets the two crash independently, lets the model stay
//! resident across search-daemon restarts, and keeps the large ONNX RSS
//! footprint off the search daemon's budget (issue #110 motivation).
//!
//! What: exposes (1) JSON-over-HTTP wire types, (2) the `EmbedderClient`
//! trait, (3) `InProcessEmbedderClient` that wraps the existing `FastEmbedder`
//! for backward compatibility, and (4) `RemoteEmbedderClient` that delegates
//! to a running `trusty-embedderd` instance over HTTP.
//!
//! Test: `cargo test -p trusty-embedder-client` covers the error type and
//! `InProcessEmbedderClient` compilation. ONNX-backed tests are in
//! `trusty-embedderd/tests/bit_identical.rs` (marked `#[ignore]`).

pub mod error;
pub mod in_process;
pub mod remote;
pub mod types;

pub use error::EmbedderError;
pub use in_process::InProcessEmbedderClient;
pub use remote::RemoteEmbedderClient;
pub use types::{EmbedRequest, EmbedResponse};

use async_trait::async_trait;

/// Trait abstracting embedding back-ends.
///
/// Why: allows trusty-search (and other callers) to be written against a
/// single interface and switch between the in-process FastEmbedder and the
/// remote `trusty-embedderd` HTTP server without touching call sites — just
/// swap the concrete type behind the `Arc<dyn EmbedderClient>`.
///
/// What: a single async primitive `embed_batch` that accepts a `Vec<String>`
/// and returns a `Vec<Vec<f32>>` of the same length, with one 384-dimensional
/// unit vector per input text.
///
/// Test: `InProcessEmbedderClient` and `RemoteEmbedderClient` both satisfy
/// this trait. Compile-time checking via `dyn EmbedderClient` in the
/// integration test `bit_identical.rs`.
#[async_trait]
pub trait EmbedderClient: Send + Sync {
    /// Embed a batch of texts.
    ///
    /// Why: batch API amortises per-call overhead; callers should group
    /// texts before calling rather than issuing one call per text.
    ///
    /// What: returns one `Vec<f32>` per input, each of length 384 (all-MiniLML6V2Q).
    /// An empty input returns an empty Vec without contacting the backend.
    ///
    /// Test: `cargo test -p trusty-embedderd --test bit_identical -- --include-ignored`
    async fn embed_batch(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>, EmbedderError>;
}
