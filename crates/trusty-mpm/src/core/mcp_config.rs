//! CRUD over the **user-scope** `mcpServers` map in
//! `<CLAUDE_CONFIG_DIR>/.claude.json`.
//!
//! Why: stock `claude mcp add` writes user-scope MCP servers into the operator's
//! real `~/.claude.json` (or, with `CLAUDE_CONFIG_DIR`, into whatever that env
//! var points at *for that one invocation*). Neither can be aimed at tm's OWN
//! tm-owned `CLAUDE_CONFIG_DIR` from an ordinary shell, so there was no ergonomic
//! way to register an MCP server that every tm-managed session sees. This module
//! backs `tm mcp add|remove|list|get`, editing the top-level `mcpServers` object
//! of the tm config dir's `.claude.json` — the true "available in all your
//! projects" user scope (empirically confirmed: such an entry shows as
//! `Scope: User config` in `claude mcp list`, NOT the `⏸ Pending approval` that
//! project-scope `.mcp.json` servers get).
//!
//! ISOLATION: every write targets ONLY `<config_dir>/.claude.json`. The same file
//! also holds OAuth state, so reads quarantine a malformed file to
//! `.claude.json.corrupt` (mirroring `standalone::trust_seed`) and all writes go
//! through [`trusty_common::claude_config::write_json_atomic`] (temp-file +
//! rename, with a `.bak` of the prior contents) so a crash mid-write can never
//! tear the file. Unrelated keys are always preserved.
//!
//! Target schema (top-level `mcpServers` map):
//! - stdio: `{"type":"stdio","command":"<bin>","args":[...],"env":{...}}`
//! - http/sse: `{"type":"http"|"sse","url":"<url>","headers":{...}}`
//!
//! Test: `tests` module below (add/remove/get/list, idempotency, malformed-JSON
//! quarantine, unrelated-key preservation, stdio vs http schema, and
//! [`managed_mcp_server_names`] union).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::{Map, Value};

/// Basename of the user-scope config file inside a `CLAUDE_CONFIG_DIR`.
const CLAUDE_JSON: &str = ".claude.json";

/// The MCP servers tm always provisions into the managed config dir.
///
/// Why: [`managed_mcp_server_names`] must always keep the three framework
/// servers pre-approved even when the on-disk `mcpServers` map is empty or has
/// been hand-edited; this is the floor of that union.
/// What: the sorted list `["trusty-memory","trusty-review","trusty-search"]`,
/// matching the keys `standalone::global_config::ensure_mcp_config` writes into
/// the managed `.mcp.json`.
/// Test: `managed_mcp_server_names_unions_builtin_with_configured`.
pub const BUILTIN_MANAGED_MCP_SERVERS: &[&str] =
    &["trusty-memory", "trusty-review", "trusty-search"];

/// Transport kind for a user-scope MCP server entry.
///
/// Why: the `add` handler validates transport-specific inputs (`-e` env is
/// stdio-only, `-H` headers are http/sse-only) and picks the right entry shape;
/// a typed enum keeps that dispatch total and self-documenting.
/// What: `Stdio` (a local subprocess) or `Http` / `Sse` (a remote endpoint).
/// Test: exercised via [`build_stdio_entry`] / [`build_remote_entry`] tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpTransport {
    /// Local subprocess speaking MCP over stdio.
    Stdio,
    /// Remote streamable-HTTP endpoint.
    Http,
    /// Remote SSE endpoint.
    Sse,
}

impl McpTransport {
    /// The JSON `"type"` discriminant Claude Code expects for this transport.
    ///
    /// Why: the on-disk entry stores the transport as a `type` string; keeping
    /// the mapping here means the two encodings can never drift.
    /// What: `"stdio" | "http" | "sse"`.
    /// Test: `build_stdio_entry_has_expected_shape`, `build_remote_entry_*`.
    pub fn as_str(self) -> &'static str {
        match self {
            McpTransport::Stdio => "stdio",
            McpTransport::Http => "http",
            McpTransport::Sse => "sse",
        }
    }
}

