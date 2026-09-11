//! Converting between [`McpServerConfig`] and Claude Code's `mcpServers` map (#7452).
//!
//! Why: trusty-mpm writes MCP servers into the user-scope `mcpServers` object
//! of Claude Code's own `.claude.json`, and trusty-agents independently
//! re-implements a reader for the identical shape in `.mcp.json`. Two parsers
//! of one on-disk format is the duplication the gap analysis' §5 flags. These
//! functions are the one encoder and the one decoder, so `tm mcp add` and any
//! `.mcp.json` reader can share them.
//!
//! What: PURE functions over JSON values. They open no file and know no path —
//! trusty-mpm keeps owning `.claude.json`'s read/merge/atomic-write, and
//! nothing here makes trusty-mpm depend on this crate's own
//! [`servers.toml`](super::file). The entry shapes are byte-identical to
//! `trusty_mpm::core::mcp_config`'s `build_stdio_entry` and
//! `build_remote_entry`, which is what the golden tests pin.
//!
//! Both directions are lossy in known ways, because Claude Code's format
//! carries less than [`McpServerConfig`] does:
//!
//! - A disabled server is OMITTED on write. Claude Code has no `enabled` key,
//!   and writing a disabled server would register it anyway.
//! - [`extensions`](McpServerConfig::extensions) is dropped on write and comes
//!   back empty on read. Those keys are consumer-specific and Claude Code
//!   would reject or ignore them.
//!
//! Test: `crates/trusty-mcp/tests/mcp_config.rs` —
//! `claude_code_stdio_entry_matches_trusty_mpm_golden`,
//! `claude_code_remote_entry_matches_trusty_mpm_golden`,
//! `claude_code_write_then_read_round_trips`.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use super::{McpConfigError, McpServerConfig, McpTransport};

/// Render servers as Claude Code's `mcpServers` map.
///
/// Why: one encoder, so the entry shape cannot drift between `tm mcp add` and
/// any other writer.
/// What: returns a JSON object keyed by server name. A stdio server becomes
/// `{"type":"stdio","command":…,"args":[…]}` with `"env"` added only when
/// non-empty; an http server becomes `{"type":"http","url":…}` with
/// `"headers"` added only when non-empty. `args` is always present, empty
/// array included — matching `trusty_mpm::core::mcp_config::build_stdio_entry`
/// exactly, because a re-add must stay idempotent against entries that module
/// already wrote. Disabled servers are omitted (see the module docs). A later
/// server with a name an earlier one used overwrites it, the same way the
/// on-disk map can only hold one entry per key.
/// Test: `claude_code_stdio_entry_matches_trusty_mpm_golden`,
/// `claude_code_remote_entry_matches_trusty_mpm_golden`,
/// `claude_code_write_omits_disabled_servers`.
pub fn write_mcp_servers(servers: &[McpServerConfig]) -> Value {
    let mut map = Map::new();
    for server in servers {
        if !server.enabled {
            continue;
        }
        map.insert(server.name.clone(), write_entry(&server.transport));
    }
    Value::Object(map)
}

/// One server's Claude Code entry value.
///
/// Why: keeps the two entry shapes side by side, where a field-name drift
/// between them is visible.
/// What: see [`write_mcp_servers`].
/// Test: `claude_code_stdio_entry_matches_trusty_mpm_golden`,
/// `claude_code_remote_entry_matches_trusty_mpm_golden`.
fn write_entry(transport: &McpTransport) -> Value {
    let mut obj = Map::new();
    match transport {
        McpTransport::Stdio { command, args, env } => {
            obj.insert("type".into(), Value::String("stdio".into()));
            obj.insert("command".into(), Value::String(command.clone()));
            obj.insert(
                "args".into(),
                Value::Array(args.iter().map(|a| Value::String(a.clone())).collect()),
            );
            if !env.is_empty() {
                obj.insert("env".into(), string_map(env));
            }
        }
        McpTransport::Http { url, headers } => {
            obj.insert("type".into(), Value::String("http".into()));
            obj.insert("url".into(), Value::String(url.clone()));
            if !headers.is_empty() {
                obj.insert("headers".into(), string_map(headers));
            }
        }
    }
    Value::Object(obj)
}

/// A `BTreeMap<String, String>` as a JSON object.
///
/// Why: `env` and `headers` share the shape.
/// Test: `claude_code_stdio_entry_matches_trusty_mpm_golden`.
fn string_map(source: &BTreeMap<String, String>) -> Value {
    Value::Object(
        source
            .iter()
            .map(|(k, v)| (k.clone(), Value::String(v.clone())))
            .collect(),
    )
}

