//! The two configuration tiers a `tcode` run reads MCP servers from (#5428).
//!
//! Why: ADR-0060 decision 2 puts ONE list of configured MCP servers at
//! `~/.trusty-tools/mcp/servers.toml`, owned by `trusty-mcp` and read by both
//! harnesses. `trusty-agents` already reads it (`crate::mcp::shared` there);
//! this is `trusty-code`'s half. The second tier is the project, because a
//! repo is where "this codebase needs that connector" is actually known — but
//! a repo is also UNTRUSTED content, so the project tier passes through
//! [`super::trust`] before it reaches the resolver.
//!
//! What: [`resolve_at`] loads the global file, reads
//! `<project>/.trusty-code/mcp.toml` through [`crate::paths::resolve_project_entry`]
//! (which brings the `.claude/` compatibility lookup with it), runs the trust
//! gate, and hands both to `trusty_mcp::config::resolve` — the ONE resolver
//! (ADR-0060 decision 5). Nothing here does precedence arithmetic of its own.
//!
//! Failure is fail-open-and-visible, never a silent empty catalog: a file that
//! exists and does not parse yields zero servers from that tier PLUS an
//! [`McpIssue`] and a `tracing::warn!` naming the path. See
//! `super::tests::config_tests::a_malformed_global_file_yields_zero_servers_and_an_issue`
//! — a plain code span, not a link: the test modules are `#[cfg(test)]` and do
//! not exist in rustdoc's public pass.
//!
//! Test: `super::tests::config_tests` — the whole module.

use std::path::{Path, PathBuf};

use serde::Deserialize;
use trusty_mcp::config::{McpConfigFile, McpServerConfig, McpTransport};

use crate::paths::{EntryKind, resolve_project_entry};

/// The project-tier file name, resolved beneath the winning config root.
///
/// Why: named once so `.trusty-code/mcp.toml` and its `.claude/mcp.toml`
/// compatibility twin cannot drift apart — [`resolve_project_entry`] walks
/// `crate::paths::SEARCH_ROOTS` for both.
/// Test: `super::tests::config_tests::a_project_disable_hides_a_global_server`.
pub const PROJECT_MCP_FILE: &str = "mcp.toml";

/// Which configuration tier a server or a finding came from.
///
/// Test: `super::tests::config_tests::a_project_set_with_a_new_command_is_refused`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpTier {
    /// `~/.trusty-tools/mcp/servers.toml`, the file shared with `trusty-agents`.
    Global,
    /// `<project>/.trusty-code/mcp.toml` (or its `.claude/` compatibility twin).
    Project,
}

impl McpTier {
    /// A stable lowercase token for logs and diagnostics.
    ///
    /// Test: `super::tests::config_tests::tier_tokens_are_stable`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Project => "project",
        }
    }
}

/// One thing wrong with an MCP configuration file, or one entry refused.
///
/// Why: the Fail-Open Check this slice is gated on — a broken file must never
/// be indistinguishable from "no servers configured". A log line alone is not
/// enough, because the caller has to be able to RENDER the difference; so the
/// finding is data, with the remedy attached.
/// What: which tier, which file, what is wrong, and what would fix it — the
/// same reason-plus-remedy shape `trusty_agents::mcp::shared::McpIssue` uses.
/// Test: `super::tests::config_tests::a_malformed_global_file_yields_zero_servers_and_an_issue`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpIssue {
    /// Which configuration tier the finding belongs to.
    pub tier: McpTier,
    /// The file the finding is about.
    pub path: PathBuf,
    /// What is wrong, phrased for a person.
    pub detail: String,
    /// What would fix it.
    pub remedy: String,
}

/// Whether one resolved server can actually be connected to.
///
/// Why: "configured" and "usable" are different facts. A server whose command
/// does not exist, or whose transport this slice cannot speak, must render as
/// present-and-broken with the reason — never as absent, and never as live.
/// What: `enabled` is the config flag; `usable` is the runtime answer, which
/// [`super::spawn`] downgrades after a failed handshake. `detail` is `None`
/// only when `usable`.
/// Test: `super::tests::config_tests::a_disabled_server_is_reported_not_dropped`,
/// `mcp_loader_e2e::spawn_failure_isolates_the_healthy_server`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerStatus {
    /// The server's name.
    pub name: String,
    /// Which tier supplied the entry that won.
    pub tier: McpTier,
    /// The resolved `enabled` flag.
    pub enabled: bool,
    /// Whether the runtime should connect to it.
    pub usable: bool,
    /// Why not, when it is not usable.
    pub detail: Option<String>,
}