/// Build a stdio MCP server entry.
///
/// Why: hand-built `json!` literals for the entry shape risk drifting in field
/// names or omitting `args`; a constructor keeps the schema in one place.
/// What: returns `{"type":"stdio","command":<command>,"args":[<args>]}`, adding
/// `"env": {<env>}` only when `env` is non-empty (Claude Code tolerates a missing
/// `env`, and omitting it keeps re-adds idempotent for the common no-env case).
/// `args` is always present (empty array when none).
/// Test: `build_stdio_entry_has_expected_shape`, `build_stdio_entry_with_env`.
pub fn build_stdio_entry(command: &str, args: &[String], env: &Map<String, Value>) -> Value {
    let mut obj = Map::new();
    obj.insert("type".into(), Value::String("stdio".into()));
    obj.insert("command".into(), Value::String(command.to_string()));
    obj.insert(
        "args".into(),
        Value::Array(args.iter().map(|a| Value::String(a.clone())).collect()),
    );
    if !env.is_empty() {
        obj.insert("env".into(), Value::Object(env.clone()));
    }
    Value::Object(obj)
}

/// Build an http/sse MCP server entry.
///
/// Why: mirrors [`build_stdio_entry`] for the remote transports so the two entry
/// shapes are defined side by side.
/// What: returns `{"type":<http|sse>,"url":<url>}`, adding `"headers": {<headers>}`
/// only when `headers` is non-empty.
/// Test: `build_remote_entry_http`, `build_remote_entry_with_headers`.
pub fn build_remote_entry(
    transport: McpTransport,
    url: &str,
    headers: &Map<String, Value>,
) -> Value {
    let mut obj = Map::new();
    obj.insert("type".into(), Value::String(transport.as_str().to_string()));
    obj.insert("url".into(), Value::String(url.to_string()));
    if !headers.is_empty() {
        obj.insert("headers".into(), Value::Object(headers.clone()));
    }
    Value::Object(obj)
}

/// Add (or replace) a user-scope MCP server, idempotently.
///
/// Why: `tm mcp add` must be safe to re-run and must never clobber the OAuth
/// state or other keys sharing `.claude.json`.
/// What: reads `<config_dir>/.claude.json` (quarantining a malformed file, see
/// [`read_config`]), ensures a top-level `mcpServers` object, and sets
/// `mcpServers[name] = entry`. When the key already maps to a value equal to
/// `entry`, nothing is written and `Ok(false)` is returned; otherwise the merged
/// config is written atomically and `Ok(true)` is returned. Creates the config
/// dir and a fresh `{}` when absent.
/// Test: `add_then_get_roundtrips`, `add_is_idempotent`, `add_preserves_other_keys`.
pub fn add_server(config_dir: &Path, name: &str, entry: Value) -> Result<bool> {
    let (path, mut config) = read_config(config_dir)?;
    let servers = mcp_servers_mut(&mut config);
    if servers.get(name) == Some(&entry) {
        return Ok(false);
    }
    servers.insert(name.to_string(), entry);
    write(&path, &config)?;
    Ok(true)
}

/// Remove a user-scope MCP server.
///
/// Why: `tm mcp remove` drops a server the operator no longer wants injected
/// into managed sessions.
/// What: reads the config, removes `mcpServers[name]`, and writes back only when
/// the key existed. Returns `Ok(true)` when a server was removed, `Ok(false)`
/// when `name` was absent (no write).
/// Test: `remove_existing_returns_true`, `remove_absent_returns_false`.
pub fn remove_server(config_dir: &Path, name: &str) -> Result<bool> {
    let (path, mut config) = read_config(config_dir)?;
    let servers = mcp_servers_mut(&mut config);
    if servers.remove(name).is_none() {
        return Ok(false);
    }
    write(&path, &config)?;
    Ok(true)
}

/// Fetch a single user-scope MCP server entry.
///
/// Why: backs `tm mcp get <name>`.
/// What: returns `Some(entry)` when `mcpServers[name]` exists, else `None`.
/// A missing config file yields `None`.
/// Test: `add_then_get_roundtrips`, `get_absent_returns_none`.
pub fn get_server(config_dir: &Path, name: &str) -> Result<Option<Value>> {
    let (_, config) = read_config(config_dir)?;
    Ok(config
        .get("mcpServers")
        .and_then(Value::as_object)
        .and_then(|m| m.get(name))
        .cloned())
}

