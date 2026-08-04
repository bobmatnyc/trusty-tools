//! CTRL-turn LLM dispatch + end-of-turn side-effect drain.
//!
//! Why: The LLM call (credential routing, local-ollama fast-path, REST/CLI
//! branch) and the side-effect drain (start_pm / initiate_self_task / stop_task)
//! are the mechanical tail of a ctrl turn. Splitting them from the
//! state-preparation + prompt-building keeps both files under the line cap.
//! What: `dispatch_ctrl_turn_llm`, `run_ctrl_turn_via_claude_cli`,
//! `run_ctrl_turn_via_rest`, and `drain_ctrl_turn_side_effects`.
//! Test: Indirect — exercised via the REPL integration tests.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};

use crate::agents::AgentConfig;
use crate::llm;
use crate::tools::ToolRegistry;

use super::super::claude_cli::run_pm_task_via_claude_cli;
use super::super::config::{
    SessionOverrides, apply_credential_routing, resolve_overridden_credentials,
};
// #4788: the ONE local-failure recovery policy, shared with the PM route.
use super::super::pm_task::local_fallback::{
    LocalFailureAction, local_failure_action, retry_remote,
};
use super::super::state::{Ctrl, PmMsg};
use super::super::util::drain_slot;
use super::CtrlTurnSideEffects;

