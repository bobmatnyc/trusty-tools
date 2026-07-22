//! System status aggregation — the `system_status` tool + `tagent system
//! status` CLI's shared, testable core (epic #3052).
//!
//! Why: the owner asked that the assistant "have (or know where to find)
//! system details, including a subsystems status check." This module is the
//! one place that answers that: it gathers trusty-agents' own identity, the
//! four trusty-* daemons' liveness, configured MCP server reachability,
//! credential configuration (names/tiers only — never values), and agent/
//! skill registry counts, then hands a single [`SystemStatusReport`] to
//! either the LLM-facing tool (`crate::tools::system_status`) or the CLI
//! (`crate::runtime::system_cmd`). Kept out of `tools/` because it has no
//! LLM-facing concerns of its own (schema, `ToolResult`) — it is pure data
//! assembly, independently testable and independently useful from the CLI.
//! What: [`gather`] runs every subsystem probe (concurrently where the probes
//! are independent I/O — the four daemon health checks) and never fails: a
//! down/unreachable/misconfigured subsystem is folded into the report as a
//! reportable state, not an `Err`. Submodules: [`daemons`] (daemon `/health`
//! probes), [`credentials`] (provider key tiers), [`registry_counts`] (agent/
//! skill discovery counts), [`format`] (human-readable rendering).
//! Test: `tests` below cover report assembly (unknown agent name, JSON
//! shape, a `/model`-override-aware report); per-subsystem behaviour is
//! tested in each submodule.

pub mod credentials;
pub mod daemons;
pub mod format;
pub mod registry_counts;

#[cfg(test)]
mod tests;

use serde::Serialize;

/// trusty-agents' own identity, as resolved for the active agent.
///
/// Why: the owner's ask includes "tagent itself" — version, build, active
/// agent, and the resolved model/provider. `model` intentionally carries the
/// provider prefix (e.g. `anthropic/claude-opus-4-6`, `ollama/qwen3:30b`) per
/// this codebase's existing model-naming convention, so a separate
/// "provider" field would only duplicate it.
/// What: `version` is `crate::build_info::VERSION`; `model`/`runner` are
/// resolved via [`resolve_self`], reusing the SAME `AgentConfig::by_name_async`
/// loader `run_pm_task_with_persona` (`ctrl/pm_task/dispatch/persona.rs`) and
/// the REPL's own `resolve_endpoint_agent` (`ctrl/repl/plain_cli.rs`) already
/// use to produce the "resolved agent endpoint" log line — never a
/// second, independent lookup.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TagentSelfStatus {
    pub version: String,
    pub active_agent: String,
    pub model: String,
    pub runner: String,
}

/// One configured MCP server's reachability.
///
/// Why: the owner's ask includes "MCP servers configured for this session —
/// names + reachable/not". Sourced from the persisted registry
/// (`~/.trusty-agents/config.toml`'s `[[mcp.services]]`) rather than the
/// live per-turn tool-registration path, since that is the one place a
/// server is durably "configured" independent of which agent is currently
/// active.
/// What: `reachable` is `false` outright when `enabled` is `false`
/// (never probed); otherwise a `stdio` service is reachable iff `command`
/// resolves on `PATH`, and an `http` service is reachable iff a bounded GET
/// against `url` completes (any response, including non-2xx, counts — the
/// probe is asking "is something listening", not "is this the right
/// endpoint").
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct McpServerStatus {
    pub name: String,
    pub transport: String,
    pub enabled: bool,
    pub reachable: bool,
}

/// The full subsystem status report.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SystemStatusReport {
    pub tagent: TagentSelfStatus,
    pub daemons: Vec<daemons::DaemonStatus>,
    pub mcp_servers: Vec<McpServerStatus>,
    pub credentials: Vec<credentials::CredentialStatus>,
    pub agent_registry_count: usize,
    pub skills_count: usize,
}

