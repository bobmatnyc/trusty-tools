//! Persona prompt and message assembly, preserving stable prefix and history order.
//! Tested through the full persona dispatch and memory test modules.
use super::super::super::super::config::{AgentIdentity, build_user_context_prefix};
use super::{ConversationTurn, classification, persona_memory};
use crate::agents::AgentConfig;
use anyhow::{Context, Result};
use async_openai::types::{
    ChatCompletionRequestAssistantMessageArgs, ChatCompletionRequestMessage,
    ChatCompletionRequestSystemMessageArgs, ChatCompletionRequestUserMessageArgs,
};
use std::path::Path;
pub(super) fn system_prompt(
    persona_cfg: &AgentConfig,
    provider: &str,
    project_path: &Path,
    persona_tool_names: &[String],
    turn_ctx: &classification::TurnContext,
    persona_memory: &persona_memory::PersonaMemory,
) -> String {
    let runner_label = match persona_cfg.agent.runner {
        crate::agents::RunnerKind::Subprocess => "subprocess",
        crate::agents::RunnerKind::Inline => "inline",
        crate::agents::RunnerKind::ClaudeCode => "claude-code",
        crate::agents::RunnerKind::InProcess => "in-process",
    };
    let identity = AgentIdentity {
        agent_name: &persona_cfg.agent.name,
        model: &persona_cfg.agent.model,
        runner: &format!("{:?}", persona_cfg.agent.runner),
        provider,
    };
    let base = build_user_context_prefix(&persona_cfg.system_prompt.content, &identity);
    let base = format!(
        "{base}{}",
        crate::skills::project::context(
            project_path,
            persona_tool_names.iter().any(|n| n == "project_skill")
        )
    );
    let base = format!(
        "{base}\n\n{}",
        crate::tools::listener_config::context(
            persona_tool_names
                .iter()
                .any(|name| name == "listener_config")
        )
    );
    let base = format!(
        "{base}{}",
        crate::tools::concierge::context(
            persona_tool_names
                .iter()
                .any(|name| name == "ask_concierge")
        )
    );
    let base = format!(
        "{base}{}",
        crate::tools::knowledge_history::context(
            persona_tool_names
                .iter()
                .any(|name| name == "knowledge_history")
        )
    );
    let base = format!(
        "{base}{}",
        crate::tools::channel::context(persona_tool_names.iter().any(|name| name == "channel"))
    );
    let base = format!(
        "{base}{}",
        crate::skills::manage::context(
            persona_tool_names
                .iter()
                .any(|n| n == "delegate_skill_configuration")
        )
    );
    let base = crate::agents::prompt_builder::SystemPromptBuilder::new(base)
        .with_agent_context(persona_cfg.agent.model.as_str(), runner_label)
        .build();
    // DOC-54 §9.6.3 stable assembly order: focused-mode context block
    // (when focused) lands BEFORE the classification instruction so the
    // prefix stays cache-stable across turns within the same focus/
    // vocabulary state (the classification block itself only changes
    // when a new label appears).
    let base = match &turn_ctx.focused_context_block {
        Some(focused_block) => format!("{base}\n\n{focused_block}"),
        None => base,
    };
    // #3840 critic HIGH-2: `classification_block` is empty when
    // `[workstreams].enabled = false` (real master switch — see
    // `build_turn_context`); appending it unconditionally would still
    // inject a blank `\n\n` into the prompt.
    let base = if turn_ctx.classification_block.is_empty() {
        base
    } else {
        format!("{base}\n\n{}", turn_ctx.classification_block)
    };
    let base = if !persona_tool_names.is_empty() {
        format!(
            "{}\n\n## Available tools\nYou have access to the following tools: {}.\nUse them when the user asks questions that require live data.",
            base,
            persona_tool_names.join(", ")
        )
    } else {
        base
    };
    // Issue #3928: the memory block goes LAST, after every block above it.
    // Its recall section is the only part of this prompt that changes on
    // EVERY turn (it is keyed to the user's query), so appending it keeps
    // the whole preceding prefix — persona body, focused-mode block,
    // classification block, tool list — cache-stable, preserving DOC-54
    // §9.6.3's prompt-cache-prefix property rather than busting it once
    // per turn. It also places the recalled facts closest to the user's
    // message, where they are most likely to be attended to.
    match persona_memory::render_memory_block(persona_memory) {
        Some(block) => format!("{base}\n\n{block}"),
        None => base,
    }
}
pub(super) fn messages(
    system_prompt: String,
    history: &[ConversationTurn],
    user_input: &str,
) -> Result<Vec<ChatCompletionRequestMessage>> {
    let mut initial_messages: Vec<ChatCompletionRequestMessage> = Vec::new();
    initial_messages.push(
        ChatCompletionRequestSystemMessageArgs::default()
            .content(system_prompt)
            .build()
            .context("failed to build persona system message")?
            .into(),
    );
    for turn in history {
        initial_messages.push(
            ChatCompletionRequestUserMessageArgs::default()
                .content(turn.user.clone())
                .build()
                .context("failed to build persona history user message")?
                .into(),
        );
        initial_messages.push(
            ChatCompletionRequestAssistantMessageArgs::default()
                .content(turn.assistant.clone())
                .build()
                .context("failed to build persona history assistant message")?
                .into(),
        );
    }
    initial_messages.push(
        ChatCompletionRequestUserMessageArgs::default()
            .content(user_input)
            .build()
            .context("failed to build persona current user message")?
            .into(),
    );
    Ok(initial_messages)
}

