//! Dispatch + argument-parsing for the MCP management tools (#244, #7454).
//!
//! Why: a single dispatch point keeps the PM/ctrl tool-call glue short — they
//! match the five names and forward here. Since #7454 the file these tools
//! mutate is the SHARED one (`~/.trusty-tools/mcp/servers.toml`,
//! `trusty_mcp::config::McpConfigFile`), not `~/.trusty-agents/config.toml`,
//! so a server the model registers is visible to `trusty-code` too and to
//! every assistant that does not override it.
//!
//! These tools write the GLOBAL tier on purpose. A per-assistant override is a
//! deliberate user setting made through `PUT /api/assistants/:id/mcp` (or by
//! hand in the assistant's `config.toml`); letting an LLM-originated tool call
//! quietly narrow one assistant's connectors would be a change nobody asked
//! for and nobody would see.
//!
//! What: `dispatch_mcp_tool(name, args)` applies the requested mutation and
//! returns a user-readable string; `parse_server_from_args` builds an
//! `McpServerConfig` from the `mcp_add` argument object, dropping the two
//! fields an LLM must never control (see below).
//! Test: `super::dispatch_mcp_*` cases.

use anyhow::{Context, Result};
use serde_json::Value;
use trusty_mcp::config::{McpConfigFile, McpServerConfig, McpTransport};

use crate::mcp::extensions;

/// Dispatch an MCP management tool by name. (#244)
///
/// Why: single dispatch point keeps the PM/ctrl tool-call dispatch glue
/// short — they match the five names and forward to this function.
/// What: loads the shared server file, applies the requested mutation,
/// persists, and returns a string for the tool-result message. Errors (bad
/// args, a broken file, a failed write) become user-readable strings so the
/// LLM can recover in the next turn rather than the turn failing.
/// Test: `dispatch_mcp_*` cases below.
pub async fn dispatch_mcp_tool(name: &str, args: &Value) -> String {
    match name {
        "mcp_list" => match load() {
            Ok((file, _)) => render_list(&file.servers),
            Err(e) => e,
        },
        "mcp_add" => match parse_server_from_args(args) {
            Ok(server) => {
                let added = server.name.clone();
                match mutate(|servers| {
                    servers.retain(|s: &McpServerConfig| s.name != server.name);
                    servers.push(server.clone());
                    true
                }) {
                    Ok(_) => format!("Added MCP server '{added}'."),
                    Err(e) => format!("Failed to add MCP server: {e}"),
                }
            }
            Err(e) => format!("Invalid mcp_add arguments: {e:#}"),
        },
        "mcp_remove" => by_name(args, "mcp_remove", |servers, n| {
            let before = servers.len();
            servers.retain(|s: &McpServerConfig| s.name != n);
            servers.len() < before
        }),
        "mcp_enable" => by_name(args, "mcp_enable", |servers, n| {
            set_enabled(servers, n, true)
        }),
        "mcp_disable" => by_name(args, "mcp_disable", |servers, n| {
            set_enabled(servers, n, false)
        }),
        other => format!("Unknown MCP tool '{other}'"),
    }
}

/// Flip one server's `enabled` flag. `false` when no server has that name.
fn set_enabled(servers: &mut [McpServerConfig], name: &str, enabled: bool) -> bool {
    match servers.iter_mut().find(|s| s.name == name) {
        Some(server) => {
            server.enabled = enabled;
            true
        }
        None => false,
    }
}

/// The three name-keyed tools, which differ only in their mutation.
///
/// Why: `mcp_remove`/`mcp_enable`/`mcp_disable` share one argument shape and
/// one "was there such a server" answer; three copies of that would be three
/// chances for the phrasing to drift.
fn by_name(
    args: &Value,
    tool: &str,
    apply: impl FnOnce(&mut Vec<McpServerConfig>, &str) -> bool,
) -> String {
    let Some(name) = args.get("name").and_then(Value::as_str) else {
        return format!("Invalid {tool} arguments: 'name' is required");
    };
    let verb = match tool {
        "mcp_remove" => "Removed",
        "mcp_enable" => "Enabled",
        _ => "Disabled",
    };
    match mutate(|servers| apply(servers, name)) {
        Ok(true) => format!("{verb} MCP server '{name}'."),
        Ok(false) => format!("No MCP server named '{name}' found."),
        Err(e) => format!("Failed to update MCP server '{name}': {e}"),
    }
}

