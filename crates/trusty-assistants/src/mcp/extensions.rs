//! Consumer-specific keys inside [`McpServerConfig::extensions`] (#7454).
//!
//! Why: ADR-0060 makes `trusty_mcp::config::McpServerConfig` the ONE
//! MCP-server config shape, deliberately the intersection of what every
//! consumer agreed on plus one escape hatch. The knobs this crate's three
//! retired schemas carried — `McpService`'s `tools`/`discover`, and
//! `EndpointConfig`'s `scopes`/`discovery_ttl_secs`/`eager_discovery`/`auth`/
//! `driver`/transport limits — are trusty-agents' own semantics, so they live
//! in `extensions` rather than as fields `trusty-mcp` would have to model for
//! one consumer. `trusty-mcp` never reads a key out of that map; this module
//! is the only place that does, so an untyped `serde_json::Value` is decoded
//! in exactly one file instead of at every call site.
//!
//! What: one `pub const` per documented key, the typed shapes behind them, and
//! a reader per key that DEFAULTS rather than fails. A hand-edited
//! `servers.toml` carrying a nonsense `discovery_ttl_secs = "soon"` must still
//! start the server with the documented default — the file is the user's, and
//! one bad optional knob is not a reason to drop a connector.
//!
//! Transport accessors ([`stdio_parts`], [`endpoint_url`]) match with a
//! wildcard arm on purpose: `McpTransport` is `#[non_exhaustive]`, so a
//! variant added upstream (`Sse` arrived on #7452's own branch) must not break
//! this crate's build.
//!
//! Test: `super::tests::extensions_tests` — the whole module.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use trusty_mcp::config::{McpServerConfig, McpTransport};

/// Human-readable blurb. Was `McpService.description` / `EndpointConfig.description`.
pub const DESCRIPTION: &str = "description";
/// Statically declared tool list. Was `McpService.tools`.
pub const TOOLS: &str = "tools";
/// Live-discover the tool surface instead of trusting [`TOOLS`]. Was `McpService.discover`.
pub const DISCOVER: &str = "discover";
/// Operator-trusted scope patterns. Was `EndpointConfig.scopes`.
pub const SCOPES: &str = "scopes";
/// Discovery cache lifetime. Was `EndpointConfig.discovery_ttl_secs`.
pub const DISCOVERY_TTL_SECS: &str = "discovery_ttl_secs";
/// Run `rpc.discover` at registry-build time. Was `EndpointConfig.eager_discovery`.
pub const EAGER_DISCOVERY: &str = "eager_discovery";
/// Credential reference for a registry endpoint. Was `EndpointConfig.auth`.
pub const AUTH: &str = "auth";
/// Which registry driver talks to this server. Was `EndpointConfig.driver`.
pub const DRIVER: &str = "driver";
/// Timeout/retry/concurrency caps. Was `EndpointConfig.transport`.
pub const TRANSPORT_LIMITS: &str = "transport_limits";
/// Where the entry came from — set by the global loader, never by a user.
pub const SOURCE: &str = "source";

/// [`DISCOVERY_TTL_SECS`]'s documented default, matching the retired
/// `EndpointConfig::default_discovery_ttl_secs`.
pub const DEFAULT_DISCOVERY_TTL_SECS: u64 = 300;

/// [`SOURCE`] value for an entry read from a project's `.mcp.json`.
pub const SOURCE_MCP_JSON: &str = "mcp-json";

/// One statically declared tool, as `McpService.tools` carried it.
///
/// Why: the static path builds one executor per declared tool and has no
/// input schema to go with it, so a name plus a blurb is the whole contract.
/// Test: `super::tests::extensions_tests::tools_round_trip`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolDescriptor {
    /// The tool name the LLM calls.
    pub name: String,
    /// What it does, fed to the model verbatim.
    #[serde(default)]
    pub description: String,
}

/// How a registry endpoint authenticates. Was `registry::config::AuthKind`.
///
/// Test: `super::tests::extensions_tests::auth_resolves_from_the_environment`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AuthKind {
    /// No credential is needed.
    None,
    /// `Authorization: Bearer $<env>`.
    BearerEnv,
    /// `<header>: $<env>`.
    HeaderEnv,
}

/// The `auth` extension. Was `registry::config::AuthConfig`.
///
/// Test: `super::tests::extensions_tests::auth_resolves_from_the_environment`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthSpec {
    /// Which scheme applies.
    pub kind: AuthKind,
    /// The environment variable holding the secret.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<String>,
    /// The header name, for [`AuthKind::HeaderEnv`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header: Option<String>,
}

