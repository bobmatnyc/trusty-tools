//! Layering the global tier with one assistant's overrides (#7454).
//!
//! Why: ADR-0060 decision 5 — `trusty_mcp::config::resolve` is the ONE
//! resolver, so this module does no precedence arithmetic of its own. What it
//! adds is the two things the resolver cannot know: which assistant is asking
//! (so the right `config.toml` is read) and whether each resolved server is
//! actually USABLE right now (decision 6 — a server whose credential does not
//! resolve is skipped with a per-server status, never fatal to the registry).
//!
//! What: [`ResolvedMcp`] carries all three views a caller needs — the global
//! list, this assistant's overrides, and the effective set — because the
//! settings pane renders the difference between them, not just the answer.
//! [`ResolvedMcp::usable`] is what every runtime consumer iterates: enabled,
//! credential-resolving servers only.
//!
//! Test: `super::super::tests::resolve_tests` — the whole module.

use std::path::Path;

use serde::Serialize;
use trusty_mcp::config::McpServerConfig;

use super::{GlobalTier, McpIssue};
use crate::assistants::mcp::{McpOverrides, read_overrides};
use crate::assistants::{AssistantHome, AssistantInstanceId};
use crate::mcp::extensions;

/// Which configuration tier a finding or a server came from.
///
/// Test: `super::super::tests::resolve_tests::an_override_only_affects_its_own_assistant`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum McpTier {
    /// The file shared with `trusty-code`, `~/.trusty-tools/mcp/servers.toml`.
    Global,
    /// One assistant home's `config.toml` `[mcp]` table.
    Assistant,
}

/// Whether one resolved server can actually be connected to.
///
/// Why: "configured" and "usable" are different facts and collapsing them is
/// the fabrication class DOC-57 §4.4 rules out — a connector whose credential
/// is missing must render as present-and-broken with the reason, never as
/// absent and never as connected.
/// What: `enabled` is the config flag; `usable` additionally requires the
/// declared credential to resolve. `reason` is `None` only when `usable`.
/// Test: `super::super::tests::resolve_tests::a_server_with_an_unresolvable_credential_is_skipped_not_fatal`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ServerStatus {
    /// The server's name.
    pub name: String,
    /// Which tier supplied the entry that won.
    pub tier: McpTier,
    /// The resolved `enabled` flag.
    pub enabled: bool,
    /// Whether a runtime consumer should connect to it.
    pub usable: bool,
    /// Why not, when it is not usable.
    pub reason: Option<String>,
}

/// One assistant's MCP configuration, all three tiers of it.
///
/// Why/What/Test: see this module's doc comment.
#[derive(Debug, Clone, Default)]
pub struct ResolvedMcp {
    /// The assistant this was resolved for, `None` for the global-only view.
    pub assistant: Option<String>,
    /// The global tier, before overrides.
    pub global: Vec<McpServerConfig>,
    /// This assistant's own `[mcp]` table.
    pub overrides: McpOverrides,
    /// The effective set, in `trusty_mcp::config::resolve`'s order.
    pub servers: Vec<McpServerConfig>,
    /// One entry per effective server, in the same order.
    pub statuses: Vec<ServerStatus>,
    /// Everything wrong with either tier's file.
    pub issues: Vec<McpIssue>,
}

impl ResolvedMcp {
    /// The servers a runtime consumer should actually connect to.
    ///
    /// Why: every consumer (the static tool path, live discovery, the OpenRPC
    /// registry) wants the same filter, and three copies of it would be three
    /// chances to forget the credential check.
    /// Test: `super::super::tests::resolve_tests::a_server_with_an_unresolvable_credential_is_skipped_not_fatal`.
    pub fn usable(&self) -> Vec<&McpServerConfig> {
        self.servers
            .iter()
            .zip(&self.statuses)
            .filter(|(_, status)| status.usable)
            .map(|(server, _)| server)
            .collect()
    }

    /// Whether a named server survived resolution and is usable.
    ///
    /// Test: `super::super::tests::resolve_tests::an_assistant_level_disable_hides_a_global_server`.
    pub fn has_usable(&self, name: &str) -> bool {
        self.statuses
            .iter()
            .any(|status| status.usable && status.name == name)
    }
}

