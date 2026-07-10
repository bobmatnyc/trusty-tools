//! Test-only mock provider + tier resolver for the SM chat-turn tests.
//!
//! Why: SM-7's acceptance tests must drive a FULL chat turn deterministically —
//! asserting the assembled §7.5 prompt, that the round is recorded, and that the
//! reply plus cost come back — with NO network and NO real model. The agent
//! depends on the [`TierResolver`] seam exactly so tests can inject a resolver
//! that hands back a [`ResolvedCall`] wrapping a recording mock [`LlmProvider`].
//! This module is that injectable pair.
//! What: [`MockChatProvider`] records every [`LlmRequest`] and replies with a
//! fixed text + a fixed per-call cost; [`MockResolver`] implements
//! [`TierResolver`] by returning a [`ResolvedCall`] over a shared
//! `MockChatProvider`, or a degraded error when configured to. Both are
//! `#[cfg(test)]`-only.
//! Test: drives `agent/chat_tests.rs`.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use crate::core::sm::config::SmInferenceConfig;
use crate::core::sm::providers::{
    LlmProvider, LlmRequest, LlmResponse, ProviderKind, ResolvedCall, SmLlmError, SmModelTier,
    TierResolver,
};

/// A recording, deterministic mock [`LlmProvider`] for the chat-turn tests.
///
/// Why: lets a test assert exactly what the SM sent the model (the §7.5
/// assembly: system prompt, compressed context, recall, rounds, message) and
/// returns a known reply + cost so the outcome is fully deterministic.
/// What: holds a fixed reply, a fixed cost, and a shared log of every request.
/// Clones share the log (`Arc<Mutex<…>>`), so a clone handed to a
/// [`ResolvedCall`] still records into the handle the test holds.
/// Test: `chat_tests.rs`.
#[derive(Clone)]
pub struct MockChatProvider {
    reply: String,
    cost_usd: f64,
    requests: Arc<Mutex<Vec<LlmRequest>>>,
    /// Optional gate that BLOCKS the first `complete` call until released (#1309).
    gate: Option<Arc<CompleteGate>>,
}

/// A one-shot gate that parks the FIRST `complete` call until the test releases it.
///
/// Why: the #1309 concurrency test must deterministically force two turns for the
/// same `conv_id` to overlap — turn A must OPEN the context engine and reach its
/// provider call while turn B is still in flight, so that (pre-fix) both open the
/// same on-disk state before either saves and one round is lost. Gating only the
/// FIRST `complete` (turn A's orchestration call) parks A there while turn B
/// either races ahead (pre-fix, no lock) or blocks on the per-conv lock (post-fix)
/// — WITHOUT deadlocking the post-fix path (only A waits on the gate, and B never
/// needs to reach `complete` for A to be released).
/// What: `calls` counts `complete` invocations across the shared provider; the
/// call at index 0 awaits `release`. Later calls pass straight through.
/// Test: `chat_tests.rs::concurrent_turns_same_conv_id_persist_both_rounds`.
pub struct CompleteGate {
    /// Notified by the test to release the parked first `complete` call.
    release: tokio::sync::Notify,
    /// Monotonic count of `complete` invocations across all clones.
    calls: AtomicUsize,
}

impl MockChatProvider {
    /// Build a mock that replies with `reply` and reports `cost_usd`.
    pub fn new(reply: impl Into<String>, cost_usd: f64) -> Self {
        Self {
            reply: reply.into(),
            cost_usd,
            requests: Arc::new(Mutex::new(Vec::new())),
            gate: None,
        }
    }

    /// Gate this provider's FIRST `complete` call on `gate` (#1309 test seam).
    ///
    /// Why: lets the concurrency test hold turn A inside its provider call until it
    /// releases the gate, deterministically forcing the overlap that reproduces the
    /// round-loss race pre-fix. Clones share the gate (an `Arc`), so the gate spans
    /// every clone the resolver hands to a `ResolvedCall`.
    /// What: returns `self` with the gate attached. Call `CompleteGate::release` to
    /// let the parked first call proceed.
    /// Test: `chat_tests.rs::concurrent_turns_same_conv_id_persist_both_rounds`.
    pub fn gated(mut self, gate: Arc<CompleteGate>) -> Self {
        self.gate = Some(gate);
        self
    }