/// Resolve trusty-agents' own identity for `active_agent`.
///
/// Why: reuses `AgentConfig::by_name_async` — the canonical loader
/// `run_pm_task_with_persona` already resolves through — rather than
/// re-deriving model/runner via a parallel lookup. A load failure (unknown
/// agent name, malformed TOML) degrades to `"unknown"` fields rather than
/// failing the whole report.
///
/// Caveat (fixed by [`self_status_from_resolved`]): this TOML-only lookup
/// does NOT see a session-scoped `/model` (or `/provider`) override — those
/// live only on the caller's in-memory session state, never on disk. Callers
/// that dispatch through a session with overrides applied (e.g.
/// `run_pm_task_with_persona`, after `overrides.model` is folded into
/// `persona_cfg.agent.model`) MUST use [`self_status_from_resolved`] /
/// [`gather_with_resolved_endpoint`] instead, passing the already-resolved
/// model/runner that literally drives the dispatch — never re-deriving them
/// here. This function remains for callers with no live session to consult
/// (the CLI's default `--agent` lookup, `ctrl`'s own registry).
/// Test: `tests::gather_never_panics_for_an_unknown_agent_name`.
async fn resolve_self(active_agent: &str) -> TagentSelfStatus {
    let (model, runner) = match crate::agents::AgentConfig::by_name_async(active_agent).await {
        Ok(cfg) => (cfg.agent.model, runner_label(cfg.agent.runner)),
        Err(_) => ("unknown".to_string(), "unknown".to_string()),
    };
    TagentSelfStatus {
        version: crate::build_info::VERSION.to_string(),
        active_agent: active_agent.to_string(),
        model,
        runner,
    }
}

/// Build `TagentSelfStatus` directly from an already-resolved model/runner —
/// no disk re-lookup, no risk of missing a session-scoped `/model` override.
///
/// Why: this is the ONLY correct source for a tool constructed inside a live
/// dispatch that has already applied `/model`/`/provider` overrides (see
/// [`resolve_self`]'s doc comment for the caveat this fixes).
/// What: wraps the caller-supplied `model`/`runner` verbatim.
/// Test: `tests::gather_with_resolved_endpoint_reflects_override_not_toml`.
fn self_status_from_resolved(
    active_agent: &str,
    model: String,
    runner: crate::agents::RunnerKind,
) -> TagentSelfStatus {
    TagentSelfStatus {
        version: crate::build_info::VERSION.to_string(),
        active_agent: active_agent.to_string(),
        model,
        runner: runner_label(runner),
    }
}

/// Render a [`crate::agents::RunnerKind`] the same way its TOML `serde`
/// representation would (`#[serde(rename_all = "kebab-case")]`) — kept local
/// since `RunnerKind` has no `Serialize`/`Display` impl of its own.
fn runner_label(runner: crate::agents::RunnerKind) -> String {
    use crate::agents::RunnerKind;
    match runner {
        RunnerKind::Subprocess => "subprocess",
        RunnerKind::Inline => "inline",
        RunnerKind::ClaudeCode => "claude-code",
        RunnerKind::InProcess => "in-process",
    }
    .to_string()
}

/// List configured MCP servers from the persisted registry and probe each
/// enabled one for reachability.
///
/// Why: a disabled service is never probed (matches the registry's own
/// "disabled means dormant" semantics); an enabled `stdio` service only
/// needs its command on `PATH` (no process is spawned); an enabled `http`
/// service gets one bounded GET.
async fn mcp_server_status() -> Vec<McpServerStatus> {
    let cfg = crate::mcp::config::GlobalConfig::load().await;
    let mut out = Vec::with_capacity(cfg.mcp.services.len());
    for svc in &cfg.mcp.services {
        let reachable = if !svc.enabled {
            false
        } else {
            match svc.transport.as_str() {
                "stdio" => !svc.command.is_empty() && command_on_path(&svc.command),
                "http" => match &svc.url {
                    Some(url) => http_reachable(url).await,
                    None => false,
                },
                _ => false,
            }
        };
        out.push(McpServerStatus {
            name: svc.name.clone(),
            transport: svc.transport.clone(),
            enabled: svc.enabled,
            reachable,
        });
    }
    out
}

