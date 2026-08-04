//! Recovery policy for a failed LOCAL model call on the conversational
//! fast path (#3766).
//!
//! Why: `history.rs` gated its retry on `local_qualifies` alone, so a failure
//! of an agent whose OWN configured model is local (ctrl shipped
//! `ollama/qwen3:30b`) never reached the fallback and surfaced a raw
//! `Connection refused` as the user's assistant reply. The retry it did have
//! reused `pm_cfg.agent.model` verbatim — the same unreachable slug — so it
//! could not succeed by construction either.
//! What: pure decision helpers, no I/O. Split out of `history.rs`, which sits
//! against the 500-SLOC production cap.
//! Test: sibling `local_fallback_tests.rs`.

/// Whether `model` routes to a locally-hosted daemon rather than a remote API.
///
/// Why: #3766 — a local daemon that isn't running fails with a raw transport
/// error, which must never reach the user as their assistant's reply.
/// What: mirrors the `ollama/` prefix `llm::adapter::adapter_for_model` routes
/// to `OllamaAdapter`; every other slug reaches a remote API.
/// Test: `local_model_detects_ollama_prefix`.
pub(super) fn is_local_model(model: &str) -> bool {
    model.starts_with("ollama/")
}

/// What the conversational fast path does when its LLM call returns an error.
///
/// Why: #3766 — makes the three outcomes explicit and testable instead of
/// implicit in match-arm guards.
/// What: `RetryRemote` carries a model proven NOT to be local; `Explain`
/// carries a user-facing message; `Propagate` keeps the original error.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum LocalFailureAction {
    RetryRemote(String),
    Explain(String),
    Propagate,
}

/// Decide how to recover from a failed conversational LLM call.
///
/// Why: #3766 — this is the decision that was wrong. The old arm was gated
/// `local_qualifies && fallback_on_error`, which conflates two unrelated
/// questions: "should I PREFER a local model" and "am I ALLOWED to recover
/// from an error". The owner's `local_inference.enabled = false` forces
/// `local_qualifies = false` on every turn, so the setting meaning "avoid
/// Ollama" was exactly what guaranteed an unrecoverable Ollama error reached
/// the user.
/// What: gates on the FAILURE, not on any preference. A LOCAL attempt that
/// failed at the TRANSPORT level is recoverable no matter what the
/// local-inference preferences say; it retries against the configured model
/// when that model is genuinely remote — guaranteeing the target is a
/// DIFFERENT, non-local endpoint than the one that just failed — and
/// otherwise returns an actionable message. Everything else propagates, so
/// genuine API errors (429, 401, 500) from either a local or a remote model
/// are never swallowed.
///
/// Deliberately takes NO preference flag: `local_inference.fallback_on_error`
/// no longer influences this path. It remains meaningful elsewhere (the
/// `ctrl_turn::dispatch` route and the `/local` REPL readout).
/// Test: `owner_path_local_inference_disabled_recovers`,
/// `enabled_local_route_falls_back_to_a_distinct_remote_model`,
/// `recovery_is_independent_of_local_inference_gate_state`,
/// `local_route_will_not_retry_the_same_local_slug`,
/// `non_transport_local_failure_propagates`,
/// `fixed_ctrl_config_propagates_remote_failures`.
pub(super) fn local_failure_action(
    effective_model: &str,
    configured_model: &str,
    transport_failure: bool,
    host: &str,
) -> LocalFailureAction {
    // Only a LOCAL attempt that failed at the transport level is recoverable
    // here — a server that answered (429/401/500) returned a real error.
    if !is_local_model(effective_model) || !transport_failure {
        return LocalFailureAction::Propagate;
    }
    if !is_local_model(configured_model) {
        return LocalFailureAction::RetryRemote(configured_model.to_string());
    }
    LocalFailureAction::Explain(format!(
        "I couldn't reach the local model `{effective_model}` at {host}, and no \
         remote model is configured to fall back to.\n\n\
         To fix this, either start the local server (`ollama serve`), or point \
         this agent at a remote model by setting `[agent] model` in its TOML \
         (for example `model = \"claude-sonnet-4-6\"`)."
    ))
}

/// Re-run the conversational turn against `remote_model`.
///
/// Why: #3766 — keeps the (long) retry call out of `history.rs`, which sits
/// against the 500-SLOC production cap, and guarantees the retry uses the
/// agent's REMOTE sampling knobs rather than the local route's overrides.
/// What: one `chat_with_tools_gated` call with an empty tool registry — a
/// conversational reply needs no tools. `remote_model` is whatever
/// [`local_failure_action`] returned as `RetryRemote`, so it is never local.
/// Test: covered indirectly by the ctrl integration tests; the model-selection
/// decision itself is unit-tested in `local_fallback_tests.rs`.
pub(super) async fn retry_remote(
    client: &async_openai::Client<async_openai::config::OpenAIConfig>,
    remote_model: &str,
    messages: Vec<async_openai::types::ChatCompletionRequestMessage>,
    pm_cfg: &crate::agents::AgentConfig,
) -> anyhow::Result<(String, crate::perf::TokenUsage)> {
    let adapter = crate::llm::adapter::adapter_for_model(remote_model);
    crate::llm::chat_with_tools_gated(
        client,
        remote_model,
        &*adapter,
        messages,
        std::sync::Arc::new(crate::tools::ToolRegistry::new()),
        None,
        pm_cfg.llm.temperature,
        pm_cfg.llm.max_tokens,
        2,
        false,
        None,
        false,
        pm_cfg.llm.strict_tool_discipline(),
        pm_cfg.llm.use_anthropic_direct,
        &pm_cfg.llm.stop_sequences,
    )
    .await
    .inspect_err(|e| tracing::error!(error = %e, "#3766 remote fallback also failed"))
}

// Sibling `_tests.rs` file (this crate's `#[path=...]` pattern).
#[cfg(test)]
#[path = "local_fallback_tests.rs"]
mod tests;
