//! Shared `.mcp.json` (`mcpServers` map) parsing (#3238).
//!
//! Why: `.mcp.json` — the standard Claude-Code-compatible file declaring
//! stdio MCP servers — was previously parsed inline inside
//! `init::seed::seed_mcp_connections` for memory-seeding only. The live
//! runtime-registration path (`crate::tools::mcp_live`) needs the exact same
//! parsing (server name, command, args, env, description) to turn each
//! `mcpServers` entry into a spec it can spawn and `tools/list`. Extracting
//! the parser here means both call sites share one implementation instead of
//! two copies drifting apart.
//! What: `McpJsonServer` is the parsed shape of one `mcpServers` entry.
//! `discover_mcp_json_paths` locates the project-local and user-global
//! `.mcp.json` files (project first, so a later `parse` loop can treat
//! project entries as taking precedence over user-global ones with the same
//! name). `parse_mcp_json_servers` parses one file's raw JSON content into a
//! `Vec<McpJsonServer>`, sorted by name for deterministic iteration order.
//! Test: `parse_mcp_json_servers_*`, `discover_mcp_json_paths_*` below.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// One parsed `mcpServers` entry from a `.mcp.json` file.
///
/// Why: Both `seed_mcp_connections` (memory indexing) and `mcp_live`
/// (runtime tool registration) need the same fields; a shared struct means
/// neither call site re-derives them from raw `serde_json::Value` by hand.
/// What: Mirrors the Claude-Code-compatible `mcpServers.<name>` shape:
/// `command`, `args`, `env` (full key/value — callers that only need key
/// names, like the memory seeder, project `.keys()`), and an optional
/// human-readable `description`.
/// Test: `parse_mcp_json_servers_extracts_all_fields`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpJsonServer {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
    pub description: Option<String>,
}

/// Locate `.mcp.json` files relevant to `project_dir`, project-local first.
///
/// Why: Project-local config should take precedence over the user's global
/// `~/.claude/.mcp.json` when both declare a server with the same name;
/// returning project-first lets callers implement "first occurrence wins"
/// for that precedence without re-deriving the search order themselves.
/// What: Returns `<project_dir>/.mcp.json` (if it exists) followed by
/// `$HOME/.claude/.mcp.json` (if it exists). Neither existing yields an
/// empty vec — callers should treat that as "no live-discovery servers from
/// .mcp.json", not an error.
/// Test: `discover_mcp_json_paths_prefers_project_then_home`.
pub fn discover_mcp_json_paths(project_dir: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let project_mcp = project_dir.join(".mcp.json");
    if project_mcp.exists() {
        paths.push(project_mcp);
    }
    if let Some(home) = std::env::var_os("HOME") {
        let user_mcp = PathBuf::from(home).join(".claude").join(".mcp.json");
        if user_mcp.exists() {
            paths.push(user_mcp);
        }
    }
    paths
}

