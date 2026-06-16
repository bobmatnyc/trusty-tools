//! The SM rolling-context engine (DOC-14 §7.2 / §7.5).
//!
//! Why: this is the orchestration layer that ties the §7 pieces together for one
//! conversation: it appends rounds, maintains the running token estimate, decides
//! WHEN to compact (round-count primary trigger + token-budget safety valve,
//! §7.2), folds evicted rounds into `compressed_context` via the injected
//! provider (§7.3), re-summarises an oversized compressed block (§7.6), persists
//! the state file after every mutation (§7.4), and assembles the working prompt
//! in the exact §7.5 order. Keeping the *policy* here — separate from the data
//! model, the compaction call, and the persistence — means each concern stays
//! small, testable, and under the SLOC cap.
//! What: [`SmContextEngine`] owns one [`SmConversation`], a `conv_id`, the
//! relevant slices of [`SmInferenceConfig`]/[`SmRoundsConfig`], and a
//! [`ConversationStore`]. [`SmContextEngine::record`] is the async entry point
//! that adds a round and runs compaction-to-convergence through a `&dyn
//! LlmProvider`. [`SmContextEngine::assemble_working_prompt`] produces the §7.5
//! ordered messages.
//! Test: `engine_tests.rs` — window eviction, evicted-content survival, the
//! goal/session-id golden, the token-budget trigger, default-vs-override model
//! selection, per-round persistence, and §7.5 assembly order.

use crate::core::sm::config::{SmInferenceConfig, SmRoundsConfig};
use crate::core::sm::providers::{ChatMessage, LlmProvider, SmLlmError};

use super::compaction::{estimate_tokens, fold_rounds, resummarise};
use super::model::{Round, SmConversation, ToolTrace};
use super::persist::{ConversationStore, ConversationStoreError};

/// Role string for a context/system-style message in the assembled prompt.
const ROLE_SYSTEM: &str = "system";
/// Role string for an operator (user) turn.
const ROLE_USER: &str = "user";
/// Role string for an SM (assistant) turn.
const ROLE_ASSISTANT: &str = "assistant";

