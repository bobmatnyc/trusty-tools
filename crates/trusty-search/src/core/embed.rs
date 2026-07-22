//! Embedding abstraction — thin facade over the shared `trusty-embedder` crate.
//!
//! Why: The `Embedder` trait + `FastEmbedder` + `MockEmbedder` previously
//! lived in this crate. They've been moved to the shared `trusty-embedder`
//! crate so trusty-memory and trusty-search ship the same implementation
//! (LRU cache, ORT warmup, deterministic mock). This module keeps the
//! existing in-crate `Embedder` trait shape (`embed(&str)` + `embed_batch(&[&str])`)
//! so the rest of trusty-search compiles unchanged.
//! What: A local `Embedder` trait that mirrors the historic API, plus a
//! blanket-impl adapter that delegates to the shared `trusty_common::embedder::Embedder`.
//! `FastEmbedder` and `MockEmbedder` are re-exports.
//! Test: existing indexer / concept_cluster tests exercise this surface;
//! shared-crate behaviour is covered upstream in `trusty-embedder`.

use anyhow::Result;
use async_trait::async_trait;

pub use trusty_common::embedder::{FastEmbedder, EMBED_DIM};

#[cfg(any(test, feature = "test-support"))]
pub use trusty_common::embedder::MockEmbedder;

/// trusty-search-flavoured embedder trait.
///
/// Why: Historic call sites pass `&str` / `&[&str]` directly. The shared
/// `trusty_common::embedder::Embedder` settled on `&[String]` as its primitive (it
/// owns the LRU cache key, so it needs owned strings anyway). This trait
/// preserves the old surface — every `&str` is cloned into a `String` on
/// the way down, which matches what the old per-call code did internally.
/// What: an async `embed(&str) -> Vec<f32>` and `embed_batch(&[&str]) ->
/// Vec<Vec<f32>>`, plus `dimension()`.
/// Test: covered indirectly via every `CodeIndexer` test that runs against
/// `MockEmbedder`.
#[async_trait]
pub trait Embedder: Send + Sync {
    async fn embed(&self, text: &str) -> Result<Vec<f32>>;
    async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>>;
    fn dimension(&self) -> usize;

    /// Active ONNX execution provider for this embedder.
    ///
    /// Why: forwards the shared-crate `Embedder::provider()` through the
    /// in-crate facade so call sites that hold a `&dyn Embedder` (i.e. the
    /// reindex pipeline) can pick provider-appropriate batch sizes without
    /// reaching past the facade.
    /// What: default returns `ExecutionProvider::Cpu`; the blanket adapter
    /// below forwards to the underlying `trusty_common::embedder::Embedder`.
    /// Test: covered by the public-surface compile check.
    fn provider(&self) -> trusty_common::embedder::ExecutionProvider {
        trusty_common::embedder::ExecutionProvider::Cpu
    }

    /// A human-readable label for the ACTUALLY-resolved backend, read back
    /// from the live embedder rather than predicted from build features/env
    /// (epic #3524 slice 5, issue #3493 P1).
    ///
    /// Why: `provider()` above is a *prediction* (`resolve_expected_provider()`
    /// — a pure function of build features + env) so it can be called before
    /// any child process has spawned. That is correct for the common Rust ort
    /// sidecar path, but it actively lies for the Python/MPS sidecar: the
    /// prediction logic has no notion of "torch selected MPS", so it reports
    /// the ORT-flavoured `CoreML(ANE)` guess while the sidecar's own startup
    /// log says `device=mps`. `/health` should prefer a real readback when one
    /// is available.
    /// What: default `None` (no real readback available — callers fall back to
    /// `provider()`'s prediction). The Python-sidecar adapter overrides this to
    /// return the actual device string reported by the sidecar over the wire
    /// (see `StdioEmbedderClient::last_reported_device`).
    /// Test: `resolved_provider_label_defaults_to_none_and_is_overridable` in
    /// this module's `tests`; the Python-sidecar override is covered by
    /// `lazy_python_adapter_reports_wire_device` in `start/tests.rs`.
    fn resolved_provider_label(&self) -> Option<String> {
        None
    }