/// List all user-scope MCP servers.
///
/// Why: backs `tm mcp list`.
/// What: returns the top-level `mcpServers` map (empty when the key or file is
/// absent).
/// Test: `list_returns_all_added`.
pub fn list_servers(config_dir: &Path) -> Result<Map<String, Value>> {
    let (_, config) = read_config(config_dir)?;
    Ok(config
        .get("mcpServers")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default())
}

/// Compute the set of MCP server names tm should pre-approve for a managed
/// session, unioning the built-in framework servers with whatever the operator
/// registered via `tm mcp add`.
///
/// Why: `standalone::trust_seed::preseed_managed_trust` writes each project's
/// `enabledMcpjsonServers` trust list so managed sessions never see the "New MCP
/// servers found" dialog. Deriving that list from the ACTUAL configured servers
/// (rather than a hardcoded three) means a `tm mcp add`-ed server is trusted on
/// the next session start with no extra bookkeeping in the CRUD paths.
/// What: takes an already-parsed `.claude.json` value, collects the top-level
/// `mcpServers` object keys, unions them with [`BUILTIN_MANAGED_MCP_SERVERS`],
/// then sorts + dedups for a deterministic (idempotency-friendly) result.
/// Test: `managed_mcp_server_names_unions_builtin_with_configured`,
/// `managed_mcp_server_names_defaults_to_builtin`.
pub fn managed_mcp_server_names(config: &Value) -> Vec<String> {
    let mut names: Vec<String> = BUILTIN_MANAGED_MCP_SERVERS
        .iter()
        .map(|s| s.to_string())
        .collect();
    if let Some(servers) = config.get("mcpServers").and_then(Value::as_object) {
        names.extend(servers.keys().cloned());
    }
    names.sort();
    names.dedup();
    names
}

// ─── internal helpers ─────────────────────────────────────────────────────

/// Read `<config_dir>/.claude.json` into a JSON object, quarantining corruption.
///
/// Why: the CLI must fail loudly on genuine I/O errors (unlike the best-effort
/// `preseed_managed_trust`, which is non-fatal at launch), yet a *malformed*
/// `.claude.json` must never permanently wedge `tm mcp` — it holds no valid
/// OAuth state when it cannot be parsed, so it is safe to quarantine.
/// What: returns `(path, object)`. A missing file yields an empty `{}`. A file
/// that is valid JSON but not an object, or is malformed, is renamed to
/// `.claude.json.corrupt` and treated as `{}`. A real read error (permissions
/// etc.) propagates as `Err`.
/// Test: `add_quarantines_malformed_json`, `add_then_get_roundtrips` (missing
/// file path).
fn read_config(config_dir: &Path) -> Result<(PathBuf, Value)> {
    let path = config_dir.join(CLAUDE_JSON);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok((path, Value::Object(Map::new())));
        }
        Err(e) => {
            return Err(e).with_context(|| format!("failed to read {}", path.display()));
        }
    };

    match serde_json::from_str::<Value>(&text) {
        Ok(v) if v.is_object() => Ok((path, v)),
        other => {
            // Valid-but-not-object OR malformed → quarantine, proceed fresh.
            let corrupt = path.with_extension("json.corrupt");
            let reason = match &other {
                Ok(_) => "valid JSON but not an object".to_string(),
                Err(err) => format!("not valid JSON ({err})"),
            };
            match std::fs::rename(&path, &corrupt) {
                Ok(()) => tracing::warn!(
                    "{} is {reason}; quarantined to {} — proceeding with fresh config",
                    path.display(),
                    corrupt.display()
                ),
                Err(rename_err) => tracing::warn!(
                    "{} is {reason}; quarantine rename failed ({rename_err}) — \
                     proceeding with fresh config",
                    path.display()
                ),
            }
            Ok((path, Value::Object(Map::new())))
        }
    }
}