/// Parse a `.mcp.json` file's raw content into its `mcpServers` entries.
///
/// Why: Centralises the exact field extraction (`command`, `args`, `env`,
/// `description`) that both `seed_mcp_connections` and `mcp_live` need,
/// so the two call sites can never silently diverge on how a field is
/// read.
/// What: Returns `Err` only on malformed JSON. A file with no `mcpServers`
/// key (or one that isn't an object) parses successfully to an empty vec —
/// that's a normal "nothing to do here" case, not an error. Entries are
/// sorted by name for deterministic iteration.
/// Test: `parse_mcp_json_servers_extracts_all_fields`,
/// `parse_mcp_json_servers_empty_without_mcp_servers_key`,
/// `parse_mcp_json_servers_errors_on_malformed_json`.
pub fn parse_mcp_json_servers(raw: &str) -> Result<Vec<McpJsonServer>> {
    let parsed: serde_json::Value =
        serde_json::from_str(raw).context("failed to parse .mcp.json content")?;

    let Some(servers) = parsed.get("mcpServers").and_then(|v| v.as_object()) else {
        return Ok(Vec::new());
    };

    let mut out: Vec<McpJsonServer> = servers
        .iter()
        .map(|(name, def)| {
            let command = def
                .get("command")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let args: Vec<String> = def
                .get("args")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|x| x.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();
            let env: HashMap<String, String> = def
                .get("env")
                .and_then(|v| v.as_object())
                .map(|o| {
                    o.iter()
                        .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                        .collect()
                })
                .unwrap_or_default();
            let description = def
                .get("description")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());

            McpJsonServer {
                name: name.clone(),
                command,
                args,
                env,
                description,
            }
        })
        .collect();

    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: Every field a live-discovery spec or a memory-seed description
    /// needs must round-trip through the parser without loss.
    /// What: Feed a two-server `.mcp.json` fixture; assert command/args/env/
    /// description all extract correctly, and iteration order is
    /// name-sorted.
    /// Test: This test.
    #[test]
    fn parse_mcp_json_servers_extracts_all_fields() {
        let raw = serde_json::json!({
            "mcpServers": {
                "vector-search": {
                    "command": "uv",
                    "args": ["run", "mcp-vector-search", "mcp"],
                    "description": "Semantic code search"
                },
                "kuzu-memory": {
                    "command": "kuzu-memory",
                    "args": ["mcp"],
                    "env": {"KUZU_MEMORY_DB": "/tmp/db"}
                }
            }
        })
        .to_string();

        let servers = parse_mcp_json_servers(&raw).unwrap();
        assert_eq!(servers.len(), 2);
        // Sorted by name: kuzu-memory before vector-search.
        assert_eq!(servers[0].name, "kuzu-memory");
        assert_eq!(servers[0].command, "kuzu-memory");
        assert_eq!(servers[0].args, vec!["mcp".to_string()]);
        assert_eq!(
            servers[0].env.get("KUZU_MEMORY_DB").map(String::as_str),
            Some("/tmp/db")
        );
        assert_eq!(servers[0].description, None);

        assert_eq!(servers[1].name, "vector-search");
        assert_eq!(
            servers[1].args,
            vec![
                "run".to_string(),
                "mcp-vector-search".to_string(),
                "mcp".to_string()
            ]
        );
        assert_eq!(
            servers[1].description.as_deref(),
            Some("Semantic code search")
        );
    }

    /// Why: A `.mcp.json`-shaped file without a `mcpServers` key (or an
    /// unrelated JSON document) must not be treated as an error — it just
    /// contributes zero servers.
    /// What: Parse `{}` and assert an empty, non-error result.
    /// Test: This test.
    #[test]
    fn parse_mcp_json_servers_empty_without_mcp_servers_key() {
        let servers = parse_mcp_json_servers("{}").unwrap();
        assert!(servers.is_empty());
    }

    /// Why: Malformed JSON must surface as an `Err` so callers can log and
    /// skip that source file rather than silently losing servers.
    /// What: Feed truncated JSON; assert `Err`.
    /// Test: This test.
    #[test]
    fn parse_mcp_json_servers_errors_on_malformed_json() {
        assert!(parse_mcp_json_servers("{ not json").is_err());
    }

    /// Why: Precedence semantics (project overrides user-global) depend on
    /// project-local paths being returned before the home path.
    /// What: Create both a project `.mcp.json` and a fake `$HOME/.claude/.mcp.json`
    /// in a temp dir; assert the returned order is project-then-home, and
    /// that a missing project file still returns the home path alone.
    /// Test: This test.
    #[test]
    fn discover_mcp_json_paths_prefers_project_then_home() {
        let _guard = crate::test_env::HOME_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let project = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let claude_dir = home.path().join(".claude");
        std::fs::create_dir_all(&claude_dir).unwrap();
        std::fs::write(claude_dir.join(".mcp.json"), "{}").unwrap();

        // SAFETY: single-threaded test setup; no concurrent HOME readers
        // within this test's lifetime.
        let prev_home = std::env::var_os("HOME");
        unsafe {
            std::env::set_var("HOME", home.path());
        }

        // No project .mcp.json yet: only the home path comes back.
        let paths_home_only = discover_mcp_json_paths(project.path());
        assert_eq!(paths_home_only.len(), 1);
        assert_eq!(paths_home_only[0], claude_dir.join(".mcp.json"));

        // Add a project .mcp.json: it must come first.
        std::fs::write(project.path().join(".mcp.json"), "{}").unwrap();
        let paths_both = discover_mcp_json_paths(project.path());
        assert_eq!(paths_both.len(), 2);
        assert_eq!(paths_both[0], project.path().join(".mcp.json"));
        assert_eq!(paths_both[1], claude_dir.join(".mcp.json"));

        unsafe {
            match prev_home {
                Some(h) => std::env::set_var("HOME", h),
                None => std::env::remove_var("HOME"),
            }
        }
    }
}