/// Resolve and apply credential routing for a ctrl turn (#408).
///
/// Why: The legacy stdin REPL path (`ctrl_chat_turn`) historically called
/// `llm::chat()` against a hardcoded `CTRL_MODEL` without consulting the
/// credential layer, so it always routed through OpenRouter and silently
/// ignored `ANTHROPIC_API_KEY` (AnthropicDirect) and
/// `CLAUDE_CODE_OAUTH_TOKEN` (ClaudeCode). This helper mirrors the ratatui
/// reference path (`run_pm_task_with_history`): it honors an optional
/// session `/model` override, then resolves credentials through the canonical
/// `resolve_overridden_credentials` (which honors a `/provider` override and
/// otherwise falls back to `pick_credentials` priority ClaudeCode >
/// AnthropicDirect > OpenRouter), then applies routing to `cfg`.
/// What: Mutates `cfg` in place (model id, `use_anthropic_direct`,
/// OpenRouter prefix qualification) and returns the resolved
/// `LlmCredentials` plus the claude-CLI short-circuit flag. Pure aside from
/// reading process env via `resolve_overridden_credentials`.
/// Test: `dispatch::tests::ctrl_creds_prefers_anthropic_direct_over_openrouter`,
/// `ctrl_creds_falls_back_to_openrouter`, and
/// `ctrl_creds_model_override_applied`.
fn resolve_ctrl_turn_credentials(
    cfg: &mut AgentConfig,
    overrides: &SessionOverrides,
) -> Result<(llm::credentials::LlmCredentials, bool)> {
    if let Some(ref m) = overrides.model {
        tracing::debug!(model = %m, "ctrl_chat_turn: applying /model session override");
        cfg.agent.model = m.clone();
    }
    let creds = resolve_overridden_credentials(cfg, overrides.provider.as_deref())?;
    let claude_cli_short_circuit = apply_credential_routing(cfg, &creds);
    Ok((creds, claude_cli_short_circuit))
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn dispatch_ctrl_turn_llm(
    ctrl: &Ctrl,
    user_input: &str,
    system_prompt: &str,
    agent_cfg: AgentConfig,
    registry: ToolRegistry,
    mcp_cfg: &crate::mcp::GlobalConfig,
    dispatch_t0: std::time::Instant,
) -> Result<String> {
    let client = llm::create_client()?;

    let mut routed_cfg = agent_cfg;
    tracing::info!(
        elapsed_ms = dispatch_t0.elapsed().as_millis() as u64,
        agent = %routed_cfg.agent.name,
        runner = ?routed_cfg.agent.runner,
        model = %routed_cfg.agent.model,
        use_anthropic_direct = routed_cfg.llm.use_anthropic_direct,
        "ctrl_chat_turn: stage1 config loaded"
    );

    // TODO(#408): the ctrl stdin REPL does not yet expose `/model` and
    // `/provider` slash commands (those live only in the ratatui `src/repl/`
    // ReplState today). When session overrides are plumbed into `Ctrl`, pass
    // them here instead of the default sentinel. Until then `Default` resolves
    // to the env-driven `pick_credentials` priority, which is the fix for the
    // original "always OpenRouter" bug.
    let overrides = SessionOverrides::default();
    let (creds, claude_cli_short_circuit) =
        resolve_ctrl_turn_credentials(&mut routed_cfg, &overrides)?;
    tracing::info!(
        elapsed_ms = dispatch_t0.elapsed().as_millis() as u64,
        creds = creds.label(),
        claude_cli_short_circuit,
        model_after_routing = %routed_cfg.agent.model,
        use_anthropic_direct = routed_cfg.llm.use_anthropic_direct,
        "ctrl_chat_turn: stage2 credentials resolved"
    );

    if claude_cli_short_circuit {
        run_ctrl_turn_via_claude_cli(ctrl, &routed_cfg, system_prompt, user_input).await
    } else {
        run_ctrl_turn_via_rest(
            &client,
            user_input,
            system_prompt,
            &routed_cfg,
            registry,
            mcp_cfg,
            dispatch_t0,
        )
        .await
    }
}

pub(crate) async fn run_ctrl_turn_via_claude_cli(
    ctrl: &Ctrl,
    routed_cfg: &AgentConfig,
    system_prompt: &str,
    user_input: &str,
) -> Result<String> {
    let project_for_cli = ctrl
        .self_project
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let mut cli_cfg = routed_cfg.clone();
    cli_cfg.system_prompt.content = system_prompt.to_string();
    run_pm_task_via_claude_cli(&project_for_cli, &cli_cfg, user_input, &[], "").await
}

/// The model `run_ctrl_turn_via_rest` actually calls for this turn.
///
/// Why: #4788 — a disabled local-inference gate does NOT mean "no local call".
/// The route falls through to the agent's OWN configured model, which for any
/// ctrl config still carrying an `ollama/` slug is itself local. That is how
/// `local_inference.enabled = false` produced an *unrecoverable* local
/// transport failure under the old flag-gated error arm, and it is why recovery
/// may never take a preference flag as an input.
/// What: the local-inference model when the gate qualified this turn, else the
/// agent's configured model.
/// Test: `ctrl_turn_gate_off_still_attempts_the_agents_own_local_model`,
/// `ctrl_turn_recovers_in_both_local_gate_states`.
fn effective_ctrl_turn_model(
    local_qualifies: bool,
    local_model: &str,
    agent_model: &str,
) -> String {
    if local_qualifies {
        local_model.to_string()
    } else {
        agent_model.to_string()
    }
}

/// Run one ctrl turn against the REST LLM path.
///
/// Why: the standalone / no-PM-attached ctrl REPL's conversational surface.
/// What: one `chat_with_tools_gated` call against
/// [`effective_ctrl_turn_model`], with the end-of-turn tool registry armed.
/// Error contract (#4788): a TRANSPORT failure of a model that resolved to a
/// LOCAL adapter is recovered — retried against a proven-remote model, or
/// answered with an actionable `Ok` message when none is configured. Every
/// other failure (429/401/500 from either a local or remote model, and any
/// remote transport failure) propagates unchanged. The raw transport error is
/// never the user's reply.
/// Test: `ctrl_turn_recovers_in_both_local_gate_states`,
/// `ctrl_turn_gate_on_falls_back_to_a_distinct_remote_model`,
/// `ctrl_turn_propagates_real_errors`.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_ctrl_turn_via_rest(
    client: &async_openai::Client<async_openai::config::OpenAIConfig>,
    user_input: &str,
    system_prompt: &str,
    routed_cfg: &AgentConfig,
    registry: ToolRegistry,
    mcp_cfg: &crate::mcp::GlobalConfig,
    dispatch_t0: std::time::Instant,
) -> Result<String> {
    use async_openai::types::{
        ChatCompletionRequestMessage, ChatCompletionRequestSystemMessageArgs,
        ChatCompletionRequestUserMessageArgs,
    };
    let messages: Vec<ChatCompletionRequestMessage> = vec![
        ChatCompletionRequestSystemMessageArgs::default()
            .content(system_prompt.to_string())
            .build()
            .context("failed to build ctrl_chat_turn system message")?
            .into(),
        ChatCompletionRequestUserMessageArgs::default()
            .content(user_input)
            .build()
            .context("failed to build ctrl_chat_turn user message")?
            .into(),
    ];
    let local_cfg = &mcp_cfg.local_inference;
    let intent_class = crate::intent::classify_intent(user_input);
    let local_qualifies = local_cfg.enabled
        && crate::local_inference::qualifies_for_local_inference(&intent_class, user_input)
        && crate::local_inference::is_ollama_available_cached(&local_cfg.ollama_host).await;
    let effective_model =
        effective_ctrl_turn_model(local_qualifies, &local_cfg.model, &routed_cfg.agent.model);
    let (effective_max_tokens, effective_use_direct) = if local_qualifies {
        tracing::info!(
            local_model = %local_cfg.model,
            ?intent_class,
            "ctrl_chat_turn: routing to local ollama fast-path"
        );
        (local_cfg.max_tokens, false)
    } else {
        (
            routed_cfg.llm.max_tokens.max(1024),
            routed_cfg.llm.use_anthropic_direct,
        )
    };

    let adapter = llm::adapter::adapter_for_model(&effective_model);
    let registry_arc = Arc::new(registry);
    let llm_t0 = std::time::Instant::now();
    tracing::info!(
        elapsed_ms = dispatch_t0.elapsed().as_millis() as u64,
        provider = ?adapter.provider(),
        model = %effective_model,
        use_anthropic_direct = effective_use_direct,
        local_route = local_qualifies,
        "ctrl_chat_turn: stage3 LLM call starting"
    );
    let local_call_result = llm::chat_with_tools_gated(
        client,
        &effective_model,
        &*adapter,
        messages.clone(),
        registry_arc,
        None,
        0.2,
        effective_max_tokens,
        2,
        false,
        None,
        false,
        routed_cfg.llm.strict_tool_discipline(),
        effective_use_direct,
        &routed_cfg.llm.stop_sequences,
    )
    .await;

    let mut used_remote_fallback = false;
    let (text, _usage) = match local_call_result {
        Ok(pair) => pair,
        // #4788: recover from ANY failed LOCAL attempt, not just the
        // `local_qualifies` route. The old arm was
        // `if local_qualifies && local_cfg.fallback_on_error`, and
        // `local_qualifies` ANDs in `local_inference.enabled` — so the setting
        // meaning "avoid Ollama" was exactly what made an Ollama transport
        // error unrecoverable, leaking a raw `Connection refused` as the
        // user's assistant reply. No preference flag is an input any more.
        Err(e) => {
            let remote_model = match local_failure_action(
                &effective_model,
                &routed_cfg.agent.model,
                llm::is_transport_error(&e),
                &llm::adapter::ollama_host(),
            ) {
                LocalFailureAction::RetryRemote(m) => m,
                // No distinct remote model to retry: an actionable message
                // beats a raw transport error as the assistant's answer.
                LocalFailureAction::Explain(msg) => {
                    tracing::error!(error = %e, model = %effective_model, "local unreachable");
                    return Ok(msg);
                }
                LocalFailureAction::Propagate => {
                    tracing::error!(error = %e, "ctrl_chat_turn LLM call failed");
                    return Err(e);
                }
            };
            tracing::warn!(error = %e, %remote_model, "local failed, retrying remote: {e:#}");
            used_remote_fallback = true;
            // The retry runs with an EMPTY tool registry (`retry_remote`), NOT
            // this route's `registry_arc`. A full-registry retry would be a
            // DOUBLE-EXECUTION hazard: `build_ctrl_registry` arms ~20 tools
            // with IMMEDIATE effects (git commit/push, ticket create/update,
            // actions_trigger, MCP add/remove, MoveFile/CreateDir, …), and
            // with `max_turns = 2` the failed attempt may already have
            // executed one — turn 0 runs a tool, turn 1 dies on transport.
            // Because `messages` is the ORIGINAL prompt (the first call took a
            // clone), the retry re-asks the same question with no record of
            // that tool call, so a remote model re-solving it re-invokes the
            // tool: duplicate commit, duplicate ticket, duplicate CI trigger.
            // The deferred side effects are unaffected — start_pm /
            // initiate_self_task / stop_task write to pending slots that
            // survive the failed attempt and still drain after this returns.
            retry_remote(client, &remote_model, messages, routed_cfg).await?
        }
    };
    let text = if used_remote_fallback {
        format!("[⚡ Ollama unavailable — using OpenRouter]\n\n{text}")
    } else {
        text
    };
    tracing::info!(
        llm_ms = llm_t0.elapsed().as_millis() as u64,
        dispatch_ms = dispatch_t0.elapsed().as_millis() as u64,
        response_chars = text.len(),
        "ctrl_chat_turn: stage4 LLM call complete"
    );
    Ok(text)
}

