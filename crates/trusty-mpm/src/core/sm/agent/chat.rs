//! The SM conversational chat turn (DOC-14 §3.4 intake, §7.5 assembly).
//!
//! Why: SM-7's deliverable is making `SessionManagerAgent::chat` real — one
//! end-to-end turn that composes every SM building block in the precise §7.5
//! order: the SM system prompt (SM-3) + the rolling-context engine's compressed
//! block (SM-5) + memory recall (SM-4) as a single system message, then the
//! recent verbatim rounds, then the current operator message; the assembled
//! prompt goes to the resolved provider (SM-2); the reply + per-call cost come
//! back; and the round is recorded into the context engine (which auto-compacts
//! and persists). Keeping the orchestration here — separate from the agent
//! struct — keeps each file focused and under the SLOC cap.
//! What: [`SmChatOutcome`] (reply + conv_id + cost), [`SmAgentError`] (typed
//! failures, with [`SmAgentError::Degraded`] mapped to the endpoint's graceful
//! 503), and the `impl` block adding [`super::SessionManagerAgent::chat`].
//! Test: `chat_tests.rs` — full turn with a mock provider (prompt includes
//! system + compressed context + recall + rounds + message; round recorded;
//! reply + cost returned), degraded mode, recall on/off, and conv_id defaulting.

use chrono::Utc;
use uuid::Uuid;

use crate::core::sm::context::{SmContextEngine, SmContextError};
use crate::core::sm::prompt::resolve_sm_prompt_default;
use crate::core::sm::providers::{LlmRequest, SmLlmError, SmModelTier};

use super::SessionManagerAgent;

/// Generation cap for one SM orchestration reply.
///
/// Why: the SM "largely relays instructions and orchestrates" (§5.4) rather than
/// emitting long prose, so a bounded ceiling keeps each turn cheap and
/// predictable while leaving ample room for a decomposition/plan reply.
/// What: passed as [`LlmRequest::max_tokens`] for the orchestration call.
/// Test: `chat_tests.rs` asserts the mock received this value.
const SM_CHAT_MAX_TOKENS: u32 = 4_096;

/// The result of one SM chat turn (maps to `sm.chat` → `{ reply, conv_id, cost? }`).
///
/// Why: the endpoint (SM-7) and the future stdio adapter (SM-STDIO) need the
/// reply text, the conversation id the turn used (so a follow-up can continue
/// the same rolling context), and the per-call cost for the DOC-13 TUI status
/// bar. Returning all three as one value keeps the call sites terse.
/// What: `reply` is the assistant text; `conv_id` is the (possibly freshly
/// minted) conversation id; `cost_usd` is the provider's estimated USD cost for
/// this single call (§5.5).
/// Test: `chat_tests.rs::chat_drives_full_turn_with_mock_provider`.
#[derive(Debug, Clone, PartialEq)]
pub struct SmChatOutcome {
    /// The assistant's reply text.
    pub reply: String,
    /// The conversation id this turn used (echoed so callers can continue it).
    pub conv_id: String,
    /// Estimated USD cost of this single provider call (§5.5).
    pub cost_usd: f64,
}

/// A failure surfaced by [`SessionManagerAgent::chat`].
///
/// Why: the endpoint must distinguish a graceful *degraded* state (no inference
/// provider configured — surface the DOC-13 "no inference" notice as a 503, §5.3)
/// from a genuine inference/persistence error (a real failure). A typed enum lets
/// the caller route each without string-matching, and keeps the SM library code
/// panic-free per the workspace convention.
/// What: [`SmAgentError::Degraded`] carries the human-readable notice for the
/// no-provider case; [`SmAgentError::Inference`] wraps a provider/resolution
/// failure; [`SmAgentError::Context`] wraps a context-engine persistence/
/// compaction failure.
/// Test: `chat_tests.rs::chat_without_provider_is_degraded`,
/// `chat_inference_error_surfaces`.
#[derive(Debug, thiserror::Error)]
pub enum SmAgentError {
    /// No inference provider has resolvable credentials (graceful, §5.3 / G6).
    /// The endpoint renders this as the "no inference configured" notice.
    #[error("{0}")]
    Degraded(String),

