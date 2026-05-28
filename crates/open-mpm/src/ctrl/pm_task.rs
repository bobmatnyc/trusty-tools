//! PM task dispatch — the canonical `run_pm_task_*` entry points and their
//! conversational helpers.
//!
//! Why: The PM-side dispatch path (history-aware, persona-aware, session-aware)
//! is the largest single concern in the ctrl module. Pulling it into its own
//! file keeps ctrl/mod.rs focused on re-exports and lets the dispatch helpers
//! coexist with their own tests.
//! What: `run_pm_task`, `run_pm_task_with_session`, `run_pm_task_with_history`,
//! `run_pm_task_with_persona`, plus the `extract_name_from_input` /
//! `save_name_to_profile` conversational fast-path helpers and the
//! `match_any_glob` persona tool-list filter.
//! Test: Unit tests at the bottom cover `extract_name_from_input` and
//! `match_any_glob`; the dispatch functions themselves are exercised
//! end-to-end via the ctrl integration tests.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};

use crate::agents::AgentConfig;
use crate::events::{self, Event};
use crate::intent::{IntentClass, classify_intent};
use crate::llm;
use crate::subprocess::SubprocessAgentRunner;
use crate::tools::{AgentRunner, ToolRegistry, delegate::DelegateToAgentTool};

use super::claude_cli::run_pm_task_via_claude_cli;
use super::config::{
    SessionOverrides, apply_credential_routing, build_deployment_footer, build_user_context_prefix,
    recall_project_memories, resolve_agent_config, resolve_overridden_credentials,
};
use super::handlers::{
    AddProjectTool, CreateDirTool, ListProjectsTool, MoveFileTool, RemoveProjectTool,
    SetActiveProjectTool, StopTaskTool, build_tm_context_block, register_ticketing_tools,
};
use super::state::ConversationTurn;

/// Entry point used by the PM actor task loop (`pm_actor_task`).
///
/// Why: Centralises the "single-shot PM round-trip" call site so the actor
/// task doesn't need to know about session ids, history, or overrides.
/// What: Delegates to `run_pm_task_with_session` with `None` session id.
/// Test: Exercised end-to-end via `actor_processes_task_and_shuts_down`.
pub(crate) async fn run_pm_task(project_path: &Path, user_input: &str) -> Result<String> {
    run_pm_task_with_session(project_path, user_input, None).await
}

/// Extract a name from a conversational name-introduction input.
///
/// Why: When the conversational fast path runs without a known user name, the
/// coordinator asks for it. The next turn from the user is typically a short
/// reply ("Bob", "I'm Bob", "My name is Alice"). This helper recognizes those
/// shapes so we can persist the name to `UserProfile` without an LLM round-trip.
/// What: Matches common introduction prefixes ("my name is ", "i'm ", "im ",
/// "i am ", "call me ", "it's ", "its "), or accepts a single bare alphabetic
/// word (2–20 chars) as a name. Returns the title-cased name on match.
/// Test: `extract_name_from_input_*` tests below cover positive and negative
/// cases (greetings and task requests must NOT match).
pub(crate) fn extract_name_from_input(input: &str) -> Option<String> {
    fn title_case(word: &str) -> String {
        let mut name = word.to_string();
        if let Some(first) = name.get_mut(0..1) {
            first.make_ascii_uppercase();
        }
        name
    }
    fn looks_like_name(word: &str, min: usize, max: usize) -> bool {
        let len = word.chars().count();
        len >= min
            && len <= max
            && word
                .chars()
                .all(|c| c.is_alphabetic() || c == '-' || c == '\'')
    }

    let trimmed = input.trim();
    let lower = trimmed.to_lowercase();
    for prefix in &[
        "my name is ",
        "i'm ",
        "im ",
        "i am ",
        "call me ",
        "it's ",
        "its ",
    ] {
        if let Some(rest) = lower.strip_prefix(prefix) {
            let word = rest.split_whitespace().next()?;
            // Reject greetings disguised as introductions ("i'm here", "i'm fine").
            const STOP_WORDS: &[&str] = &[
                "here", "fine", "good", "well", "ok", "okay", "back", "ready", "sorry", "the", "a",
                "an", "not",
            ];
            if STOP_WORDS.contains(&word) {
                return None;
            }
            if looks_like_name(word, 2, 40) {
                return Some(title_case(word));
            }
            return None;
        }
    }

    // Single-word input that looks like a name.
    let words: Vec<&str> = trimmed.split_whitespace().collect();
    if words.len() == 1 {
        let word = words[0];
        // Reject common single-word non-names (greetings, thanks, etc.)
        let lw = word.to_lowercase();
        const NON_NAMES: &[&str] = &[
            "hello", "hi", "hey", "yo", "sup", "thanks", "thank", "ok", "okay", "yes", "no", "yep",
            "nope", "help", "quit", "exit", "stop", "done",
        ];
        if NON_NAMES.contains(&lw.as_str()) {
            return None;
        }
        if word.chars().all(|c| c.is_alphabetic()) && looks_like_name(word, 2, 20) {
            return Some(title_case(word));
        }
    }
    None
}

