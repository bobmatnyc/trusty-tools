//! Loading and running the MCP servers a `tcode` task run is configured with
//! (#5428, slice 1).
//!
//! Why: `tcode` could reach exactly one MCP server before this — `search_code`,
//! which hard-codes `trusty-search` (`crate::tools::trusty_search`). Every
//! other connector the operator configures in the file `trusty-agents` already
//! reads (`~/.trusty-tools/mcp/servers.toml`, ADR-0060 decision 2) was
//! invisible here. This module is the loader that closes that gap: shared
//! config in, registered tools out.
//!
//! What: [`McpToolSet::load`](crate::mcp::McpToolSet::load) is the whole
//! lifecycle and runs ONCE per task run, before the agent loop — resolve the
//! two tiers ([`config`](crate::mcp::config)), gate the untrusted project tier
//! ([`trust`](crate::mcp::trust)), spawn each enabled stdio server in isolation
//! ([`spawn`](crate::mcp::spawn)), and wrap each advertised tool as a
//! `ToolExecutor` ([`tool`](crate::mcp::tool)).
//! [`register_configured_tools`](crate::mcp::register_configured_tools) is the
//! cheap per-registry half, and is what the two registry-building sites call.
//! The set is shared across every registry a run builds, so the servers are
//! spawned once and the client handles stay alive exactly as long as a tool
//! that can call them.
//!
//! Every link above is spelled `crate::mcp::…` rather than module-relative:
//! `pub mod mcp;` in `lib.rs` carries its own `///` block, and rustdoc merges
//! that with this `//!` one and resolves the whole thing in the CRATE ROOT's
//! scope, where a bare `config` or `trust` names nothing (#5428).
//!
//! What this slice does NOT do: no catalog over the API or CLI (slice 2), and
//! no remote transports — an `http` or `sse` entry is recognised and reported
//! as not usable, never silently dropped.
//!
//! Test: `tests` — the unit modules, which are `#[cfg(test)]` and so cannot be
//! a link target in rustdoc's public pass; `crates/trusty-code/tests/mcp_loader_e2e.rs`
//! — spawn, register and dispatch against a real stdio MCP server.

pub mod config;
pub mod spawn;
pub mod tool;
pub mod trust;

#[cfg(test)]
mod tests;

use std::path::Path;
use std::sync::Arc;

use crate::tools::registry::ToolRegistry;
use crate::tools::traits::ToolExecutor;

pub use config::{McpIssue, McpTier, ProjectOverrides, ResolvedMcp, ServerStatus};
pub use tool::{McpBridgeTool, composed_name};

/// Why a recognised but unimplemented transport is not connected.
///
/// Why: one string, so the config-time grading and the spawn-time refusal
/// cannot describe the same condition two different ways.
/// Test: `tests::config_tests::a_remote_transport_is_reported_not_usable`.
pub const REMOTE_UNSUPPORTED: &str = "remote transports not supported in this slice";

/// What a run should say about its MCP configuration.
///
/// Why: the Fail-Open Check — a caller must be able to tell "no servers are
/// configured" from "the configuration is broken", and that difference is only
/// legible if the findings travel with the result. `statuses` is the
/// per-server answer; `issues` is everything wrong with a FILE or refused by
/// the trust gate.
/// What: `tools` names every tool actually registered, in registration order.
/// Test: `tests::set_tests::a_malformed_global_file_reaches_the_diagnostics`.
#[derive(Debug, Clone, Default)]
pub struct McpDiagnostics {
    /// One entry per configured server, in resolver order.
    pub statuses: Vec<ServerStatus>,
    /// Everything wrong with either tier's file, or refused by the trust gate.
    pub issues: Vec<McpIssue>,
    /// The composed names of every tool registered.
    pub tools: Vec<String>,
}

impl McpDiagnostics {
    /// How many configured servers this run is actually connected to.
    ///
    /// Test: `tests::set_tests::an_unconfigured_run_registers_nothing_and_reports_nothing`.
    pub fn usable_count(&self) -> usize {
        self.statuses.iter().filter(|s| s.usable).count()
    }

    /// Emit the run's MCP state: `info` for what worked, `warn` for what did not.
    ///
    /// Why: the two registry-building sites should not each decide how to
    /// narrate this, and a broken config must reach stderr whether or not the
    /// caller renders the struct.
    /// Test: `tests::set_tests::a_malformed_global_file_reaches_the_diagnostics`
    /// covers the data; the log calls themselves carry no branching logic.
    pub fn log(&self) {
        tracing::info!(
            servers = self.statuses.len(),
            usable = self.usable_count(),
            tools = self.tools.len(),
            "mcp: configured servers loaded",
        );
        for status in self.statuses.iter().filter(|s| !s.usable) {
            tracing::warn!(
                server = %status.name,
                tier = status.tier.as_str(),
                detail = status.detail.as_deref().unwrap_or(""),
                "mcp: server not usable",
            );
        }
        for issue in &self.issues {
            tracing::warn!(
                tier = issue.tier.as_str(),
                path = %issue.path.display(),
                detail = %issue.detail,
                remedy = %issue.remedy,
                "mcp: configuration problem",
            );
        }
    }
}