/// Borrow (creating if needed) the top-level `mcpServers` object.
///
/// Why: every CRUD op mutates the same nested map; centralising the "ensure it
/// is an object" coercion avoids repeating the guard.
/// What: inserts an empty `mcpServers` object when absent (or replaces a
/// non-object value with one) and returns a mutable reference to it.
/// Test: covered transitively by every `add`/`remove` test.
fn mcp_servers_mut(config: &mut Value) -> &mut Map<String, Value> {
    let root = config
        .as_object_mut()
        .expect("read_config always returns an object");
    let servers = root
        .entry("mcpServers")
        .or_insert_with(|| Value::Object(Map::new()));
    if !servers.is_object() {
        *servers = Value::Object(Map::new());
    }
    servers
        .as_object_mut()
        .expect("mcpServers coerced to object above")
}

/// Atomically persist the merged config to `path`.
///
/// Why: `.claude.json` holds OAuth state; a torn write would brick auth.
/// What: delegates to [`trusty_common::claude_config::write_json_atomic`]
/// (temp-file + rename, backing the prior file up to `.claude.json.bak`).
/// Test: covered transitively by every write-path test.
fn write(path: &Path, config: &Value) -> Result<()> {
    trusty_common::claude_config::write_json_atomic(path, config)
        .map_err(|e| anyhow::anyhow!("failed to write {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn stdio(command: &str, args: &[&str]) -> Value {
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        build_stdio_entry(command, &args, &Map::new())
    }

    #[test]
    fn build_stdio_entry_has_expected_shape() {
        let e = stdio("echo", &["hi"]);
        assert_eq!(e["type"], "stdio");
        assert_eq!(e["command"], "echo");
        assert_eq!(e["args"], serde_json::json!(["hi"]));
        assert!(e.get("env").is_none(), "env omitted when empty");
    }

    #[test]
    fn build_stdio_entry_with_env() {
        let mut env = Map::new();
        env.insert("API_KEY".into(), Value::String("xxx".into()));
        let e = build_stdio_entry("srv", &[], &env);
        assert_eq!(e["env"]["API_KEY"], "xxx");
        assert_eq!(e["args"], serde_json::json!([]), "args always present");
    }

    #[test]
    fn build_remote_entry_http() {
        let e = build_remote_entry(McpTransport::Http, "https://x/mcp", &Map::new());
        assert_eq!(e["type"], "http");
        assert_eq!(e["url"], "https://x/mcp");
        assert!(e.get("headers").is_none());
        // sse shares the shape with a different discriminant.
        let s = build_remote_entry(McpTransport::Sse, "https://x/sse", &Map::new());
        assert_eq!(s["type"], "sse");
    }

    #[test]
    fn build_remote_entry_with_headers() {
        let mut h = Map::new();
        h.insert("Authorization".into(), Value::String("Bearer t".into()));
        let e = build_remote_entry(McpTransport::Http, "https://x", &h);
        assert_eq!(e["headers"]["Authorization"], "Bearer t");
    }

    #[test]
    fn add_then_get_roundtrips() {
        let tmp = TempDir::new().unwrap();
        // config dir does not exist yet — add must create it + a fresh {}.
        let cfg = tmp.path().join("claude-config");
        assert!(add_server(&cfg, "echo", stdio("echo", &["hi"])).unwrap());
        let got = get_server(&cfg, "echo").unwrap().expect("server present");
        assert_eq!(got["command"], "echo");
        // The file must be valid JSON with a top-level mcpServers map.
        let text = std::fs::read_to_string(cfg.join(CLAUDE_JSON)).unwrap();
        let v: Value = serde_json::from_str(&text).unwrap();
        assert!(v["mcpServers"]["echo"].is_object());
    }

    #[test]
    fn add_is_idempotent() {
        let tmp = TempDir::new().unwrap();
        let cfg = tmp.path().join("cc");
        assert!(add_server(&cfg, "echo", stdio("echo", &["hi"])).unwrap());
        // Second identical add → no change, no rewrite.
        assert!(!add_server(&cfg, "echo", stdio("echo", &["hi"])).unwrap());
    }

    #[test]
    fn add_preserves_other_keys() {
        let tmp = TempDir::new().unwrap();
        let cfg = tmp.path().join("cc");
        std::fs::create_dir_all(&cfg).unwrap();
        std::fs::write(
            cfg.join(CLAUDE_JSON),
            r#"{"oauthAccount":{"keep":"me"},"mcpServers":{"pre":{"type":"stdio","command":"pre","args":[]}}}"#,
        )
        .unwrap();
        add_server(&cfg, "echo", stdio("echo", &["hi"])).unwrap();
        let v: Value =
            serde_json::from_str(&std::fs::read_to_string(cfg.join(CLAUDE_JSON)).unwrap()).unwrap();
        assert_eq!(v["oauthAccount"]["keep"], "me", "unrelated key preserved");
        assert!(v["mcpServers"]["pre"].is_object(), "existing server kept");
        assert!(v["mcpServers"]["echo"].is_object(), "new server added");
    }

    #[test]
    fn remove_existing_returns_true() {
        let tmp = TempDir::new().unwrap();
        let cfg = tmp.path().join("cc");
        add_server(&cfg, "echo", stdio("echo", &["hi"])).unwrap();
        assert!(remove_server(&cfg, "echo").unwrap());
        assert!(get_server(&cfg, "echo").unwrap().is_none());
    }

    #[test]
    fn remove_absent_returns_false() {
        let tmp = TempDir::new().unwrap();
        let cfg = tmp.path().join("cc");
        assert!(!remove_server(&cfg, "nope").unwrap());
    }

    #[test]
    fn get_absent_returns_none() {
        let tmp = TempDir::new().unwrap();
        let cfg = tmp.path().join("cc");
        assert!(get_server(&cfg, "nope").unwrap().is_none());
    }

    #[test]
    fn list_returns_all_added() {
        let tmp = TempDir::new().unwrap();
        let cfg = tmp.path().join("cc");
        add_server(&cfg, "a", stdio("a", &[])).unwrap();
        add_server(
            &cfg,
            "b",
            build_remote_entry(McpTransport::Http, "https://b", &Map::new()),
        )
        .unwrap();
        let all = list_servers(&cfg).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all["a"]["type"], "stdio");
        assert_eq!(all["b"]["type"], "http");
    }

    #[test]
    fn add_quarantines_malformed_json() {
        let tmp = TempDir::new().unwrap();
        let cfg = tmp.path().join("cc");
        std::fs::create_dir_all(&cfg).unwrap();
        std::fs::write(cfg.join(CLAUDE_JSON), b"{ not valid json !!!").unwrap();
        // add must succeed despite the corrupt file.
        add_server(&cfg, "echo", stdio("echo", &["hi"])).unwrap();
        assert!(
            cfg.join(".claude.json.corrupt").exists(),
            "corrupt file quarantined, not deleted"
        );
        let v: Value =
            serde_json::from_str(&std::fs::read_to_string(cfg.join(CLAUDE_JSON)).unwrap()).unwrap();
        assert!(v["mcpServers"]["echo"].is_object());
    }

    #[test]
    fn managed_mcp_server_names_defaults_to_builtin() {
        let names = managed_mcp_server_names(&serde_json::json!({}));
        assert_eq!(
            names,
            vec!["trusty-memory", "trusty-review", "trusty-search"]
        );
    }

    #[test]
    fn managed_mcp_server_names_unions_builtin_with_configured() {
        let config = serde_json::json!({
            "mcpServers": {
                "aaa-custom": { "type": "stdio", "command": "x", "args": [] },
                "trusty-memory": { "type": "stdio", "command": "trusty-memory", "args": [] }
            }
        });
        let names = managed_mcp_server_names(&config);
        // Built-in three + the custom server, sorted + deduped (trusty-memory
        // appears once despite being in both the builtin list and the config).
        assert_eq!(
            names,
            vec![
                "aaa-custom",
                "trusty-memory",
                "trusty-review",
                "trusty-search"
            ]
        );
    }
}