    /// Non-blocking readback: has the supervisor backing this embedder
    /// permanently given up respawning its sidecar? (Review finding, PR
    /// #3560 HIGH fix.)
    ///
    /// Why: `FallbackEmbedderAdapter` (`commands/start/embedder_fallback.rs`)
    /// needs to trip its one-way latch on the SAME condition its module doc
    /// claims to mirror — the supervisor's own `max_restarts` give-up
    /// ceiling (`should_give_up` in
    /// `trusty_common::embedder_client::supervisor`) — rather than an
    /// independently-counted, faster-firing proxy at the request layer. The
    /// Python sidecar is multi-flight (the epic's own design): several
    /// concurrent requests can fail against the same stale client during a
    /// SINGLE crash episode, and a request-count threshold could previously
    /// cross that count well before the supervisor itself had exhausted its
    /// restart budget.
    /// What: default `false` — no supervisor to observe (remote HTTP/UDS
    /// adapters, in-process, the Rust ort fallback embedder itself, and test
    /// doubles). The Python-sidecar `LazySlotEmbedderAdapter` overrides this
    /// to forward `LazyEmbedderHandle::supervisor_gave_up()`, which in turn
    /// forwards `SupervisorHandle::has_given_up()`.
    /// Test: `supervisor_gave_up_defaults_to_false_and_is_overridable` in
    /// this module's `tests`; the lazy-adapter override is covered by
    /// `lazy_python_adapter_reports_supervisor_gave_up` in `start/tests.rs`.
    fn supervisor_gave_up(&self) -> bool {
        false
    }
}