    /// The most recent request, if any.
    pub fn last_request(&self) -> Option<LlmRequest> {
        self.requests.lock().expect("mock lock").last().cloned()
    }

    /// How many times `complete` has been invoked on this (shared) provider.
    ///
    /// Why: a test that drives two router calls through one shared provider must be
    /// able to assert the provider was actually invoked twice, so a future refactor
    /// that accidentally dedups/short-circuits the calls fails loudly rather than
    /// silently passing.
    /// What: returns the length of the shared request log (clones share it, so the
    /// count spans every clone handed to a [`ResolvedCall`]).
    /// Test: `coordinator_sm_tests::session_manager_chat_alias_matches_coordinator`.
    pub fn request_count(&self) -> usize {
        self.requests.lock().expect("mock lock").len()
    }
}

impl CompleteGate {
    /// Build a gate whose first `complete` call will park until released.
    ///
    /// Why: the concurrency test creates one gate, shares it with the provider, and
    /// releases it once both turns have settled.
    /// What: returns an `Arc<CompleteGate>` (shared with the provider clones).
    /// Test: `chat_tests.rs::concurrent_turns_same_conv_id_persist_both_rounds`.
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            release: tokio::sync::Notify::new(),
            calls: AtomicUsize::new(0),
        })
    }

    /// Release the parked first `complete` call so turn A can finish.
    ///
    /// Why: the test releases the gate after both turns have reached steady state.
    /// What: notifies the parked waiter (or stores a permit if it has not parked
    /// yet, so the release can never be lost to a race).
    /// Test: `chat_tests.rs::concurrent_turns_same_conv_id_persist_both_rounds`.
    pub fn release(&self) {
        self.release.notify_one();
    }
}

#[async_trait]
impl LlmProvider for MockChatProvider {
    fn name(&self) -> &str {
        "mock"
    }

    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, SmLlmError> {
        // #1309 test seam: park the FIRST complete call until the gate is released.
        if let Some(gate) = &self.gate
            && gate.calls.fetch_add(1, Ordering::SeqCst) == 0
        {
            gate.release.notified().await;
        }
        let model = req.model.clone();
        self.requests.lock().expect("mock lock").push(req);
        Ok(LlmResponse {
            text: self.reply.clone(),
            model,
            input_tokens: 10,
            output_tokens: 5,
            latency_ms: 1,
            cost_usd: self.cost_usd,
        })
    }
}

/// What a [`MockResolver`] does when asked to resolve a tier.
///
/// Why: tests need both the happy path (return a mock provider) and the degraded
/// path (no provider configured) without a real registry.
/// What: `Provider` wraps a shared [`MockChatProvider`]; `Degraded` returns
/// [`SmLlmError::Degraded`].
/// Test: `chat_tests.rs`.
#[derive(Clone)]
pub enum MockResolution {
    /// Resolve every tier to this shared provider with the tier's model id.
    Provider(MockChatProvider),
    /// Resolve every tier to a graceful degraded error.
    Degraded,
    /// Resolve every tier to a non-degraded inference error (unknown provider).
    Validation,
    /// Resolve the FIRST `n` calls to the shared provider, then degraded for the
    /// rest. Models the data-integrity case where the orchestration reply succeeds
    /// (call 1) but every subsequent compaction/orchestration resolution in
    /// `record_round` finds no provider — forcing the verbatim no-compaction record.
    ProviderThenDegraded {
        /// The shared provider returned for the first `n` calls.
        provider: MockChatProvider,
        /// How many leading calls succeed before degraded kicks in.
        n: usize,
        /// Shared monotonic count of `resolve` calls so far.
        calls: Arc<AtomicUsize>,
    },
}

