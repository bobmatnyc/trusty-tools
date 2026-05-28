//! Single CTRL-level LLM turn dispatch.
//!
//! Why: The conversational ctrl turn (no PM attached) drives a focused LLM
//! call with a registry of tools that queue side effects (start_pm,
//! initiate_self_task, stop_task). Separating it from PM-task dispatch keeps
//! both lifecycles legible.
//! What: `ctrl_chat_turn` orchestrates `prepare_ctrl_turn_state` →
//! `resolve_ctrl_turn_agent_config` → `build_ctrl_turn_system_prompt` →
//! `dispatch_ctrl_turn_llm` → `drain_ctrl_turn_side_effects`.
//! Test: Indirect — exercised via REPL integration tests; the side-effect
//! drain helpers are pure and could be unit-tested if the parent slot Arcs
//! were extracted into a small struct (kept here for now to keep diff small).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use async_openai::types::ChatCompletionTool;

use crate::agents::AgentConfig;
use crate::llm;
use crate::tools::ToolRegistry;

use super::claude_cli::run_pm_task_via_claude_cli;
use super::config::{
    CTRL_SYSTEM_PROMPT, apply_credential_routing, build_deployment_footer, recall_project_memories,
    resolve_ctrl_agent_config,
};
use super::handlers::{PmStatusRow, PmStopHandle, build_ctrl_registry, build_tm_context_block};
use super::state::{Ctrl, PmMsg};
use super::util::drain_slot;

/// Pending side-effect slots populated by the ctrl tool handlers during the
/// LLM turn and drained afterwards by `drain_ctrl_turn_side_effects`.
///
/// Purpose: Groups the three independent `Arc<Mutex<Option<...>>>` slots
/// (filled by tools while the LLM runs, drained at end-of-turn) so
/// `drain_ctrl_turn_side_effects` doesn't need a parameter list per slot.
pub(crate) struct CtrlTurnSideEffects {
    /// Drained at end-of-turn to perform a real `Ctrl::connect`.
    pub(crate) pending_connect: Arc<Mutex<Option<String>>>,
    /// Optional self-task forwarded after a successful self-connect (#182).
    pub(crate) pending_self_task: Arc<Mutex<Option<String>>>,
    /// PM-name target queued by `stop_task` (#202).
    pub(crate) pending_stop: Arc<Mutex<Option<String>>>,
}

/// Output of `prepare_ctrl_turn_state` — everything the rest of the turn
/// needs that depends on the live `Ctrl` snapshot.
pub(crate) struct CtrlTurnState {
    pub(crate) registry: ToolRegistry,
    pub(crate) openai_tools: Vec<ChatCompletionTool>,
    pub(crate) side_effects: CtrlTurnSideEffects,
}

// Purpose: Build the per-turn registry, snapshot live PM state, and
// allocate the pending side-effect slots that tool callbacks will fill.
pub(crate) async fn prepare_ctrl_turn_state(ctrl: &Ctrl) -> Result<CtrlTurnState> {
    let pending_connect: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let pending_self_task: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let pending_stop: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

    let task_status_snapshot: Vec<PmStatusRow> = ctrl
        .pms
        .values()
        .map(|h| (h.name.clone(), h.status.clone(), h.last_message.clone()))
        .collect();
    let stop_snapshot: Vec<PmStopHandle> = ctrl
        .pms
        .iter()
        .map(|(key, h)| (h.name.clone(), key.clone(), h.tx.clone()))
        .collect();

    let registry = build_ctrl_registry(
        ctrl.memory.clone(),
        pending_connect.clone(),
        ctrl.self_project.clone(),
        pending_self_task.clone(),
        task_status_snapshot,
        ctrl.docs_index.clone(),
        ctrl.active_project.clone(),
        pending_stop.clone(),
        stop_snapshot,
    )
    .await;
    let openai_tools: Vec<ChatCompletionTool> = registry.openai_tools()?;

    Ok(CtrlTurnState {
        registry,
        openai_tools,
        side_effects: CtrlTurnSideEffects {
            pending_connect,
            pending_self_task,
            pending_stop,
        },
    })
}