/// The project tier's override table, as written in `mcp.toml`.
///
/// Why: two lists rather than one tagged enum list, for the reason
/// `trusty_agents::assistants::mcp::McpOverrides` gives — `disabled = ["x"]`
/// is one line a person will actually hand-write, and the tagged
/// array-of-tables spelling is four.
/// What: `servers` are candidate `Set` overrides (each subject to the trust
/// gate in [`super::trust`]); `disabled` names global servers this project
/// does not connect to. The file's ROOT is this table — the file is already
/// named `mcp.toml`, so a wrapping `[mcp]` table would be redundant.
/// `deny_unknown_fields` is deliberate: a mistyped key that silently dropped a
/// `disabled` entry would be exactly the invisible failure this module exists
/// to prevent, so it surfaces as an [`McpIssue`] instead.
/// Test: `super::tests::config_tests::a_project_disable_hides_a_global_server`,
/// `super::tests::config_tests::a_misspelled_key_is_refused_with_an_issue`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectOverrides {
    /// Servers this project asks for, by name, subject to the trust gate.
    #[serde(default)]
    pub servers: Vec<McpServerConfig>,
    /// Global servers this project does not connect to.
    // `disable` is accepted as an alias so the singular spelling a person
    // reaches for first is not silently swallowed by `deny_unknown_fields`.
    #[serde(default, alias = "disable")]
    pub disabled: Vec<String>,
}

impl ProjectOverrides {
    /// Whether this project overrides nothing.
    pub fn is_empty(&self) -> bool {
        self.servers.is_empty() && self.disabled.is_empty()
    }
}

/// Both tiers, layered, with everything that went wrong on the way.
///
/// Why: the caller renders the difference between what is configured and what
/// is live, not just the answer — so all three views travel together.
/// What: `global` is the shared file before overrides, `overrides` the
/// project's table as written, `servers` the effective list in
/// `trusty_mcp::config::resolve`'s order, and `statuses` one entry per
/// effective server in the same order.
/// Test: `super::tests::config_tests` — the whole module.
#[derive(Debug, Clone, Default)]
pub struct ResolvedMcp {
    /// The global tier, before overrides.
    pub global: Vec<McpServerConfig>,
    /// The project's own table, as parsed.
    pub overrides: ProjectOverrides,
    /// The effective set, in resolver order.
    pub servers: Vec<McpServerConfig>,
    /// One entry per effective server, in the same order.
    pub statuses: Vec<ServerStatus>,
    /// Everything wrong with either tier, or refused by the trust gate.
    pub issues: Vec<McpIssue>,
}

impl ResolvedMcp {
    /// The servers the runtime should try to connect to.
    ///
    /// Test: `super::tests::config_tests::a_disabled_server_is_reported_not_dropped`.
    pub fn usable(&self) -> Vec<&McpServerConfig> {
        self.servers
            .iter()
            .zip(&self.statuses)
            .filter(|(_, status)| status.usable)
            .map(|(server, _)| server)
            .collect()
    }
}

/// Load the global tier, fail-open with a visible finding.
///
/// Why: ABSENCE is not a fault — a fresh install has no connectors. A file
/// that EXISTS and does not parse is, and collapsing the two would start a run
/// with its connectors quietly gone. `load_or_default` draws exactly that line.
/// What: returns the servers plus any issue. Never `Err`: no configuration
/// problem is fatal to a task run.
/// Test: `super::tests::config_tests::a_malformed_global_file_yields_zero_servers_and_an_issue`,
/// `super::tests::config_tests::an_absent_global_file_is_empty_without_an_issue`.
pub fn load_global(path: &Path) -> (Vec<McpServerConfig>, Vec<McpIssue>) {
    match McpConfigFile::load_or_default(path) {
        Ok(file) => (file.servers, Vec::new()),
        Err(e) => {
            // Deliberately loud: a run about to start with zero connectors
            // because this file is broken must say so, not merely look empty.
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "mcp: the shared MCP server file could not be read; starting with ZERO MCP servers",
            );
            (
                Vec::new(),
                vec![McpIssue {
                    tier: McpTier::Global,
                    path: path.to_path_buf(),
                    detail: format!("the shared MCP server file could not be read: {e}"),
                    remedy: "Fix the TOML in that file, or delete it to start from no configured \
                             MCP servers. Until then this run connects to none."
                        .to_string(),
                }],
            )
        }
    }
}

