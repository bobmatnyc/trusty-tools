//! Agent-config resolution, credential routing, and shared prompt fragments.
//!
//! Why: Both PM and ctrl-persona dispatch paths agree on the same set of
//! resolution rules (which TOML to load, what credentials to honor, how to
//! prefix the system prompt). Centralising the helpers keeps drift between
//! `run_pm_task_with_history`, `run_pm_task_with_persona`, and `ctrl_chat_turn`
//! impossible by construction.
//! What: `SessionOverrides`, `resolve_overridden_credentials`,
//! `resolve_agent_config`, `resolve_ctrl_agent_config`,
//! `apply_credential_routing`, `build_deployment_footer`,
//! `build_user_context_prefix`, and `recall_project_memories`.
//! Test: `apply_credential_routing_*`, `build_deployment_footer_*`, and
//! `resolve_agent_config_*` cases in `ctrl::tests`.

use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::agents::AgentConfig;
use crate::llm;

/// Session-scoped overrides applied at PM dispatch time (#284).
///
/// Why: The REPL `/model` and `/provider` slash commands let the user pin a
/// model id or credential routing path for the rest of the session without
/// editing TOML. The override fields are passed through to
/// `run_pm_task_with_persona` and `run_pm_task_with_history` so they can be
/// applied AFTER `AgentConfig::load()` and BEFORE `apply_credential_routing()`.
/// What: Two optional knobs. `model` overrides `cfg.agent.model`. `provider`
/// is one of `"claude-code"`, `"bedrock"`, `"openrouter"` and replaces the
/// `pick_credentials()` env probe for the duration of the dispatch.
/// Bookmarks: `"anthropic-api"` and `"openai-api"` are NOT yet wired up — add
/// them to `resolve_overridden_credentials` when the time comes.
/// Test: Construct via `Default::default()`; existing call sites pass that
/// sentinel and behave exactly as before. Wiring is verified by `cargo check`.
#[derive(Debug, Clone, Default)]
pub struct SessionOverrides {
    pub model: Option<String>,
    pub provider: Option<String>,
    /// Resolved principal for this dispatch (#481).
    ///
    /// Why: Transports that authenticate users (Slack RBAC) must carry the
    /// caller's identity into tool dispatch so `filter_tools_for_user` gates
    /// the persona toolset by the caller's `ServiceTier` rather than the
    /// default CLI identity (which is `All` — unrestricted). `None` falls
    /// back to `UserIdentity::default()` so existing CLI/REPL callers behave
    /// exactly as before.
    pub user: Option<crate::rbac::UserIdentity>,
}