/// Read Claude Code's `mcpServers` map back into server configs.
///
/// Why: one decoder, replacing the second independent `.mcp.json` parser.
/// What: `json` is the `mcpServers` VALUE itself, not the enclosing
/// `.claude.json` / `.mcp.json` document — the caller indexes `["mcpServers"]`
/// first, because only the caller knows which of the two files it read.
/// Output is ordered by name (`serde_json`'s object is a `BTreeMap` here), so
/// the result is deterministic. Every returned server is `enabled`, with empty
/// `extensions`.
///
/// Fails closed on anything it cannot represent, rather than skipping the
/// entry: a value that is not an object, a `"type"` that is neither `stdio`
/// nor `http`, an entry with neither `command` nor `url`, or a non-string in
/// `args` / `env` / `headers`. `"type": "sse"` is rejected by name —
/// [`McpTransport`] has no SSE variant, and folding SSE into `Http` would
/// silently change which protocol a client speaks. An entry that omits
/// `"type"` is read as stdio when it carries `command` and as http when it
/// carries `url`, which is how Claude Code itself treats the older shape.
/// Test: `claude_code_write_then_read_round_trips`, `read_infers_transport_without_type`,
/// `read_rejects_unknown_transport`, `read_rejects_sse_transport`,
/// `read_rejects_non_object_entry`.
pub fn read_mcp_servers(json: &Value) -> Result<Vec<McpServerConfig>, McpConfigError> {
    let map = json
        .as_object()
        .ok_or(McpConfigError::ClaudeCodeShape { found: kind(json) })?;

    let mut out = Vec::with_capacity(map.len());
    for (name, entry) in map {
        out.push(McpServerConfig::new(name.clone(), read_entry(name, entry)?));
    }
    Ok(out)
}

/// Decode one entry value.
///
/// Why: keeps [`read_mcp_servers`] to its map walk and puts every rejection
/// reason in one place.
/// Test: `read_rejects_unknown_transport`, `read_rejects_sse_transport`.
fn read_entry(name: &str, entry: &Value) -> Result<McpTransport, McpConfigError> {
    let obj = entry
        .as_object()
        .ok_or_else(|| bad(name, "not an object"))?;

    let declared = match obj.get("type") {
        None => None,
        Some(Value::String(s)) => Some(s.as_str()),
        Some(other) => {
            return Err(bad(
                name,
                &format!("`type` is {}, expected a string", kind(other)),
            ));
        }
    };

    let is_stdio = match declared {
        Some("stdio") => true,
        Some("http") => false,
        Some("sse") => {
            return Err(bad(
                name,
                "`type` is \"sse\", which this crate does not model (#7452 covers stdio and http)",
            ));
        }
        Some(other) => {
            return Err(bad(name, &format!("unknown transport type {other:?}")));
        }
        None if obj.contains_key("command") => true,
        None if obj.contains_key("url") => false,
        None => {
            return Err(bad(
                name,
                "no `type`, and neither `command` nor `url` to infer one from",
            ));
        }
    };

    if is_stdio {
        let command = string_field(name, obj, "command")?;
        let args = match obj.get("args") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Array(items)) => items
                .iter()
                .map(|item| match item {
                    Value::String(s) => Ok(s.clone()),
                    other => Err(bad(
                        name,
                        &format!("`args` holds {}, expected a string", kind(other)),
                    )),
                })
                .collect::<Result<Vec<_>, _>>()?,
            Some(other) => {
                return Err(bad(
                    name,
                    &format!("`args` is {}, expected an array", kind(other)),
                ));
            }
        };
        Ok(McpTransport::Stdio {
            command,
            args,
            env: read_string_map(name, obj, "env")?,
        })
    } else {
        Ok(McpTransport::Http {
            url: string_field(name, obj, "url")?,
            headers: read_string_map(name, obj, "headers")?,
        })
    }
}

/// A required string field of an entry.
///
/// Test: `read_rejects_non_object_entry`.
fn string_field(
    name: &str,
    obj: &Map<String, Value>,
    field: &str,
) -> Result<String, McpConfigError> {
    match obj.get(field) {
        Some(Value::String(s)) => Ok(s.clone()),
        Some(other) => Err(bad(
            name,
            &format!("`{field}` is {}, expected a string", kind(other)),
        )),
        None => Err(bad(name, &format!("missing `{field}`"))),
    }
}

/// An optional string-to-string object field (`env` / `headers`).
///
/// Why: both are optional, both reject a non-string value rather than
/// stringifying it — an `env` value silently rendered as `"123"` would differ
/// from what the operator wrote.
/// Test: `claude_code_write_then_read_round_trips`, `read_rejects_non_string_env_value`.
fn read_string_map(
    name: &str,
    obj: &Map<String, Value>,
    field: &str,
) -> Result<BTreeMap<String, String>, McpConfigError> {
    match obj.get(field) {
        None | Some(Value::Null) => Ok(BTreeMap::new()),
        Some(Value::Object(entries)) => entries
            .iter()
            .map(|(k, v)| match v {
                Value::String(s) => Ok((k.clone(), s.clone())),
                other => Err(bad(
                    name,
                    &format!("`{field}.{k}` is {}, expected a string", kind(other)),
                )),
            })
            .collect(),
        Some(other) => Err(bad(
            name,
            &format!("`{field}` is {}, expected an object", kind(other)),
        )),
    }
}

/// Build a [`McpConfigError::ClaudeCodeEntry`].
///
/// Test: `read_rejects_unknown_transport`.
fn bad(name: &str, message: &str) -> McpConfigError {
    McpConfigError::ClaudeCodeEntry {
        name: name.to_string(),
        message: message.to_string(),
    }
}

/// A JSON value's type, for an error message.
///
/// Test: `read_rejects_non_object_entry`.
fn kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}