/// Which registry driver reaches this server. Was `registry::config::DriverKind`.
///
/// Test: `super::tests::extensions_tests::driver_defaults_to_none`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DriverKind {
    /// JSON-RPC 2.0 / OpenRPC over the subprocess's stdio.
    Direct,
    /// MCP `2024-11-05` over the subprocess's stdio.
    StdioMcp,
}

/// Per-endpoint transport caps. Was `registry::config::TransportConfig`.
///
/// Test: `super::tests::extensions_tests::transport_limits_default_to_unset`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransportLimits {
    /// Per-request timeout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    /// Retry budget.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_retries: Option<u32>,
    /// In-flight request cap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_concurrency: Option<usize>,
}

/// Decode one key, or the type's default when it is absent or unreadable.
///
/// Why: see the module doc — one malformed optional knob must not drop a
/// server. The failure is logged at debug rather than swallowed silently, so
/// a user who typed the value wrong can still find out why it did nothing.
fn read<T: for<'de> Deserialize<'de> + Default>(server: &McpServerConfig, key: &str) -> T {
    let Some(raw) = server.extensions.get(key) else {
        return T::default();
    };
    serde_json::from_value(raw.clone()).unwrap_or_else(|e| {
        tracing::debug!(server = %server.name, key, error = %e, "mcp extensions: unreadable key, using the default");
        T::default()
    })
}

/// Decode one optional key. `None` when absent OR unreadable.
fn read_opt<T: for<'de> Deserialize<'de>>(server: &McpServerConfig, key: &str) -> Option<T> {
    let raw = server.extensions.get(key)?;
    serde_json::from_value(raw.clone())
        .map_err(|e| {
            tracing::debug!(server = %server.name, key, error = %e, "mcp extensions: unreadable key, treating as absent");
        })
        .ok()
}

/// The server's human-readable blurb, or an empty string.
/// Test: `super::tests::extensions_tests::description_defaults_to_empty`.
pub fn description(server: &McpServerConfig) -> String {
    read(server, DESCRIPTION)
}

/// The statically declared tool list, empty when the server live-discovers.
/// Test: `super::tests::extensions_tests::tools_round_trip`.
pub fn tools(server: &McpServerConfig) -> Vec<ToolDescriptor> {
    read(server, TOOLS)
}

/// Whether the tool surface is live-discovered. Defaults to `false`.
/// Test: `super::tests::extensions_tests::discover_defaults_to_false`.
pub fn discover(server: &McpServerConfig) -> bool {
    read(server, DISCOVER)
}

/// The operator's trusted scope patterns, empty when none are declared.
/// Test: `super::tests::extensions_tests::scopes_round_trip`.
pub fn scopes(server: &McpServerConfig) -> Vec<String> {
    read(server, SCOPES)
}

/// The discovery cache lifetime, defaulting to [`DEFAULT_DISCOVERY_TTL_SECS`].
/// Test: `super::tests::extensions_tests::discovery_ttl_falls_back_to_the_documented_default`.
pub fn discovery_ttl_secs(server: &McpServerConfig) -> u64 {
    read_opt(server, DISCOVERY_TTL_SECS).unwrap_or(DEFAULT_DISCOVERY_TTL_SECS)
}

/// Whether to run `rpc.discover` at registry-build time. Defaults to `false`.
/// Test: `super::tests::extensions_tests::driver_defaults_to_none`.
pub fn eager_discovery(server: &McpServerConfig) -> bool {
    read(server, EAGER_DISCOVERY)
}

/// The declared credential reference, if any.
/// Test: `super::tests::extensions_tests::auth_resolves_from_the_environment`.
pub fn auth(server: &McpServerConfig) -> Option<AuthSpec> {
    read_opt(server, AUTH)
}

/// The registry driver, if this server is a registry endpoint at all.
/// Test: `super::tests::extensions_tests::driver_defaults_to_none`.
pub fn driver(server: &McpServerConfig) -> Option<DriverKind> {
    read_opt(server, DRIVER)
}

/// The transport caps, all unset by default.
/// Test: `super::tests::extensions_tests::transport_limits_default_to_unset`.
pub fn transport_limits(server: &McpServerConfig) -> TransportLimits {
    read(server, TRANSPORT_LIMITS)
}

/// Where this entry came from, when the loader recorded it.
/// Test: `super::tests::global_tests::mcp_json_entries_are_marked_and_ranked_last`.
pub fn source(server: &McpServerConfig) -> Option<String> {
    read_opt(server, SOURCE)
}