/// Read `<project>/.trusty-code/mcp.toml`, fail-open with a visible finding.
///
/// Why: the project file is optional and user-editable; a typo in it must not
/// end a task run, but must not vanish either.
/// What: resolves the path through [`resolve_project_entry`], so a project
/// that still carries `.claude/mcp.toml` is read without a second lookup here.
/// An absent file is no overrides and no issue; an unreadable or unparseable
/// one is no overrides WITH an issue.
/// Test: `super::tests::config_tests::an_absent_project_file_is_no_overrides`,
/// `super::tests::config_tests::a_misspelled_key_is_refused_with_an_issue`.
pub fn read_project_overrides(project_root: &Path) -> (ProjectOverrides, Vec<McpIssue>) {
    let resolved = resolve_project_entry(project_root, PROJECT_MCP_FILE, EntryKind::File);
    let path = resolved.path;
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return (ProjectOverrides::default(), Vec::new());
    };
    match toml::from_str::<ProjectOverrides>(&raw) {
        Ok(overrides) => (overrides, Vec::new()),
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "mcp: the project MCP override file could not be read; using the global set alone",
            );
            (
                ProjectOverrides::default(),
                vec![McpIssue {
                    tier: McpTier::Project,
                    path,
                    detail: format!("the project MCP override file could not be read: {e}"),
                    remedy: "Fix the TOML in that file. Until then this project's MCP overrides \
                             are ignored and the global servers are used unchanged."
                        .to_string(),
                }],
            )
        }
    }
}

/// Layer the global tier with the project tier's TRUSTED overrides.
///
/// Why: the one entry point [`super::McpToolSet`] calls, and the one place the
/// two tiers meet. `global_path` is a parameter so tests point at a tempdir
/// and never touch the real tree.
/// What: loads both tiers, runs [`super::trust::gate`] over the project's
/// `Set` entries, calls `trusty_mcp::config::resolve`, and grades each
/// survivor. Grading here (rather than at the consumer) is what keeps the
/// statuses a caller renders and the servers it connects to from disagreeing.
/// Test: `super::tests::config_tests` — the whole module.
pub fn resolve_at(global_path: &Path, project_root: &Path) -> ResolvedMcp {
    let (global, mut issues) = load_global(global_path);
    let (overrides, project_issues) = read_project_overrides(project_root);
    issues.extend(project_issues);

    let project_path = resolve_project_entry(project_root, PROJECT_MCP_FILE, EntryKind::File).path;
    let (accepted, refused) = super::trust::gate(&global, &overrides, &project_path);
    issues.extend(refused);

    let overridden: Vec<&str> = accepted.iter().map(|o| o.name()).collect();
    let servers = trusty_mcp::config::resolve(&global, &accepted);
    let statuses = servers
        .iter()
        .map(|server| grade(server, &overridden))
        .collect();

    ResolvedMcp {
        global,
        overrides,
        servers,
        statuses,
        issues,
    }
}

/// One server's config-time [`ServerStatus`].
///
/// What: a disabled server is REPORTED, never dropped — a caller must be able
/// to show what it is choosing not to connect to. A remote transport is
/// reported too, as not-usable: this slice speaks stdio only, and silently
/// omitting an `http` entry would read as a missing config rather than an
/// unimplemented one.
/// Test: `super::tests::config_tests::a_disabled_server_is_reported_not_dropped`,
/// `super::tests::config_tests::a_remote_transport_is_reported_not_usable`.
fn grade(server: &McpServerConfig, overridden: &[&str]) -> ServerStatus {
    let tier = if overridden.contains(&server.name.as_str()) {
        McpTier::Project
    } else {
        McpTier::Global
    };
    let status = |usable: bool, detail: Option<&str>| ServerStatus {
        name: server.name.clone(),
        tier,
        enabled: server.enabled,
        usable,
        detail: detail.map(str::to_string),
    };
    if !server.enabled {
        return status(false, Some("disabled in configuration"));
    }
    match server.transport {
        McpTransport::Stdio { .. } => status(true, None),
        // #5428: recognized, not an error — slice 1 spawns local subprocesses
        // only. Slice 2 is where a remote client would land.
        _ => status(false, Some(super::REMOTE_UNSUPPORTED)),
    }
}
