//! Deterministic inference backend for tests (test-support, #9278 A5).
//!
//! Why: real LLM calls must never run during `cargo test`. Ruling D3 (#9278)
//! keeps test doubles out of the ADR-0066 frozen surface, so this module is
//! compiled only for this crate's tests and under `memory-core-test-support`.
//! What: `MockInference`, an `Inference` that returns fixture actions.
//! Test: `semantic_consolidation::tests` and `dream::tests`.

use anyhow::Result;
use async_trait::async_trait;

use crate::engine::palace::Drawer;

use super::inference::Inference;
use super::types::ConsolidationAction;

/// Deterministic inference backend for unit and integration tests.
///
/// Why: real LLM calls must never run during `cargo test` (they hit the
/// network, cost money, and are non-deterministic). `MockInference` returns
/// pre-configured fixture actions so tests are fast and reproducible.
/// What: constructed with a `Vec<ConsolidationAction>` that is cloned on
/// every `consolidate` call, plus a call counter so tests can assert the
/// right number of batches fired.
/// Test: Used directly in `semantic_consolidation::tests` and
/// `dream::tests::dream_cycle_semantic_consolidation_*`.
pub struct MockInference {
    /// Actions returned for every batch (regardless of batch content).
    pub fixture_actions: Vec<ConsolidationAction>,
    /// Counts how many `consolidate` calls have been made.
    pub call_count: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl MockInference {
    /// Create a mock that always returns `fixture_actions`.
    ///
    /// Why: test setup: callers supply the desired output so assertions are
    /// predictable.
    /// What: stores the fixture list and initialises the call counter to 0.
    /// Test: exercised by every test in `semantic_consolidation::tests`.
    pub fn new(fixture_actions: Vec<ConsolidationAction>) -> Self {
        Self {
            fixture_actions,
            call_count: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }

    /// Create a mock that returns no actions (no-op consolidation).
    ///
    /// Why: integration tests that verify the dream cycle completes when
    /// consolidation finds nothing to do.
    /// What: constructs with an empty fixture list.
    /// Test: `dream_cycle_semantic_consolidation_no_inference`.
    pub fn no_op() -> Self {
        Self::new(vec![])
    }
}

#[async_trait]
impl Inference for MockInference {
    fn name(&self) -> &str {
        "mock"
    }

    async fn consolidate(&self, _drawers: &[Drawer]) -> Result<Vec<ConsolidationAction>> {
        self.call_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(self.fixture_actions.clone())
    }
}