/// Resolve the effective `LlmCredentials` honoring an optional session
/// `provider_override` (#284).
///
/// Why: When the user has run `/provider <name>`, we must bypass the normal
/// `pick_credentials()` env probe and route through the requested credential
/// path instead. Centralising the override-vs-env decision here keeps both
/// dispatch entrypoints (`run_pm_task_with_history`, `run_pm_task_with_persona`)
/// in lock-step.
/// What: Three valid override values:
///   - `"claude-code"` → `LlmCredentials::ClaudeCode` (claude CLI subprocess)
///   - `"openrouter"` → `LlmCredentials::OpenRouter` (REST client)
///   - `"bedrock"`    → ensures the model id carries the `bedrock/` prefix
///     (auto-prepending when absent) and returns `LlmCredentials::OpenRouter`
///     as a placeholder. Bedrock dispatch is driven by the `bedrock/` model
///     prefix in `chat_with_tools_gated`, not by a credential variant — see
///     `src/llm/mod.rs` Bedrock branch.
/// Any other override value returns an error so the user's typo doesn't
/// silently fall through to env defaults. When `provider_override` is `None`
/// we delegate to `pick_credentials()` exactly as before.
/// Test: `cargo check`; `apply_credential_routing` tests still pass since
/// this helper is composed before that point.
pub(crate) fn resolve_overridden_credentials(
    cfg: &mut AgentConfig,
    provider_override: Option<&str>,
) -> Result<llm::credentials::LlmCredentials> {
    use llm::credentials::LlmCredentials;
    match provider_override {
        Some("claude-code") => Ok(LlmCredentials::ClaudeCode),
        Some("openrouter") => Ok(LlmCredentials::OpenRouter),
        Some("bedrock") => {
            // Bedrock dispatch is model-prefix driven. Ensure prefix is set.
            if !cfg.agent.model.starts_with("bedrock/") {
                cfg.agent.model = format!("bedrock/{}", cfg.agent.model);
            }
            // Placeholder credential — adapter inspects the model prefix and
            // routes to AWS Bedrock; the OpenRouter variant just lets
            // `apply_credential_routing` skip the use_anthropic_direct flag
            // and the claude-cli short-circuit. The OpenRouter path's bare-
            // model qualification is a no-op since the model already starts
            // with `bedrock/` (see `qualify_openrouter_model`).
            Ok(LlmCredentials::OpenRouter)
        }
        Some("local") => {
            // Ollama dispatch is also model-prefix driven (`ollama/<name>`).
            // The adapter detects the prefix and overrides the OpenAI-compatible
            // base URL to point at the local ollama server. We piggyback on the
            // OpenRouter credential variant since ollama needs no auth header;
            // the LLM HTTP layer will skip auth when the endpoint's
            // `auth_header_value` is empty (see `OllamaAdapter::api_endpoint`).
            if !cfg.agent.model.starts_with("ollama/") {
                cfg.agent.model = format!("ollama/{}", cfg.agent.model);
            }
            Ok(LlmCredentials::OpenRouter)
        }
        // Bookmarked for future wiring: "anthropic-api", "openai-api".
        Some(other) => anyhow::bail!(
            "unknown provider override '{}'. Valid: openrouter, claude-code, bedrock, local",
            other
        ),
        None => llm::credentials::pick_credentials(Some(cfg.agent.runner))
            .ok_or_else(|| anyhow::anyhow!("{}", llm::credentials::missing_credentials_error())),
    }
}

/// Build the `## User Context` block that prefixes the system prompt for both
/// `run_pm_task_with_history` and `run_pm_task_with_persona`.
///
/// Why: The two PM dispatch paths used to hand-roll an identical block to
/// inject the user's name, timezone, and current local date/time so the LLM
/// can address the user and answer "what time is it?" naturally. The two
/// copies had drifted in their unknown-user branch (one omitted the
/// `user_name = "(unknown)"` line). Extracting one helper closes the
/// divergence and gives every future caller the same context format.
/// What: Loads `UserProfile`, formats `chrono::Local::now()` as
/// `YYYY-MM-DD HH:MM:SS TZ`, prepends a `## User Context` block, and returns
/// the combined string with `base_content` appended after a blank line.
/// Test: Indirectly via the PM/persona dispatch paths; absence of profile
/// should still produce a `user_name = "(unknown)"` line and a current
/// date/time line.
pub(crate) fn build_user_context_prefix(base_content: &str) -> String {
    use crate::identity::user_profile::UserProfile;
    let profile = UserProfile::load();
    let now_local = chrono::Local::now();
    let now_str = now_local.format("%Y-%m-%d %H:%M:%S %Z").to_string();
    match profile {
        Some(ref p) if !p.name.trim().is_empty() => format!(
            "## User Context\nuser_name = \"{}\"\ntimezone = \"{}\"\nCurrent date and time: {}\n\n{}",
            p.name,
            p.timezone.as_deref().unwrap_or("UTC"),
            now_str,
            base_content
        ),
        _ => format!(
            "## User Context\nuser_name = \"(unknown)\"\nCurrent date and time: {}\n\n{}",
            now_str, base_content
        ),
    }
}

