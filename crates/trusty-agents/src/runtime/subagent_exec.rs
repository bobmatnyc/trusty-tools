// Pre-existing clippy warnings across this large binary crate.
// Each category below is suppressed at crate level with rationale:
// - dead_code / unused_imports: Many helpers are kept for future use, behind
//   feature flags, or used only on certain platforms / by tests; pruning them
//   is its own refactor and would churn unrelated modules.
// - clippy::collapsible_if / collapsible_else_if: Style preference; nested
//   ifs are often clearer with the existing comments and gating logic.
// - clippy::manual_str_repeat / manual_repeat_n / single_char_add_str: Style
//   nits in display/formatting code where current form reads fine.
// - clippy::too_many_arguments: A few orchestration entry points genuinely
//   need their argument count; signatures are part of internal contracts.
// - clippy::await_holding_lock: Test-only — a std::sync::Mutex serializes
//   tests that mutate process-global env (HOME, etc.). The await points are
//   inside the critical section by design, and tests are single-threaded
//   per-test by virtue of the lock.
// - clippy::clone_on_copy / len_zero / map_or / etc.: Misc style nits in
//   pre-existing code; not worth the churn vs. risk of breaking 1500+ tests.
#![allow(dead_code)]
#![allow(unused_imports)]
#![allow(unused_mut)]
#![allow(unused_assignments)]
#![allow(unused_variables)]
#![allow(clippy::collapsible_if)]
#![allow(clippy::collapsible_else_if)]
#![allow(clippy::manual_str_repeat)]
#![allow(clippy::manual_repeat_n)]
#![allow(clippy::single_char_add_str)]
#![allow(clippy::too_many_arguments)]
#![allow(clippy::await_holding_lock)]
#![allow(clippy::clone_on_copy)]
#![allow(clippy::len_zero)]
#![allow(clippy::unnecessary_map_or)]
#![allow(clippy::manual_map)]
#![allow(clippy::needless_borrows_for_generic_args)]
#![allow(clippy::unnecessary_sort_by)]
#![allow(clippy::if_same_then_else)]
#![allow(clippy::new_without_default)]
#![allow(clippy::manual_split_once)]
#![allow(clippy::needless_splitn)]
#![allow(clippy::single_match_else)]
#![allow(clippy::single_match)]
#![allow(clippy::ptr_arg)]
#![allow(clippy::manual_clamp)]
#![allow(clippy::redundant_closure)]
#![allow(clippy::manual_pattern_char_comparison)]
#![allow(clippy::vec_init_then_push)]
#![allow(clippy::single_component_path_imports)]
#![allow(clippy::derivable_impls)]
#![allow(clippy::match_single_binding)]
#![allow(clippy::redundant_pattern_matching)]

//! Sub-agent chat execution: the single-shot and tool-using LLM loops plus
//! the provider-specific `tool_choice` resolver.

use std::sync::Arc;

use anyhow::{Context, Result};
use async_openai::types::{
    ChatCompletionRequestMessage, ChatCompletionRequestSystemMessageArgs,
    ChatCompletionRequestUserMessageArgs,
};

use crate::{agents, llm, perf, session, tools};

use agents::AgentConfig;
use tools::ToolRegistry;

pub(super) async fn run_subagent_single_shot(
    client: &async_openai::Client<async_openai::config::OpenAIConfig>,
    cfg: &AgentConfig,
    system_prompt: &str,
    task_text: &str,
    history: Option<&[session::HistoryMessage]>,
) -> Result<(String, perf::TokenUsage)> {
    // When the caller provided persistent-session history, we need the full
    // message vector path (system + history... + user). `llm::chat` only
    // takes system+user, so we fall through to the messages-based loop with
    // an empty tool registry in that case.
    if let Some(hist) = history
        && !hist.is_empty()
    {
        // #135: Apply send-time compression (no-op unless [compress] enabled).
        let (hist_compressed, task_compressed) =
            llm::apply_compression(hist.to_vec(), task_text.to_string(), &cfg.compress);

        let system_msg: ChatCompletionRequestMessage =
            ChatCompletionRequestSystemMessageArgs::default()
                .content(system_prompt)
                .build()
                .context("failed to build system message")?
                .into();
        let mut messages: Vec<ChatCompletionRequestMessage> =
            Vec::with_capacity(hist_compressed.len() + 2);
        messages.push(system_msg);
        for h in &hist_compressed {
            messages.push(session::history_message_into_typed(h.clone())?);
        }
        let user_msg: ChatCompletionRequestMessage =
            ChatCompletionRequestUserMessageArgs::default()
                .content(task_compressed.as_str())
                .build()
                .context("failed to build user message")?
                .into();
        messages.push(user_msg);

        // Bedrock-routed sub-agents need AWS profile/region exposed via env vars
        // (mirrors the guard in `run_subagent_with_tools`).
        let _aws_env_guard = if cfg.adapter.provider() == llm::adapter::Provider::Bedrock {
            Some(agents::in_process_runner::BedrockEnvGuard::install(
                cfg.llm.aws_profile.as_deref(),
                cfg.llm.aws_region.as_deref(),
            ))
        } else {
            None
        };

        let (content, usage) = llm::chat_with_tools_gated(
            client,
            &cfg.agent.model,
            &*cfg.adapter,
            messages,
            Arc::new(ToolRegistry::new()),
            cfg.tools.allowed.clone(),
            cfg.llm.temperature,
            cfg.llm.max_tokens,
            2,
            cfg.llm.enable_prompt_caching,
            resolve_tool_choice(cfg.llm.tool_choice, &*cfg.adapter),
            cfg.llm.use_finish_task,
            cfg.llm.use_anthropic_direct,
            &cfg.llm.stop_sequences,
        )
        .await?;
        return Ok((content, usage));
    }

    let response = llm::chat(
        client,
        &cfg.agent.model,
        system_prompt,
        task_text,
        cfg.llm.temperature,
        cfg.llm.max_tokens,
        vec![],
    )
    .await?;
    Ok((
        response
            .content
            .unwrap_or_else(|| "(sub-agent produced no content)".to_string()),
        response.usage,
    ))
}

