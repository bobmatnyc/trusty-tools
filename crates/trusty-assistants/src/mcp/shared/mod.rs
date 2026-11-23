//! The global MCP tier: the file this crate shares with `trusty-code` (#7454).
//!
//! Why: ADR-0060 decision 2 puts ONE list of configured MCP servers at
//! `~/.trusty-tools/mcp/servers.toml`, owned by `trusty-mcp` and read by both
//! harnesses. Before #7454 this crate had no such file and three schemas of
//! its own; this module is where the shared file becomes a list of servers
//! this crate can act on, with the two things a file alone cannot carry —
//! what went wrong reading it, and what a project's `.mcp.json` adds.
//!
//! What: [`GlobalTier`] is the loaded list plus its [`McpIssue`]s.
//! [`load_global_at`] reads the shared file, seeding it once from the retired
//! tables ([`migrate`]) when it is absent. [`merge_mcp_json`] folds a
//! project's `.mcp.json` in behind the shared file, through
//! `trusty_mcp::config::read_mcp_servers` — the ONE decoder of that format
//! now, replacing the second parser the gap analysis flagged.
//!
//! Failure is fail-closed per config and never fatal (ADR-0060 decision 6): a
//! shared file that exists and does not parse yields ZERO servers plus an
//! issue, never the stale legacy tables and never a silent empty list. The
//! difference matters — an assistant that starts with no connectors because
//! the file is broken must say so, or it is indistinguishable from one whose
//! connectors were never configured.
//!
//! Test: `super::tests::global_tests` — the whole module.

pub mod migrate;
pub mod resolve;

use std::path::{Path, PathBuf};

use trusty_mcp::config::{McpConfigError, McpConfigFile, McpServerConfig};

use crate::mcp::extensions;

pub use resolve::{
    McpTier, ResolvedMcp, ServerStatus, resolve_for_assistant, resolve_here, resolve_in,
};

/// One thing wrong with an MCP configuration file.
///
/// Why: ADR-0060 requires a VISIBLE status rather than a silent fallback, and
/// "visible" means the surface rendering connectors can name the file and say
/// what to do about it. A bare log line is not that.
/// What: which tier the file belongs to, its path, what is wrong, and what
/// would fix it — the same reason-plus-remedy shape
/// [`crate::assistants::HomeIssue`] uses, so the concierge narrates both the
/// same way.
/// Test: `super::tests::global_tests::a_malformed_shared_file_yields_zero_servers_and_an_issue`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpIssue {
    /// Which configuration tier the file belongs to.
    pub tier: McpTier,
    /// The file the finding is about.
    pub path: PathBuf,
    /// What is wrong, phrased for a person.
    pub detail: String,
    /// What would fix it.
    pub remedy: String,
}

/// The global tier as loaded: the servers plus anything wrong with the file.
///
/// Why/What/Test: see this module's doc comment.
#[derive(Debug, Clone, Default)]
pub struct GlobalTier {
    /// The configured servers, in file order.
    pub servers: Vec<McpServerConfig>,
    /// Everything wrong with the file, empty when it loaded cleanly.
    pub issues: Vec<McpIssue>,
    /// The shared file's path, named whether or not it exists.
    pub path: PathBuf,
}

/// The shared file's path on this machine.
///
/// Why: a one-line wrapper so no call site spells the layout itself — the path
/// belongs to `trusty-mcp`, which `trusty-code` reads too.
/// Test: covered through [`load_global`].
pub fn shared_path() -> Result<PathBuf, McpConfigError> {
    trusty_mcp::config::default_path()
}

/// Read the shared file, seeding it once from the retired tables.
///
/// Why: the `_at` form is this crate's injected-dependency convention — tests
/// pass tempdirs and never touch the real tree.
/// What: runs [`migrate::migrate_if_absent`] first, so an upgrading user's
/// connectors are present on the very first read. Then loads the file:
/// `load_or_default` treats ABSENCE as an empty list (a fresh install has no
/// connectors, which is not a fault) while a file that exists and is broken
/// yields zero servers AND an issue.
/// Test: `super::tests::global_tests::a_malformed_shared_file_yields_zero_servers_and_an_issue`,
/// `super::tests::global_tests::an_absent_shared_file_is_empty_without_an_issue`,
/// `super::tests::migrate_tests::migrates_once_and_never_again`.
pub fn load_global_at(shared: &Path, legacy_config: &Path) -> GlobalTier {
    if let Some(report) = migrate::migrate_if_absent(legacy_config, shared) {
        tracing::info!(moved = %report.summary(), "mcp: seeded the shared server file from the retired tables");
    }
    match McpConfigFile::load_or_default(shared) {
        Ok(file) => GlobalTier {
            servers: file.servers,
            issues: Vec::new(),
            path: shared.to_path_buf(),
        },
        Err(e) => {
            // Deliberately loud: an assistant about to start with no
            // connectors because this file is broken must say so at startup,
            // not only when someone opens the settings pane.
            tracing::warn!(
                path = %shared.display(),
                error = %e,
                "mcp: the shared MCP server file could not be read; starting with ZERO MCP servers",
            );
            GlobalTier {
                servers: Vec::new(),
                issues: vec![McpIssue {
                    tier: McpTier::Global,
                    path: shared.to_path_buf(),
                    detail: format!("the shared MCP server file could not be read: {e}"),
                    remedy: "Fix the TOML in that file, or delete it to start from no configured \
                             MCP servers. Until then this assistant connects to none."
                        .to_string(),
                }],
                path: shared.to_path_buf(),
            }
        }
    }
}

