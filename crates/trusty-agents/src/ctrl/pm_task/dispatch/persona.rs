//! Persona-agent chat turn (#254).
//!
//! Why: The REPL `/agent` command switches the active ctrl conversation to a
//! non-coding persona (e.g. `personal-assistant`). These personas have their
//! own system prompt, model, and tools-gated registry — distinct enough from
//! the history-delegation path to live in their own file.
//! What: `run_pm_task_with_persona`.
//! Test: Manual via tmux — `/agent personal-assistant` then "who are you?".

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};

use crate::agents::AgentConfig;
use crate::llm;
use crate::subprocess::SubprocessAgentRunner;
use crate::tools::registry::dead_scope;
use crate::tools::registry::scope::ScopePattern;
use crate::tools::{AgentRunner, ToolRegistry, delegate::DelegateToAgentTool};

use super::super::super::claude_cli::run_pm_task_via_claude_cli;
use super::super::super::config::{
    AgentIdentity, SessionOverrides, apply_credential_routing, build_user_context_prefix,
    resolve_overridden_credentials,
};
use super::super::super::handlers::{
    AddProjectTool, CreateDirTool, ListProjectsTool, MoveFileTool, RemoveProjectTool,
    SetActiveProjectTool, StopTaskTool, register_ticketing_tools,
};
use super::super::super::state::ConversationTurn;
use super::classification;
use super::persona_gate::{
    filter_persona_tool_names_for_tier, persona_allowed_tools, persona_max_turns,
};
use super::persona_memory;