    /// A provider resolution or `complete` call failed (a real inference error).
    #[error("session-manager inference failed: {0}")]
    Inference(#[source] SmLlmError),

    /// Recording the round into the rolling-context engine failed.
    #[error("session-manager context update failed: {0}")]
    Context(#[from] SmContextError),
}

impl SessionManagerAgent {
    /// Drive one conversational SM turn end-to-end (§3.4 intake + §7.5 assembly).
    ///
    /// Why: this is SM-7's headline deliverable. It ties the SM building blocks
    /// into a single, deterministic-to-test turn so the `coordinator/chat`
    /// endpoint (and the stdio adapter) can converse through the SM exactly as
    /// the spec mandates — system prompt + compressed context + recall + recent
    /// rounds + message → provider → reply + cost → record the round.
    /// What: (1) resolves the conversation id (caller-supplied or a fresh UUID);
    /// (2) opens the per-`conv_id` [`SmContextEngine`] under the runtime's
    /// `data_root`; (3) builds the SM system prompt via
    /// [`resolve_sm_prompt_default`]; (4) recalls top-k SM-palace memory for the
    /// message (feature-gated, skipped gracefully when absent or on error);
    /// (5) assembles the §7.5 working prompt; (6) resolves the orchestration-tier
    /// provider via the [`ProviderRegistry`](crate::core::sm::providers::ProviderRegistry)
    /// — a [`SmLlmError::Degraded`] here becomes [`SmAgentError::Degraded`];
    /// (7) calls `complete` at the configured temperature; (8) records the
    /// `(message, reply)` round (which auto-compacts + persists); and (9) returns
    /// the reply, conv_id, and per-call cost. With no runtime (the inert
    /// [`super::SessionManagerAgent::new`] agent) it returns
    /// [`SmAgentError::Degraded`] immediately — no provider exists.
    /// Test: `chat_tests.rs` — full turn, degraded, recall on/off, conv_id default.
    pub async fn chat(
        &self,
        message: &str,
        conv_id: Option<&str>,
    ) -> Result<SmChatOutcome, SmAgentError> {
        let Some(runtime) = self.runtime.as_ref() else {
            return Err(SmAgentError::Degraded(degraded_notice()));
        };

        let conv_id = conv_id
            .filter(|c| !c.trim().is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| Uuid::new_v4().to_string());

        let inference = &self.config.inference;
        let rounds = &self.config.rounds;

        // (2) Open (or resume) the rolling-context engine for this conversation.
        let mut engine = SmContextEngine::open(&conv_id, &runtime.data_root, inference, rounds)?;

        // (3) The SM system prompt (SM-3), with any operator overrides layered in.
        let system_prompt = resolve_sm_prompt_default();

        // (4) Memory recall (SM-4) — §7.5 step 3. Skipped gracefully when the
        // feature is off, no palace is wired, or recall errors (degrade, never
        // fail the turn on a recall miss).
        let recall = self.recall_block(runtime, message).await;

        // (5) Assemble the §7.5 working prompt: ONE system message (prompt +
        // compressed context + recall), then recent rounds, then the message.
        let messages = engine.assemble_working_prompt(&system_prompt, recall.as_deref(), message);

        // (6) Resolve the orchestration-tier provider (SM-2). Degraded → graceful.
        let resolved = runtime
            .resolver
            .resolve(inference, SmModelTier::Orchestration)
            .await
            .map_err(map_resolve_error)?;

        // (7) The provider request carries the system prompt separately (some
        // providers want it out-of-band); strip the leading system message from
        // the assembled list and pass the rest as conversation turns.
        let (system, turns) = split_system_message(messages);
        let req = LlmRequest {
            model: resolved.model.clone(),
            system,
            messages: turns,
            temperature: inference.temperature,
            max_tokens: SM_CHAT_MAX_TOKENS,
        };
        let response = resolved
            .provider
            .complete(req)
            .await
            .map_err(SmAgentError::Inference)?;

        // (8) Record the round (auto-compacts older rounds + persists, §7.2/§7.4).
        // The compaction call reuses the orchestration provider with the
        // compaction-tier model; resolving that tier degrades gracefully (a
        // recall/compaction miss must never fail an otherwise-successful turn).
        self.record_round(runtime, &mut engine, message, &response.text)
            .await?;

        Ok(SmChatOutcome {
            reply: response.text,
            conv_id,
            cost_usd: response.cost_usd,
        })
    }

    /// Resolve the §7.5 step-3 memory-recall block for `message`, or `None`.
    ///
    /// Why: recall enriches the working prompt with the SM palace's most
    /// relevant durable knowledge (§8.3), but it is best-effort: a missing
    /// palace, the feature being off, or a recall error must degrade to "no
    /// recall" rather than failing the chat turn.
    /// What: under `sm-memory`, when a palace handle is present, runs
    /// [`SmMemory::recall`](crate::core::sm::memory::SmMemory::recall) and joins
    /// the hit contents into a single block; returns `None` on no-hits/error or
    /// when memory is unavailable. Without the feature, always `None`.
    /// Test: `chat_tests.rs::chat_includes_recall_when_memory_present` (feature),
    /// `chat_works_without_memory_feature` (no feature).
    #[cfg(feature = "sm-memory")]
    async fn recall_block(&self, runtime: &super::AgentRuntime, message: &str) -> Option<String> {
        let memory = runtime.memory.as_ref()?;
        match memory.recall(message).await {
            Ok(hits) if !hits.is_empty() => {
                let joined = hits
                    .iter()
                    .map(|h| h.drawer.content.trim())
                    .filter(|c| !c.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n");
                (!joined.trim().is_empty()).then_some(joined)
            }
            Ok(_) => None,
            Err(e) => {
                tracing::debug!("sm chat: memory recall failed, continuing without it: {e}");
                None
            }
        }
    }

    /// No-memory build: recall is always absent.
    ///
    /// Why: without the `sm-memory` feature the heavy memory-core dependency is
    /// not compiled in, so the chat turn composes prompt + context + provider
    /// with no recall — and must still work.
    /// What: always returns `None`.
    /// Test: `chat_tests.rs::chat_works_without_memory_feature`.
    #[cfg(not(feature = "sm-memory"))]
    async fn recall_block(&self, _runtime: &super::AgentRuntime, _message: &str) -> Option<String> {
        None
    }

    /// Record the completed `(message, reply)` round into the context engine.
    ///
    /// Why: §7.2/§7.4 require every round to be appended, compacted if the window/
    /// budget overflows, and atomically persisted so context survives a restart.
    /// The compaction call is dependency-injected — it reuses the resolved
    /// provider with the compaction-tier model — so this code never builds a
    /// provider. Resolving the compaction tier is best-effort: a resolution miss
    /// (degraded compaction) must not fail an otherwise-successful chat turn, so
    /// it is logged and the round is recorded without compaction in that case.
    /// What: resolves the compaction-tier model + provider; on success records
    /// through it; on a resolve failure logs at debug and records through the
    /// orchestration provider with the orchestration model (no compaction
    /// occurs unless the window/budget actually overflows). A persistence error
    /// (disk) propagates as [`SmAgentError::Context`].
    /// Test: `chat_tests.rs::chat_records_round`.
    async fn record_round(
        &self,
        runtime: &super::AgentRuntime,
        engine: &mut SmContextEngine,
        message: &str,
        reply: &str,
    ) -> Result<(), SmAgentError> {
        let inference = &self.config.inference;
        let resolved = runtime
            .resolver
            .resolve(inference, SmModelTier::Compaction)
            .await;
        match resolved {
            Ok(call) => {
                engine
                    .record(
                        call.provider.as_ref(),
                        &call.model,
                        message.to_string(),
                        reply.to_string(),
                        Utc::now(),
                        Vec::new(),
                    )
                    .await?;
                Ok(())
            }
            Err(e) => {
                // Compaction provider unavailable (e.g. degraded). Record the
                // round with a no-compaction-needed path: we still must persist
                // the verbatim round. The context engine always needs *a*
                // provider for the record signature, but it only invokes it when
                // a trigger fires; pass through the orchestration build instead.
                tracing::debug!(
                    "sm chat: compaction tier unavailable ({e}); recording round without compaction"
                );
                let call = runtime
                    .resolver
                    .resolve(inference, SmModelTier::Orchestration)
                    .await
                    .map_err(map_resolve_error)?;
                engine
                    .record(
                        call.provider.as_ref(),
                        &call.model,
                        message.to_string(),
                        reply.to_string(),
                        Utc::now(),
                        Vec::new(),
                    )
                    .await?;
                Ok(())
            }
        }
    }
}

/// The graceful "no inference configured" notice (DOC-13 parity, §5.3).
///
/// Why: degraded mode must surface the same operator-facing message the legacy
/// `coordinator/chat` 503 used, so the TUI renders a consistent notice whether
/// the SM is on or off.
/// What: returns the fixed notice string.
/// Test: `chat_tests.rs::chat_without_provider_is_degraded` asserts it.
pub(crate) fn degraded_notice() -> String {
    "session manager has no inference provider configured \
     (set ANTHROPIC_API_KEY, AWS credentials, or OPENROUTER_API_KEY)"
        .to_string()
}

/// Map a provider-resolution [`SmLlmError`] into an [`SmAgentError`], folding the
/// graceful degraded variant into [`SmAgentError::Degraded`].
///
/// Why: only the *degraded* (no-credentials) case is a graceful notice; every
/// other resolution failure (unknown provider, bad model) is a real error the
/// endpoint should report as such. Centralising the split keeps both call sites
/// (orchestration + compaction resolution) consistent.
/// What: `Degraded` → [`SmAgentError::Degraded`] with [`degraded_notice`]; any
/// other variant → [`SmAgentError::Inference`].
/// Test: `chat_tests.rs::chat_without_provider_is_degraded`,
/// `chat_unknown_provider_is_inference_error`.
fn map_resolve_error(err: SmLlmError) -> SmAgentError {
    if err.is_degraded() {
        SmAgentError::Degraded(degraded_notice())
    } else {
        SmAgentError::Inference(err)
    }
}

/// Split the assembled §7.5 messages into the out-of-band system string and the
/// remaining conversation turns.
///
/// Why: [`SmContextEngine::assemble_working_prompt`] consolidates the system
/// prompt + compressed context + recall into ONE leading `system`-role message
/// (so multi-system-message-averse providers stay happy), but [`LlmRequest`]
/// carries the system prompt as a dedicated `system` field separate from the
/// `messages` turns. This adapts one shape to the other without re-deriving the
/// §7.5 order.
/// What: if the first message is `system`-role, returns `(its content, the
/// rest)`; otherwise returns `(String::new(), all messages)`.
/// Test: `chat_tests.rs::chat_drives_full_turn_with_mock_provider` (asserts the
/// mock saw the system prompt + compressed/recall in `system`, turns in `messages`).
fn split_system_message(
    mut messages: Vec<crate::core::sm::providers::ChatMessage>,
) -> (String, Vec<crate::core::sm::providers::ChatMessage>) {
    if messages.first().is_some_and(|m| m.role == "system") {
        let system = messages.remove(0).content;
        (system, messages)
    } else {
        (String::new(), messages)
    }
}

#[cfg(test)]
#[path = "chat_tests.rs"]
mod chat_tests;
