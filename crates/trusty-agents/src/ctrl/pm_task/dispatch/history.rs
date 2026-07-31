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
use crate::rbac::ServiceTier;
use crate::subprocess::SubprocessAgentRunner;
use crate::tools::cross_product::{CallerAuthority, NON_CODING_TARGETS};
use crate::tools::pm_bridge::PmBridgeTool;
use crate::tools::pm_bridge_backend::ProcessPmBridge;
use crate::tools::subagent_allow::SubagentAllowSet;
use crate::tools::{AgentRunner, ToolRegistry, delegate::DelegateToAgentTool};

use super::super::super::claude_cli::run_pm_task_via_claude_cli;
use super::super::super::config::{
    AgentIdentity, SessionOverrides, apply_credential_routing, build_deployment_footer,
    build_user_context_prefix, recall_project_memories, resolve_agent_config,
    resolve_overridden_credentials,
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

    let config_dir = project_path.join(".trusty-agents").join("agents");
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
        let identity = AgentIdentity {
            agent_name: &pm_cfg.agent.name,
            model: &pm_cfg.agent.model,
            runner: &format!("{:?}", pm_cfg.agent.runner),
            provider: creds.label(),
        };
        let base = build_user_context_prefix(&pm_cfg.system_prompt.content, &identity);

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
        let tm_state_dir = project_path.join(".trusty-agents").join("state");
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

        // Chat-streaming demo: when NOT routed to local ollama and the model
        // reaches an OpenRouter-transport endpoint, stream the reply
        // token-by-token onto the event bus and return the assembled full text
        // (identical to the blocking path downstream). Local-inference and any
        // streaming failure fall through to the blocking `chat_with_tools_gated`
        // call below.
        if !local_qualifies
            && llm::stream::streaming_supported(&effective_model, effective_use_direct)
        {
            let messages =
                llm::stream::build_messages(&system_prompt, &truncated_history, user_input);
            match llm::stream::stream_reply(
                &effective_model,
                messages,
                &sid,
                &pm_cfg.agent.name,
                llm::stream::DEFAULT_STREAM_CADENCE,
                // #3758: the SAME sampling knobs the blocking
                // `chat_with_tools_gated` call below sends — including
                // `effective_max_tokens`, which the local-route branch above
                // may have overridden.
                trusty_common::chat::SamplingParams {
                    temperature: Some(pm_cfg.llm.temperature),
                    max_tokens: Some(effective_max_tokens),
                    stop: pm_cfg.llm.stop_sequences.clone(),
                },
            )
            .await
            {
                Ok(assembly) => {
                    tracing::info!(
                        model = %effective_model,
                        llm_ms = llm_t0.elapsed().as_millis() as u64,
                        response_chars = assembly.content.len(),
                        "ctrl conversational fast path: streamed LLM call complete"
                    );
                    return Ok(assembly.content);
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "conversational streaming failed; falling back to blocking chat"
                    );
                }
            }
        }

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
            pm_cfg.llm.strict_tool_discipline(),
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
                    pm_cfg.llm.strict_tool_discipline(),
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

    // Security fix (delegate_to_agent injection-to-RCE path, item 3, #4126):
    // this THIRD dispatch path (backing the REPL, Slack, Telegram, and the
    // API server, and `ctrl.toml` is the standalone-mode default) built its
    // `DelegateToAgentTool` with NO taint and NO `allowed_target_roles` at
    // all — `pm_cfg` resolves to `ctrl.toml`, which declares
    // `role = "assistant"` (deliberately, per #3812, for role-filtered
    // picker grouping), the SAME role value the OTHER two dispatch paths
    // (`runtime::subagent_mode`, `persona.rs`) treat as "holds live
    // external-content tools (web_search here), must never produce an
    // unrestricted delegate_to_agent spawn." See `ctrl_delegate_posture`.
    let inbound_taint = crate::runtime::tool_registry::parse_delegation_taint_env(
        std::env::var("TAGENT_DELEGATION_TAINT_ALLOW").ok(),
    );
    let (delegation_taint, allowed_target_roles, delegator_tier) = ctrl_delegate_posture(
        &pm_cfg.agent.role,
        pm_cfg.tools.allow.as_deref(),
        inbound_taint.as_deref(),
        pm_cfg.agent.tier(),
    );
    let runner: Arc<dyn AgentRunner> = Arc::new(
        SubprocessAgentRunner::new()
            .with_config_dir(Some(config_dir.clone()))
            .with_delegation_taint(delegation_taint),
    );

    let mut registry = ToolRegistry::new();
    // #3737: thread the task's session id so a successful delegation emits
    // `AgentSpawned` and the API server can attribute the answer to the
    // specialist that produced it (chat-bubble responder attribution).
    // ADR-0024: the delegator's IDENTITY (role = KIND, plus tier) is declared
    // in one call, so this path gets the kind predicate and the tier gate
    // together and cannot acquire one without the other. `pm_cfg.agent.role`
    // verbatim: for `ctrl.toml` that is `assistant` (#3812) and the kind
    // predicate applies; for a resolved `pm.toml` it is `orchestrator`, which
    // the predicate ignores by design (ADR-0024 predicate 1's scope caveat),
    // so orchestrator delegation is unchanged.
    let mut delegate_tool = DelegateToAgentTool::new(runner)
        .with_config_dir(config_dir.clone())
        .with_session_id(sid.clone())
        // ADR-0024 decision 4: the reachable-set whitelist joins role and tier
        // in the same indivisible identity declaration. For `ctrl.toml`
        // (`role = "assistant"`, #3812) this is the binding gate; for a
        // resolved `pm.toml` (`role = "orchestrator"`) `execute` does not apply
        // it at all, so orchestrator delegation is unchanged.
        .with_delegator(
            pm_cfg.agent.role.clone(),
            delegator_tier,
            crate::tools::subagent_allow::SubagentAllowSet::over(
                crate::agents::delegation::ASSISTANT_REACHABLE_SUBAGENTS,
                pm_cfg.subagents.delegate_allowed.as_deref(),
            ),
        );
    if let Some(roles) = allowed_target_roles {
        delegate_tool = delegate_tool.with_allowed_target_roles(roles);
    }
    registry.register(Arc::new(delegate_tool));
    // #3052 PR B (lane 3): the opaque tm<->tcode bridge, distinct from lane 2
    // (`delegate_to_agent` above). RBAC-locked to deny ReadOnly + Analytics.
    // #4026/#4028: the caller's `[subagents].allowed` list is resolved into the
    // bridge's fail-closed allow-set here, at registration, so `execute` never
    // consults anything the LLM can influence mid-turn. Absent section = EMPTY
    // = named cross-product targeting stays off. #4030's domain authority will
    // feed the same `SubagentAllowSet` seam.
    registry.register(Arc::new(
        PmBridgeTool::new(Arc::new(ProcessPmBridge::from_project(
            project_path.to_path_buf(),
        )))
        .with_restricted_tiers(vec![ServiceTier::ReadOnly, ServiceTier::Analytics])
        .with_allow_set(SubagentAllowSet::over(
            NON_CODING_TARGETS,
            pm_cfg.subagents.allowed.as_deref(),
        ))
        // `user_authority` has no `AgentConfig` field yet (#3074/AUTH-1), so
        // the fail-closed `Standard` tier is reported until it lands — never
        // an invented tier.
        .with_origin(pm_cfg.agent.name.clone(), CallerAuthority::Standard),
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
        let state_dir = project_path.join(".trusty-agents").join("state");
        crate::tools::tm_tools::register_tm_tools_for_state_dir(&mut registry, &state_dir);
    }

    // system_status epic (#3052): ctrl (the default coordination persona)
    // gets the same on-demand subsystem-health tool as the `assistant`
    // persona (`ctrl/pm_task/dispatch/persona.rs`). Unlike the persona path,
    // ctrl's registry here is not gated by a `[tools].allow` list, so
    // registering it is sufficient to make it callable. `with_resolved_endpoint`
    // (not `new`) so a `/model` override applied above is reflected instead
    // of silently re-derived from the on-disk TOML — see
    // `tools::system_status` module docs.
    registry.register(Arc::new(
        crate::tools::system_status::SystemStatusTool::with_resolved_endpoint(
            pm_cfg.agent.name.clone(),
            pm_cfg.agent.model.clone(),
            pm_cfg.agent.runner,
        ),
    ));

    let adapter = llm::adapter::adapter_for_model(&pm_cfg.agent.model);
    let registry_arc = Arc::new(registry);
    // code-critic BLOCK finding 2: this path backs Slack/Telegram/API, all of
    // which can carry a real, less-than-`All` `ServiceTier` in
    // `overrides.user` — without this, `dispatch_gated` (which only checks a
    // NAME allowlist, never `restricted_tiers`) would let a ReadOnly/Analytics
    // caller invoke `dispatch_task` regardless of its RBAC gate. See
    // `allowed_tool_names_for`'s docs.
    let allowed_tools = allowed_tool_names_for(&registry_arc, overrides.user.as_ref());
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
        allowed_tools,
        pm_cfg.llm.temperature,
        pm_cfg.llm.max_tokens,
        4,
        false,
        None,
        false,
        pm_cfg.llm.strict_tool_discipline(),
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

/// Compute the delegation-taint and role-allowlist posture
/// `run_pm_task_with_history` applies to the `DelegateToAgentTool` it builds
/// (security fix — item 3, #4126).
///
/// Why: Pulled out of the ~500-line dispatch body specifically so this
/// fail-closed / role-gate behavior is unit-testable without a live LLM call
/// — mirrors why `runtime::tool_registry::scope_for_delegation` and
/// `compose_delegation_taint` are pure functions rather than inlined at
/// their call sites. This was the THIRD `delegate_to_agent` construction
/// site (besides `runtime::subagent_mode`/`runtime::tool_registry` and
/// `ctrl::pm_task::dispatch::persona`) and, until this fix, the only one
/// with no taint and no `allowed_target_roles` at all — even though
/// `resolve_agent_config` can resolve `ctrl.toml`, which declares
/// `role = "assistant"` (#3812).
/// What: a non-assistant role (e.g. a resolved `pm.toml`, `role =
/// "orchestrator"`) returns `(None, None, AgentTier::for_kind(role))` —
/// completely unaffected on the taint/role front; `pm` is the trusted
/// orchestrator, not a sandboxed persona, and this dispatch path must keep
/// working exactly as before for it. The THIRD element resolves
/// `AgentTier::L0Orchestration` for the orchestrator kind (#4169, epic
/// #4167) — not because `pm`/`ctrl`-as-orchestrator IS an L0 persona, but
/// because it sits OUTSIDE the L0/L1 persona tier model entirely (already
/// fully trusted, already unrestricted) and must not be newly blocked from
/// reaching a future L0 persona by a gate that was never meant to restrict
/// it; see `DelegateToAgentTool::delegator_tier`'s doc comment for the same
/// pattern.
///
/// #4497: that third element was a hardcoded `AgentTier::L0Orchestration`
/// literal — the SECOND mechanism by which an agent could become L0, running
/// alongside the role-derived rule every assistant already used, so
/// `AgentInfo::tier()` and this dispatch posture disagreed about `pm`. The
/// rule (`AgentTier::for_kind`) now recognizes the orchestrator kind itself
/// and this branch is a thin delegation to it. For every config
/// `resolve_agent_config` can actually return — `pm.toml` (`orchestrator`),
/// `ctrl.toml` (`assistant`), `ctrl_default()` (`controller`) — the answer is
/// identical to the literal it replaced. It differs only for a hand-edited
/// config carrying a specialist role, where it now fails CLOSED to
/// `L1Standard` instead of handing out L0 by fallthrough.
/// `role == "assistant"` returns `(Some(taint), Some(roles),
/// declared_tier)`: `taint` is
/// `compose_delegation_taint(native_allow, inbound_taint).unwrap_or_default()`
/// — ALWAYS `Some`, even when empty, matching
/// `build_assistant_tier_registry`'s fail-closed convention (an absent
/// `[tools].allow` on `ctrl.toml` narrows to deny-all, never skips tainting);
/// `roles` is always `ASSISTANT_ALLOWED_DELEGATE_ROLES` — the same
/// worker + peer-assistant allowlist `build_assistant_tier_registry` uses,
/// deliberately excluding `orchestrator`/`controller`/`observer`/`analysis`;
/// `declared_tier` is `pm_cfg.agent.tier()` verbatim (fail-closed to
/// `L1Standard` for every persona shipped before #4168).
/// Test: `ctrl_delegate_posture_orchestrator_role_is_unrestricted`,
/// `ctrl_delegate_posture_assistant_role_fails_closed_without_allow`,
/// `ctrl_delegate_posture_assistant_role_forwards_native_allow`,
/// `ctrl_delegate_posture_assistant_role_always_gates_target_roles`,
/// `ctrl_delegate_posture_orchestrator_role_reports_l0_tier`,
/// `ctrl_delegate_posture_assistant_role_reports_its_declared_tier`,
/// `ctrl_delegate_posture_specialist_role_is_not_silently_l0`.
fn ctrl_delegate_posture(
    role: &str,
    native_allow: Option<&[String]>,
    inbound_taint: Option<&[String]>,
    declared_tier: crate::agents::AgentTier,
) -> (
    Option<Vec<String>>,
    Option<Vec<String>>,
    crate::agents::AgentTier,
) {
    if !crate::agents::delegation::is_assistant_kind(role) {
        return (None, None, crate::agents::AgentTier::for_kind(role));
    }
    let taint =
        crate::runtime::tool_registry::compose_delegation_taint(native_allow, inbound_taint)
            .unwrap_or_default();
    let roles = crate::runtime::tool_registry::ASSISTANT_ALLOWED_DELEGATE_ROLES
        .iter()
        .map(|s| s.to_string())
        .collect();
    (Some(taint), Some(roles), declared_tier)
}

/// Compute `chat_with_tools_gated`'s `allowed_tools` argument from an
/// optional caller `UserIdentity`.
///
/// Why (code-critic BLOCK, epic #3052 PR B finding 2): `chat_with_tools_gated`'s
/// dispatch loop calls `registry.dispatch_gated`, which enforces ONLY a name
/// allowlist (`llm/tool_loop/mod.rs`) — it never consults
/// `ToolExecutor::restricted_tiers()` / `UserIdentity::can_access_tier`, so
/// passing `None` unconditionally (the pre-fix behavior) let ANY caller
/// invoke a tier-restricted tool like `dispatch_task` regardless of its RBAC
/// gate. This path backs Slack/Telegram/API, and Slack plumbs a real,
/// authenticated `ServiceTier` into `overrides.user`
/// (`slack::rbac`/`slack::handlers`), so a ReadOnly/Analytics Slack user
/// could otherwise reach `dispatch_task`. Mirrors the identical
/// `registry.filter_tools_for_user` computation
/// `ctrl::pm_task::dispatch::persona::run_pm_task_with_persona` already uses
/// for the same property (see `persona.rs`'s `allowed_by_tier`).
///
/// Reconciliation with #3576 (`fix(trusty-agents): enforce RBAC tier and
/// agent-declared tool scopes at dispatch`, merged to main as this fix
/// landed): #3576 closed two DIFFERENT enforcement gaps — `PythonToolPlugin`
/// never overriding `restricted_tiers()` (#3236), and agent-declared
/// `[tools].scopes` never being consulted inside
/// `persona::filter_persona_tool_names` (#3208). Neither touches
/// `dispatch_gated`, `dispatch_for_user`, or this (`run_pm_task_with_history`)
/// code path — #3208's fix is entirely local to `persona.rs`'s OWN filter
/// function, which this file does not call. So this fix is NOT redundant
/// with, nor superseded by, #3576: it remains the ONLY RBAC-tier
/// enforcement on the `run_pm_task_with_history` (ctrl/Slack/Telegram/API)
/// dispatch path, complementary to (not overlapping with) #3576's
/// persona-path and `PythonToolPlugin` fixes. `dispatch_task` also needs no
/// `ToolExecutor::scope()` override for #3208's purposes — it is a native
/// in-process tool (like `delegate_to_agent`/`run_bash`), gated entirely by
/// name/glob allowlist + RBAC tier, exactly the category #3576's own scope
/// doc comment says is unaffected by scope-pattern gating.
/// What: `None` (no `UserIdentity` — the local REPL/CLI path never sets
/// `overrides.user`) returns `None`, meaning NO filtering: every registered
/// tool stays callable, preserving the existing unauthenticated-CLI
/// behavior exactly (the RBAC `ServiceTier` default is `All`, i.e.
/// unrestricted, so this is not a silent widening). `Some(user)` returns
/// `Some(names)` restricted to `registry.filter_tools_for_user(user)` — only
/// the tools `user`'s tier may access. Passed straight through as
/// `chat_with_tools_gated`'s `allowed_tools`, this makes `dispatch_gated`'s
/// name-allowlist check ALSO enforce the RBAC tier, closing the gap without
/// touching `dispatch_gated` itself.
/// Test: `allowed_tool_names_for_no_user_returns_none_full_access`,
/// `allowed_tool_names_for_read_only_excludes_dispatch_task`,
/// `allowed_tool_names_for_analytics_excludes_dispatch_task`,
/// `allowed_tool_names_for_all_tier_includes_dispatch_task`,
/// `dispatch_task_not_invocable_for_read_only_user_through_dispatch_gated`
/// (the integration-level proof: drives the REAL `ToolRegistry::dispatch_gated`
/// enforcement path with the computed allowlist, not just the computation
/// in isolation).
fn allowed_tool_names_for(
    registry: &ToolRegistry,
    user: Option<&crate::rbac::UserIdentity>,
) -> Option<Vec<String>> {
    let user = user?;
    Some(
        registry
            .filter_tools_for_user(user)
            .into_iter()
            .map(|t| t.name().to_string())
            .collect(),
    )
}

// Sibling `_tests.rs` file (this crate's `#[path=...]` pattern) — keeps
// this production file under the 500-SLOC cap. See `history_tests.rs`'s
// module docs for what code-critic finding this covers.
#[cfg(test)]
#[path = "history_tests.rs"]
mod rbac_gate_tests;