/// Errors the context engine can surface (library → `thiserror`).
///
/// Why: `record` performs two fallible kinds of work — the compaction LLM call
/// (provider) and the state-file persistence (store) — plus model resolution. A
/// typed enum lets callers (SM-7) distinguish "compaction degraded, keep serving"
/// from "disk failed" without string matching, and keeps SM-5 panic-free.
/// What: wraps [`SmLlmError`] (resolution + compaction) and
/// [`ConversationStoreError`] (persistence).
/// Test: `record_surfaces_provider_error`, `record_surfaces_store_error` paths in
/// `engine_tests.rs`.
#[derive(Debug, thiserror::Error)]
pub enum SmContextError {
    /// A compaction / re-summarisation LLM call (or model resolution) failed.
    #[error("context compaction failed: {0}")]
    Compaction(#[from] SmLlmError),

    /// Persisting the conversation state file failed.
    #[error("context persistence failed: {0}")]
    Persist(#[from] ConversationStoreError),
}

/// The rolling auto-compaction context engine for ONE conversation (§7).
///
/// Why: one engine instance == one `conv_id`'s live context (SM-7 keeps a map of
/// these). Holding the config slices and the store on the engine means `record`
/// and `assemble_working_prompt` take only their per-call inputs (the provider,
/// the model, the current message), which keeps the call sites — and the tests —
/// simple.
/// What: owns the mutable [`SmConversation`], the `conv_id`, the rolling-window
/// size, the token budget + compressed-context cap (from [`SmInferenceConfig`]),
/// and a [`ConversationStore`]. The compaction provider and resolved model are
/// passed to [`record`](Self::record) per call (dependency injection) so the
/// engine never constructs a concrete provider.
/// Test: every test in `engine_tests.rs`.
pub struct SmContextEngine {
    /// Stable conversation id (drives the state-file name).
    conv_id: String,
    /// The live conversation state (§7.1).
    conversation: SmConversation,
    /// Verbatim rolling-window size, `> N` triggers compaction (§7.2a).
    window: usize,
    /// Token safety-valve budget; estimate `>` this triggers compaction (§7.2b).
    token_budget: usize,
    /// Max tokens the compressed block may hold before re-summarisation (§7.6).
    compressed_max_tokens: usize,
    /// Atomic state-file store (§7.4).
    store: ConversationStore,
}

impl SmContextEngine {
    /// Build (or resume) an engine for `conv_id` rooted at `data_root`.
    ///
    /// Why: on startup/resume the engine must reload any persisted state for this
    /// conversation (§7.4) so context survives a daemon restart; a fresh conv_id
    /// loads as empty. The config slices size the window and budgets. The data
    /// root is injectable so tests use a tempdir.
    /// What: constructs a [`ConversationStore`] under `data_root`, loads the
    /// conversation for `conv_id` (empty if absent), and copies the window/budget
    /// numbers out of the config. A load failure (corrupt file) surfaces as a
    /// [`SmContextError::Persist`].
    /// Test: `engine_resumes_persisted_conversation`,
    /// `new_conversation_starts_empty`.
    pub fn open(
        conv_id: impl Into<String>,
        data_root: impl Into<std::path::PathBuf>,
        inference: &SmInferenceConfig,
        rounds: &SmRoundsConfig,
    ) -> Result<Self, SmContextError> {
        let conv_id = conv_id.into();
        let store = ConversationStore::new(data_root);
        let conversation = store.load(&conv_id)?;
        Ok(Self {
            conv_id,
            conversation,
            window: rounds.window as usize,
            token_budget: inference.context_token_budget as usize,
            compressed_max_tokens: inference.compressed_context_max_tokens as usize,
            store,
        })
    }

    /// Read-only access to the live conversation (for §7.5 / `sm.context.get`).
    ///
    /// Why: SM-7's `sm.context.get` endpoint and the tests need to inspect the
    /// compressed block, window, counters, and estimate without mutating them.
    /// What: returns a shared reference to the inner [`SmConversation`].
    /// Test: used throughout `engine_tests.rs`.
    pub fn conversation(&self) -> &SmConversation {
        &self.conversation
    }

    /// The conversation id this engine is bound to.
    ///
    /// Why: callers map engines by id; exposing it keeps that lookup honest.
    /// What: returns the `conv_id`.
    /// Test: trivial; used by callers.
    pub fn conv_id(&self) -> &str {
        &self.conv_id
    }

    /// Record a completed round, compacting if the window/budget overflows (§7.2).
    ///
    /// Why: this is the engine's heart — append the verbatim round, update the
    /// running token estimate, then fold the oldest round(s) into
    /// `compressed_context` while EITHER trigger holds (round-count `>` window OR
    /// estimate `>` budget), re-summarise the compressed block if it grows past
    /// its cap (§7.6), and atomically persist after the mutation (§7.4). The
    /// compaction call is dependency-injected (`provider` + resolved
    /// `compaction_model`) so production uses the Haiku-tier provider and tests
    /// use a mock — this code never builds a concrete provider.
    /// What: pushes a [`Round`] from `(user, assistant, tool_calls)` stamped with
    /// `ts`; recomputes `token_estimate` from scratch (compressed block + every
    /// verbatim round) so it stays exact; loops `compact_once` until neither
    /// trigger holds (or the window can't shrink further); then saves. Returns the
    /// number of rounds evicted this call (0 = no compaction).
    /// Test: `window_evicts_oldest_round`, `evicted_content_lands_in_summary`,
    /// `golden_ids_survive_compaction`, `token_budget_triggers_compaction`,
    /// `default_compaction_uses_summary_model`,
    /// `compaction_model_override_is_honored`, `state_file_written_each_record`.
    pub async fn record(
        &mut self,
        provider: &dyn LlmProvider,
        compaction_model: &str,
        user: impl Into<String>,
        assistant: impl Into<String>,
        ts: chrono::DateTime<chrono::Utc>,
        tool_calls: Vec<ToolTrace>,
    ) -> Result<usize, SmContextError> {
        let round = Round::new(user, assistant, ts, tool_calls);
        self.conversation.recent_rounds.push_back(round);
        self.conversation.total_rounds += 1;
        self.recompute_estimate();

        let mut evicted = 0usize;
        // Compact while a trigger holds AND there is still a round we can evict.
        // We always keep at least one verbatim round so the window never empties
        // out from under a single huge round (the token-budget safety valve still
        // folds the others / re-summarises the compressed block, §7.2/§7.6).
        while self.should_compact() && self.conversation.recent_rounds.len() > 1 {
            self.compact_once(provider, compaction_model).await?;
            evicted += 1;
        }

        self.store.save(&self.conv_id, &self.conversation)?;
        Ok(evicted)
    }

    /// True when EITHER §7.2 trigger holds: window over size OR estimate over
    /// budget.
    ///
    /// Why: §7.2 fires on whichever condition is met first; round-count is the
    /// primary trigger, the token budget is the safety valve. Encoding both in
    /// one predicate keeps the `record` loop readable.
    /// What: returns `recent_rounds.len() > window || token_estimate >
    /// token_budget`.
    /// Test: `window_evicts_oldest_round` (count path),
    /// `token_budget_triggers_compaction` (budget path).
    fn should_compact(&self) -> bool {
        self.conversation.recent_rounds.len() > self.window
            || self.conversation.token_estimate > self.token_budget
    }

    /// Fold the single oldest round into `compressed_context` (§7.3), then
    /// re-summarise the block if it now exceeds its cap (§7.6).
    ///
    /// Why: one overflow event evicts the oldest round; doing exactly one eviction
    /// per call keeps the compaction calls bounded and the loop in `record` in
    /// control of convergence.
    /// What: pops the front round, calls [`fold_rounds`] with the current
    /// compressed block + that round through the injected provider, replaces
    /// `compressed_context` with the response text, recomputes the estimate, and —
    /// if the compressed block's own estimate exceeds `compressed_max_tokens` —
    /// runs [`resummarise`] and recomputes again.
    /// Test: `evicted_content_lands_in_summary`, `golden_ids_survive_compaction`,
    /// `oversized_summary_is_resummarised`.
    async fn compact_once(
        &mut self,
        provider: &dyn LlmProvider,
        compaction_model: &str,
    ) -> Result<(), SmContextError> {
        let Some(oldest) = self.conversation.recent_rounds.pop_front() else {
            return Ok(());
        };
        let evicted = [oldest];
        let resp = fold_rounds(
            provider,
            compaction_model,
            &self.conversation.compressed_context,
            &evicted,
        )
        .await?;
        self.conversation.compressed_context = resp.text;
        self.recompute_estimate();

        // §7.6: keep the compressed block within its token cap.
        if self.compressed_max_tokens > 0
            && estimate_tokens(self.conversation.compressed_context.len())
                > self.compressed_max_tokens
        {
            let resp = resummarise(
                provider,
                compaction_model,
                &self.conversation.compressed_context,
            )
            .await?;
            self.conversation.compressed_context = resp.text;
            self.recompute_estimate();
        }
        Ok(())
    }

    /// Recompute `token_estimate` from the compressed block + every verbatim round.
    ///
    /// Why: keeping a running counter incrementally is error-prone across evictions
    /// and re-summarisations; recomputing from the authoritative state after each
    /// mutation is cheap (chars/4 over a bounded window) and always correct.
    /// What: sums the char length of `compressed_context` and every round, then
    /// applies the chars/4 heuristic, storing the result in `token_estimate`.
    /// Test: `token_estimate_tracks_content` and indirectly every compaction test.
    fn recompute_estimate(&mut self) {
        let chars = self.conversation.compressed_context.len()
            + self
                .conversation
                .recent_rounds
                .iter()
                .map(Round::char_len)
                .sum::<usize>();
        self.conversation.token_estimate = estimate_tokens(chars);
    }

    /// Assemble the working-prompt messages in the exact §7.5 order.
    ///
    /// Why: §7.5 mandates a precise message order — system → compressed context →
    /// memory recall → recent rounds → current message. Producing it here (not at
    /// the call site) guarantees the order is correct and identical everywhere,
    /// and lets SM-7 simply pass the assembled SM system prompt and the SM-4
    /// recall text it already has.
    /// What: builds a `Vec<ChatMessage>` in five ordered parts — (1) the system
    /// message `system_prompt` (skipped if empty); (2) the compressed-context
    /// block as a `system` message prefixed "Earlier in this conversation:"
    /// (skipped if empty); (3) the memory-recall block as a `system` message
    /// prefixed "Relevant memory:" (skipped if `memory_recall` is empty/None);
    /// (4) each verbatim recent round as alternating `user`/`assistant` turns;
    /// (5) the current operator `message` as the final `user` turn. The recall
    /// text is a PARAMETER (not fetched here) so SM-7 passes SM-4's recall
    /// results; SM-5 does not wire memory.
    /// Test: `assembly_order_is_exact`, `assembly_skips_empty_blocks`.
    pub fn assemble_working_prompt(
        &self,
        system_prompt: &str,
        memory_recall: Option<&str>,
        message: &str,
    ) -> Vec<ChatMessage> {
        let mut msgs: Vec<ChatMessage> = Vec::new();

        // 1. System message.
        if !system_prompt.trim().is_empty() {
            msgs.push(ChatMessage {
                role: ROLE_SYSTEM.to_string(),
                content: system_prompt.to_string(),
            });
        }

        // 2. Compressed-context block.
        if !self.conversation.compressed_context.trim().is_empty() {
            msgs.push(ChatMessage {
                role: ROLE_SYSTEM.to_string(),
                content: format!(
                    "Earlier in this conversation: {}",
                    self.conversation.compressed_context
                ),
            });
        }

        // 3. Memory-recall block (injected by the caller, SM-4 results).
        if let Some(recall) = memory_recall
            && !recall.trim().is_empty()
        {
            msgs.push(ChatMessage {
                role: ROLE_SYSTEM.to_string(),
                content: format!("Relevant memory: {recall}"),
            });
        }

        // 4. Recent verbatim rounds as alternating user/assistant turns.
        for round in &self.conversation.recent_rounds {
            msgs.push(ChatMessage {
                role: ROLE_USER.to_string(),
                content: round.user.clone(),
            });
            msgs.push(ChatMessage {
                role: ROLE_ASSISTANT.to_string(),
                content: round.assistant.clone(),
            });
        }

        // 5. Current operator message.
        msgs.push(ChatMessage {
            role: ROLE_USER.to_string(),
            content: message.to_string(),
        });

        msgs
    }
}

#[cfg(test)]
#[path = "engine_tests.rs"]
mod tests;