/// Layer `global` with `overrides`, and grade every survivor.
///
/// Why: the injectable core — tests hand it a tier and a table, production
/// resolves both from disk first. Grading happens HERE rather than at each
/// consumer so the statuses a settings pane renders and the servers the
/// runtime connects to can never disagree.
/// What: calls `trusty_mcp::config::resolve`, then builds one [`ServerStatus`]
/// per result: `usable` unless the entry is disabled or its declared
/// credential does not resolve. An override's parse failure is folded in as an
/// assistant-tier [`McpIssue`] and the global set is used unchanged, which is
/// ADR-0060 decision 6's middle case.
/// Test: `super::super::tests::resolve_tests` — the whole module.
pub fn resolve_in(
    assistant: Option<&str>,
    global: GlobalTier,
    overrides: McpOverrides,
    override_error: Option<(std::path::PathBuf, String)>,
) -> ResolvedMcp {
    let mut issues = global.issues;
    if let Some((path, detail)) = override_error {
        tracing::warn!(
            path = %path.display(),
            detail = %detail,
            "mcp: this assistant's [mcp] overrides could not be read; using the global set",
        );
        issues.push(McpIssue {
            tier: McpTier::Assistant,
            path,
            detail,
            remedy: "Fix the `[mcp]` table in that file. Until then this assistant uses the \
                     global MCP servers unchanged."
                .to_string(),
        });
    }

    let overridden: Vec<String> = overrides
        .servers
        .iter()
        .map(|s| s.name.clone())
        .chain(overrides.disabled.iter().cloned())
        .collect();
    let servers = trusty_mcp::config::resolve(&global.servers, &overrides.as_overrides());
    let statuses = servers
        .iter()
        .map(|server| grade(server, &overridden))
        .collect();

    ResolvedMcp {
        assistant: assistant.map(str::to_string),
        global: global.servers,
        overrides,
        servers,
        statuses,
        issues,
    }
}

/// One server's [`ServerStatus`].
///
/// What: a disabled server is reported, never dropped — the pane must show it
/// as DISABLED with its reason (DOC-57 §4.4 C-03.2). An enabled server whose
/// credential does not resolve is likewise reported, with the missing
/// variable named; the secret itself never reaches this string because
/// [`extensions::resolve_auth`] never puts it there.
fn grade(server: &McpServerConfig, overridden: &[String]) -> ServerStatus {
    let tier = if overridden.iter().any(|name| name == &server.name) {
        McpTier::Assistant
    } else {
        McpTier::Global
    };
    if !server.enabled {
        return ServerStatus {
            name: server.name.clone(),
            tier,
            enabled: false,
            usable: false,
            reason: Some("disabled in configuration".to_string()),
        };
    }
    match extensions::resolve_auth(server) {
        Ok(_) => ServerStatus {
            name: server.name.clone(),
            tier,
            enabled: true,
            usable: true,
            reason: None,
        },
        Err(reason) => {
            tracing::warn!(
                server = %server.name,
                reason = %reason,
                "mcp: skipping this server for this assistant; the rest of the registry is unaffected",
            );
            ServerStatus {
                name: server.name.clone(),
                tier,
                enabled: true,
                usable: false,
                reason: Some(reason),
            }
        }
    }
}

/// Resolve one assistant's effective MCP servers against the live tree.
///
/// Why: the production wrapper every runtime consumer calls. `assistant` is
/// `None` for a context with no assistant identity (a sub-agent run, a CLI
/// listing with no `--assistant`), which resolves to the global tier alone
/// rather than to nothing.
/// What: loads the global tier (seeding it once from the retired tables),
/// folds in a trusted project `.mcp.json`, reads the assistant's `[mcp]`
/// table, and layers them. Every failure along the way becomes an issue on
/// the result; none of them is an `Err`, because ADR-0060 decision 6 makes
/// none of them fatal to assistant startup.
/// Test: covered through [`resolve_in`]; the wrapper adds only the disk reads.
pub async fn resolve_for_assistant(assistant: Option<&str>, project_dir: &Path) -> ResolvedMcp {
    let mut global = match super::load_global() {
        Ok(global) => global,
        Err(e) => {
            tracing::warn!(error = %e, "mcp: no shared MCP server file path resolves; no servers configured");
            GlobalTier::default()
        }
    };
    let trusted = crate::mcp::config::GlobalConfig::load()
        .await
        .mcp
        .trust_project_mcp_json;
    super::merge_mcp_json(
        &mut global,
        &super::discover_mcp_json_paths(project_dir),
        trusted,
    );

    let Some(home) = assistant
        .and_then(|name| AssistantInstanceId::new(name).ok())
        .and_then(|id| {
            crate::assistants::assistants_root()
                .ok()
                .map(|root| AssistantHome::under(root, id))
        })
    else {
        return resolve_in(assistant, global, McpOverrides::default(), None);
    };

    let read = read_overrides(&home);
    let error = read.error.map(|detail| (read.path, detail));
    resolve_in(assistant, global, read.overrides, error)
}

/// [`resolve_for_assistant`] against the process's working directory.
///
/// Why: several callers (the concierge turn, the CLI listing) have no project
/// path of their own, and the only thing the path decides is which
/// `.mcp.json` is considered — which is inert anyway unless the operator
/// trusted it.
/// Test: covered through [`resolve_for_assistant`].
pub async fn resolve_here(assistant: Option<&str>) -> ResolvedMcp {
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    resolve_for_assistant(assistant, &cwd).await
}