pub(super) fn append_cli_context(persona_cfg: &mut AgentConfig, project_path: &Path) {
    persona_cfg
        .system_prompt
        .content
        .push_str(&crate::tools::channel::context(false));
    persona_cfg
        .system_prompt
        .content
        .push_str(&crate::skills::project::context(project_path, false));
    persona_cfg.system_prompt.content.push_str("\n\n");
    persona_cfg
        .system_prompt
        .content
        .push_str(&crate::tools::listener_config::context(false));
    persona_cfg
        .system_prompt
        .content
        .push_str(crate::skills::manage::context(false));
}

pub(super) struct TurnCompletion<'a> {
    pub project_path: &'a Path,
    pub persona_name: &'a str,
    pub client: &'a async_openai::Client<async_openai::config::OpenAIConfig>,
    pub persona_cfg: &'a AgentConfig,
    pub user_input: &'a str,
    pub turn_ctx: &'a classification::TurnContext,
    pub workstreams_socket: &'a Path,
}
/// Why: typed attachment history must finish durably without duplicating the user turn.
/// What: classify the answer, then use the turn's memory owner or the existing text-history sink.
/// Test: `finish_turn_no_marker_returns_unchanged`; trusty-common asset_ownership_and_restart_are_enforced covers durable ownership.
pub(super) async fn finish_turn(
    context: TurnCompletion<'_>,
    content: String,
    attachment_turn: Option<&crate::chat_attachments::AttachmentTurn>,
    activities: Vec<serde_json::Value>,
) -> Result<String> {
    let TurnCompletion {
        project_path,
        persona_name,
        client,
        persona_cfg,
        user_input,
        turn_ctx,
        workstreams_socket,
    } = context;
    let display = classification::finish_turn(
        project_path,
        persona_name,
        client,
        persona_cfg,
        user_input,
        content,
        turn_ctx,
        workstreams_socket,
    )
    .await?;
    if let Some(turn) = attachment_turn {
        turn.finish(&display)
            .await
            .context("Reply completed, but durable chat persistence failed")?;
    } else {
        persona_memory::spawn_persist_turn_with_activity(
            &persona_cfg.stores,
            Some(workstreams_socket),
            persona_name,
            user_input,
            &display,
            activities,
        );
    }
    Ok(display)
}

/// Why: provider configuration and attachment capability checks must agree before inference.
/// What: resolve credentials, apply the configured route, and reject unsupported attachment providers.
/// Test: `configured_keyless_provider_resolution_preserves_routing`.
pub(super) fn resolve_provider(
    persona_cfg: &mut AgentConfig,
    provider: Option<&str>,
    attachment_turn: Option<&crate::chat_attachments::AttachmentTurn>,
    persona_name: &str,
) -> Result<(crate::llm::credentials::LlmCredentials, bool)> {
    let creds =
        super::super::super::super::config::resolve_overridden_credentials(persona_cfg, provider)?;
    let claude_cli_short_circuit =
        super::super::super::super::config::apply_credential_routing(persona_cfg, &creds);
    tracing::info!(
        persona = %persona_name,
        agent = %persona_cfg.agent.name,
        runner = ?persona_cfg.agent.runner,
        model = %persona_cfg.agent.model,
        creds = creds.label(),
        claude_cli_short_circuit,
        use_anthropic_direct = persona_cfg.llm.use_anthropic_direct,
        "run_pm_task_with_persona: credentials resolved"
    );
    if let Some(turn) = attachment_turn {
        turn.validate_provider(persona_cfg, creds.label(), claude_cli_short_circuit)?;
    }
    Ok((creds, claude_cli_short_circuit))
}
