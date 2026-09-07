//! Unified RPC client surface for the `trusty-embedderd` standalone process.
//!
//! Why: the `FastEmbedder` in-process path serialises all embedding through
//! one ONNX session, which can be a bottleneck under concurrent load. Extracting
//! embedding into a dedicated process (`trusty-embedderd`) lets the two crash
//! independently and keeps the large ONNX RSS footprint off the search daemon's
//! budget (issue #110 motivation). This module provides the single `EmbedderClient`
//! trait that abstracts over all three deployment modes:
//!
//! 1. **InProcess** — wraps `FastEmbedder` directly (zero config, backward compat)
//! 2. **UDS remote** — newline-framed JSON-RPC 2.0 to `trusty-embedderd` over a
//!    Unix Domain Socket (issue #164)
//! 3. **Stdio sidecar** — newline-framed JSON-RPC 2.0 over piped stdin/stdout of a
//!    child `trusty-embedderd --stdio` process (issue #110 Phase 2 default).
//!    Lifecycle is managed by `EmbedderSupervisor`.
//!
//! #6289: a fourth mode, `RemoteEmbedderClient`, spoke `POST /embed` over TCP to
//! `trusty-embedderd --http`. That listener is retired under ADR-0032 (no
//! trusty-* service owns HTTP), so the client went with it — dial the socket
//! with `UdsEmbedderClient` instead.
//!
//! What: exposes (1) the JSON-over-UDS wire types, (2) the `EmbedderClient`
//! trait, (3) `InProcessEmbedderClient` for backward compatibility, (4)
//! `UdsEmbedderClient` (UDS), (5) `StdioEmbedderClient` (stdio sidecar), and
//! (6) `EmbedderSupervisor` (auto-spawn lifecycle manager). The `embed_client`
//! module (UDS-only, PR #157) is retired by issue #164 — use `UdsEmbedderClient`
//! from this module instead.
//!
//! Test: `cargo test -p trusty-common --features embedder-client` covers the
//! error type, wire-type round-trips, and client construction. ONNX-backed tests
//! are in `trusty-embedderd/tests/bit_identical.rs` (marked `#[ignore]`).

pub mod error;
pub mod in_process;
mod spawn_retry;
pub mod stdio;
pub mod supervisor;
pub mod types;
pub mod uds;

pub use error::EmbedderError;
pub use in_process::InProcessEmbedderClient;
pub use stdio::StdioEmbedderClient;
pub use supervisor::{
    EmbedderSupervisor, SupervisorConfig, SupervisorHandle, cuda_sidecar_batch_cap,
    locate_embedderd_binary, sidecar_batch_size,
};
pub use types::{EmbedRequest, EmbedResponse};
pub use uds::UdsEmbedderClient;

use async_trait::async_trait;

/// Trait abstracting embedding back-ends.
///
/// Why: allows trusty-search (and other callers) to be written against a
/// single interface and switch between the in-process FastEmbedder and the
/// remote `trusty-embedderd` process without touching call sites — just
/// swap the concrete type behind the `Arc<dyn EmbedderClient>`.
///
/// What: a single async primitive `embed_batch` that accepts a `Vec<String>`
/// and returns a `Vec<Vec<f32>>` of the same length, with one 384-dimensional
/// unit vector per input text.
///
/// Test: `InProcessEmbedderClient` and `UdsEmbedderClient` both satisfy
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

    /// The backend device/provider actually reported by the live process over
    /// the wire on its most recent successful response, if the transport
    /// carries that information (epic #3524 slice 5, issue #3493 P1).
    ///
    /// Why: most transports (HTTP, UDS, in-process) have no such readback and
    /// the predicted `ExecutionProvider` remains the best available answer.
    /// `StdioEmbedderClient` overrides this — the Python/MPS sidecar echoes
    /// its actual `torch` device (`"mps"` / `"cuda"` / `"cpu"`) in every
    /// response frame.
    /// What: default `None`.
    /// Test: `StdioEmbedderClient`'s override is covered by
    /// `reader_task_captures_wire_device` in `stdio_tests.rs`.
    fn last_reported_device(&self) -> Option<String> {
        None
    }
}
