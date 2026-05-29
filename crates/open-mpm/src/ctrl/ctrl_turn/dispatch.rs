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
use super::super::config::apply_credential_routing;
use super::super::state::{Ctrl, PmMsg};
use super::super::util::drain_slot;
use super::CtrlTurnSideEffects;

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

    let creds = llm::credentials::pick_credentials(Some(routed_cfg.agent.runner))
        .ok_or_else(|| anyhow::anyhow!("{}", llm::credentials::missing_credentials_error()))?;
    let claude_cli_short_circuit = apply_credential_routing(&mut routed_cfg, &creds);
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