/// Write one extension key, dropping it when the value fails to serialise.
///
/// Why: the migration and the `.mcp.json` import both build `extensions`
/// from typed values, and a `serde_json` failure on a plain struct is a
/// programmer error, not a user-facing condition — recording nothing is
/// strictly better than refusing to migrate the entry.
/// Test: `super::tests::migrate_tests::converts_a_legacy_stdio_service`.
pub fn set<T: Serialize>(extensions: &mut BTreeMap<String, Value>, key: &str, value: &T) {
    match serde_json::to_value(value) {
        Ok(json) => {
            extensions.insert(key.to_string(), json);
        }
        Err(e) => tracing::warn!(key, error = %e, "mcp extensions: value could not be recorded"),
    }
}

/// The command line and environment of a stdio server.
///
/// Why: every spawn path in this crate needs exactly these three, and the
/// wildcard arm keeps a non-stdio (or newly added) transport a graceful
/// `None` rather than a compile error or a panic.
/// What: `Some((command, args, env))` for [`McpTransport::Stdio`], `None` for
/// every other transport.
/// Test: `super::tests::extensions_tests::stdio_parts_only_answer_for_stdio`.
pub fn stdio_parts(
    server: &McpServerConfig,
) -> Option<(&str, &[String], &BTreeMap<String, String>)> {
    match &server.transport {
        McpTransport::Stdio { command, args, env } => Some((command.as_str(), args, env)),
        _ => None,
    }
}

/// The endpoint URL of a remote server, whatever remote transport it uses.
///
/// Why: the UI and the knowledge pane report "where does this connect to"
/// without caring whether the wire is streamable HTTP or SSE.
/// Test: `super::tests::extensions_tests::stdio_parts_only_answer_for_stdio`.
pub fn endpoint_url(server: &McpServerConfig) -> Option<&str> {
    match &server.transport {
        McpTransport::Http { url, .. } => Some(url.as_str()),
        _ => None,
    }
}

/// A short, stable label for the transport, for logs and the UI.
///
/// Test: `super::tests::extensions_tests::stdio_parts_only_answer_for_stdio`.
pub fn transport_label(server: &McpServerConfig) -> &'static str {
    match &server.transport {
        McpTransport::Stdio { .. } => "stdio",
        McpTransport::Http { .. } => "http",
        // `McpTransport` is `#[non_exhaustive]`; an upstream variant this
        // crate has not been taught about is reported honestly rather than
        // mislabelled as one of the two above.
        _ => "remote",
    }
}

/// Resolve this server's declared credential from the process environment.
///
/// Why: ADR-0060 decision 6 — a server whose credential does not resolve is
/// SKIPPED with a per-server status, never fatal to the registry. Returning
/// the reason (rather than a bare `false`) is what lets the status name the
/// missing variable, which is the only actionable thing to tell a user.
/// What: `Ok(None)` when nothing is declared or the scheme is
/// [`AuthKind::None`]; `Ok(Some((header, value)))` when the variable resolves;
/// `Err(reason)` when a variable is named and is missing or empty. The secret
/// itself never appears in the error.
/// Test: `super::tests::extensions_tests::auth_resolves_from_the_environment`,
/// `super::tests::extensions_tests::a_missing_credential_names_the_variable`.
pub fn resolve_auth(server: &McpServerConfig) -> Result<Option<(String, String)>, String> {
    let Some(spec) = auth(server) else {
        return Ok(None);
    };
    match spec.kind {
        AuthKind::None => Ok(None),
        AuthKind::BearerEnv | AuthKind::HeaderEnv => {
            let Some(var) = spec.env.as_deref().map(str::trim).filter(|v| !v.is_empty()) else {
                return Err("declares an `auth` block with no `env` variable to read".to_string());
            };
            let value = std::env::var(var)
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
                .ok_or_else(|| format!("credential environment variable `{var}` is not set"))?;
            let header = match spec.kind {
                AuthKind::HeaderEnv => spec
                    .header
                    .as_deref()
                    .map(str::trim)
                    .filter(|h| !h.is_empty())
                    .ok_or_else(|| {
                        "declares `kind = \"header-env\"` with no `header` name".to_string()
                    })?
                    .to_string(),
                _ => "Authorization".to_string(),
            };
            let rendered = match spec.kind {
                AuthKind::BearerEnv => format!("Bearer {value}"),
                _ => value,
            };
            Ok(Some((header, rendered)))
        }
    }
}