/// Adapter: every shared `trusty_common::embedder::Embedder` automatically implements
/// the in-crate trait via owned-string conversion.
#[async_trait]
impl<E> Embedder for E
where
    E: trusty_common::embedder::Embedder,
{
    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        trusty_common::embedder::embed_one(self, text).await
    }

    async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        let owned: Vec<String> = texts.iter().map(|s| (*s).to_owned()).collect();
        <E as trusty_common::embedder::Embedder>::embed_batch(self, &owned).await
    }

    fn dimension(&self) -> usize {
        <E as trusty_common::embedder::Embedder>::dimension(self)
    }

    fn provider(&self) -> trusty_common::embedder::ExecutionProvider {
        <E as trusty_common::embedder::Embedder>::provider(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use trusty_common::embedder::ExecutionProvider;

    /// Verify that the blanket adapter correctly delegates `embed_batch` to the
    /// underlying `trusty_common::embedder::Embedder` implementation.
    ///
    /// Uses `MockEmbedder` (deterministic, no ONNX I/O) with a known dimension
    /// and a batch of N strings, then asserts shape and that each vector is
    /// non-empty (MockEmbedder always produces non-zero content for non-empty
    /// inputs).
    #[tokio::test]
    async fn embed_adapter_delegates_embed_batch_correctly() {
        const DIM: usize = 16;
        let mock = MockEmbedder::new(DIM);

        // Feed 3 distinct strings through the in-crate Embedder facade.
        let texts = ["hello world", "rust async tokio", "code search engine"];
        let result = Embedder::embed_batch(&mock, &texts)
            .await
            .expect("embed_batch should not fail with MockEmbedder");

        // Shape: one vector per input text.
        assert_eq!(
            result.len(),
            texts.len(),
            "embed_batch must return one vector per input"
        );

        // Dimension: every vector has the expected width.
        for (i, vec) in result.iter().enumerate() {
            assert_eq!(
                vec.len(),
                DIM,
                "vector[{i}] has wrong dimension: expected {DIM}, got {}",
                vec.len()
            );
        }

        // Non-trivial content: MockEmbedder hashes input bytes so non-empty
        // strings must produce at least one non-zero component.
        for (i, vec) in result.iter().enumerate() {
            let nonzero = vec.iter().any(|&x| x != 0.0);
            assert!(
                nonzero,
                "vector[{i}] is all zeros — MockEmbedder contract violated"
            );
        }
    }

    /// Verify that `embed_batch` with distinct inputs produces distinct output
    /// vectors — the adapter must not collapse all results to the same value.
    #[tokio::test]
    async fn embed_adapter_produces_distinct_vectors_for_distinct_inputs() {
        const DIM: usize = 32;
        let mock = MockEmbedder::new(DIM);

        let texts = ["alpha", "beta"];
        let result = Embedder::embed_batch(&mock, &texts)
            .await
            .expect("embed_batch should not fail");

        assert_ne!(
            result[0], result[1],
            "distinct inputs must produce distinct embedding vectors"
        );
    }

    /// Verify that `provider()` passes through from the underlying
    /// `trusty_common::embedder::Embedder`. `MockEmbedder` does not override
    /// `provider()`, so the shared-crate default (`ExecutionProvider::Cpu`)
    /// must be surfaced through the in-crate facade.
    #[test]
    fn embed_adapter_provider_passthrough_returns_cpu_for_mock() {
        let mock = MockEmbedder::new(8);
        // MockEmbedder uses the shared-crate default, which is Cpu.
        assert_eq!(
            Embedder::provider(&mock),
            ExecutionProvider::Cpu,
            "provider() passthrough must return Cpu for MockEmbedder"
        );
    }

    /// `resolved_provider_label()` defaults to `None` for any embedder that
    /// doesn't override it (issue #3493 P1 / epic #3524 slice 5) — callers
    /// must fall back to `provider()`'s prediction rather than crash or
    /// silently report a wrong wire-readback.
    #[test]
    fn resolved_provider_label_defaults_to_none_and_is_overridable() {
        let mock = MockEmbedder::new(8);
        assert_eq!(
            Embedder::resolved_provider_label(&mock),
            None,
            "default resolved_provider_label() must be None"
        );

        struct FakeRealReadback;
        #[async_trait]
        impl Embedder for FakeRealReadback {
            async fn embed(&self, _text: &str) -> Result<Vec<f32>> {
                unimplemented!()
            }
            async fn embed_batch(&self, _texts: &[&str]) -> Result<Vec<Vec<f32>>> {
                unimplemented!()
            }
            fn dimension(&self) -> usize {
                8
            }
            fn resolved_provider_label(&self) -> Option<String> {
                Some("MPS".to_string())
            }
        }
        assert_eq!(
            Embedder::resolved_provider_label(&FakeRealReadback),
            Some("MPS".to_string()),
            "an override must be honoured"
        );
    }

    /// `supervisor_gave_up()` defaults to `false` for any embedder that
    /// doesn't override it (review finding, PR #3560 HIGH fix) — callers
    /// (`FallbackEmbedderAdapter`) must never trip on an embedder with no
    /// supervisor to observe, and an explicit override must be honoured.
    #[test]
    fn supervisor_gave_up_defaults_to_false_and_is_overridable() {
        let mock = MockEmbedder::new(8);
        assert!(
            !Embedder::supervisor_gave_up(&mock),
            "default supervisor_gave_up() must be false"
        );

        struct FakeGivenUp;
        #[async_trait]
        impl Embedder for FakeGivenUp {
            async fn embed(&self, _text: &str) -> Result<Vec<f32>> {
                unimplemented!()
            }
            async fn embed_batch(&self, _texts: &[&str]) -> Result<Vec<Vec<f32>>> {
                unimplemented!()
            }
            fn dimension(&self) -> usize {
                8
            }
            fn supervisor_gave_up(&self) -> bool {
                true
            }
        }
        assert!(
            Embedder::supervisor_gave_up(&FakeGivenUp),
            "an override must be honoured"
        );
    }
}