/// [`load_global_at`] against the live paths.
///
/// What: `Err` only when no home directory resolves, which is the one case
/// where there is no shared file to name.
/// Test: covered through [`load_global_at`]; the wrapper adds only the two
/// path lookups.
pub fn load_global() -> Result<GlobalTier, McpConfigError> {
    let shared = shared_path()?;
    let legacy = crate::mcp::config::GlobalConfig::config_path()
        .unwrap_or_else(|_| PathBuf::from("config.toml"));
    Ok(load_global_at(&shared, &legacy))
}

/// The `.mcp.json` files relevant to `project_dir`, project-local first.
///
/// Why: unchanged from the behaviour the retired `mcp::mcp_json` module
/// documented — project-local config takes precedence over the user's global
/// `~/.claude/.mcp.json`, and returning project-first is what lets the merge
/// implement that with first-occurrence-wins.
/// What: `<project_dir>/.mcp.json` then `$HOME/.claude/.mcp.json`, each only
/// when it exists. Neither existing is an empty vec, not an error.
/// Test: `super::tests::global_tests::mcp_json_paths_prefer_the_project`.
pub fn discover_mcp_json_paths(project_dir: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let project = project_dir.join(".mcp.json");
    if project.exists() {
        paths.push(project);
    }
    if let Some(home) = std::env::var_os("HOME") {
        let user = PathBuf::from(home).join(".claude").join(".mcp.json");
        if user.exists() {
            paths.push(user);
        }
    }
    paths
}

/// Fold `.mcp.json` servers into the global list, behind the shared file.
///
/// Why: `.mcp.json` is the Claude-Code-compatible declaration a project may
/// ship, and it is UNTRUSTED, repo-controlled content — auto-spawning whatever
/// it declares is a remote-code-execution vector for anyone who clones a
/// hostile repo (#3266). The trust gate stays exactly where it was; what
/// changes is that one decoder (`trusty_mcp::config::read_mcp_servers`) now
/// reads the format instead of two.
/// What: does nothing unless `trusted`. Otherwise, for each path in order, any
/// entry whose name is not already present is APPENDED — so the shared file
/// always wins a name collision and an earlier `.mcp.json` wins a later one,
/// the precedence the retired `gather_specs` documented. Imported entries are
/// marked with the [`extensions::SOURCE`] and [`extensions::DISCOVER`] keys:
/// a `.mcp.json` entry carries no static tool list, so live discovery is the
/// only way it can contribute anything.
/// Test: `super::tests::global_tests::mcp_json_entries_are_marked_and_ranked_last`,
/// `super::tests::global_tests::untrusted_mcp_json_contributes_nothing`.
pub fn merge_mcp_json(tier: &mut GlobalTier, paths: &[PathBuf], trusted: bool) {
    if !trusted {
        tracing::debug!(
            "mcp: mcp.trust_project_mcp_json is false (default); .mcp.json contributes nothing"
        );
        return;
    }
    for path in paths {
        let raw = match std::fs::read_to_string(path) {
            Ok(raw) => raw,
            Err(e) => {
                tracing::debug!(path = %path.display(), error = %e, "mcp: .mcp.json unreadable, skipping");
                continue;
            }
        };
        let document: serde_json::Value = match serde_json::from_str(&raw) {
            Ok(document) => document,
            Err(e) => {
                tier.issues.push(McpIssue {
                    tier: McpTier::Global,
                    path: path.clone(),
                    detail: format!("`.mcp.json` is not valid JSON: {e}"),
                    remedy: "Fix the JSON, or remove the file. Its servers are unavailable \
                             until then."
                        .to_string(),
                });
                continue;
            }
        };
        let Some(value) = document.get("mcpServers") else {
            continue;
        };
        let imported = match trusty_mcp::config::read_mcp_servers(value) {
            Ok(imported) => imported,
            Err(e) => {
                tier.issues.push(McpIssue {
                    tier: McpTier::Global,
                    path: path.clone(),
                    detail: format!("`.mcp.json` declares a server this harness cannot read: {e}"),
                    remedy: "Correct the entry's `type`, `command` or `url`. The rest of the \
                             file is unavailable until it parses."
                        .to_string(),
                });
                continue;
            }
        };
        for mut server in imported {
            if tier.servers.iter().any(|s| s.name == server.name) {
                tracing::debug!(
                    server = %server.name,
                    path = %path.display(),
                    "mcp: .mcp.json entry shadowed by an entry of the same name, skipping",
                );
                continue;
            }
            extensions::set(
                &mut server.extensions,
                extensions::SOURCE,
                &extensions::SOURCE_MCP_JSON,
            );
            extensions::set(&mut server.extensions, extensions::DISCOVER, &true);
            tier.servers.push(server);
        }
    }
}
