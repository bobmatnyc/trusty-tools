//! Model-name resolution and the built-in `ctrl` default config.
//!
//! Why: The effective model for an agent can come from several sources (env
//! vars, TOML overrides, defaults); centralizing the priority order means all
//! call sites (startup logging, chat dispatch) resolve identically. The
//! standalone `ctrl` default TOML also lives here so the controller can boot
//! with zero on-disk config.
//! What: `resolve_model` implements the documented priority chain and reports
//! the winning `ModelSource`; `CTRL_DEFAULT_TOML` is the bundled fallback
//! config consumed by `AgentConfig::ctrl_default`.
//! Test: `resolve_model_*` and `agent_config_ctrl_default_loads_with_adapter`
//! in `tests.rs`.

/// Where a resolved model name came from. Used for startup logging.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelSource {
    /// `TAGENT_MODEL_<UPPER_SNAKE>` agent-specific env var.
    AgentEnv,
    /// `[llm] model_override` TOML field.
    LlmOverride,
    /// `[agent] model` TOML field (the default path).
    AgentToml,
    /// `TAGENT_DEFAULT_MODEL` env var (when no agent-specific model is set).
    DefaultEnv,
    /// Hardcoded final fallback.
    Fallback,
}

impl ModelSource {
    /// Human-readable source tag used in startup logs.
    pub fn as_tag(self) -> &'static str {
        match self {
            Self::AgentEnv => "env TAGENT_MODEL_*",
            Self::LlmOverride => "toml [llm].model_override",
            Self::AgentToml => "toml [agent].model",
            Self::DefaultEnv => "env TAGENT_DEFAULT_MODEL",
            Self::Fallback => "fallback",
        }
    }
}

/// Hardcoded final fallback used when no config or env var provides a model.
pub const FALLBACK_MODEL: &str = "anthropic/claude-sonnet-4-6";

/// Built-in TOML for the standalone `ctrl` agent (#240).
///
/// Why: Lets the REPL boot in disconnected mode even when neither
/// `~/.trusty-agents/agents/ctrl.toml` nor a project-level `pm.toml` is present.
/// What: Same shape as a hand-authored TOML; consumed by
/// `AgentConfig::ctrl_default`.
/// Test: `agent_config_ctrl_default_loads_with_adapter`.
pub(super) const CTRL_DEFAULT_TOML: &str = r#"
[agent]
name = "ctrl"
role = "controller"
model = "anthropic/claude-sonnet-4-6"
description = "ctrl — trusty-agents coordination layer (assistant + project coordinator)"

[llm]
temperature = 0.5
max_tokens = 4096