// Purpose: Resolve `ctrl.toml` (or fall back to the built-in default).
pub(crate) async fn resolve_ctrl_turn_agent_config(ctrl: &Ctrl) -> (AgentConfig, Option<PathBuf>) {
    if let Some(self_path) = &ctrl.self_project {
        match resolve_ctrl_agent_config(self_path).await {
            Ok((c, p)) => (c, p),
            Err(e) => {
                tracing::warn!(error = %e, "failed to resolve ctrl agent config; using built-in default");
                (AgentConfig::ctrl_default(), None)
            }
        }
    } else {
        (AgentConfig::ctrl_default(), None)
    }
}

// Purpose: Assemble the full CTRL system prompt.
pub(crate) async fn build_ctrl_turn_system_prompt(
    ctrl: &Ctrl,
    user_input: &str,
    agent_cfg: &AgentConfig,
    agent_cfg_path: Option<&Path>,
    openai_tools_count: usize,
    mcp_cfg: &crate::mcp::GlobalConfig,
) -> String {
    let base_prompt = if agent_cfg.system_prompt.content.trim().is_empty() {
        CTRL_SYSTEM_PROMPT.to_string()
    } else {
        agent_cfg.system_prompt.content.clone()
    };

    let runner_label = match agent_cfg.agent.runner {
        crate::agents::RunnerKind::Subprocess => "subprocess",
        crate::agents::RunnerKind::Inline => "inline",
        crate::agents::RunnerKind::ClaudeCode => "claude-code",
        crate::agents::RunnerKind::InProcess => "in-process",
    };
    let mut builder = crate::agents::prompt_builder::SystemPromptBuilder::new(base_prompt)
        .with_agent_context(agent_cfg.agent.model.as_str(), runner_label);

    use crate::tools::traits::SkillResolver;
    let skill_resolver = crate::tools::skill_loader::FsSkillResolver::from_defaults();
    let mut injected_skills: std::collections::HashSet<String> = std::collections::HashSet::new();
    if let Some(skills) = &agent_cfg.system_prompt.skills
        && !skills.is_empty()
    {
        for s in skills {
            if let Some(text) = skill_resolver.resolve(s) {
                builder = builder.add_skill(format!("# Skill: {s}\n\n{text}"));
                injected_skills.insert(s.clone());
            } else {
                tracing::warn!(skill = %s, "ctrl skill not found; skipping");
            }
        }
    }

    {
        let config_dir = crate::default_bundled_config_dir();
        let search_paths = crate::skills::registry::skill_search_paths(&config_dir);
        let skill_reg = crate::skills::registry::SkillRegistry::load(&search_paths);
        let dynamic_skills = skill_reg.search(user_input, 3);
        for s in dynamic_skills {
            if injected_skills.contains(&s) {
                continue;
            }
            if let Some(text) = skill_resolver.resolve(&s) {
                builder = builder.add_skill(format!("# Skill: {s}\n\n{text}"));
                injected_skills.insert(s.clone());
                tracing::debug!(skill = %s, "ctrl: injected dynamic skill via BM25 search");
            }
        }
    }

    if let Some(section) = mcp_cfg.render_prompt_section("ctrl") {
        builder = builder.add_mcp_layer(section);
    }

    let is_ctrl_persona = agent_cfg.agent.name == "ctrl";

    if !is_ctrl_persona && let Some(proj) = &ctrl.self_project {
        let q = &user_input[..200.min(user_input.len())];
        let memories = recall_project_memories(proj, q, 5).await;
        if !memories.is_empty() {
            builder = builder.add_memory_layer(memories);
        }
    }

    let mut system_prompt = builder.build();

    {
        let project_root = ctrl
            .self_project
            .clone()
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
        let state_dir = project_root.join(".open-mpm").join("state");
        let tm_block = build_tm_context_block(&state_dir).await;
        if !tm_block.is_empty() {
            system_prompt.push_str("\n\n");
            system_prompt.push_str(&tm_block);
        }
    }

    if let Some(p) = &ctrl.self_project {
        system_prompt.push_str(&format!(
            "\n\nYou are running inside your own project at {}.\nYou can check your own status with self_project_status() and initiate development tasks on yourself with initiate_self_task(task).",
            p.display()
        ));
    }

    if let Some(up) = &ctrl.user_profile {
        let mut block = format!("\n\n## User Context\nUser name: {}", up.name);
        if let Some(email) = up.email.as_deref() {
            block.push_str(&format!("\nEmail: {email}"));
        }
        if let Some(tz) = up.timezone.as_deref() {
            block.push_str(&format!("\nTimezone: {tz}"));
        }
        system_prompt.push_str(&block);
    }

    {
        let now_str = chrono::Local::now()
            .format("%Y-%m-%d %H:%M:%S %Z")
            .to_string();
        system_prompt.push_str(&format!("\n\nCurrent date and time: {}", now_str));
    }

    if !is_ctrl_persona {
        let runner_label = match agent_cfg.agent.runner {
            crate::agents::RunnerKind::Subprocess => "subprocess (SubprocessAgentRunner)",
            crate::agents::RunnerKind::Inline => "inline (InlineAgentRunner)",
            crate::agents::RunnerKind::ClaudeCode => "claude-code (ClaudeCodeAgentRunner)",
            crate::agents::RunnerKind::InProcess => "in-process (InProcessAgentRunner)",
        };
        let skills_count = agent_cfg
            .system_prompt
            .skills
            .as_ref()
            .map(|s| s.len())
            .unwrap_or(0);
        let mcp_count = mcp_cfg.services_for_role("ctrl").len();
        let project_label = ctrl
            .self_project
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "(none — running standalone)".to_string());
        let config_label = agent_cfg_path
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "(built-in default — no on-disk ctrl.toml)".to_string());

        system_prompt.push_str(&build_deployment_footer(
            &agent_cfg.agent.name,
            runner_label,
            &agent_cfg.agent.model,
            crate::build_info::VERSION,
            skills_count,
            Some(openai_tools_count),
            Some(mcp_count),
            &project_label,
            Some(&config_label),
        ));
    }

    system_prompt
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

