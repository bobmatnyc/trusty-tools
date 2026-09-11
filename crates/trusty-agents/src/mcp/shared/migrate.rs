//! One-time conversion of the retired per-crate MCP tables (#7454).
//!
//! Why: before ADR-0060 this crate declared its MCP servers in TWO tables of
//! `~/.trusty-agents/config.toml` — `[[mcp.services]]` and
//! `[[tool_registry.endpoints]]` — with two different shapes for the same
//! idea. Both are gone from [`crate::mcp::config::GlobalConfig`], so a user
//! upgrading into this release would otherwise find every connector they had
//! configured silently absent. This module reads those tables ONE more time,
//! converts them to `trusty_mcp::McpServerConfig`, and writes them into the
//! shared file `trusty-code` and `trusty-agents` now both read.
//!
//! What: [`migrate_if_absent`] writes the shared file only when it does not
//! exist, so it is safe to call on every load and can never overwrite a file a
//! user (or `tm mcp add`) has since edited. The legacy tables are then left on
//! disk, untouched and unread — this crate does not delete a user's file
//! content to record that it finished with it, and a leftover table costs
//! nothing because nothing parses it any more.
//!
//! The legacy shapes are re-declared here, prefixed `Legacy`, rather than
//! imported: the types they mirror were DELETED, which is the point of #7454,
//! and a reader that exists only to drain an old format belongs with the
//! drain.
//!
//! Test: `super::super::tests::migrate_tests` — the whole module.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;
use trusty_mcp::config::{McpConfigFile, McpServerConfig, McpTransport};

use crate::mcp::extensions;

/// What one [`migrate_if_absent`] call moved.
///
/// Why: the migration is invisible unless it says what it did — a user whose
/// connectors moved files deserves one log line naming them, and a test needs
/// something to assert on other than the file's bytes.
/// Test: `super::super::tests::migrate_tests::migration_reports_what_moved`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MigrationReport {
    /// Server names taken from `[[mcp.services]]`.
    pub services: Vec<String>,
    /// Server names taken from `[[tool_registry.endpoints]]`.
    pub endpoints: Vec<String>,
}

impl MigrationReport {
    /// Whether the legacy file carried nothing to move.
    pub fn is_empty(&self) -> bool {
        self.services.is_empty() && self.endpoints.is_empty()
    }

    /// One line naming every moved server, for the startup log.
    pub fn summary(&self) -> String {
        let mut names: Vec<&str> = Vec::with_capacity(self.services.len() + self.endpoints.len());
        names.extend(self.services.iter().map(String::as_str));
        names.extend(self.endpoints.iter().map(String::as_str));
        names.join(", ")
    }
}

/// The two retired tables, read partially so nothing else in the file matters.
#[derive(Debug, Default, Deserialize)]
struct LegacyFile {
    #[serde(default)]
    mcp: LegacyMcpSection,
    #[serde(default)]
    tool_registry: Option<LegacyRegistrySection>,
}

#[derive(Debug, Default, Deserialize)]
struct LegacyMcpSection {
    #[serde(default)]
    services: Vec<LegacyService>,
}

#[derive(Debug, Default, Deserialize)]
struct LegacyRegistrySection {
    #[serde(default)]
    endpoints: Vec<LegacyEndpoint>,
}

/// The retired `[[mcp.services]]` entry shape.
#[derive(Debug, Deserialize)]
struct LegacyService {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    command: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    transport: String,
    #[serde(default = "enabled_default")]
    enabled: bool,
    #[serde(default)]
    tools: Vec<extensions::ToolDescriptor>,
    #[serde(default)]
    discover: bool,
}

/// The retired `[[tool_registry.endpoints]]` entry shape.
#[derive(Debug, Deserialize)]
struct LegacyEndpoint {
    name: String,
    #[serde(default)]
    driver: Option<extensions::DriverKind>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    args: Option<Vec<String>>,
    #[serde(default = "enabled_default")]
    enabled: bool,
    #[serde(default)]
    scopes: Vec<String>,
    #[serde(default)]
    discovery_ttl_secs: Option<u64>,
    #[serde(default)]
    eager_discovery: bool,
    #[serde(default)]
    auth: Option<extensions::AuthSpec>,
    #[serde(default)]
    transport: Option<extensions::TransportLimits>,
}

fn enabled_default() -> bool {
    true
}

/// Convert both legacy tables from one raw `config.toml` body.
///
/// Why: separated from the file I/O so the conversion itself is testable
/// against a string, and so a caller holding the text already does not read
/// it twice.
/// What: `[[mcp.services]]` entries first (they were the primary registry),
/// then `[[tool_registry.endpoints]]`; an endpoint whose name a service
/// already claimed is DROPPED, because the shared file rejects a duplicate
/// name and the service entry is the one the runtime actually spawned. A body
/// that is not valid TOML converts to nothing rather than failing — this runs
/// on a load path, and a broken legacy file is the old world's problem.
/// Test: `super::super::tests::migrate_tests::converts_a_legacy_stdio_service`,
/// `super::super::tests::migrate_tests::converts_a_legacy_registry_endpoint`,
/// `super::super::tests::migrate_tests::a_service_wins_a_name_collision`.
pub fn convert(raw: &str) -> (Vec<McpServerConfig>, MigrationReport) {
    let parsed: LegacyFile = match toml::from_str(raw) {
        Ok(parsed) => parsed,
        Err(e) => {
            tracing::warn!(error = %e, "mcp migration: legacy config.toml did not parse; nothing moved");
            return (Vec::new(), MigrationReport::default());
        }
    };

    let mut servers: Vec<McpServerConfig> = Vec::new();
    let mut report = MigrationReport::default();

    for service in parsed.mcp.services {
        if servers.iter().any(|s| s.name == service.name) {
            continue;
        }
        report.services.push(service.name.clone());
        servers.push(from_service(service));
    }

    for endpoint in parsed.tool_registry.unwrap_or_default().endpoints {
        if servers.iter().any(|s| s.name == endpoint.name) {
            tracing::debug!(
                endpoint = %endpoint.name,
                "mcp migration: a [[mcp.services]] entry already claims this name; dropping the endpoint"
            );
            continue;
        }
        report.endpoints.push(endpoint.name.clone());
        servers.push(from_endpoint(endpoint));
    }

    (servers, report)
}

