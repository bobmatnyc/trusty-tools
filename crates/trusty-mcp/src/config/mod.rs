//! The one MCP-server configuration authority for the trusty-* ecosystem (#7452).
//!
//! Why: four crates had each grown their own "describe an MCP server" shape —
//! `trusty_mpm::core::mcp_config`'s Claude-Code JSON entries,
//! `trusty_common::claude_config::mcp_server_entry`'s minimal `{command,args}`
//! value, and two different TOML structs inside `trusty-agents`
//! (`mcp::config::McpService` and `tools::registry::config::EndpointConfig`).
//! Four shapes, zero sharing, and two independent parsers of the identical
//! `.mcp.json` on-disk format. The 2026-09-11 gap analysis
//! (`docs/research/trusty-agents-mcp-connectors-gap-analysis-2026-09-11.md` §5)
//! names `trusty-mcp` as the crate those should collapse onto, because it owns
//! none of this today and is already the crate every MCP participant links.
//!
//! What: [`McpServerConfig`] and [`McpTransport`] (the shape), [`file`] (one
//! shared TOML file plus its canonical path), [`resolve`](mod@resolve) (a global tier
//! layered with per-consumer overrides), and [`claude_code`] (pure conversion
//! to and from the `mcpServers` map Claude Code reads). This module holds no
//! transport code: spawning a stdio server and speaking JSON-RPC to it stays
//! `trusty_common::stdio_mcp_client::StdioMcpClient`, which keeps `trusty-mcp`
//! the lean rlib ADR-0040 left it and avoids a dependency edge in the wrong
//! direction.
//!
//! The whole module is behind the default-off `config` feature, for the same
//! reason `daemon_bridge_json_rpc` is: a consumer that links this crate only
//! for the wire types must pay for nothing it does not use.
//!
//! Test: `crates/trusty-mcp/tests/mcp_config.rs` — `toml_round_trip_preserves_extensions`,
//! `load_rejects_duplicate_names`, `resolve_set_replaces_wholesale`,
//! `claude_code_stdio_entry_matches_trusty_mpm_golden`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub mod claude_code;
pub mod file;
pub mod resolve;

pub use claude_code::{read_mcp_servers, write_mcp_servers};
pub use file::{McpConfigFile, default_path, default_path_at};
pub use resolve::{McpServerOverride, resolve};

/// Everything that can go wrong reading, writing, or converting MCP config.
///
/// Why: consumers must distinguish a missing file (expected on a fresh
/// install) from a file that exists and is wrong. The second case must fail
/// loudly: silently treating unparseable TOML as "no servers configured"
/// would start an assistant with its connectors quietly absent, which reads
/// as a product bug rather than a config error. Every variant that can name a
/// path carries one, so the operator is told which file to fix.
/// What: a `thiserror` enum, per the library-crate error convention. I/O and
/// parse failures carry the offending path; the Claude-Code variants carry the
/// entry name instead, because those functions are pure and never see a file.
/// Test: `load_missing_file_is_io_error`, `load_rejects_malformed_toml`,
/// `load_rejects_duplicate_names`, `read_rejects_unknown_transport`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum McpConfigError {
    /// Reading or writing the config file failed.
    #[error("MCP config I/O error at {path}: {source}")]
    Io {
        /// The path the failed operation targeted.
        path: PathBuf,
        /// The underlying I/O error.
        source: std::io::Error,
    },

    /// The file exists but is not valid TOML, or does not match the schema.
    #[error("MCP config parse error at {path}: {message}")]
    Parse {
        /// The file being parsed when the error occurred.
        path: PathBuf,
        /// The underlying `toml` message.
        message: String,
    },

    /// The in-memory config could not be rendered as TOML.
    #[error("MCP config serialise error for {path}: {message}")]
    Serialize {
        /// The file the render was destined for.
        path: PathBuf,
        /// The underlying `toml` message.
        message: String,
    },

    /// Two servers in one file claim the same name.
    #[error("duplicate MCP server name {name:?} in {path}")]
    DuplicateName {
        /// The file carrying the collision.
        path: PathBuf,
        /// The name that appears more than once.
        name: String,
    },

    /// The home directory could not be determined, so no default path exists.
    #[error("cannot resolve the default MCP config path: no home directory")]
    NoHome,

    /// A Claude-Code `mcpServers` entry could not be read as an [`McpServerConfig`].
    #[error("invalid Claude Code MCP entry {name:?}: {message}")]
    ClaudeCodeEntry {
        /// The map key the bad entry sits under.
        name: String,
        /// What was wrong with it.
        message: String,
    },

    /// The value handed to [`read_mcp_servers`] was not a JSON object.
    #[error("Claude Code mcpServers value is {found}, expected a JSON object")]
    ClaudeCodeShape {
        /// The JSON type actually found.
        found: &'static str,
    },
}