fn command_on_path(cmd: &str) -> bool {
    std::process::Command::new("which")
        .arg(cmd)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

async fn http_reachable(url: &str) -> bool {
    let Ok(client) = reqwest::Client::builder()
        .timeout(daemons::PROBE_TIMEOUT)
        .build()
    else {
        return false;
    };
    tokio::time::timeout(daemons::PROBE_TIMEOUT, client.get(url).send())
        .await
        .map(|r| r.is_ok())
        .unwrap_or(false)
}

/// Assemble the full subsystem status report for `active_agent`.
///
/// Why: the single entry point for callers with NO live session to consult
/// (the CLI's default `--agent` lookup, `ctrl`'s own registry) — resolves
/// `active_agent`'s identity via a fresh TOML lookup (see [`resolve_self`]'s
/// caveat). Callers dispatching inside a session that may carry a `/model`/
/// `/provider` override MUST use [`gather_with_resolved_endpoint`] instead.
/// What: resolves trusty-agents' own identity, then delegates the rest of
/// assembly to [`gather_inner`].
/// Test: `tests::gather_never_panics_for_an_unknown_agent_name`.
pub async fn gather(active_agent: &str) -> SystemStatusReport {
    gather_inner(resolve_self(active_agent).await).await
}

/// Assemble the full subsystem status report using an ALREADY-RESOLVED
/// model/runner — the live-session-aware counterpart to [`gather`].
///
/// Why: this is the function `system_status`-tool call sites inside a live
/// dispatch (e.g. `run_pm_task_with_persona`, after `overrides.model` is
/// folded into `persona_cfg.agent.model`) must call, so a `/model` or
/// `/provider` override is reflected instead of silently re-derived from
/// the on-disk TOML. See [`resolve_self`]'s doc comment for the full
/// rationale.
/// What: builds `tagent` identity via [`self_status_from_resolved`] (no
/// disk lookup), then delegates the rest of assembly to [`gather_inner`].
/// Test: `tests::gather_with_resolved_endpoint_reflects_override_not_toml`.
pub async fn gather_with_resolved_endpoint(
    active_agent: &str,
    model: String,
    runner: crate::agents::RunnerKind,
) -> SystemStatusReport {
    gather_inner(self_status_from_resolved(active_agent, model, runner)).await
}

/// Shared assembly core for [`gather`] and [`gather_with_resolved_endpoint`]
/// — everything except resolving the `tagent` self-identity, which the two
/// public entry points source differently (see their doc comments).
///
/// Why: every probe degrades gracefully (see module docs), so this function
/// itself never fails.
/// What: probes the four daemons CONCURRENTLY via `tokio::join!` (independent
/// I/O, each individually bounded by [`daemons::PROBE_TIMEOUT`]), lists MCP
/// server reachability, classifies credential tiers (names/tiers only), and
/// counts the agent/skill registries.
/// Test: `tests::gather_never_panics_for_an_unknown_agent_name`.
async fn gather_inner(tagent: TagentSelfStatus) -> SystemStatusReport {
    let (search, memory, analyze, mpm) = tokio::join!(
        daemons::probe_search(),
        daemons::probe_memory(),
        daemons::probe_analyze(),
        daemons::probe_mpm(),
    );

    let mcp_servers = mcp_server_status().await;

    let credentials =
        credentials::list_status(trusty_common::inference::credentials::default_store().as_ref());

    let project_root = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let (agent_registry_count, skills_count) = registry_counts::counts(&project_root).await;

    SystemStatusReport {
        tagent,
        daemons: vec![search, memory, analyze, mpm],
        mcp_servers,
        credentials,
        agent_registry_count,
        skills_count,
    }
}