/// One `[[mcp.services]]` entry as an [`McpServerConfig`].
///
/// What: `transport = "http"` with a `url` becomes [`McpTransport::Http`];
/// everything else becomes [`McpTransport::Stdio`], which is what the legacy
/// runtime assumed when it saw anything but `"http"`. `description`, `tools`
/// and `discover` move into `extensions` under the keys
/// [`crate::mcp::extensions`] documents.
fn from_service(service: LegacyService) -> McpServerConfig {
    let transport = match (service.transport.as_str(), service.url) {
        ("http", Some(url)) => McpTransport::Http {
            url,
            headers: BTreeMap::new(),
        },
        _ => McpTransport::Stdio {
            command: service.command,
            args: service.args,
            env: service.env,
        },
    };
    let mut out = McpServerConfig::new(service.name, transport);
    out.enabled = service.enabled;
    if !service.description.is_empty() {
        extensions::set(
            &mut out.extensions,
            extensions::DESCRIPTION,
            &service.description,
        );
    }
    if !service.tools.is_empty() {
        extensions::set(&mut out.extensions, extensions::TOOLS, &service.tools);
    }
    if service.discover {
        extensions::set(&mut out.extensions, extensions::DISCOVER, &true);
    }
    out
}

/// One `[[tool_registry.endpoints]]` entry as an [`McpServerConfig`].
///
/// What: a registry endpoint is always a subprocess, so the transport is
/// [`McpTransport::Stdio`] with the endpoint's own `command`/`args` (empty
/// when it declared none — the driver refuses that case with its own message,
/// which is a better error than dropping the entry here). Every registry-only
/// knob moves into `extensions`.
fn from_endpoint(endpoint: LegacyEndpoint) -> McpServerConfig {
    let transport = McpTransport::Stdio {
        command: endpoint.command.unwrap_or_default(),
        args: endpoint.args.unwrap_or_default(),
        env: BTreeMap::new(),
    };
    let mut out = McpServerConfig::new(endpoint.name, transport);
    out.enabled = endpoint.enabled;
    extensions::set(
        &mut out.extensions,
        extensions::DRIVER,
        &endpoint.driver.unwrap_or(extensions::DriverKind::Direct),
    );
    if let Some(description) = endpoint.description.filter(|d| !d.is_empty()) {
        extensions::set(&mut out.extensions, extensions::DESCRIPTION, &description);
    }
    if !endpoint.scopes.is_empty() {
        extensions::set(&mut out.extensions, extensions::SCOPES, &endpoint.scopes);
    }
    if let Some(ttl) = endpoint.discovery_ttl_secs {
        extensions::set(&mut out.extensions, extensions::DISCOVERY_TTL_SECS, &ttl);
    }
    if endpoint.eager_discovery {
        extensions::set(&mut out.extensions, extensions::EAGER_DISCOVERY, &true);
    }
    if let Some(auth) = endpoint.auth {
        extensions::set(&mut out.extensions, extensions::AUTH, &auth);
    }
    if let Some(limits) = endpoint.transport {
        extensions::set(&mut out.extensions, extensions::TRANSPORT_LIMITS, &limits);
    }
    out
}

/// Seed the shared file from the legacy tables, exactly once.
///
/// Why: see the module doc. Guarding on the shared file's ABSENCE rather than
/// on a "migrated" marker means the guard is the same fact the user can see —
/// there is no hidden state that can disagree with the file on disk.
/// What: returns `None` when the shared file already exists, when the legacy
/// file is absent or unreadable, or when it declares no servers. Otherwise
/// writes the shared file and returns the report. A failed write is logged and
/// returns `None`: the connectors are then unavailable for this run, which the
/// caller reports as an issue, but nothing is lost from the legacy file.
/// Test: `super::super::tests::migrate_tests::migrates_once_and_never_again`,
/// `super::super::tests::migrate_tests::an_existing_shared_file_is_left_alone`.
pub fn migrate_if_absent(legacy_config: &Path, shared: &Path) -> Option<MigrationReport> {
    if shared.exists() {
        return None;
    }
    let raw = std::fs::read_to_string(legacy_config).ok()?;
    let (servers, report) = convert(&raw);
    if servers.is_empty() {
        return None;
    }
    if let Err(e) = McpConfigFile::new(servers).save(shared) {
        tracing::warn!(
            path = %shared.display(),
            error = %e,
            "mcp migration: could not write the shared server file; the legacy tables are untouched",
        );
        return None;
    }
    tracing::info!(
        path = %shared.display(),
        moved = %report.summary(),
        "mcp migration: moved the legacy MCP tables into the shared server file (#7454)",
    );
    Some(report)
}
