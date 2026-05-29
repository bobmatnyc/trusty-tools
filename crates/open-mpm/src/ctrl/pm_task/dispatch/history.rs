//! History-aware PM task dispatch (tool-armed delegation + conversational
//! fast-path).
//!
//! Why: `run_pm_task_with_history` is the canonical multi-turn PM round-trip —
//! it carries prior turns, routes conversational inputs through a local-ollama
//! fast-path, and otherwise arms the delegation tool registry. It's the largest
//! single function in the ctrl module, so it gets its own file.
//! What: `run_pm_task_with_history`.
//! Test: Exercised end-to-end via the ctrl integration tests; the pure helpers
//! it calls (`extract_name_from_input`) are unit-tested in
//! `ctrl::tests::pm_task_tests`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};

use crate::events::{self, Event};
use crate::intent::{IntentClass, classify_intent};
use crate::llm;
use crate::subprocess::SubprocessAgentRunner;
use crate::tools::{AgentRunner, ToolRegistry, delegate::DelegateToAgentTool};

use super::super::super::claude_cli::run_pm_task_via_claude_cli;
use super::super::super::config::{
    SessionOverrides, apply_credential_routing, build_deployment_footer, build_user_context_prefix,
    recall_project_memories, resolve_agent_config, resolve_overridden_credentials,
};
use super::super::super::handlers::{
    AddProjectTool, CreateDirTool, ListProjectsTool, MoveFileTool, RemoveProjectTool,
    SetActiveProjectTool, StopTaskTool, build_tm_context_block, register_ticketing_tools,
};
use super::super::super::state::ConversationTurn;
use super::super::helpers::{extract_name_from_input, save_name_to_profile};