/// Every MCP tool a task run can dispatch, plus why any server is missing.
///
/// Why: the lifecycle is per-RUN, not per-registry. A `RegistryFactory` builds
/// a fresh registry for each delegation, and spawning the operator's servers
/// again on each of those would multiply child processes and startup latency
/// for no gain. Loading once and sharing `Arc`s keeps the spawn off every hot
/// path, and — because each tool holds its server's client handle — keeps the
/// children alive for exactly as long as something can call them.
/// What: construct with [`McpToolSet::load`], then hand it to
/// [`register_configured_tools`] at each registry site.
/// Test: `tests::set_tests`, `mcp_loader_e2e::end_to_end_dispatch_echoes_through_the_registry`.
#[derive(Default)]
pub struct McpToolSet {
    tools: Vec<Arc<dyn ToolExecutor>>,
    diagnostics: McpDiagnostics,
}

impl McpToolSet {
    /// Resolve, spawn and discover, against the live global config path.
    ///
    /// Why: the production entry point. A home directory that does not resolve
    /// is a configuration problem, not a fatal one, so it becomes an empty set
    /// rather than an error a task run has to handle.
    /// What: delegates to [`McpToolSet::load_at`] with
    /// `trusty_mcp::config::default_path()`. Under a cargo test harness it
    /// loads NOTHING: this is the one path that reads the OPERATOR's real
    /// `servers.toml`, and honouring it from a test run would spawn their
    /// actual daemons as a side effect of `cargo test` — the production-state
    /// mutation `trusty_common::running_under_test_harness` exists to refuse
    /// (#4255). A test that wants a real server injects its own config through
    /// [`McpToolSet::load_at`].
    /// Test: `tests::set_tests::the_live_path_loads_nothing_under_a_test_harness`;
    /// `mcp_loader_e2e::end_to_end_dispatch_echoes_through_the_registry` covers
    /// the real transport through `load_at`.
    pub async fn load(project_root: &Path) -> Self {
        if trusty_common::running_under_test_harness() {
            tracing::debug!("mcp: test harness detected; not reading the operator's server file");
            return Self::default();
        }
        match trusty_mcp::config::default_path() {
            Ok(path) => Self::load_at(&path, project_root).await,
            Err(e) => {
                tracing::warn!(error = %e, "mcp: no shared MCP server file path resolves; no servers configured");
                Self::default()
            }
        }
    }

    /// [`McpToolSet::load`] with both tier locations injected.
    ///
    /// Why: every test points this at a tempdir, so no test run can read — or
    /// spawn anything named by — the developer's real configuration.
    /// What: resolves both tiers, then attempts each usable server
    /// INDEPENDENTLY. A server that fails to connect downgrades its own status
    /// and contributes no tools; it never affects another server or the run. A
    /// tool whose composed name is already taken within this set, or whose
    /// schema is over the cap, is skipped with an issue rather than silently
    /// shadowing or bloating the surface.
    /// Test: `tests::set_tests::a_failed_server_does_not_stop_the_others`,
    /// `mcp_loader_e2e::spawn_failure_isolates_the_healthy_server`.
    pub async fn load_at(global_path: &Path, project_root: &Path) -> Self {
        Self::load_within(global_path, project_root, spawn::HANDSHAKE_TIMEOUT).await
    }