pub(crate) async fn drain_ctrl_turn_side_effects(
    ctrl: &mut Ctrl,
    side_effects: &CtrlTurnSideEffects,
    outputs: &mut Vec<String>,
) {
    let to_connect = drain_slot(&side_effects.pending_connect);
    if let Some(path) = to_connect {
        match ctrl.connect(&path).await {
            Ok(msg) => outputs.push(msg),
            Err(e) => outputs.push(format!("start_pm error: {e:#}")),
        }
    }

    let to_self_task = drain_slot(&side_effects.pending_self_task);
    if let Some(task_text) = to_self_task {
        match ctrl.dispatch_task(task_text).await {
            Ok(out) => outputs.push(out),
            Err(e) => outputs.push(format!("initiate_self_task dispatch error: {e:#}")),
        }
    }

    let to_stop = drain_slot(&side_effects.pending_stop);
    if let Some(target_name) = to_stop {
        let key_opt = ctrl
            .pms
            .iter()
            .find(|(_, h)| h.name == target_name)
            .map(|(k, _)| k.clone());
        if let Some(key) = key_opt {
            if let Some(handle) = ctrl.pms.remove(&key) {
                let _ = handle.tx.send(PmMsg::Shutdown).await;
                if ctrl.active.as_deref() == Some(key.as_str()) {
                    ctrl.active = None;
                }
                let mut connected = ctrl.connected_pms.lock().await;
                connected.remove(&handle.name);
                outputs.push(format!("Stopped PM[{}]", handle.name));
            }
        } else {
            outputs.push(format!("stop_task: no PM named {target_name}"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::AgentConfig;
    use crate::llm::credentials::LlmCredentials;
    use serial_test::serial;

    /// Helper: clear all three credential env vars so each test starts from a
    /// known-empty environment. SAFETY: every test below is `#[serial]` AND
    /// holds `crate::test_env::ENV_LOCK` to serialize against the rest of the
    /// crate's env-touching tests (#274 / #408).
    ///
    /// #3464: forces the process-global `.env.local` `OnceLock` loader to
    /// have already fired before removing anything — see
    /// `crate::test_env::force_env_local_loaded`'s docs for why a
    /// remove-only helper is not reliably idempotent against that loader.
    fn clear_creds_env() {
        crate::test_env::force_env_local_loaded();
        unsafe {
            std::env::remove_var("OPENROUTER_API_KEY");
            std::env::remove_var("ANTHROPIC_API_KEY");
            std::env::remove_var("CLAUDE_CODE_OAUTH_TOKEN");
        }
    }

    /// Regression for #408: with both ANTHROPIC_API_KEY and OPENROUTER_API_KEY
    /// set, the legacy ctrl stdin path must route AnthropicDirect (flipping
    /// `use_anthropic_direct`), NOT silently downgrade to OpenRouter.
    #[test]
    #[serial]
    fn ctrl_creds_prefers_anthropic_direct_over_openrouter() {
        let _g = crate::test_env::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        clear_creds_env();
        unsafe {
            std::env::set_var("ANTHROPIC_API_KEY", "sk-ant-api03-test");
            std::env::set_var("OPENROUTER_API_KEY", "sk-or-v1-test");
        }
        let mut cfg = AgentConfig::ctrl_default();
        cfg.llm.use_anthropic_direct = false;
        let (creds, short_circuit) =
            resolve_ctrl_turn_credentials(&mut cfg, &SessionOverrides::default())
                .expect("credentials must resolve when env vars are set");
        assert_eq!(creds, LlmCredentials::AnthropicDirect);
        assert!(!short_circuit);
        assert!(
            cfg.llm.use_anthropic_direct,
            "AnthropicDirect must flip use_anthropic_direct"
        );
        clear_creds_env();
    }

    /// When only OPENROUTER_API_KEY is set the legacy path must still work
    /// (preserve pre-#408 behavior) and route via OpenRouter.
    #[test]
    #[serial]
    fn ctrl_creds_falls_back_to_openrouter() {
        let _g = crate::test_env::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        clear_creds_env();
        unsafe {
            std::env::set_var("OPENROUTER_API_KEY", "sk-or-v1-test");
        }
        let mut cfg = AgentConfig::ctrl_default();
        cfg.llm.use_anthropic_direct = false;
        let (creds, short_circuit) =
            resolve_ctrl_turn_credentials(&mut cfg, &SessionOverrides::default())
                .expect("OpenRouter-only env must resolve");
        assert_eq!(creds, LlmCredentials::OpenRouter);
        assert!(!short_circuit);
        assert!(
            !cfg.llm.use_anthropic_direct,
            "OpenRouter must not flip use_anthropic_direct"
        );
        clear_creds_env();
    }

    /// With no credentials configured the legacy path must surface an error
    /// instead of defaulting to OpenRouter.
    ///
    /// Also sandboxes `$HOME` to a fresh, never-written-to tempdir (a
    /// pre-existing gap independent of #3464 — this test cleared only the
    /// three env vars and never isolated the secure-store tier, so on any
    /// machine/CI runner with a real `~/.trusty-tools/credentials.toml`
    /// entry for openrouter/anthropic, `resolve_ctrl_turn_credentials` would
    /// resolve a real credential from the store and this always-erroring
    /// expectation would fail deterministically). Holds `HOME_LOCK` for the
    /// whole body per `test_env`'s documented convention.
    #[test]
    #[serial]
    fn ctrl_creds_errors_when_nothing_configured() {
        let _g = crate::test_env::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _home_guard = crate::test_env::HOME_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        clear_creds_env();

        let tmp = tempfile::TempDir::new().expect("tempdir");
        let prev_home = std::env::var_os("HOME");
        // SAFETY: HOME_LOCK held for the entire test body.
        unsafe {
            std::env::set_var("HOME", tmp.path());
        }

        let mut cfg = AgentConfig::ctrl_default();
        let res = resolve_ctrl_turn_credentials(&mut cfg, &SessionOverrides::default());
        assert!(
            res.is_err(),
            "no credentials must be an error, not a default"
        );

        // SAFETY: HOME_LOCK still held.
        unsafe {
            match prev_home {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
        }
    }

    /// A session `/model` override (when plumbed) must replace the agent model
    /// before credential routing qualifies it for OpenRouter.
    #[test]
    #[serial]
    fn ctrl_creds_model_override_applied() {
        let _g = crate::test_env::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        clear_creds_env();
        unsafe {
            std::env::set_var("OPENROUTER_API_KEY", "sk-or-v1-test");
        }
        let mut cfg = AgentConfig::ctrl_default();
        let overrides = SessionOverrides {
            model: Some("claude-haiku-4-5".to_string()),
            ..Default::default()
        };
        let (creds, _short_circuit) = resolve_ctrl_turn_credentials(&mut cfg, &overrides)
            .expect("override path must resolve with OpenRouter set");
        assert_eq!(creds, LlmCredentials::OpenRouter);
        // OpenRouter routing qualifies the bare claude id with the provider
        // prefix, proving the override flowed through credential routing.
        assert_eq!(cfg.agent.model, "anthropic/claude-haiku-4-5");
        clear_creds_env();
    }

    // ---- #4788: local-failure recovery on the ctrl_turn REPL route ----
    //
    // Mirrors `pm_task::dispatch::local_fallback_tests` for this second
    // conversational surface. The old arm here was
    // `Err(e) if local_qualifies && local_cfg.fallback_on_error`, with a bare
    // `Err(e) => return Err(e)` below it — so a user with
    // `local_inference.enabled = false` got no retry at all and received
    //
    //   error sending request for url (http://localhost:11434/...):
    //   Connection refused (os error 61)
    //
    // as their assistant's reply. These tests drive the same two inputs the
    // production arm computes: the model this route actually attempted
    // (`effective_ctrl_turn_model`) and the agent's configured model.

    const OLLAMA_SLUG: &str = "ollama/qwen3:30b";
    const HOST: &str = "http://localhost:11434";
    /// Reserved port, never listening — a real `Connection refused`.
    const DEAD_LOCAL_HOST: &str = "http://127.0.0.1:1";
    /// The call failed at the transport level (`Connection refused`).
    const TRANSPORT: bool = true;

    /// Mirror of `run_ctrl_turn_via_rest`'s error arm: derive the model this
    /// route attempted, then ask the shared policy what to do about its failure.
    fn action_for(
        local_qualifies: bool,
        local_model: &str,
        agent_model: &str,
        transport: bool,
    ) -> LocalFailureAction {
        let effective = effective_ctrl_turn_model(local_qualifies, local_model, agent_model);
        local_failure_action(&effective, agent_model, transport, HOST)
    }

    /// The defect. With ctrl's on-disk config carrying an `ollama/` slug, BOTH
    /// `local_inference.enabled` states attempt a local model — and the old
    /// flag-gated arm recovered in NEITHER: `false` skipped the arm entirely,
    /// `true` retried the same unreachable slug. Recovery must now happen in
    /// both, because the preference is not an input to it.
    #[test]
    fn ctrl_turn_recovers_in_both_local_gate_states() {
        for (label, local_qualifies) in [("enabled = false", false), ("enabled = true", true)] {
            let action = action_for(local_qualifies, OLLAMA_SLUG, OLLAMA_SLUG, TRANSPORT);
            assert_ne!(
                action,
                LocalFailureAction::Propagate,
                "{label}: a local transport failure must never propagate raw"
            );
            let LocalFailureAction::Explain(msg) = action else {
                panic!("{label}: expected Explain (no distinct remote configured)");
            };
            assert!(msg.contains(HOST), "{label}: must name the host: {msg}");
            assert!(
                msg.contains("[agent] model"),
                "{label}: must name the config key to change: {msg}"
            );
            assert!(
                !msg.contains("Connection refused") && !msg.contains("os error"),
                "{label}: must not leak transport-error text to the user: {msg}"
            );
        }
    }

    /// Gate ON with a REMOTE agent model: the route attempts the local
    /// inference model, and a transport failure retries a genuinely distinct
    /// remote target — never the slug that just failed.
    #[test]
    fn ctrl_turn_gate_on_falls_back_to_a_distinct_remote_model() {
        let action = action_for(true, "ollama/llama3", "claude-sonnet-4-6", TRANSPORT);
        assert_eq!(
            action,
            LocalFailureAction::RetryRemote("claude-sonnet-4-6".to_string())
        );
    }

    /// Gate OFF does not mean "no local call" — the route falls through to the
    /// agent's OWN model. This is the mechanism the old flag-gated arm missed.
    #[test]
    fn ctrl_turn_gate_off_still_attempts_the_agents_own_local_model() {
        assert_eq!(
            effective_ctrl_turn_model(false, "ollama/llama3", OLLAMA_SLUG),
            OLLAMA_SLUG
        );
        assert_eq!(
            effective_ctrl_turn_model(true, "ollama/llama3", OLLAMA_SLUG),
            "ollama/llama3"
        );
    }

    /// The test that actually pins the fix. Everything above is a decision
    /// test — it re-derives the inputs and would keep passing against the
    /// pre-fix guard. This one drives the REAL `run_ctrl_turn_via_rest`:
    /// `OLLAMA_HOST` points at a dead port, so `chat_with_tools_gated` returns
    /// a genuine `Connection refused` transport `Err`, and the assertion is
    /// that the user gets an explanatory `Ok` instead of that error.
    ///
    /// Restore the pre-fix arm (`Err(e) if local_qualifies &&
    /// local_cfg.fallback_on_error` + `Err(e) => return Err(e)`) and this test
    /// fails: `local_inference.enabled = false` forces `local_qualifies =
    /// false`, so the guard never matches and the raw error propagates.
    // The env guard MUST outlive the awaited call — `OLLAMA_HOST` has to stay
    // set for the whole dispatch. Sound here for the reason `test_env.rs`
    // documents: the default `#[tokio::test]` flavor is current-thread, so
    // every await stays on the one OS thread, and `#[serial]` keeps any other
    // env-touching test out of the window.
    #[allow(
        clippy::await_holding_lock,
        reason = "#4788: env must stay set across the awaited dispatch; current-thread runtime + #[serial]"
    )]
    #[tokio::test]
    #[serial]
    async fn ctrl_turn_rest_recovers_from_a_real_local_transport_failure() {
        let _g = crate::test_env::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        clear_creds_env();
        // SAFETY: ENV_LOCK held for the whole test body.
        unsafe {
            // A credential must resolve or `create_client` bails before the
            // code under test runs. Never used: the local call fails at the
            // transport layer and the agent's model is local, so the recovery
            // is `Explain` — no remote request is ever issued.
            std::env::set_var("OPENROUTER_API_KEY", "sk-or-v1-test");
            // Port 1 is reserved and never listening: a real `Connection
            // refused` from the local adapter, not a simulated error.
            std::env::set_var("OLLAMA_HOST", DEAD_LOCAL_HOST);
        }

        // The owner's live shape: ctrl's own model is a local slug, and local
        // inference is DISABLED — the combination the old guard could not
        // recover from.
        let mut cfg = AgentConfig::ctrl_default();
        cfg.agent.model = OLLAMA_SLUG.to_string();
        cfg.llm.use_anthropic_direct = false;
        let mut mcp_cfg = crate::mcp::GlobalConfig::default();
        mcp_cfg.local_inference.enabled = false;
        // Proves no preference flag can switch recovery off — the old arm
        // required this to be true AND `enabled` to be true.
        mcp_cfg.local_inference.fallback_on_error = false;

        let client = llm::create_client().expect("client construction");
        let result = run_ctrl_turn_via_rest(
            &client,
            "what is my status?",
            "You are ctrl.",
            &cfg,
            ToolRegistry::new(),
            &mcp_cfg,
            std::time::Instant::now(),
        )
        .await;

        let reply = result.expect("a local transport failure must not reach the user as Err");
        assert!(
            !reply.contains("Connection refused") && !reply.contains("os error"),
            "must not leak transport-error text as the assistant's reply: {reply}"
        );
        assert!(
            reply.contains(DEAD_LOCAL_HOST),
            "must name the unreachable host: {reply}"
        );
        assert!(
            reply.contains("[agent] model"),
            "must name the config key to change: {reply}"
        );

        // SAFETY: ENV_LOCK still held.
        unsafe {
            std::env::remove_var("OLLAMA_HOST");
        }
        clear_creds_env();
    }

    /// The fix must not swallow real errors: a REMOTE transport failure, and a
    /// local model that ANSWERED with an error (429/401/500), both propagate.
    #[test]
    fn ctrl_turn_propagates_real_errors() {
        assert_eq!(
            action_for(false, "ollama/llama3", "claude-sonnet-4-6", TRANSPORT),
            LocalFailureAction::Propagate,
            "remote transport failure is not a local-recovery case"
        );
        assert_eq!(
            action_for(true, "ollama/llama3", "claude-sonnet-4-6", false),
            LocalFailureAction::Propagate,
            "a local server that answered with an error returned a real error"
        );
    }
}