/// Read the shared file, or the message to hand the model instead.
///
/// Read-only: `mcp_list` alone. Every mutation goes through [`mutate`], which
/// reads inside the lock.
fn load() -> Result<(McpConfigFile, std::path::PathBuf), String> {
    let path = shared_path()?;
    let file = McpConfigFile::load_or_default(&path)
        .map_err(|e| format!("Failed to read the MCP server file: {e}"))?;
    Ok((file, path))
}

/// The shared file's path, or the message to hand the model instead.
fn shared_path() -> Result<std::path::PathBuf, String> {
    trusty_mcp::config::default_path()
        .map_err(|e| format!("Failed to locate the MCP server file: {e}"))
}

/// Read-modify-write the shared file under one held lock, skipping the write
/// when nothing changed.
///
/// Why: `~/.trusty-tools/mcp/servers.toml` is shared across processes — a
/// durable API server, a GUI, `trusty-code`, and any `tagent` invocation can
/// all reach it — so reading it OUTSIDE the write is a lost update, not a torn
/// one. `McpConfigFile::save`'s tmp+rename already makes each individual write
/// atomic; what it cannot do is stop two `mcp_add` calls from both reading the
/// same list and the later save dropping the earlier server (#7454).
/// [`crate::state_writer::atomic_update`] holds the advisory lock across read,
/// decide and publish, which is the only place that window closes. It
/// publishes through the same 0600 tmp+rename `save` uses, so the file mode is
/// unchanged.
/// What: the bytes the lock protects are parsed INSIDE the closure via
/// `McpConfigFile::parse` — no second read of the path. `Ok(false)` when
/// `apply` reports no change (the closure returns `None` and nothing is
/// written); `Ok(true)` after a successful publish; `Err(message)` for a read,
/// parse or write failure, already phrased for the model.
/// Test: `super::tests::concurrent_mcp_add_calls_all_survive`,
/// `super::tests::dispatch_mcp_enable_on_a_missing_server_writes_nothing`.
fn mutate(apply: impl FnOnce(&mut Vec<McpServerConfig>) -> bool) -> Result<bool, String> {
    let path = shared_path()?;
    crate::state_writer::atomic_update(&path, |existing| {
        let mut file = match existing {
            // An absent file is an empty list, matching `load_or_default`: a
            // fresh install has no connectors, which is not a fault.
            None => McpConfigFile::default(),
            Some(bytes) => {
                let text = std::str::from_utf8(bytes)
                    .map_err(|e| anyhow::anyhow!("the MCP server file is not UTF-8: {e}"))?;
                McpConfigFile::parse(text, &path)?
            }
        };
        if !apply(&mut file.servers) {
            return Ok(None);
        }
        Ok(Some(file.render(&path)?.into_bytes()))
    })
    .map_err(|e| format!("Failed to update the MCP server file: {e:#}"))
}

/// Render the configured servers for the `mcp_list` tool result.
///
/// Why: the model summarises available external capability from this string,
/// so a disabled server must appear MARKED rather than be omitted — otherwise
/// the model reports a connector as absent when the user can simply re-enable
/// it.
/// Test: `dispatch_mcp_list_renders_disabled_servers`.
fn render_list(servers: &[McpServerConfig]) -> String {
    if servers.is_empty() {
        return "No MCP servers configured.".to_string();
    }
    let mut out = format!("Configured MCP servers ({}):\n\n", servers.len());
    for server in servers {
        let marker = if server.enabled { "✓" } else { "✗" };
        let suffix = if server.enabled { "" } else { " (disabled)" };
        out.push_str(&format!(
            "{marker} {} [{}] — {}{suffix}\n",
            server.name,
            extensions::transport_label(server),
            extensions::description(server),
        ));
        let tools = extensions::tools(server);
        if !tools.is_empty() {
            let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
            out.push_str(&format!("  Tools: {}\n", names.join(", ")));
        }
        out.push('\n');
    }
    out.trim_end().to_string()
}