    /// [`McpToolSet::load_at`] with the per-server startup bound supplied.
    ///
    /// Why: `budget` is per SERVER, and the servers are connected CONCURRENTLY
    /// — sequentially, N wedged connectors would cost N × the bound at task
    /// startup, so the worst case grew with the operator's config. Injecting
    /// the bound is what lets a test prove that in a second or two instead of
    /// waiting out two real timeouts.
    /// What: `futures_util::future::join_all` preserves its input's order, so
    /// the results are applied in CONFIG order and `statuses`/`tools` stay
    /// deterministic regardless of which server answered first.
    /// Test: `mcp_loader_e2e::a_hung_server_does_not_serialise_its_siblings`,
    /// `tests::set_tests::a_failed_server_does_not_stop_the_others`.
    pub async fn load_within(
        global_path: &Path,
        project_root: &Path,
        budget: std::time::Duration,
    ) -> Self {
        let resolved = config::resolve_at(global_path, project_root);
        let mut set = Self {
            tools: Vec::new(),
            diagnostics: McpDiagnostics {
                statuses: resolved.statuses,
                issues: resolved.issues,
                tools: Vec::new(),
            },
        };

        let attempts: Vec<(usize, &trusty_mcp::config::McpServerConfig)> = resolved
            .servers
            .iter()
            .enumerate()
            .filter(|(index, _)| set.diagnostics.statuses[*index].usable)
            .collect();
        let outcomes = futures_util::future::join_all(
            attempts
                .iter()
                .map(|(_, server)| spawn::connect_within(server, budget)),
        )
        .await;

        for ((index, server), outcome) in attempts.into_iter().zip(outcomes) {
            match outcome {
                Ok(connected) => set.absorb(&server.name, connected),
                Err(reason) => {
                    tracing::warn!(
                        server = %server.name,
                        reason = %reason,
                        "mcp: server unavailable; the rest of this run is unaffected",
                    );
                    let status = &mut set.diagnostics.statuses[index];
                    status.usable = false;
                    status.detail = Some(reason);
                }
            }
        }
        set
    }

    /// Turn one connected server's advertised tools into registrable executors.
    ///
    /// What: skips, with an issue, a tool whose composed name this set already
    /// carries and a tool whose schema [`tool::build_schema`] refused.
    /// Test: `mcp_loader_e2e::end_to_end_dispatch_echoes_through_the_registry`.
    fn absorb(&mut self, server: &str, connected: spawn::Connected) {
        for advertised in &connected.tools {
            let name = composed_name(server, &advertised.name);
            if self.diagnostics.tools.contains(&name) {
                self.diagnostics.issues.push(McpIssue {
                    tier: McpTier::Global,
                    path: std::path::PathBuf::from(server),
                    detail: format!("the tool {name:?} is advertised more than once"),
                    remedy: "Only the first was registered. Check the server for a duplicate \
                             tool name."
                        .to_string(),
                });
                continue;
            }
            match McpBridgeTool::new(server, advertised, Arc::clone(&connected.client)) {
                Ok(bridge) => {
                    self.diagnostics.tools.push(name);
                    self.tools.push(Arc::new(bridge));
                }
                Err(reason) => self.diagnostics.issues.push(McpIssue {
                    tier: McpTier::Global,
                    path: std::path::PathBuf::from(server),
                    detail: format!("the tool {name:?} was not registered because {reason}"),
                    remedy: "Ask the server's author to shrink the advertised inputSchema."
                        .to_string(),
                }),
            }
        }
    }

    /// What this run should say about its MCP configuration.
    pub fn diagnostics(&self) -> &McpDiagnostics {
        &self.diagnostics
    }

    /// Whether any tool was loaded at all.
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    /// The composed name of every tool this set carries, for assertions.
    ///
    /// Test: `tests::set_tests::a_failed_server_does_not_stop_the_others`.
    #[cfg(test)]
    pub(crate) fn tool_names(&self) -> Vec<&str> {
        self.tools.iter().map(|t| t.name()).collect()
    }
}

/// Register every loaded MCP tool into `registry`.
///
/// Why: the single call the two registry-building sites make, so neither has
/// to know anything about MCP beyond "hand it the set". Kept separate from
/// [`McpToolSet::load`] because a registry is built per delegation while the
/// servers are spawned per run.
/// What: returns the set's diagnostics plus any collision found HERE — a
/// composed name already registered is skipped rather than overwriting a tool
/// the run depends on. `ToolRegistry::register` would otherwise replace it
/// silently outside a debug build.
/// Test: `mcp_loader_e2e::a_name_already_in_the_registry_is_skipped`,
/// `mcp_loader_e2e::end_to_end_dispatch_echoes_through_the_registry`.
pub fn register_configured_tools(registry: &mut ToolRegistry, set: &McpToolSet) -> McpDiagnostics {
    let mut diagnostics = set.diagnostics.clone();
    for tool in &set.tools {
        let name = tool.name().to_string();
        if registry.contains(&name) {
            tracing::warn!(tool = %name, "mcp: a tool of this name is already registered; skipping");
            diagnostics.tools.retain(|t| t != &name);
            diagnostics.issues.push(McpIssue {
                tier: McpTier::Global,
                path: std::path::PathBuf::from(&name),
                detail: format!("the tool {name:?} collides with one already registered"),
                remedy: "Rename the server in ~/.trusty-tools/mcp/servers.toml so its tools \
                         compose to a free name."
                    .to_string(),
            });
            continue;
        }
        registry.register(Arc::clone(tool));
    }
    diagnostics
}
