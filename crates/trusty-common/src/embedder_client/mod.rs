//! Unified RPC client surface for the `trusty-embedderd` standalone process.
//!
//! Why: the `FastEmbedder` in-process path serialises all embedding through
//! one ONNX session, which can be a bottleneck under concurrent load. Extracting
//! embedding into a dedicated process (`trusty-embedderd`) lets the two crash
//! independently and keeps the large ONNX RSS footprint off the search daemon's
//! budget (issue #110 motivation). This module provides the single `EmbedderClient`
//! trait that abstracts over all four deployment modes:
//!
//! 1. **InProcess** — wraps `FastEmbedder` directly (zero config, backward compat)
//! 2. **HTTP remote** — `POST /embed` JSON to a running `trusty-embedderd` over TCP
//! 3. **UDS remote** — newline-framed JSON-RPC 2.0 to `trusty-embedderd` over a
//!    Unix Domain Socket (issue #164; lower latency than HTTP on local hosts)
//! 4. **Stdio sidecar** — newline-framed JSON-RPC 2.0 over piped stdin/stdout of a
//!    child `trusty-embedderd --stdio` process (issue #110 Phase 2 default).
//!    Lifecycle is managed by `EmbedderSupervisor`.
//!
//! What: exposes (1) JSON-over-HTTP and JSON-over-UDS wire types, (2) the
//! `EmbedderClient` trait, (3) `InProcessEmbedderClient` for backward
//! compatibility, (4) `RemoteEmbedderClient` (HTTP), (5) `UdsEmbedderClient`
//! (UDS), (6) `StdioEmbedderClient` (stdio sidecar), and (7)
//! `EmbedderSupervisor` (auto-spawn lifecycle manager). The `embed_client`
//! module (UDS-only, PR #157) is retired by issue #164 — use `UdsEmbedderClient`
//! from this module instead.
//!
//! Test: `cargo test -p trusty-common --features embedder-client` covers the
//! error type, wire-type round-trips, and client construction. ONNX-backed tests
//! are in `trusty-embedderd/tests/bit_identical.rs` (marked `#[ignore]`).

pub mod error;
pub mod in_process;
pub mod remote;
pub mod stdio;
pub mod supervisor;
pub mod types;
pub mod uds;

pub use error::EmbedderError;
pub use in_process::InProcessEmbedderClient;
pub use remote::RemoteEmbedderClient;
pub use stdio::StdioEmbedderClient;
pub use supervisor::{EmbedderSupervisor, SupervisorConfig, locate_embedderd_binary};
pub use types::{EmbedRequest, EmbedResponse};
pub use uds::UdsEmbedderClient;

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