/// Run a single CTRL-level LLM turn with the four tools and apply
/// any queued side-effects (start_pm) when the turn returns.
///
/// Why: Non-slash input at the CTRL prompt should go through the assistant,
/// not directly to a PM, when no PM is active. Keeps the "terse senior dev"
/// voice as the CTRL experience and lets the LLM auto-route e.g. a bare
/// path into a start_pm call.
/// What: Reads like a recipe — prepare per-turn state, resolve agent config,
/// build the system prompt, dispatch the LLM call, then drain pending side
/// effects.
/// Test: `ctrl_chat_turn_routes_start_pm` / `ctrl_chat_turn_returns_text`.
pub(crate) async fn ctrl_chat_turn(ctrl: &mut Ctrl, user_input: &str) -> Result<String> {
    let dispatch_t0 = std::time::Instant::now();
    tracing::info!(
        input_len = user_input.len(),
        "ctrl_chat_turn: dispatch start"
    );

    let CtrlTurnState {
        registry,
        openai_tools,
        side_effects,
    } = prepare_ctrl_turn_state(ctrl).await?;
    let (agent_cfg, agent_cfg_path) = resolve_ctrl_turn_agent_config(ctrl).await;

    let mcp_cfg = crate::mcp::GlobalConfig::load().await;

    let system_prompt = build_ctrl_turn_system_prompt(
        ctrl,
        user_input,
        &agent_cfg,
        agent_cfg_path.as_deref(),
        openai_tools.len(),
        &mcp_cfg,
    )
    .await;

    let response_content = dispatch_ctrl_turn_llm(
        ctrl,
        user_input,
        &system_prompt,
        agent_cfg,
        registry,
        &mcp_cfg,
        dispatch_t0,
    )
    .await?;

    let mut outputs: Vec<String> = Vec::new();
    if !response_content.trim().is_empty() {
        outputs.push(response_content);
    }

    drain_ctrl_turn_side_effects(ctrl, &side_effects, &mut outputs).await;

    Ok(outputs.join("\n"))
}