/// A mock [`TierResolver`] for the SM chat-turn tests.
///
/// Why: the agent resolves a provider per request through `Arc<dyn TierResolver>`;
/// injecting this mock lets the test control the provider (and degraded/error
/// behaviour) with no env credentials and no network.
/// What: resolves the tier's configured model via the real
/// [`crate::core::sm::providers::resolve_tier_model`] (so the test exercises the
/// tier-selection logic too) and wraps the shared mock provider in a
/// [`ResolvedCall`]; or returns the configured error.
/// Test: `chat_tests.rs`.
#[derive(Clone)]
pub struct MockResolver {
    resolution: MockResolution,
}

impl MockResolver {
    /// A resolver that hands back `provider` for every tier.
    pub fn with_provider(provider: MockChatProvider) -> Self {
        Self {
            resolution: MockResolution::Provider(provider),
        }
    }

    /// A resolver that always reports degraded (no provider).
    pub fn degraded() -> Self {
        Self {
            resolution: MockResolution::Degraded,
        }
    }

    /// A resolver that always reports a non-degraded inference (validation) error.
    pub fn validation() -> Self {
        Self {
            resolution: MockResolution::Validation,
        }
    }

    /// A resolver that succeeds for the first `n` resolutions, then degrades.
    ///
    /// Why: the data-integrity test needs the orchestration reply to succeed
    /// (resolution #1) while every later compaction/orchestration resolution in
    /// `record_round` fails, so the turn must persist the round VERBATIM via the
    /// no-compaction path instead of dropping it.
    /// What: returns a resolver whose first `n` `resolve` calls hand back
    /// `provider`, and whose subsequent calls return [`SmLlmError::Degraded`].
    /// Test: `chat_records_round_when_no_provider_for_compaction`.
    pub fn provider_then_degraded(provider: MockChatProvider, n: usize) -> Self {
        Self {
            resolution: MockResolution::ProviderThenDegraded {
                provider,
                n,
                calls: Arc::new(AtomicUsize::new(0)),
            },
        }
    }
}

#[async_trait]
impl TierResolver for MockResolver {
    async fn resolve(
        &self,
        cfg: &SmInferenceConfig,
        tier: SmModelTier,
    ) -> Result<ResolvedCall, SmLlmError> {
        match &self.resolution {
            MockResolution::Provider(provider) => resolved_call(cfg, tier, provider),
            MockResolution::Degraded => Err(SmLlmError::Degraded(
                "mock: no provider configured".to_string(),
            )),
            MockResolution::Validation => {
                Err(SmLlmError::Validation("mock: unknown provider".to_string()))
            }
            MockResolution::ProviderThenDegraded { provider, n, calls } => {
                // `fetch_add` returns the prior count, so the first `n` calls
                // (indices 0..n) succeed and the rest degrade.
                let idx = calls.fetch_add(1, Ordering::SeqCst);
                if idx < *n {
                    resolved_call(cfg, tier, provider)
                } else {
                    Err(SmLlmError::Degraded(
                        "mock: provider exhausted after first call".to_string(),
                    ))
                }
            }
        }
    }
}

/// Build a [`ResolvedCall`] for `tier` wrapping the shared mock `provider`.
///
/// Why: two resolver arms (`Provider`, `ProviderThenDegraded`) build the same
/// resolved call; centralising it keeps them consistent and exercises the real
/// tier-model selection in both.
/// What: resolves the tier model via the real selection logic, strips any
/// provider prefix to a bare model id, and wraps the cloned mock provider.
/// Test: `chat_tests.rs`.
fn resolved_call(
    cfg: &SmInferenceConfig,
    tier: SmModelTier,
    provider: &MockChatProvider,
) -> Result<ResolvedCall, SmLlmError> {
    let tier_model = crate::core::sm::providers::resolve_tier_model(cfg, tier)?;
    let (_kind, bare) =
        crate::core::sm::providers::resolve_provider_and_model(&tier_model, ProviderKind::Auto);
    Ok(ResolvedCall {
        provider: Arc::new(provider.clone()),
        model: bare,
        kind: ProviderKind::Anthropic,
    })
}