/// Parse the `mcp_add` argument object into an [`McpServerConfig`].
///
/// Why: this is the ONE place LLM-originated input becomes a server this
/// harness may spawn, so the two fields an LLM must never control are stripped
/// here rather than trusted anywhere downstream.
/// What: `name`, `description` and `transport` are required; `command`/`args`
/// build a stdio transport and `url` a remote one. `discover` and `env` are
/// dropped with a warning — see the inline notes for the attack each one
/// closes.
/// Test: `dispatch_mcp_add_strips_discover_true`,
/// `dispatch_mcp_add_strips_undeclared_env_field`.
fn parse_server_from_args(args: &Value) -> Result<McpServerConfig> {
    let name = args
        .get("name")
        .and_then(Value::as_str)
        .context("'name' is required")?
        .to_string();
    let description = args
        .get("description")
        .and_then(Value::as_str)
        .context("'description' is required")?
        .to_string();
    let transport_kind = args
        .get("transport")
        .and_then(Value::as_str)
        .context("'transport' is required")?;
    let args_vec = args
        .get("args")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    // SECURITY (#3266): `env` is intentionally NOT read from `args`.
    // `mcp_add` is dispatched from LLM-originated tool calls, and `env` is not
    // declared in this tool's schema (`mcp_tool_definitions` in `schema.rs`),
    // so any `env` object a call includes is undeclared input. Honouring it
    // would let a prompt-injected or hallucinated call inject arbitrary
    // environment variables (credentials, `LD_PRELOAD`-style hijacks, proxy
    // overrides) into a spawned server's process environment. A server that
    // genuinely needs env vars is configured by a human editing the shared
    // file directly.
    if args.get("env").is_some() {
        tracing::warn!(
            "mcp_add: rejected undeclared 'env' field from LLM-originated tool call; server added with no env vars"
        );
    }

    let transport = match transport_kind {
        "stdio" => McpTransport::Stdio {
            command: args
                .get("command")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            args: args_vec,
            env: Default::default(),
        },
        "http" => McpTransport::Http {
            url: args
                .get("url")
                .and_then(Value::as_str)
                .context("'url' is required for transport = 'http'")?
                .to_string(),
            headers: Default::default(),
        },
        other => anyhow::bail!("'transport' must be 'stdio' or 'http' (got '{other}')"),
    };

    let mut server = McpServerConfig::new(name, transport);
    server.enabled = args.get("enabled").and_then(Value::as_bool).unwrap_or(true);
    extensions::set(
        &mut server.extensions,
        extensions::DESCRIPTION,
        &description,
    );

    // SECURITY (#3266 follow-up): `discover` is a declared `mcp_add` schema
    // field, but it is forced to `false` here regardless of what the call
    // requests. `gather_specs` auto-spawns any enabled, stdio,
    // `discover = true` server, so without this strip a prompt-injected call
    // could set `discover: true` with an attacker-chosen `command` and the
    // very next persona turn would spawn that binary. Live discovery stays
    // available, but only via a human editing the shared file.
    if args.get("discover").and_then(Value::as_bool) == Some(true) {
        tracing::warn!(
            "mcp_add: rejected 'discover: true' from LLM-originated tool call; server added with discover=false"
        );
    }

    let declared: Vec<extensions::ToolDescriptor> = args
        .get("tools")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|t| {
                    let n = t.get("name").and_then(Value::as_str)?;
                    let d = t.get("description").and_then(Value::as_str)?;
                    Some(extensions::ToolDescriptor {
                        name: n.to_string(),
                        description: d.to_string(),
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if !declared.is_empty() {
        extensions::set(&mut server.extensions, extensions::TOOLS, &declared);
    }

    Ok(server)
}