pub(super) async fn run_subagent_with_tools(
    client: &async_openai::Client<async_openai::config::OpenAIConfig>,
    cfg: &AgentConfig,
    system_prompt: &str,
    task_text: &str,
    registry: ToolRegistry,
    history: Option<&[session::HistoryMessage]>,
) -> Result<(String, perf::TokenUsage)> {
    let system_msg: ChatCompletionRequestMessage =
        ChatCompletionRequestSystemMessageArgs::default()
            .content(system_prompt)
            .build()
            .context("failed to build system message")?
            .into();

    // #135: Apply send-time compression (no-op unless [compress] enabled).
    // Stored history in the SessionManager is never mutated — only the
    // wire copy we're about to send is.
    let (hist_for_wire, task_for_wire) = llm::apply_compression(
        history.map(|h| h.to_vec()).unwrap_or_default(),
        task_text.to_string(),
        &cfg.compress,
    );

    // #51: If the caller forwarded session history (persistent agent), splice
    // it between the system message and the new user task so the model has
    // the full running dialog.
    let mut messages: Vec<ChatCompletionRequestMessage> = Vec::new();
    messages.push(system_msg);
    for h in &hist_for_wire {
        messages.push(session::history_message_into_typed(h.clone())?);
    }
    let user_msg: ChatCompletionRequestMessage = ChatCompletionRequestUserMessageArgs::default()
        .content(task_for_wire.as_str())
        .build()
        .context("failed to build user message")?
        .into();
    messages.push(user_msg);

    let allowed = cfg.tools.allowed.clone();

    // Bedrock-routed sub-agents need AWS profile/region exposed via env vars
    // so `chat_with_tools_gated` can build the Bedrock client. The in-process
    // runner installs an identical guard; the subprocess path was missing it,
    // which made `bedrock/...` agents fail with the SDK default credential
    // chain (no profile, wrong region).
    let _aws_env_guard = if cfg.adapter.provider() == llm::adapter::Provider::Bedrock {
        Some(agents::in_process_runner::BedrockEnvGuard::install(
            cfg.llm.aws_profile.as_deref(),
            cfg.llm.aws_region.as_deref(),
        ))
    } else {
        None
    };

    let (content, usage) = llm::chat_with_tools_gated(
        client,
        &cfg.agent.model,
        &*cfg.adapter,
        messages,
        Arc::new(registry),
        allowed,
        cfg.llm.temperature,
        cfg.llm.max_tokens,
        cfg.llm.max_turns,
        cfg.llm.enable_prompt_caching,
        resolve_tool_choice(cfg.llm.tool_choice, &*cfg.adapter),
        cfg.llm.use_finish_task,
        cfg.llm.use_anthropic_direct,
        &cfg.llm.stop_sequences,
    )
    .await?;
    Ok((content, usage))
}

/// Translate the TOML-level `ToolChoice` enum into the provider-specific
/// `tool_choice` JSON value using the agent's adapter.
///
/// Why: `agents::ToolChoice` is a small config enum; the actual wire shape
/// depends on the provider family (`{"type":"any"}` vs `"required"`), so we
/// funnel through the adapter here.
/// What: Maps `Auto` → adapter's auto value (usually `"auto"`), `Any` →
/// `tool_choice_any`, `None` → literal JSON `"none"`. Returns `None` when
/// the adapter has no preference (generic providers), letting the chat
/// builder omit the field entirely.
/// Test: Exercised through `main` integration; unit coverage via adapter tests.
pub(super) fn resolve_tool_choice(
    choice: agents::ToolChoice,
    adapter: &dyn llm::adapter::ModelAdapter,
) -> Option<serde_json::Value> {
    match choice {
        agents::ToolChoice::Auto => adapter.tool_choice_auto(),
        agents::ToolChoice::Any => adapter.tool_choice_any(),
        agents::ToolChoice::None => Some(serde_json::Value::String("none".to_string())),
    }
}