/// Best-effort semantic recall over the project's embedded memory store (#275).
///
/// Why: The PM and ctrl prompts get a project-memory layer so the LLM is
/// grounded in prior decisions/conventions without the user re-stating them.
/// Previously this shelled out to the `kuzu-memory` MCP binary; that path was
/// fire-and-forget (no Rust write site, silent empty on missing binary) and
/// shared no schema with the in-process redb+usearch store where every other
/// memory tool actually writes. This helper routes recall through the same
/// store + embedder used by `memory_recall`, eliminating split-brain memory.
/// What: Opens `<project>/.open-mpm/sessions/default` as a `RedbUsearchStore`,
/// embeds the query via `FastEmbedder`, searches `Segment::AgentMemory`, and
/// returns up to `top_k` `payload.content` strings (falling back to the raw
/// JSON payload when no `content` field is present). Any error — store
/// missing, embedder init failure, search error — collapses to an empty Vec
/// so prompt building never blocks on memory recall.
/// Test: Both call sites (PM `run_pm_task_with_history` and ctrl
/// `run_ctrl`) exercise the empty-Vec path on any cold project; populated
/// recall is covered by `memory_recall` integration tests in `tools/memory.rs`.
pub(crate) async fn recall_project_memories(
    project_dir: &Path,
    query: &str,
    top_k: usize,
) -> Vec<String> {
    let session_dir = project_dir
        .join(".open-mpm")
        .join("sessions")
        .join("default");
    if !session_dir.exists() {
        return Vec::new();
    }
    let store = match crate::memory::open_memory_store(&session_dir) {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!(error = %e, "recall_project_memories: store open failed");
            return Vec::new();
        }
    };
    let embedder = match crate::memory::FastEmbedder::new() {
        Ok(e) => e,
        Err(e) => {
            tracing::debug!(error = %e, "recall_project_memories: embedder unavailable");
            return Vec::new();
        }
    };
    let qvec = match crate::memory::Embedder::embed_single(&embedder, query) {
        Ok(v) => v,
        Err(e) => {
            tracing::debug!(error = %e, "recall_project_memories: embed failed");
            return Vec::new();
        }
    };
    let hits = match store
        .search(crate::memory::Segment::AgentMemory, &qvec, top_k)
        .await
    {
        Ok(h) => h,
        Err(e) => {
            tracing::debug!(error = %e, "recall_project_memories: search failed");
            return Vec::new();
        }
    };
    hits.into_iter()
        .map(|h| {
            h.payload
                .get("content")
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .unwrap_or_else(|| h.payload.to_string())
        })
        .filter(|s| !s.trim().is_empty())
        .collect()
}

/// Resolve the agent config that drives `run_pm_task_with_history` (#240).
///
/// Why: The REPL has two modes — connected (a project has been attached via
/// `/connect`, so `pm.toml` is the source of truth) and standalone (no
/// project, so `ctrl.toml` should be loaded from the user's home directory).
/// Previously the controller hard-coded a single `ctrl.toml` lookup under
/// the project's `.open-mpm/agents/` directory and failed loudly when it
/// wasn't there. This helper centralizes the priority order and keeps the
/// REPL launchable even with zero on-disk config.
/// What: Tries, in order:
///   1. `{project_path}/.open-mpm/agents/pm.toml` (connected mode)
///   2. `~/.open-mpm/agents/ctrl.toml` (user-level standalone)
///   3. `{project_path}/.open-mpm/agents/ctrl.toml` (project-level ctrl)
///   4. `AgentConfig::ctrl_default()` — the bundled fallback.
/// Test: `resolve_agent_config_prefers_pm_toml`,
/// `resolve_agent_config_falls_back_to_user_ctrl`,
/// `resolve_agent_config_falls_back_to_project_ctrl`,
/// `resolve_agent_config_returns_builtin_when_nothing_on_disk`.
pub(crate) async fn resolve_agent_config(
    project_path: &Path,
) -> Result<(AgentConfig, Option<PathBuf>)> {
    let pm_path = project_path
        .join(".open-mpm")
        .join("agents")
        .join("pm.toml");
    if pm_path.is_file() {
        return Ok((AgentConfig::load(&pm_path)?, Some(pm_path)));
    }

    if let Some(home) = dirs::home_dir() {
        let user_ctrl = home.join(".open-mpm").join("agents").join("ctrl.toml");
        if user_ctrl.is_file() {
            return Ok((AgentConfig::load(&user_ctrl)?, Some(user_ctrl)));
        }
    }

    let project_ctrl = project_path
        .join(".open-mpm")
        .join("agents")
        .join("ctrl.toml");
    if project_ctrl.is_file() {
        return Ok((AgentConfig::load(&project_ctrl)?, Some(project_ctrl)));
    }

    Ok((AgentConfig::ctrl_default(), None))
}

