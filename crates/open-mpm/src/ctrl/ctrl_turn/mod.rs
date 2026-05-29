//! Single CTRL-level LLM turn dispatch.
//!
//! Why: The conversational ctrl turn (no PM attached) drives a focused LLM
//! call with a registry of tools that queue side effects (start_pm,
//! initiate_self_task, stop_task). Separating it from PM-task dispatch keeps
//! both lifecycles legible.
//! What: This module owns per-turn state preparation, agent-config resolution,
//! and system-prompt assembly plus the `ctrl_chat_turn` orchestrator; the LLM
//! dispatch + side-effect drain live in the `dispatch` submodule.
//! Test: Indirect — exercised via REPL integration tests; the side-effect
//! drain helpers are pure and could be unit-tested if the parent slot Arcs
//! were extracted into a small struct (kept here for now to keep diff small).

mod dispatch;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use async_openai::types::ChatCompletionTool;

use crate::agents::AgentConfig;
use crate::tools::ToolRegistry;

use super::config::{
    CTRL_SYSTEM_PROMPT, build_deployment_footer, recall_project_memories, resolve_ctrl_agent_config,
};
use super::handlers::{PmStatusRow, PmStopHandle, build_ctrl_registry, build_tm_context_block};
use super::state::Ctrl;

use dispatch::{dispatch_ctrl_turn_llm, drain_ctrl_turn_side_effects};

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