/// Multi-turn variant of `run_pm_task_with_session` that prepends `history`
/// as alternating user/assistant messages before the current `user_input`.
///
/// Why: Lets the REPL hold a back-and-forth conversation with CTRL/PM
/// instead of every task being stateless. The single-turn entry point
/// (`run_pm_task_with_session`) now just delegates here with an empty slice.
/// What: Builds a `Vec<ChatCompletionRequestMessage>` from `history` (user,
/// assistant, user, assistant, …) followed by the new user message, then
/// runs the same conversational fast-path / tool-armed delegation logic as
/// the original function — but routed through `chat_with_tools_gated` so the
/// prior turns are carried into the request.
/// Test: `ctrl::tests::ctrl_history_builds_messages` (history -> message
/// sequence); the REPL integration is exercised manually for now since LLM
/// calls aren't part of the unit test surface.
pub async fn run_pm_task_with_history(
    project_path: &Path,
    user_input: &str,
    history: &[ConversationTurn],
    session_id: Option<String>,
    overrides: SessionOverrides,
) -> Result<String> {
    use async_openai::types::{
        ChatCompletionRequestAssistantMessageArgs, ChatCompletionRequestMessage,
        ChatCompletionRequestSystemMessageArgs, ChatCompletionRequestUserMessageArgs,
    };

    tracing::debug!(
        project = %project_path.display(),
        history_turns = history.len(),
        input_len = user_input.len(),
        "ctrl::run_pm_task_with_history entered"
    );

    let sid = session_id.unwrap_or_default();
    events::publish(Event::PmThinking {
        session_id: sid.clone(),
        text: events::preview(user_input, 240),
    });

    let config_dir = project_path.join(".open-mpm").join("agents");
    let (mut pm_cfg, _pm_cfg_path) = resolve_agent_config(project_path).await?;

    if let Some(ref m) = overrides.model {
        tracing::debug!(model = %m, "applying /model session override");
        pm_cfg.agent.model = m.clone();
    }

    let creds = resolve_overridden_credentials(&mut pm_cfg, overrides.provider.as_deref())?;
    let claude_cli_short_circuit = apply_credential_routing(&mut pm_cfg, &creds);
    tracing::info!(
        agent = %pm_cfg.agent.name,
        runner = ?pm_cfg.agent.runner,
        model = %pm_cfg.agent.model,
        creds = creds.label(),
        claude_cli_short_circuit,
        use_anthropic_direct = pm_cfg.llm.use_anthropic_direct,
        "run_pm_task_with_history: credentials resolved"
    );

    // Inject deployment context into the system prompt.
    {
        let runner_label = match creds {
            llm::credentials::LlmCredentials::ClaudeCode => "claude-code (ClaudeCodeAgentRunner)",
            llm::credentials::LlmCredentials::AnthropicDirect => "anthropic-direct",
            llm::credentials::LlmCredentials::OpenRouter => "openrouter",
        };
        let skills_count = pm_cfg
            .system_prompt
            .skills
            .as_ref()
            .map(|s| s.len())
            .unwrap_or(0);
        let project_label = project_path.display().to_string();
        let deployment_block = build_deployment_footer(
            &pm_cfg.agent.name,
            runner_label,
            &pm_cfg.agent.model,
            crate::build_info::VERSION,
            skills_count,
            None,
            None,
            &project_label,
            None,
        );
        pm_cfg.system_prompt.content.push_str(&deployment_block);
    }

    if claude_cli_short_circuit {
        tracing::info!("ctrl PM turn → claude CLI (no API-key credential available)");
        return run_pm_task_via_claude_cli(project_path, &pm_cfg, user_input, history, &sid).await;
    }
    let client = llm::create_client()?;

    let _bedrock_env_guard = if llm::adapter::adapter_for_model(&pm_cfg.agent.model).provider()
        == llm::adapter::Provider::Bedrock
    {
        Some(crate::agents::in_process_runner::BedrockEnvGuard::install(
            pm_cfg.llm.aws_profile.as_deref(),
            pm_cfg.llm.aws_region.as_deref(),
        ))
    } else {
        None
    };

    // Build augmented system prompt with optional user profile context.
    let system_prompt: String = {
        let base = build_user_context_prefix(&pm_cfg.system_prompt.content);

        let runner_label = match pm_cfg.agent.runner {
            crate::agents::RunnerKind::Subprocess => "subprocess",
            crate::agents::RunnerKind::Inline => "inline",
            crate::agents::RunnerKind::ClaudeCode => "claude-code",
            crate::agents::RunnerKind::InProcess => "in-process",
        };
        let mut builder = crate::agents::prompt_builder::SystemPromptBuilder::new(base)
            .with_agent_context(pm_cfg.agent.model.as_str(), runner_label);
        let mcp_cfg = crate::mcp::GlobalConfig::load().await;
        if let Some(section) = mcp_cfg.render_prompt_section(&pm_cfg.agent.role) {
            builder = builder.add_mcp_layer(section);
        }
        let q = &user_input[..200.min(user_input.len())];
        let memories = recall_project_memories(project_path, q, 5).await;
        if !memories.is_empty() {
            builder = builder.add_memory_layer(memories);
        }
        let mut prompt = builder.build();
        let tm_state_dir = project_path.join(".open-mpm").join("state");
        let tm_block = build_tm_context_block(&tm_state_dir).await;
        if !tm_block.is_empty() {
            prompt.push_str("\n\n");
            prompt.push_str(&tm_block);
        }
        prompt
    };

    let mut initial_messages: Vec<ChatCompletionRequestMessage> = Vec::new();
    initial_messages.push(
        ChatCompletionRequestSystemMessageArgs::default()
            .content(system_prompt.clone())
            .build()
            .context("failed to build system message")?
            .into(),
    );
    let truncated_history: Vec<ConversationTurn> =
        crate::compress::truncate_history(history, &crate::compress::TokenBudget::default());
    for turn in &truncated_history {
        initial_messages.push(
            ChatCompletionRequestUserMessageArgs::default()
                .content(turn.user.clone())
                .build()
                .context("failed to build history user message")?
                .into(),
        );
        initial_messages.push(
            ChatCompletionRequestAssistantMessageArgs::default()
                .content(turn.assistant.clone())
                .build()
                .context("failed to build history assistant message")?
                .into(),
        );
    }
    initial_messages.push(
        ChatCompletionRequestUserMessageArgs::default()
            .content(user_input)
            .build()
            .context("failed to build current user message")?
            .into(),
    );

    // Fast path: conversational inputs skip the delegation pipeline.
    if matches!(classify_intent(user_input), IntentClass::Conversational) {
        tracing::info!("intent classifier: Conversational fast path");

        if let Some(name) = extract_name_from_input(user_input) {
            save_name_to_profile(&name);
            let greeting = format!(
                "Nice to meet you, {}! What would you like to build today?",
                name
            );
            events::publish(Event::PmThinking {
                session_id: sid,
                text: greeting.clone(),
            });
            return Ok(greeting);
        }

        let local_global_cfg = crate::mcp::GlobalConfig::load().await;
        let local_cfg = &local_global_cfg.local_inference;
        let local_qualifies = local_cfg.enabled
            && crate::local_inference::qualifies_for_local_inference(
                &IntentClass::Conversational,
                user_input,
            )
            && crate::local_inference::is_ollama_available_cached(&local_cfg.ollama_host).await;
        let (effective_model, effective_max_tokens, effective_use_direct) = if local_qualifies {
            tracing::info!(
                local_model = %local_cfg.model,
                "run_pm_task_with_history: routing conversational to local ollama"
            );
            (local_cfg.model.clone(), local_cfg.max_tokens, false)
        } else {
            (
                pm_cfg.agent.model.clone(),
                pm_cfg.llm.max_tokens,
                pm_cfg.llm.use_anthropic_direct,
            )
        };

        let adapter = llm::adapter::adapter_for_model(&effective_model);
        let llm_t0 = std::time::Instant::now();
        tracing::info!(
            model = %effective_model,
            history_turns = history.len(),
            local_route = local_qualifies,
            "ctrl LLM call start (conversational fast path)"
        );
        let local_call = llm::chat_with_tools_gated(
            &client,
            &effective_model,
            &*adapter,
            initial_messages.clone(),
            Arc::new(ToolRegistry::new()),
            None,
            pm_cfg.llm.temperature,
            effective_max_tokens,
            2,
            false,
            None,
            false,
            effective_use_direct,
            &pm_cfg.llm.stop_sequences,
        )
        .await;
        let mut used_remote_fallback = false;
        let (content, _usage) = match local_call {
            Ok(pair) => pair,
            Err(e) if local_qualifies && local_cfg.fallback_on_error => {
                tracing::warn!(
                    error = %e,
                    "local inference failed, falling back to remote: {e:#}"
                );
                used_remote_fallback = true;
                let remote_adapter = llm::adapter::adapter_for_model(&pm_cfg.agent.model);
                llm::chat_with_tools_gated(
                    &client,
                    &pm_cfg.agent.model,
                    &*remote_adapter,
                    initial_messages.clone(),
                    Arc::new(ToolRegistry::new()),
                    None,
                    pm_cfg.llm.temperature,
                    pm_cfg.llm.max_tokens,
                    2,
                    false,
                    None,
                    false,
                    pm_cfg.llm.use_anthropic_direct,
                    &pm_cfg.llm.stop_sequences,
                )
                .await
                .inspect_err(|e| {
                    tracing::error!(error = %e, "ctrl::run_pm_task_with_history conversational fast-path remote fallback also failed")
                })?
            }
            Err(e) => {
                tracing::error!(error = %e, "ctrl::run_pm_task_with_history conversational fast-path LLM call failed");
                return Err(e);
            }
        };
        let content = if used_remote_fallback {
            format!("[⚡ Ollama unavailable — using OpenRouter]\n\n{content}")
        } else {
            content
        };
        tracing::info!(
            elapsed_ms = llm_t0.elapsed().as_millis() as u64,
            response_len = content.len(),
            "ctrl LLM call done (conversational fast path)"
        );
        events::publish(Event::PmThinking {
            session_id: sid,
            text: events::preview(&content, 240),
        });
        return Ok(content);
    }

    let runner: Arc<dyn AgentRunner> =
        Arc::new(SubprocessAgentRunner::new().with_config_dir(Some(config_dir.clone())));

    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(
        DelegateToAgentTool::new(runner).with_config_dir(config_dir.clone()),
    ));
    let stop_pending: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let active_project_slot: Arc<Mutex<Option<PathBuf>>> = Arc::new(Mutex::new(None));
    registry.register(Arc::new(AddProjectTool));
    registry.register(Arc::new(ListProjectsTool));
    registry.register(Arc::new(RemoveProjectTool));
    registry.register(Arc::new(StopTaskTool {
        snapshot: Vec::new(),
        pending_stop: stop_pending,
    }));
    registry.register(Arc::new(SetActiveProjectTool {
        active_project: active_project_slot,
    }));
    registry.register(Arc::new(MoveFileTool));
    registry.register(Arc::new(CreateDirTool));
    registry.register(Arc::new(
        crate::tools::web_search::BraveSearchTool::from_env(),
    ));
    {
        let project_root =
            std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        let search_tool =
            crate::tools::native_search::SearchCodeTool::new_auto(&project_root).await;
        registry.register(Arc::new(search_tool));
    }
    {
        let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        registry.register(Arc::new(crate::tools::run_bash::RunBashTool::new(cwd)));
    }
    for tool in crate::tools::mcp_tools::mcp_tool_executors() {
        registry.register(tool);
    }
    register_ticketing_tools(&mut registry).await;

    {
        let state_dir = project_path.join(".open-mpm").join("state");
        crate::tools::tm_tools::register_tm_tools_for_state_dir(&mut registry, &state_dir);
    }

    let adapter = llm::adapter::adapter_for_model(&pm_cfg.agent.model);
    let registry_arc = Arc::new(registry);
    let llm_t0 = std::time::Instant::now();
    tracing::info!(
        model = %pm_cfg.agent.model,
        history_turns = history.len(),
        "ctrl LLM call start (tool-armed delegation)"
    );
    let (content, _usage) = llm::chat_with_tools_gated(
        &client,
        &pm_cfg.agent.model,
        &*adapter,
        initial_messages,
        registry_arc,
        None,
        pm_cfg.llm.temperature,
        pm_cfg.llm.max_tokens,
        4,
        false,
        None,
        false,
        pm_cfg.llm.use_anthropic_direct,
        &pm_cfg.llm.stop_sequences,
    )
    .await
    .inspect_err(|e| {
        tracing::error!(error = %e, "ctrl::run_pm_task_with_history tool-armed delegation LLM call failed")
    })?;
    tracing::info!(
        elapsed_ms = llm_t0.elapsed().as_millis() as u64,
        response_len = content.len(),
        "ctrl LLM call done (tool-armed delegation)"
    );

    events::publish(Event::PmThinking {
        session_id: sid,
        text: events::preview(&content, 240),
    });
    Ok(content)
}