/// Run a single conversation turn against a persona agent (#254).
///
/// Why: The REPL `/agent` command lets the user switch the active ctrl
/// conversation to a non-coding persona (e.g. `personal-assistant` /
/// `cto-assistant`). #3285 (Bob decision, 2026-07-19): named personas must
/// be able to call tools directly, so this path now reaches TOOL PARITY
/// with the session path (`run_pm_task_with_history`) — the same
/// delegation (`delegate_to_agent`), CTRL project-management
/// (`add_project`, `list_projects`, `remove_project`, `stop_task`,
/// `set_active_project`), filesystem (`move_file`, `create_dir`), search
/// (`search_code`), shell (`run_bash`), ticketing, `tm`, and live-MCP tools
/// are all REGISTERED into the persona's registry below. Registering a tool
/// is not the same as granting it, though: what actually reaches the LLM is
/// still gated per persona by `filter_persona_tool_names`, which keeps only
/// names matching the persona's `[tools].allow` glob list AND passing the
/// RBAC-tier filter (`registry.filter_tools_for_user`) — a persona can only
/// reach what it explicitly declares. A persona with no `[tools].allow`
/// section at all still gets zero tools (see the `else` arm below),
/// preserving the original tools-OFF-by-default posture for personas that
/// don't opt in. Full `[tools].scopes` (OpenRPC scope-pattern) enforcement
/// is tracked separately by #3208 and is not implemented here; this change
/// does not widen a persona's reach beyond its declared `allow` list.
/// What: Loads `<project>/.trusty-agents/agents/<persona_name>.toml`, builds the
/// same date/time-injected system prompt the default ctrl path uses, arms a
/// `ToolRegistry` filtered per the persona's declared `allow` globs, then
/// makes a `chat_with_tools_gated` call carrying the prior conversation
/// history. Returns the assistant text.
/// Test: `filter_persona_tool_names_respects_allow_globs`,
/// `filter_persona_tool_names_new_delegation_tools_require_explicit_allow`,
/// `filter_persona_tool_names_delegation_tools_surface_when_allowed`,
/// `filter_persona_tool_names_respects_rbac_tier` pin the allow/scope
/// gating at the unit level. Manual via tmux — `/agent personal-assistant`
/// then "who are you?" → identifies as Izzie, knows Masa.
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

    // #3223/#3224 (Trusty Agents agent roster, epic #3052): resolve
    // through the canonical `AgentConfig::by_name_async` loader instead of
    // the hand-rolled project/$HOME `.toml`-only lookup this function used
    // to do inline. `by_name_async` walks the SAME tier order the old code
    // did (project `.trusty-agents/agents/` then `$HOME/.trusty-agents/agents`,
    // #3061) but ALSO resolves flat `.md` personalization overlays (with
    // their `extends:` chain, #3055) and directory-package agents — neither
    // of which the old inline lookup understood. Practically: a roster
    // entry backed by a user's `~/.trusty-agents/agents/<slug>.md` overlay
    // (written by the personalization editor) now dispatches correctly
    // through this persona-chat path, not just the subprocess `--direct`
    // path that already used `by_name_async` internally.
    let mut persona_cfg = AgentConfig::by_name_async(persona_name)
        .await
        .with_context(|| format!("persona agent '{persona_name}' not found"))?;

    if let Some(ref m) = overrides.model {
        tracing::debug!(persona = %persona_name, model = %m, "applying /model override");
        persona_cfg.agent.model = m.clone();
    }

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
        // DOC-54 §9.6 note: the claude-cli subprocess path runs an entirely
        // separate execution model and is out of scope for the filterable-
        // context slice — deferred, documented in the PR body.
        return run_pm_task_via_claude_cli(project_path, &persona_cfg, user_input, history, "")
            .await;
    }
    let persona_llm_t0 = std::time::Instant::now();

    let client = llm::create_client()?;

    // DOC-54 §9.6.1/§9.6.3 (filterable context, demo-day 2026-07-24):
    // fetch the closed label vocabulary + (when focused) assemble the
    // focused-mode context block BEFORE the system prompt is built below,
    // so both land in the same prompt-cache-stable position every turn.
    // Computed once and threaded explicitly (not re-derived via a hidden
    // global inside `classification`) so both `build_turn_context` and
    // `finish_turn` below share one value and remain independently testable
    // against a mock daemon.
    let workstreams_base_url = crate::memory::trusty_client::default_trusty_url();
    let turn_ctx = classification::build_turn_context(
        project_path,
        overrides.focused_workstream.as_deref(),
        persona_cfg.workstreams.recent_window,
        persona_cfg.workstreams.enabled,
        &workstreams_base_url,
    )
    .await;

    // Issue #3928: recall the agent's OWN bound palace (`[[stores]].palace`,
    // #3878) for this turn. Until now that binding was inert on the chat path
    // — only `default_search_index()` was read (for `vector_search` routing
    // below) — so the runtime handed the model no memory at all and personas
    // truthfully-but-wrongly reported themselves as stateless. Resolved here,
    // alongside the DOC-54 turn context, so both land before the system
    // prompt is assembled.
    let memory_search_base = trusty_common::resolve_daemon_base_url("trusty-search");
    let persona_memory = persona_memory::build_persona_memory(
        &persona_cfg.stores,
        Some(&workstreams_base_url),
        memory_search_base.as_deref(),
        user_input,
    )
    .await;

    // #3933: compile `[skills].allow` down to exact tool names and UNION them
    // with `[tools].allow` before the gate below. This is a resolver in front
    // of the enforcement path, not a change to it — `patterns` still feeds the
    // same `filter_persona_tool_names` call it always did, so the RBAC tier
    // filter, the scope gate and `persona_allowed_tools`'s never-`None`
    // behaviour are untouched. With no `[skills]` section the returned value is
    // `persona_cfg.tools.allow` verbatim, which is why this is invisible to
    // every existing agent. A dangling skill id resolves to nothing and is
    // logged here rather than silently dropped (DOC-57 S-11).
    let skill_catalog = crate::skills::manifest::SkillCatalog::builtin().with_authored(
        crate::skills::manifest::authored::load_from_paths(
            &crate::skills::sources::SkillSourceRegistry::load(project_path).resolved_paths(),
        ),
    );
    let (effective_patterns, unresolved_skills) = crate::skills::manifest::effective_tool_patterns(
        persona_cfg.tools.allow.as_ref(),
        persona_cfg.skills.allow.as_ref(),
        &skill_catalog,
    );
    if !unresolved_skills.is_empty() {
        tracing::warn!(
            persona = %persona_name,
            unresolved = ?unresolved_skills,
            "[skills].allow names skills that resolved to nothing; they grant no tools"
        );
    }

    let (persona_registry, persona_tool_names): (ToolRegistry, Vec<String>) =
        if let Some(patterns) = effective_patterns {
            let mut registry = ToolRegistry::new();
            for tool in crate::tools::mcp_tools::mcp_tool_executors() {
                registry.register(tool);
            }
            for tool in crate::tools::mcp_service_tools::mcp_service_tool_executors().await {
                registry.register(tool);
            }
            // #3987: `advertised_scopes` is the UNFILTERED vocabulary every
            // endpoint published, captured here because the registry only
            // keeps what the operator's `[[endpoints]].scopes` policy let
            // through. Feeding the dead-grant diagnostic the post-policy set
            // would make an operator's deliberate narrowing look like a
            // broken agent grant — see `build_with_scope_vocabulary`.
            let mut advertised_scopes: Vec<String> = Vec::new();
            {
                let global_config = crate::mcp::config::GlobalConfig::load().await;
                match crate::tools::registry::ToolRegistryBuilder::from_config(&global_config)
                    .build_with_scope_vocabulary()
                    .await
                {
                    Ok((execs, vocabulary)) => {
                        for tool in execs {
                            registry.register(tool);
                        }
                        advertised_scopes = vocabulary;
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

            // #3285: tool parity with the session path
            // (`run_pm_task_with_history`) — delegation, CTRL
            // project-management, filesystem, search, and shell tools are
            // now registered here too. Whether a persona can actually reach
            // any of them is still decided below by `filter_persona_tool_names`
            // against `patterns` (the persona's `[tools].allow` globs), so
            // registering them here does not by itself grant access.
            let config_dir = project_path.join(".trusty-agents").join("agents");
            // Security fix (delegate_to_agent injection-to-RCE path): this
            // in-process persona-chat dispatch is the SECOND place (besides
            // the `--direct`/`--agent` subprocess path handled in
            // `runtime::subagent_mode`/`runtime::tool_registry`) that builds
            // a `DelegateToAgentTool` for an assistant-tier persona holding
            // live external-content tools (Gmail/Drive/web) plus
            // `delegate_to_agent`. Tag the runner with THIS persona's own
            // `[tools].allow` posture (`patterns`, already resolved above)
            // whenever the persona's role is the assistant tier
            // (`runtime::tool_registry::ASSISTANT_TIER_ROLE` — literal here
            // because that constant is private to `runtime`, mirroring the
            // existing `cfg.agent.role == "assistant"` comparison in
            // `repl::agent_commands`) so whatever this persona spawns is
            // narrowed to its own posture regardless of the spawned agent's
            // declared role. Non-assistant-tier callers of this same chat
            // path (if any) are unaffected — `None` is a byte-identical no-op.
            // Security fix (multi-hop taint composition, item 1, #4126): this
            // in-process persona-chat dispatch is always a fresh top-level
            // turn today (the ONLY way an already-tainted delegation reaches
            // this process is via a subprocess spawn, and every subprocess
            // spawn -- `--agent <name>` -- runs through
            // `runtime::subagent_mode::run_subagent` instead, never this
            // function). Reading `TAGENT_DELEGATION_TAINT_ALLOW` here anyway
            // and composing it via `compose_delegation_taint` (rather than
            // forwarding `patterns` verbatim) means this call site can never
            // silently widen past an inbound taint if that assumption ever
            // stops holding, at zero behavioral cost today: no env var is set
            // on this process, so `inbound_taint` is `None` and the result is
            // byte-identical to the prior `Some(patterns.clone())`/`None`
            // split.
            let inbound_taint = crate::runtime::tool_registry::parse_delegation_taint_env(
                std::env::var("TAGENT_DELEGATION_TAINT_ALLOW").ok(),
            );
            let delegation_taint = if persona_cfg.agent.role == "assistant" {
                crate::runtime::tool_registry::compose_delegation_taint(
                    Some(&patterns),
                    inbound_taint.as_deref(),
                )
            } else {
                None
            };
            // #4169 (epic #4167): the one-directional L0/L1 delegation gate.
            // This persona-chat dispatch path (backing the REPL `/agent`
            // command) is the SECOND site — besides `runtime::subagent_mode`
            // — that builds a `DelegateToAgentTool` for a persona that may
            // hold live external-content tools. `role == "assistant"` is the
            // SAME population `delegation_taint` above already gates; a
            // non-assistant-tier role here is treated the same way the
            // taint above treats it — outside the persona tier model
            // entirely (mirrors `ctrl_delegate_posture`'s `role !=
            // "assistant"` branch in `history.rs`) — so it is never gated by
            // the L1->L0 boundary, exactly as it is never tainted.
            let delegator_tier = if persona_cfg.agent.role == "assistant" {
                persona_cfg.agent.tier()
            } else {
                crate::agents::AgentTier::L0Orchestration
            };
            let runner: Arc<dyn AgentRunner> = Arc::new(
                SubprocessAgentRunner::new()
                    .with_config_dir(Some(config_dir.clone()))
                    .with_delegation_taint(delegation_taint),
            );
            registry.register(Arc::new(
                // #3737: thread the session id so a persona that delegates
                // onward attributes the answer to the specialist that produced
                // it (chat-bubble responder attribution).
                DelegateToAgentTool::new(runner)
                    .with_config_dir(config_dir.clone())
                    .with_session_id(sid.clone())
                    .with_delegator_tier(delegator_tier),
            ));
            registry.register(Arc::new(AddProjectTool));
            registry.register(Arc::new(ListProjectsTool));
            registry.register(Arc::new(RemoveProjectTool));
            registry.register(Arc::new(StopTaskTool {
                snapshot: Vec::new(),
                pending_stop: Arc::new(Mutex::new(None)),
            }));
            registry.register(Arc::new(SetActiveProjectTool {
                active_project: Arc::new(Mutex::new(None)),
            }));
            registry.register(Arc::new(MoveFileTool));
            registry.register(Arc::new(CreateDirTool));
            {
                let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
                let search_tool = crate::tools::native_search::SearchCodeTool::new_auto(&cwd).await;
                registry.register(Arc::new(search_tool));
            }
            {
                let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
                registry.register(Arc::new(crate::tools::run_bash::RunBashTool::new(cwd)));
            }
            {
                let state_dir = project_path.join(".trusty-agents").join("state");
                crate::tools::tm_tools::register_tm_tools_for_state_dir(&mut registry, &state_dir);
            }

            registry.register(Arc::new(
                crate::tools::web_search::BraveSearchTool::from_env(),
            ));

            // #3864 / #3816: `vector_search`, bound to THIS persona's OKG
            // store index. Personas (cto-assistant, izzie) grant
            // `vector_search` in `[tools].allow` but it was never registered
            // on this path — the persona-documented `vector_search(index_id=…)`
            // call shape resolved to nothing, which is the bug #3864 filed.
            // Registering it here with the persona's bound store means an
            // unqualified search hits the agent's OWN knowledge; an explicit
            // `index_id` still overrides. Personas with no `[[stores]]`
            // binding get `None` and the tool's pre-existing local-index
            // behaviour. As with every tool above, registration alone grants
            // nothing — `filter_persona_tool_names` against `patterns` still
            // decides reachability.
            //
            // #3232/#4009: the SAME tool also carries this persona's tier-2
            // attached indexes (`[tools].search_indexes`, already
            // extends-unioned by the time `persona_cfg` is loaded) and its
            // enforcement posture. Attached ids are enumerated in the tool
            // schema so the model can see them; they only GATE anything when
            // the persona opts in with `enforce_search_indexes = true`. A
            // persona declaring neither gets `(vec![], false)` — identical to
            // the pre-#3232 registration above.
            registry.register(Arc::new(
                crate::tools::memory::VectorSearchTool::new()
                    .with_default_index(
                        persona_cfg
                            .stores
                            .default_search_index()
                            .map(str::to_string),
                    )
                    .with_attached_indexes(persona_cfg.tools.resolved_search_indexes())
                    .with_index_enforcement(persona_cfg.tools.search_indexes_enforced()),
            ));

            // system_status epic (#3052): lets a persona report on-demand
            // trusty-* subsystem health (daemons, MCP servers, credential
            // tiers, registry counts) without it being injected into every
            // turn. Registering it here only makes it reachable — whether
            // this persona can actually call it is still gated below by
            // `filter_persona_tool_names` against its `[tools].allow` list.
            //
            // `with_resolved_endpoint` (not `new`) is required here: by this
            // point `persona_cfg.agent.model`/`.runner` already carry any
            // `/model` override applied above (line ~131) — passing the
            // bare `persona_name` string would make the tool re-derive the
            // STALE on-disk model, silently ignoring the override the user
            // just set. See `tools::system_status` module docs.
            registry.register(Arc::new(
                crate::tools::system_status::SystemStatusTool::with_resolved_endpoint(
                    persona_name,
                    persona_cfg.agent.model.clone(),
                    persona_cfg.agent.runner,
                ),
            ));

            // #3052: izzie weather/Metro-North tools. Registered
            // unconditionally into the platform-hosted superset (like git /
            // ticketing / system_status above); whether a persona can call
            // `get_weather`/`get_train_schedule`/`get_train_alerts` is still
            // decided by `filter_persona_tool_names` against its
            // `[tools].allow` globs — izzie opts in, other personas don't.
            for tool in crate::tools::izzie::izzie_tools() {
                registry.register(tool);
            }

            // OKG builder tools — same posture as the izzie block above: added
            // to the platform-hosted superset here, admitted per-persona by
            // `filter_persona_tool_names` against `[tools].allow`. Keeping both
            // assistant dispatch paths (this one and
            // `runtime::tool_registry::build_assistant_tier_registry`) in sync
            // is what #3745 item C fixed for izzie; do not register in only one.
            for tool in crate::tools::okg::okg_tools() {
                registry.register(tool);
            }

            // #4171 (epic #4167): L0-only read-only session-state tools, kept
            // in sync with `runtime::tool_registry::build_assistant_tier_registry`
            // (the #3745-item-C discipline: a capability model that holds on
            // only one dispatch path is a bug that surfaces as "it works in
            // chat"). CONDITIONAL, unlike every block above:
            // `session_state_tools` returns an empty vector for any tier other
            // than L0, so an L1 persona's registry never contains these
            // executors and the duplicate-name `debug_assert!` in
            // `ToolRegistry::register` can never fire against the
            // trusty-mpm-proxied `session_list`/`session_status` registered
            // higher up (different names by design — see
            // `tools::session_state`).
            for tool in crate::tools::session_state::session_state_tools(
                project_path,
                persona_cfg.agent.tier(),
            ) {
                registry.register(tool);
            }

            for plugin in crate::tools::agent_plugin::plugins_for_persona(persona_name) {
                for tool in &plugin.tools {
                    registry.register(std::sync::Arc::clone(tool));
                }
            }

            // #3238: live-discover MCP servers (`[[mcp.services]] discover =
            // true` and `.mcp.json`) and register whatever tools they
            // actually advertise, on top of everything registered above.
            // `existing_names` seeds the dedup set so a live-discovered tool
            // never shadows a tool already registered from another source.
            {
                let existing_names: std::collections::HashSet<String> = registry
                    .schemas()
                    .iter()
                    .filter_map(|s| {
                        s.get("function")
                            .and_then(|f| f.get("name"))
                            .and_then(|n| n.as_str())
                            .map(String::from)
                    })
                    .collect();
                for tool in
                    crate::tools::mcp_live::live_mcp_tool_executors(project_path, &existing_names)
                        .await
                {
                    registry.register(tool);
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
            // #3208: agent-declared scope enforcement. `tool_scopes` maps the
            // (few) OpenRPC-registry-discovered tools to their scope string;
            // `agent_scope_patterns` is this persona's own declared patterns.
            // Native/in-process tools carry no scope and are unaffected — see
            // `filter_persona_tool_names`.
            //
            // #3936: resolved via `agents::permissions::effective_scopes`
            // rather than reading `persona_cfg.tools.scopes` directly, so
            // `[permissions].scopes` (DOC-57 §7.2) is honoured with the SAME
            // CC-9 precedence (`[permissions].scopes` wins over legacy
            // `[tools].scopes` when a file declares both) the
            // `/permissions` route reports — the ONE place this precedence
            // is decided, so the route's `enforced: true` claim for a
            // `[permissions].scopes`-only agent is never aspirational. Byte-
            // identical to the pre-#3936 read when `[permissions]` is absent.
            let tool_scopes = registry.tool_scopes();
            let agent_scope_patterns: Vec<ScopePattern> =
                crate::agents::permissions::effective_scopes(
                    &persona_cfg.tools,
                    &persona_cfg.permissions,
                )
                .unwrap_or_default()
                .into_iter()
                .map(ScopePattern::new)
                .collect();
            // #3987 (option C): this is the ONLY place that holds both halves
            // of the intersection at once — the agent's resolved patterns
            // (post-`extends` union) AND the scope vocabulary the registry
            // actually built — so it is where a pattern that can match
            // nothing becomes detectable. Emitted here, immediately before
            // the gate that would otherwise drop the surface in silence.
            // Still a WARN and not a hard failure. #3987's option B has
            // removed the dead `google.read` from the shipped base, so the
            // sequencing constraint that forced a warning is discharged and
            // escalation is unblocked — but promoting this to a fatal would
            // stop USER-authored agents from loading, which is its own
            // decision. See `tools::registry::dead_scope`'s module doc.
            dead_scope::warn_dead_scope_patterns(
                persona_name,
                &dead_scope::dead_scope_patterns(
                    &agent_scope_patterns,
                    // Both halves: what the registry actually kept AND what
                    // the endpoints advertised before operator filtering, so
                    // a narrowed `[[endpoints]].scopes` policy can never be
                    // mistaken for a dead grant.
                    &dead_scope::reachable_scopes(
                        tool_scopes.values().chain(advertised_scopes.iter()),
                    ),
                ),
            );
            // #4171 (epic #4167): `_for_tier` is `filter_persona_tool_names`
            // plus the L0-only read-only session-state strip — a FOURTH,
            // independent, DENY-ONLY gate keyed on the persona's TIER rather
            // than on its declared allow/scope posture. It is applied to
            // `kept` because that is the single value reaching BOTH the
            // advertised schema list and `ToolRegistry::dispatch_gated` (via
            // `persona_allowed_tools`). Deliberately NOT given the
            // `role != "assistant"` escape the delegation gate has above:
            // every persona reaching this path must declare `tier = "l0"` to
            // keep a session-state name, and no shipped persona declares one
            // today (pinned by
            // `assistant_tier_grants_delegation_and_blackboxes_internal_tools`),
            // so this strips nothing that currently works. See
            // `persona_gate::filter_persona_tool_names_for_tier`.
            let kept = filter_persona_tool_names_for_tier(
                all_names,
                &patterns,
                &allowed_by_tier,
                &tool_scopes,
                &agent_scope_patterns,
                persona_cfg.agent.tier(),
            );
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
            provider: creds.label(),
        };
        let base = build_user_context_prefix(&persona_cfg.system_prompt.content, &identity);
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
        match persona_memory::render_memory_block(&persona_memory) {
            Some(block) => format!("{base}\n\n{block}"),
            None => base,
        }
    };

    // Chat-streaming demo: for a tools-off persona turn on an OpenRouter-routed
    // model, stream the reply token-by-token onto the event bus (relayed to the
    // GUI via `/api/events`) and return the assembled full text — byte-identical
    // to the blocking path from the caller's perspective, so history /
    // `PmResponse` / attribution downstream are unaffected. A tool-armed persona
    // (`!persona_tool_names.is_empty()`) keeps the multi-turn tool loop below,
    // and any streaming failure falls through to that same blocking path.
    if persona_tool_names.is_empty()
        && llm::stream::streaming_supported(
            &persona_cfg.agent.model,
            persona_cfg.llm.use_anthropic_direct,
        )
    {
        let messages = llm::stream::build_messages(&system_prompt, history, user_input);
        match llm::stream::stream_reply(
            &persona_cfg.agent.model,
            messages,
            &sid,
            &persona_cfg.agent.name,
            llm::stream::DEFAULT_STREAM_CADENCE,
            // #3758: the SAME sampling knobs the blocking tool-loop call below
            // sends, so a streamed reply's style/verbosity/stopping matches.
            trusty_common::chat::SamplingParams {
                temperature: Some(persona_cfg.llm.temperature),
                max_tokens: Some(persona_cfg.llm.max_tokens),
                stop: persona_cfg.llm.stop_sequences.clone(),
            },
        )
        .await
        {
            Ok(assembly) => {
                tracing::info!(
                    persona = %persona_name,
                    llm_ms = persona_llm_t0.elapsed().as_millis() as u64,
                    response_chars = assembly.content.len(),
                    "run_pm_task_with_persona: streamed LLM call complete"
                );
                let display = classification::finish_turn(
                    project_path,
                    persona_name,
                    &client,
                    &persona_cfg,
                    user_input,
                    assembly.content,
                    &turn_ctx,
                    &workstreams_base_url,
                )
                .await?;
                // #3928: persist to the agent's OWN palace chat session (the
                // `ws:`-tagged drawer `finish_turn` writes goes to the
                // project-slug palace instead). Detached — the answer is
                // already on its way.
                persona_memory::spawn_persist_turn(
                    &persona_cfg.stores,
                    Some(&workstreams_base_url),
                    persona_name,
                    user_input,
                    &display,
                );
                return Ok(display);
            }
            Err(e) => {
                tracing::warn!(
                    persona = %persona_name,
                    error = %e,
                    "persona streaming failed; falling back to blocking chat"
                );
            }
        }
    }

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
    // #3208: NEVER collapse an empty tool list to `None` here — see
    // `persona_allowed_tools` for why that would silently reopen the full
    // registered tool surface instead of denying dispatch.
    let allowed_tools = persona_allowed_tools(persona_tool_names.clone());
    let max_turns = persona_max_turns(!persona_tool_names.is_empty(), &persona_cfg.llm);
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
        persona_cfg.llm.strict_tool_discipline(),
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

    let display = classification::finish_turn(
        project_path,
        persona_name,
        &client,
        &persona_cfg,
        user_input,
        content,
        &turn_ctx,
        &workstreams_base_url,
    )
    .await?;
    persona_memory::spawn_persist_turn(
        &persona_cfg.stores,
        Some(&workstreams_base_url),
        persona_name,
        user_input,
        &display,
    );
    Ok(display)
}

#[cfg(test)]
#[path = "persona_tests.rs"]
mod tests;