/// Persist a detected user name to `~/.open-mpm/user.toml`.
///
/// Why: When the conversational fast path detects a name introduction it must
/// save the name immediately so the next turn's system prompt sees it. Without
/// this, the coordinator keeps re-asking ("What's your name?") in a loop.
/// What: Loads the existing profile (or starts a default one), updates the
/// name only when currently empty (don't clobber a real name with a partial
/// match), and writes the file. Failures are logged but non-fatal — the user
/// still gets a greeting, they just won't be remembered next session.
/// Test: Covered by the `extract_name_from_input_*` unit tests plus an
/// end-to-end check via `cat ~/.open-mpm/user.toml` after running the binary.
pub(crate) fn save_name_to_profile(name: &str) {
    use crate::identity::user_profile::UserProfile;
    let mut profile = UserProfile::load().unwrap_or_default();
    if profile.name.trim().is_empty() {
        profile.name = name.to_string();
        if profile.created_at.is_empty() {
            profile.created_at = chrono::Utc::now().to_rfc3339();
        }
        match profile.save() {
            Ok(()) => tracing::info!(name = %name, "user name saved to profile"),
            Err(e) => tracing::warn!(error = %e, "failed to save user name"),
        }
    } else {
        tracing::debug!(
            existing = %profile.name,
            detected = %name,
            "profile already has a name; not overwriting"
        );
    }
}

/// Match a tool name against a list of glob patterns (#255).
///
/// Why: Persona TOMLs accept `["mcp_*", "git_log"]` so operators don't have
/// to enumerate every dynamic tool name. A purpose-built matcher avoids
/// pulling in the `glob` crate for two patterns of behavior.
/// What: Returns `true` if `name` exactly equals a pattern, OR a pattern
/// ends with `*` and `name` starts with the pattern's prefix. Empty pattern
/// list returns false (caller treats `None` as "no filter" separately).
/// Test: `match_any_glob_handles_suffix_wildcard` below.
pub(crate) fn match_any_glob(name: &str, patterns: &[String]) -> bool {
    patterns.iter().any(|p| {
        if let Some(prefix) = p.strip_suffix('*') {
            name.starts_with(prefix)
        } else {
            name == p
        }
    })
}

/// Same as `run_pm_task` but tags every emitted event with `session_id` so
/// SSE subscribers can filter to a specific UI task.
///
/// Why: The thin-CLI controller socket (`handle_socket_connection`) and any
/// other future caller can mint a uuid up-front and propagate it through to
/// every downstream emission so the UI's per-task view stays coherent. When
/// `session_id` is `None` we still emit events, just unfiltered.
/// What: Wraps the existing PM round-trip with `events::emit` calls at the
/// transition points (turn start, delegation tool call, turn done). Errors
/// trigger an `AgentFailed`-equivalent emission via the caller.
pub async fn run_pm_task_with_session(
    project_path: &Path,
    user_input: &str,
    session_id: Option<String>,
) -> Result<String> {
    run_pm_task_with_history(
        project_path,
        user_input,
        &[],
        session_id,
        SessionOverrides::default(),
    )
    .await
}

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