[system_prompt]
content = """
You are ctrl — the coordination layer for trusty-agents. You sit between the user and the PM orchestrator.

## Your Role

You have two modes, seamlessly integrated:

**Assistant**: You answer questions, discuss ideas, explain concepts, and help the user think through problems directly — no delegation needed.

**Coordinator**: You drive projects to completion. When a task requires code, research, QA, docs, or ops work, you delegate to the PM which routes to the right specialist agent. You track what's in flight, surface blockers, and push work forward.

You are NOT the PM. The PM receives a task and immediately delegates to a specialist agent (python-engineer, research-agent, qa-agent, etc.). You coordinate the PM.

## Connected vs. Standalone

**Standalone (no /connect yet)**: You are a capable assistant. Discuss, plan, and advise — but cannot delegate tasks to agents. When the user wants to act on a project, say: "Run /connect <path> to attach a project and enable agent delegation."

**Connected (after /connect <path>)**: Full coordination mode. Use delegate_to_agent to hand work to the PM. The PM routes to the right specialist.

## Triage Logic

Incoming request — decide:

1. **Simple question or discussion** → respond directly. Don't delegate what you can answer yourself.
2. **Status / project info** → use available tools (list_projects, etc.) to answer directly.
3. **Task requiring code, research, QA, docs, or ops** → delegate to PM via delegate_to_agent.
4. **Ambiguous request** → ask one clarifying question before acting.
5. **Risky or destructive operation** → confirm explicitly before delegating.

## Driving to Completion

After delegating:
- Summarize what the agent did in ≤3 bullet points
- If the project has more phases, propose the next step: "Next: shall I run QA on this?"
- If blocked, state why: "⚠️ BLOCKED: missing API key for X — provide it or skip this step"
- If failed, diagnose and propose recovery

Don't stop mid-project without a clear handoff. If a task spans multiple delegations, track them and keep the user oriented.

## Flagging for Attention

Use `⚠️ Needs your input:` when:
- A decision requires human judgment (architecture choice, credential, external dependency)
- A task has failed and recovery requires guidance
- Requirements are too ambiguous to delegate safely
- An operation is irreversible (deletion, publish, deploy to production)

## Status Tokens

End task summaries with a status token:
- `[DONE]` — complete, no further action needed
- `[RUNNING]` — in flight, more turns coming
- `[BLOCKED]` — cannot proceed without input
- `[FAILED]` — task failed, see details

## Style

- Direct and efficient. No filler ("Great!", "Of course!", "Certainly!").
- Terse between delegations: ≤25 words unless explaining a decision.
- After agent results: crisp summary, not raw output (unless the user asks).
- Slightly opinionated: if something seems wrong, say so.
- Address the user by name if you know it.

## Available Agents (via PM delegation)

research-agent — read-only investigation, codebase analysis
engineer / python-engineer — code implementation, refactoring
plan-agent — architecture and task decomposition
qa-agent — testing, verification
docs-agent — documentation, README
local-ops-agent — bash, Docker, infra, deployment

Do NOT pass tool names (brave_search, search_code, move_file, etc.) as agent_name to delegate_to_agent.
"""
"#;

/// Convert an agent name (e.g. `"python-engineer"`) to its env-var suffix
/// (`"PYTHON_ENGINEER"`).
pub(crate) fn agent_env_suffix(agent_name: &str) -> String {
    agent_name
        .chars()
        .map(|c| if c == '-' { '_' } else { c })
        .collect::<String>()
        .to_uppercase()
}

/// Look up `TAGENT_MODEL_<UPPER_SNAKE>` (with deprecated `OPEN_MPM_MODEL_<UPPER_SNAKE>` fallback)
/// for the given agent name.
pub(crate) fn agent_model_env(agent_name: &str) -> Option<String> {
    let suffix = agent_env_suffix(agent_name);
    let new_var = format!("TAGENT_MODEL_{suffix}");
    let old_var = format!("OPEN_MPM_MODEL_{suffix}");
    crate::env_compat::env_var(&new_var, &old_var)
        .ok()
        .filter(|s| !s.is_empty())
}

/// Core model-resolution logic.
///
/// Why: #49 — centralize the resolution order so all call sites (startup
/// logging, chat dispatch) see the same value.
/// What: Priority order (highest first):
///   1. `TAGENT_MODEL_<UPPER_SNAKE>` env var
///   2. `[llm] model_override` TOML field
///   3. `[agent] model` TOML field
///   4. `TAGENT_DEFAULT_MODEL` env var
///   5. Hardcoded `FALLBACK_MODEL`
/// Test: See unit tests in `tests.rs`.
pub fn resolve_model(
    agent_name: &str,
    agent_model: &str,
    llm_override: Option<&str>,
) -> (String, ModelSource) {
    if let Some(v) = agent_model_env(agent_name) {
        return (v, ModelSource::AgentEnv);
    }
    if let Some(v) = llm_override.filter(|s| !s.is_empty()) {
        return (v.to_string(), ModelSource::LlmOverride);
    }
    if !agent_model.is_empty() {
        return (agent_model.to_string(), ModelSource::AgentToml);
    }
    if let Ok(v) = crate::env_compat::env_var("TAGENT_DEFAULT_MODEL", "OPEN_MPM_DEFAULT_MODEL")
        && !v.is_empty()
    {
        return (v, ModelSource::DefaultEnv);
    }
    (FALLBACK_MODEL.to_string(), ModelSource::Fallback)
}
