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
    let (effective_model, effective_max_tokens, effective_use_direct) = if local_qualifies {
        tracing::info!(
            local_model = %local_cfg.model,
            ?intent_class,
            "ctrl_chat_turn: routing to local ollama fast-path"
        );
        (local_cfg.model.clone(), local_cfg.max_tokens, false)
    } else {
        (
            routed_cfg.agent.model.clone(),
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
        registry_arc.clone(),
        None,
        0.2,
        effective_max_tokens,
        2,
        false,
        None,
        false,
        effective_use_direct,
        &routed_cfg.llm.stop_sequences,
    )
    .await;

    let mut used_remote_fallback = false;
    let (text, _usage) = match local_call_result {
        Ok(pair) => pair,
        Err(e) if local_qualifies && local_cfg.fallback_on_error => {
            tracing::warn!(
                error = %e,
                "local inference failed, falling back to remote: {e:#}"
            );
            used_remote_fallback = true;
            let remote_adapter = llm::adapter::adapter_for_model(&routed_cfg.agent.model);
            llm::chat_with_tools_gated(
                client,
                &routed_cfg.agent.model,
                &*remote_adapter,
                messages,
                registry_arc,
                None,
                0.2,
                routed_cfg.llm.max_tokens.max(1024),
                2,
                false,
                None,
                false,
                routed_cfg.llm.use_anthropic_direct,
                &routed_cfg.llm.stop_sequences,
            )
            .await?
        }
        Err(e) => return Err(e),
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
    fn clear_creds_env() {
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
    #[test]
    #[serial]
    fn ctrl_creds_errors_when_nothing_configured() {
        let _g = crate::test_env::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        clear_creds_env();
        let mut cfg = AgentConfig::ctrl_default();
        let res = resolve_ctrl_turn_credentials(&mut cfg, &SessionOverrides::default());
        assert!(
            res.is_err(),
            "no credentials must be an error, not a default"
        );
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
}