/// How a client reaches one MCP server.
///
/// Why: the three shapes every consumer already models — a local subprocess
/// speaking MCP over its stdio, and the two remote forms. Making the transport
/// an enum rather than a `transport: String` plus six `Option` fields (the
/// shape `trusty_agents::mcp::config::McpService` grew) means an entry cannot
/// be half-stdio and half-remote, and a match over it is total.
/// What: `Stdio` carries the command line and its environment; `Http` and
/// `Sse` each carry a URL and its request headers. The variants match
/// `trusty_mpm::core::mcp_config::McpTransport`'s three one for one, so
/// `tm mcp add` can route through [`write_mcp_servers`] without losing a
/// transport. `Http` and `Sse` stay separate rather than sharing one variant
/// with a flag, because Claude Code's `type` discriminant is what decides
/// which protocol the client speaks. Every collection is a `BTreeMap`, so the
/// serialised form is byte-stable across runs — a config file that round-trips
/// through load/save must not reorder on every write.
/// Test: `toml_round_trip_preserves_extensions`,
/// `claude_code_remote_entry_matches_trusty_mpm_golden`,
/// `claude_code_sse_entry_matches_trusty_mpm_golden`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
#[non_exhaustive]
pub enum McpTransport {
    /// A local subprocess speaking MCP over stdio.
    Stdio {
        /// The executable to run.
        command: String,
        /// Arguments passed to it, in order.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        args: Vec<String>,
        /// Extra environment variables for the child process.
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        env: BTreeMap<String, String>,
    },
    /// A remote streamable-HTTP endpoint.
    Http {
        /// The endpoint URL.
        url: String,
        /// Extra request headers.
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        headers: BTreeMap<String, String>,
    },
    /// A remote server-sent-events endpoint.
    Sse {
        /// The endpoint URL.
        url: String,
        /// Extra request headers.
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        headers: BTreeMap<String, String>,
    },
}

/// One configured MCP server, as every trusty-* consumer should describe it.
///
/// Why: the single shape the four existing ones collapse onto. It is
/// deliberately the intersection plus one escape hatch, not the union: name,
/// transport and an enable flag are what all four agree on, and everything
/// consumer-specific goes in [`extensions`](Self::extensions) rather than
/// growing a field here that only one crate ever sets.
/// What: `extensions` is an uninterpreted `BTreeMap<String, serde_json::Value>`
/// — trusty-agents' `scopes`, `discovery_ttl_secs` and `auth` live there, and
/// this crate never reads a key out of it. That keeps the authority over the
/// shape here while leaving each consumer's own semantics with the consumer.
///
/// There is NO credential-reference field, on purpose: replacing inline
/// `env` secrets with credential references is #4568's work, and adding a
/// field here before that design lands would fix the wrong shape into the
/// one type every consumer is about to depend on. Secrets in `env` today are
/// exactly as exposed as they were in `McpService.env`; #7452 moves the
/// shape, not the secret handling.
///
/// `#[non_exhaustive]` keeps future fields additive, so construct with
/// [`McpServerConfig::new`] and adjust the public fields afterwards.
/// Test: `toml_round_trip_preserves_extensions`, `new_defaults_to_enabled`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct McpServerConfig {
    /// The server's unique key within one config file. Overrides match on it.
    pub name: String,

    // Field order is load-bearing for TOML: `toml` renders a struct in
    // declaration order and a bare value may not follow a table, so the two
    // scalar fields must precede `transport` and `extensions`.
    /// Whether a consumer should connect to this server. Defaults to `true`.
    #[serde(default = "enabled_default")]
    pub enabled: bool,

    /// How to reach it.
    pub transport: McpTransport,

    /// Consumer-specific keys, never interpreted by this crate.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extensions: BTreeMap<String, serde_json::Value>,
}

/// `serde`'s default for [`McpServerConfig::enabled`].
///
/// Why: an entry that omits `enabled` means "use it", so absence must
/// deserialise to `true` rather than `bool::default()`'s `false`.
/// Test: `omitted_enabled_defaults_to_true`.
fn enabled_default() -> bool {
    true
}

impl McpServerConfig {
    /// A new enabled server with no extensions.
    ///
    /// Why: `#[non_exhaustive]` blocks a struct literal from another crate, so
    /// this is how every consumer builds one.
    /// What: `enabled` is `true` and `extensions` empty; set either field
    /// directly afterwards.
    /// Test: `new_defaults_to_enabled`.
    pub fn new(name: impl Into<String>, transport: McpTransport) -> Self {
        Self {
            name: name.into(),
            enabled: true,
            transport,
            extensions: BTreeMap::new(),
        }
    }
}