/// Resolve the agent config used by `ctrl_chat_turn` — the conversational
/// ctrl persona, NOT the PM coordinator (#298).
///
/// Why: `resolve_agent_config` was historically shared between
/// `run_pm_task_with_history` (which legitimately wants pm.toml) and
/// `ctrl_chat_turn` (which wants ctrl.toml). When the harness runs INSIDE
/// its own repo (`detect_self_project()` succeeds and points at open-mpm),
/// the project's `.open-mpm/agents/pm.toml` exists and shadowed ctrl.toml,
/// causing every ctrl turn to load the heavy sonnet PM prompt. Result:
/// 30s responses for "hello" because ctrl was running PM-shaped requests
/// against claude-sonnet-4-6 with the full delegation tool surface.
/// What: Searches for `ctrl.toml` first (project then user) and only falls
/// back to pm.toml when neither ctrl.toml is available — and even then the
/// caller should treat this as a legacy path rather than a happy path.
/// Order:
///   1. `{project_path}/.open-mpm/agents/ctrl.toml`
///   2. `~/.open-mpm/agents/ctrl.toml`
///   3. `{project_path}/.open-mpm/agents/pm.toml` (last-resort)
///   4. `AgentConfig::ctrl_default()`
/// Test: `resolve_ctrl_agent_config_prefers_project_ctrl_over_pm`,
/// `resolve_ctrl_agent_config_falls_back_to_user_ctrl`.
pub(crate) async fn resolve_ctrl_agent_config(
    project_path: &Path,
) -> Result<(AgentConfig, Option<PathBuf>)> {
    let project_ctrl = project_path
        .join(".open-mpm")
        .join("agents")
        .join("ctrl.toml");
    if project_ctrl.is_file() {
        return Ok((AgentConfig::load(&project_ctrl)?, Some(project_ctrl)));
    }

    if let Some(home) = dirs::home_dir() {
        let user_ctrl = home.join(".open-mpm").join("agents").join("ctrl.toml");
        if user_ctrl.is_file() {
            return Ok((AgentConfig::load(&user_ctrl)?, Some(user_ctrl)));
        }
    }

    let pm_fallback = project_path
        .join(".open-mpm")
        .join("agents")
        .join("pm.toml");
    if pm_fallback.is_file() {
        return Ok((AgentConfig::load(&pm_fallback)?, Some(pm_fallback)));
    }

    Ok((AgentConfig::ctrl_default(), None))
}

/// Apply the canonical 3-way credential routing rules to `cfg` (#271).
///
/// Why: The same credential-routing block was copy-pasted across
///   `run_pm_task_with_history`, `run_pm_task_with_persona`, and (after #271)
///   `ctrl_chat_turn`. Centralising it means every dispatch path agrees on
///   precedence (ClaudeCode > AnthropicDirect > OpenRouter) and any future
///   credential type only needs to be wired up in one place.
/// What: For `AnthropicDirect` flips `cfg.llm.use_anthropic_direct = true`
///   (forces the chat loop down the api.anthropic.com path). For `OpenRouter`
///   qualifies bare Claude / Anthropic model ids with the `anthropic/`
///   provider prefix. For `ClaudeCode` does nothing — the caller is expected
///   to short-circuit to `run_pm_task_via_claude_cli` separately because that
///   path takes a different shape (no async-openai client, single-shot CLI).
///   Returns `true` when the caller MUST short-circuit to the claude CLI.
/// Test: `apply_credential_routing_anthropic_direct_sets_flag`,
///   `apply_credential_routing_openrouter_qualifies_model`,
///   `apply_credential_routing_claude_code_signals_short_circuit`.
pub(crate) fn apply_credential_routing(
    cfg: &mut AgentConfig,
    creds: &llm::credentials::LlmCredentials,
) -> bool {
    use llm::credentials::LlmCredentials;
    match creds {
        LlmCredentials::AnthropicDirect => {
            cfg.llm.use_anthropic_direct = true;
            false
        }
        LlmCredentials::ClaudeCode => true,
        LlmCredentials::OpenRouter => {
            let qualified = llm::credentials::qualify_openrouter_model(creds, &cfg.agent.model);
            if qualified != cfg.agent.model {
                tracing::debug!(
                    from = %cfg.agent.model,
                    to = %qualified,
                    "qualifying bare claude model id for OpenRouter"
                );
                cfg.agent.model = qualified;
            }
            false
        }
    }
}