/// Run a single conversation turn against a persona agent (#254).
///
/// Why: The REPL `/agent` command lets the user switch the active ctrl
/// conversation to a non-coding persona (e.g. `personal-assistant` /
/// `cto-assistant`). These personas should NOT have delegation tools wired
/// up — they're intended as direct chat partners with their own system
/// prompt and model.
/// What: Loads `<project>/.open-mpm/agents/<persona_name>.toml`, builds the
/// same date/time-injected system prompt the default ctrl path uses, then
/// makes a tools-OFF `chat_with_tools_gated` call carrying the prior
/// conversation history. Returns the assistant text.
/// Test: Manual via tmux — `/agent personal-assistant` then "who are you?"
/// → identifies as Izzie, knows Masa.
pub async fn run_pm_task_with_persona(
    project_path: &Path,
    persona_name: &str,
    user_input: &str,
    history: &[ConversationTurn],
    session_id: Option<String>,
    overrides: SessionOverrides,
) -> Result<String> {
    use async_openai::types::{
        ChatCompletionRequestAssistantMessageArgs, ChatCompletionRequestMessage,
        ChatCompletionRequestSystemMessageArgs, ChatCompletionRequestUserMessageArgs,
    };

    let sid = session_id.unwrap_or_default();

    let project_persona = project_path
        .join(".open-mpm")
        .join("agents")
        .join(format!("{}.toml", persona_name));
    let mut persona_cfg = if project_persona.is_file() {
        AgentConfig::load(&project_persona)?
    } else if let Some(home) = dirs::home_dir() {
        let user_persona = home
            .join(".open-mpm")
            .join("agents")
            .join(format!("{}.toml", persona_name));
        if user_persona.is_file() {
            AgentConfig::load(&user_persona)?
        } else {
            anyhow::bail!(
                "persona agent '{}' not found at {} or {}",
                persona_name,
                project_persona.display(),
                user_persona.display()
            );
        }
    } else {
        anyhow::bail!(
            "persona agent '{}' not found at {}",
            persona_name,
            project_persona.display()
        );
    };

    if let Some(ref m) = overrides.model {
        tracing::debug!(persona = %persona_name, model = %m, "applying /model override");
        persona_cfg.agent.model = m.clone();
    }

    let _ = sid;
    let creds = resolve_overridden_credentials(&mut persona_cfg, overrides.provider.as_deref())?;
    let claude_cli_short_circuit = apply_credential_routing(&mut persona_cfg, &creds);
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
    if claude_cli_short_circuit {
        return run_pm_task_via_claude_cli(project_path, &persona_cfg, user_input, history, "")
            .await;
    }
    let persona_llm_t0 = std::time::Instant::now();

    let client = llm::create_client()?;

    let (persona_registry, persona_tool_names): (ToolRegistry, Vec<String>) =
        if let Some(patterns) = persona_cfg.tools.allow.clone() {
            let mut registry = ToolRegistry::new();
            for tool in crate::tools::mcp_tools::mcp_tool_executors() {
                registry.register(tool);
            }
            for tool in crate::tools::mcp_service_tools::mcp_service_tool_executors().await {
                registry.register(tool);
            }
            {
                let global_config = crate::mcp::config::GlobalConfig::load().await;
                match crate::tools::registry::ToolRegistryBuilder::from_config(&global_config)
                    .build()
                    .await
                {
                    Ok(execs) => {
                        for tool in execs {
                            registry.register(tool);
                        }
                    }
                    Err(e) => {
                        tracing::warn!("tool registry init failed: {e}");
                    }
                }
            }
            if let Ok(repo) = crate::git::GitRepo::open(
                &std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            ) {
                for tool in crate::tools::git_tools::git_tools(repo.root.clone()) {
                    registry.register(tool);
                }
            }
            register_ticketing_tools(&mut registry).await;

            registry.register(Arc::new(
                crate::tools::web_search::BraveSearchTool::from_env(),
            ));

            for plugin in crate::tools::agent_plugin::plugins_for_persona(persona_name) {
                for tool in &plugin.tools {
                    registry.register(std::sync::Arc::clone(tool));
                }
            }

            let all_names: Vec<String> = registry
                .schemas()
                .into_iter()
                .filter_map(|s| {
                    s.get("function")
                        .and_then(|f| f.get("name"))
                        .and_then(|n| n.as_str())
                        .map(String::from)
                })
                .collect();
            let mut kept: Vec<String> = all_names
                .into_iter()
                .filter(|name| match_any_glob(name, &patterns))
                .collect();
            let rbac_user = overrides.user.clone().unwrap_or_default();
            let allowed_by_tier: std::collections::HashSet<String> = registry
                .filter_tools_for_user(&rbac_user)
                .into_iter()
                .map(|t| t.schema())
                .filter_map(|s| {
                    s.get("function")
                        .and_then(|f| f.get("name"))
                        .and_then(|n| n.as_str())
                        .map(String::from)
                })
                .collect();
            kept.retain(|name| allowed_by_tier.contains(name));
            tracing::info!(
                persona = %persona_name,
                tools = ?kept,
                rbac_user = %rbac_user.id,
                rbac_tier = ?rbac_user.tier,
                "persona tool registry built"
            );
            (registry, kept)
        } else {
            (ToolRegistry::new(), Vec::new())
        };

    let system_prompt: String = {
        let base = build_user_context_prefix(&persona_cfg.system_prompt.content);
        let runner_label = match persona_cfg.agent.runner {
            crate::agents::RunnerKind::Subprocess => "subprocess",
            crate::agents::RunnerKind::Inline => "inline",
            crate::agents::RunnerKind::ClaudeCode => "claude-code",
            crate::agents::RunnerKind::InProcess => "in-process",
        };
        let base = crate::agents::prompt_builder::SystemPromptBuilder::new(base)
            .with_agent_context(persona_cfg.agent.model.as_str(), runner_label)
            .build();
        if !persona_tool_names.is_empty() {
            format!(
                "{}\n\n## Available tools\nYou have access to the following tools: {}.\nUse them when the user asks questions that require live data.",
                base,
                persona_tool_names.join(", ")
            )
        } else {
            base
        }
    };

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

    let adapter = llm::adapter::adapter_for_model(&persona_cfg.agent.model);
    let allowed_tools = if persona_tool_names.is_empty() {
        None
    } else {
        Some(persona_tool_names.clone())
    };
    let max_turns = if persona_tool_names.is_empty() { 2 } else { 4 };
    let (content, _usage) = llm::chat_with_tools_gated(
        &client,
        &persona_cfg.agent.model,
        &*adapter,
        initial_messages,
        Arc::new(persona_registry),
        allowed_tools,
        persona_cfg.llm.temperature,
        persona_cfg.llm.max_tokens,
        max_turns,
        false,
        None,
        false,
        persona_cfg.llm.use_anthropic_direct,
        &persona_cfg.llm.stop_sequences,
    )
    .await
    .context("persona LLM call failed")?;
    tracing::info!(
        persona = %persona_name,
        llm_ms = persona_llm_t0.elapsed().as_millis() as u64,
        response_chars = content.len(),
        "run_pm_task_with_persona: LLM call complete"
    );

    Ok(content)
}

// Tests live in `ctrl::tests` — see `ctrl/tests.rs`. Keeping them centralised
// lets a single `mod tests` block share helpers across the dispatch modules
// without having to expose `pub(crate)` re-exports purely for test use.