/// Build the canonical "## Deployment Configuration" footer for a system
/// prompt (#271).
///
/// Why: The PM/ctrl injects a deployment-context block so the LLM can answer
///   "what model am I running?" honestly. Previously two call sites
///   (`run_pm_task_with_history` and `ctrl_chat_turn`) built nearly-identical
///   blocks with slightly different fields, drifting over time. Centralising
///   keeps the wording consistent for users and lets future fields (e.g. a
///   tracing session id) be added in one place.
/// What: Returns a leading `\n\n` plus a markdown bullet list. Optional
///   fields (`tools_count`, `mcp_count`, `config_label`) are omitted when
///   `None` so call sites can pass only what they have on hand.
/// Test: `build_deployment_footer_includes_required_fields`,
///   `build_deployment_footer_omits_optional_fields_when_none`.
// Why: Footer assembly genuinely needs every one of these fields and they
// are all flat scalars — bundling them into a struct just to satisfy the
// 7-argument heuristic would obscure the call sites without buying real
// abstraction. Allow locally.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_deployment_footer(
    agent_name: &str,
    runner_label: &str,
    model: &str,
    version: &str,
    skills_count: usize,
    tools_count: Option<usize>,
    mcp_count: Option<usize>,
    project_label: &str,
    config_label: Option<&str>,
) -> String {
    let mut out = String::from("\n\n## Deployment Configuration\n");
    out.push_str(&format!(" - Agent: {agent_name}\n"));
    out.push_str(&format!(" - Model: {model}\n"));
    out.push_str(&format!(" - Runner: {runner_label}\n"));
    out.push_str(&format!(" - Version: v{version}\n"));
    if let Some(tools) = tools_count {
        out.push_str(&format!(" - Tools available: {tools}\n"));
    }
    out.push_str(&format!(" - Skills loaded: {skills_count}\n"));
    if let Some(mcp) = mcp_count {
        out.push_str(&format!(" - MCP connections: {mcp}\n"));
    }
    out.push_str(&format!(" - Project: {project_label}\n"));
    if let Some(cfg) = config_label {
        out.push_str(&format!(" - Config: {cfg}\n"));
    }
    out
}

/// Base system prompt for the CTRL LLM — terse senior-dev voice.
///
/// Why: CTRL talks to the user often (project switching, "what did I do
/// yesterday?", memory recall). A fluffy assistant style wastes their time.
/// Under 300 tokens; the LLM should answer in sentences not paragraphs.
/// What: Documents the four tools CTRL has and the expected behaviors.
// #185: Taskmaster persona — autonomous, results-driven coordination.
pub(crate) const CTRL_SYSTEM_PROMPT: &str = "You are the Taskmaster — an autonomous project coordination controller that manages AI coding projects and drives tasks to completion.

## Your Persona
You are proactive, direct, and results-driven. You don't just route tasks — you own them until they're done. When something breaks, you fix it or clearly explain why you can't.

## Core Responsibilities
1. **Drive tasks to completion**: When a PM is working on a task, monitor progress. If a phase fails, attempt recovery before escalating.
2. **Handle blockers**: Try to resolve failures autonomously (retry with context, switch approach, load a relevant skill) up to 2 times. Only escalate to the user when you've exhausted options, and when you do, be specific: what failed, what you tried, what you need.
3. **Communicate status clearly**: Proactive updates — 'Task X: code phase complete (wave 3/5), QA starting', 'BLOCKED: QA failed twice on bcrypt error — applying python-compat fix and retrying'.
4. **Track task state**: Maintain awareness of what's queued, running, blocked, and done.
5. **Post-task debrief**: After each task, give a concise summary: what was built, test results, any retries needed, cost.

## Tools Available
- start_pm(project_path) → start a PM session for a project
- list_projects() → known projects
- self_project_status() → your own project's version and git state
- initiate_self_task(task) → run a task on your own project (self-improvement)
- task_status() → list active and recently completed PM tasks
- memory_store/memory_recall → cross-project context
- search_docs(query) → search project documentation semantically. Use this to answer questions about how open-mpm works, its configuration, agents, skills, and workflows.

## Rules
- Never say 'I can't help with that' — find a path forward or explain the specific blocker
- Always confirm task completion with evidence (test counts, file counts, cost)
- When a task runs >30 min without output, proactively check status
- Prefer action over asking for permission on routine decisions
";
