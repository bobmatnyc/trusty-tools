//! CLI handlers for `tm mcp add|remove|list|get`.
//!
//! Why: register user-level MCP servers into tm's OWN tm-owned
//! `CLAUDE_CONFIG_DIR` — the top-level `mcpServers` map of that dir's
//! `.claude.json`, which stock `claude mcp add` cannot target. Every tm-managed
//! session then sees the server (user scope, "available in all your projects";
//! no approval dialog). The CRUD itself lives in
//! [`trusty_mpm::core::mcp_config`]; these handlers are the thin CLI shell that
//! validates transport-specific flags, resolves the config dir, and formats
//! output — shaped like `commands::standalone`'s `*_cmd(&ManagedPaths, …)`.
//! Test: `cli_parses_mcp_*` in `tests.rs`; parse helpers unit-tested below;
//! CRUD in `core::mcp_config`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::{Map, Value};

use crate::cli::McpTransportArg;
use trusty_mpm::core::mcp_config::{
    self, McpTransport, build_remote_entry, build_stdio_entry, strip_arg_separator,
};
use trusty_mpm::core::mcp_test::{self, McpTestResult};

/// Resolve the `.claude.json`-bearing config dir for a `tm mcp` invocation.
///
/// Why: the default target is the DAEMON-managed dir
/// (`~/.trusty-tools/trusty-mpm/claude-config/`) so servers reach every managed
/// session; `--root` switches to a STANDALONE root via the same precedence chain
/// `tm register`/`tm ls` use, reusing that resolver verbatim.
/// What: with `root = None` returns
/// [`trusty_mpm::core::trusty_tools_config::managed_claude_config_dir`]; with
/// `root = Some(_)` returns `resolve_managed_paths(root)?.claude_config_dir`.
/// Test: exercised end-to-end via the `tm mcp` handlers.
fn resolve_config_dir(root: Option<&str>) -> Result<PathBuf> {
    match root {
        Some(_) => Ok(super::managed_root::resolve_managed_paths(root)?.claude_config_dir),
        None => trusty_mpm::core::trusty_tools_config::managed_claude_config_dir()
            .context("cannot resolve home directory for the managed claude config dir"),
    }
}

/// Parse `KEY=VALUE` pairs into a JSON object.
///
/// Why: `-e KEY=VALUE` (repeatable) mirrors `claude mcp add -e`.
/// What: splits each item on the FIRST `=`; errors on a missing `=`.
/// Test: `parse_kv_env_splits_on_first_equals`.
fn parse_env(pairs: &[String]) -> Result<Map<String, Value>> {
    let mut map = Map::new();
    for p in pairs {
        let (k, v) = p
            .split_once('=')
            .with_context(|| format!("invalid --env '{p}': expected KEY=VALUE"))?;
        map.insert(k.to_string(), Value::String(v.to_string()));
    }
    Ok(map)
}

/// Parse `Name: Value` headers into a JSON object.
///
/// Why: `-H "Name: Value"` (repeatable) mirrors `claude mcp add --header`.
/// What: splits each item on the FIRST `:`, trimming surrounding whitespace from
/// the value; errors on a missing `:`.
/// Test: `parse_headers_splits_on_first_colon`.
fn parse_headers(items: &[String]) -> Result<Map<String, Value>> {
    let mut map = Map::new();
    for h in items {
        let (k, v) = h
            .split_once(':')
            .with_context(|| format!("invalid --header '{h}': expected 'Name: Value'"))?;
        map.insert(k.trim().to_string(), Value::String(v.trim().to_string()));
    }
    Ok(map)
}

/// Handle `tm mcp add <name> …`.
///
/// Why: the primary verb — upsert a user-scope MCP server into the tm config dir.
/// What: validates transport-specific inputs (stdio takes `-e`/args + a command
/// and WARNS (never blocks) when the command is absent from `PATH`, matching
/// `claude mcp add`; http/sse take `-H` + a URL), builds the entry, and calls
/// [`mcp_config::add_server`]. Prints whether the server was added/updated or
/// already present. `command_and_args` is the raw trailing-var-arg slice from
/// clap: its first element is the command (stdio) or URL (http/sse); any
/// remaining elements are stdio subprocess args (rejected for http/sse).
/// Test: `cli_parses_mcp_add`, `cli_parses_mcp_add_http`; CRUD in `core::mcp_config`.
pub(crate) fn add_cmd(
    root: Option<&str>,
    name: &str,
    transport: McpTransportArg,
    env: &[String],
    header: &[String],
    command_and_args: &[String],
) -> Result<()> {
    let config_dir = resolve_config_dir(root)?;
    let command_or_url = command_and_args.first().map(String::as_str);
    // Drop a leading `--` separator (clap's `trailing_var_arg` keeps it in the
    // captured tail after the command positional; persisting it verbatim breaks
    // every npx/uv-style stdio server). Only the first token is stripped — a
    // legitimate deeper `--` is preserved. See `mcp_config::strip_arg_separator`.
    let args = strip_arg_separator(command_and_args.get(1..).unwrap_or_default());

    let entry = match transport {
        McpTransportArg::Stdio => {
            if !header.is_empty() {
                bail!("--header is only valid for http/sse transports, not stdio");
            }
            let command = command_or_url
                .context("stdio transport requires a <command> positional argument")?;
            // Match `claude mcp add`: warn but do NOT block when the command is
            // not resolvable on PATH (it may be installed later or shadowed).
            if trusty_common::bin_resolve::resolve_binary(command).is_none() {
                tracing::warn!(
                    "command '{command}' for MCP server '{name}' was not found on PATH; \
                     adding anyway (it must resolve at session launch)"
                );
                eprintln!("warning: '{command}' not found on PATH — adding '{name}' anyway");
            }
            let env = parse_env(env)?;
            build_stdio_entry(command, args, &env)
        }
        McpTransportArg::Http | McpTransportArg::Sse => {
            if !env.is_empty() {
                bail!("--env is only valid for the stdio transport, not http/sse");
            }
            if !args.is_empty() {
                bail!("subprocess args (after `--`) are only valid for the stdio transport");
            }
            let url = command_or_url
                .context("http/sse transport requires a <url> positional argument")?;
            let headers = parse_headers(header)?;
            let t = if matches!(transport, McpTransportArg::Http) {
                McpTransport::Http
            } else {
                McpTransport::Sse
            };
            build_remote_entry(t, url, &headers)
        }
    };

    let changed = mcp_config::add_server(&config_dir, name, entry)?;
    if changed {
        println!(
            "Added MCP server '{name}' to {}",
            config_dir.join(".claude.json").display()
        );
    } else {
        println!("MCP server '{name}' already present (no change)");
    }
    Ok(())
}

/// Handle `tm mcp remove <name>`.
///
/// Why: drop a server the operator no longer wants injected into managed sessions.
/// What: calls [`mcp_config::remove_server`]; prints whether it was removed or
/// was already absent.
/// Test: `cli_parses_mcp_remove`; CRUD in `core::mcp_config`.
pub(crate) fn remove_cmd(root: Option<&str>, name: &str) -> Result<()> {
    let config_dir = resolve_config_dir(root)?;
    if mcp_config::remove_server(&config_dir, name)? {
        println!("Removed MCP server '{name}'");
    } else {
        println!("MCP server '{name}' not found (no change)");
    }
    Ok(())
}

/// Handle `tm mcp list [--json]`.
///
/// Why: operators need an overview of which user-scope servers managed sessions
/// will load.
/// What: lists the top-level `mcpServers` map as a table (name → type + target)
/// or, with `--json`, the raw map.
/// Test: `cli_parses_mcp_list`; CRUD in `core::mcp_config`.
pub(crate) fn list_cmd(root: Option<&str>, json: bool) -> Result<()> {
    let config_dir = resolve_config_dir(root)?;
    let servers = mcp_config::list_servers(&config_dir)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&servers)?);
        return Ok(());
    }
    if servers.is_empty() {
        println!(
            "No user-scope MCP servers registered in {}",
            config_dir.display()
        );
        println!("  Add one with: tm mcp add <name> -- <command> [args...]");
        return Ok(());
    }
    let name_w = servers.keys().map(String::len).max().unwrap_or(4).max(4);
    println!(
        "MCP servers in {} ({}):",
        config_dir.display(),
        servers.len()
    );
    println!("  {:<name_w$}  TYPE   TARGET", "NAME");
    for (name, entry) in &servers {
        println!(
            "  {:<name_w$}  {:<5}  {}",
            name,
            entry_type(entry),
            entry_target(entry)
        );
    }
    Ok(())
}

/// Handle `tm mcp get <name> [--json]`.
///
/// Why: inspect a single server's full definition.
/// What: prints the entry as pretty JSON (`--json`) or a short field summary;
/// errors if the server is absent.
/// Test: `cli_parses_mcp_get`; CRUD in `core::mcp_config`.
pub(crate) fn get_cmd(root: Option<&str>, name: &str, json: bool) -> Result<()> {
    let config_dir = resolve_config_dir(root)?;
    let Some(entry) = mcp_config::get_server(&config_dir, name)? else {
        bail!("MCP server '{name}' not found in {}", config_dir.display());
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&entry)?);
    } else {
        println!("{name}:");
        println!("  Type:   {}", entry_type(&entry));
        println!("  Target: {}", entry_target(&entry));
        println!("  Scope:  User config (available in all managed sessions)");
    }
    Ok(())
}

/// Handle `tm mcp test [<name>] [--json]`.
///
/// Why: registration only proves a server's config is well-formed, not that it
/// starts and speaks MCP. `test` spawns each server and runs the real handshake
/// so operators (and CI) get a definitive pass/fail.
/// What: resolves the config dir, selects targets (one named server, or the
/// full user-scope + built-in union when `name` is `None`), probes each with a
/// bounded timeout via [`mcp_test::run_all`], then prints a table (or `--json`).
/// Exits with status 1 if ANY server failed (so it is usable as a CI gate);
/// a not-found named server is a hard error. Async because the stdio handshake
/// and http reachability check are I/O-bound.
/// Test: `cli_parses_mcp_test` in `tests.rs`; probe/selection logic in
/// `core::mcp_test`.
pub(crate) async fn test_cmd(root: Option<&str>, name: Option<&str>, json: bool) -> Result<()> {
    let config_dir = resolve_config_dir(root)?;
    let servers = mcp_config::list_servers(&config_dir)?;
    let targets = mcp_test::select_targets(&servers, name)?;
    let results = mcp_test::run_all(targets, mcp_test::DEFAULT_PROBE_TIMEOUT).await;

    if json {
        let arr: Vec<Value> = results.iter().map(McpTestResult::to_json).collect();
        println!("{}", serde_json::to_string_pretty(&Value::Array(arr))?);
    } else {
        print_test_table(&results, &config_dir);
    }

    // Non-zero exit if any server failed, matching `tm services`' convention so
    // `tm mcp test` slots straight into CI. Exit here (not a returned Err) to
    // avoid printing anyhow's error chrome after the report.
    if mcp_test::any_failed(&results) {
        std::process::exit(1);
    }
    Ok(())
}

/// Render the human-readable results table for `tm mcp test`.
fn print_test_table(results: &[McpTestResult], config_dir: &Path) {
    if results.is_empty() {
        println!("No MCP servers to test in {}", config_dir.display());
        return;
    }
    println!(
        "Testing {} MCP server(s) from {}:",
        results.len(),
        config_dir.display()
    );
    let name_w = results
        .iter()
        .map(|r| r.name.len())
        .max()
        .unwrap_or(4)
        .max(4);
    println!("  {:<name_w$}  TRANSPORT  RESULT  DETAIL", "NAME");
    for r in results {
        let (label, detail) = r.render();
        println!(
            "  {:<name_w$}  {:<9}  {:<6}  {}",
            r.name,
            r.transport.as_str(),
            label,
            detail
        );
    }
    let passed = results.iter().filter(|r| r.passed()).count();
    println!("\n{passed}/{} passed", results.len());
}

/// The `type` discriminant of an entry, defaulting to `stdio` when absent.
fn entry_type(entry: &Value) -> &str {
    entry.get("type").and_then(Value::as_str).unwrap_or("stdio")
}

/// A one-line human summary of an entry's connection target (command+args or URL).
fn entry_target(entry: &Value) -> String {
    if let Some(url) = entry.get("url").and_then(Value::as_str) {
        return url.to_string();
    }
    let cmd = entry.get("command").and_then(Value::as_str).unwrap_or("");
    let mut args: Vec<&str> = entry
        .get("args")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    // Show the EFFECTIVE argv: drop a legacy stored leading `--` separator so
    // `tm mcp list/get` reflect what actually gets spawned (mirrors the spawn-path
    // and write-path normalisation in `mcp_config::strip_arg_separator`).
    if args.first() == Some(&"--") {
        args.remove(0);
    }
    if args.is_empty() {
        cmd.to_string()
    } else {
        format!("{cmd} {}", args.join(" "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_kv_env_splits_on_first_equals() {
        let m = parse_env(&["A=1".into(), "B=x=y".into()]).unwrap();
        assert_eq!(m["A"], "1");
        assert_eq!(m["B"], "x=y", "only the first '=' splits");
    }

    #[test]
    fn parse_env_rejects_missing_equals() {
        assert!(parse_env(&["NOEQ".into()]).is_err());
    }

    #[test]
    fn parse_headers_splits_on_first_colon() {
        let m = parse_headers(&["Authorization: Bearer a:b".into()]).unwrap();
        assert_eq!(
            m["Authorization"], "Bearer a:b",
            "value trimmed, first ':' splits"
        );
    }

    #[test]
    fn parse_headers_rejects_missing_colon() {
        assert!(parse_headers(&["NoColon".into()]).is_err());
    }

    #[test]
    fn entry_target_formats_command_and_url() {
        let stdio = serde_json::json!({"type":"stdio","command":"echo","args":["hi","there"]});
        assert_eq!(entry_target(&stdio), "echo hi there");
        let http = serde_json::json!({"type":"http","url":"https://x/mcp"});
        assert_eq!(entry_target(&http), "https://x/mcp");
    }

    #[test]
    fn entry_target_shows_effective_argv_dropping_legacy_separator() {
        // A registry entry written before the fix still displays the effective
        // argv (no leading `--`) in `tm mcp list/get`.
        let legacy = serde_json::json!({
            "type":"stdio","command":"npx",
            "args":["--","-y","@modelcontextprotocol/server-github"]
        });
        assert_eq!(
            entry_target(&legacy),
            "npx -y @modelcontextprotocol/server-github"
        );
    }

    /// The npx shape `tm mcp add github npx -- -y @scope/pkg` must persist WITHOUT
    /// the `--` sentinel (clap's `trailing_var_arg` captures it as the first tail
    /// token). `--root` is tier-1 precedence so the write lands in
    /// `<tempdir>/claude-config`, letting us assert the stored args round-trip.
    #[test]
    fn add_strips_leading_separator_before_persisting() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path().to_str().unwrap();
        let command_and_args = vec![
            "npx".to_string(),
            "--".to_string(),
            "-y".to_string(),
            "@modelcontextprotocol/server-github".to_string(),
        ];
        add_cmd(
            Some(root),
            "github",
            McpTransportArg::Stdio,
            &[],
            &[],
            &command_and_args,
        )
        .unwrap();

        let cfg = std::path::Path::new(root).join("claude-config");
        let entry = mcp_config::get_server(&cfg, "github")
            .unwrap()
            .expect("server persisted");
        assert_eq!(entry["command"], "npx");
        assert_eq!(
            entry["args"],
            serde_json::json!(["-y", "@modelcontextprotocol/server-github"]),
            "the `--` sentinel must not be persisted into stored args"
        );
    }

    /// Round-trip proof that only the LEADING `--` is stripped: a deeper `--`
    /// (a real inner separator some CLIs need) survives into the stored args.
    #[test]
    fn add_preserves_deeper_separator() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path().to_str().unwrap();
        // `tm mcp add x uv -- run -- tool` → tail = ["uv","--","run","--","tool"].
        let command_and_args = vec![
            "uv".to_string(),
            "--".to_string(),
            "run".to_string(),
            "--".to_string(),
            "tool".to_string(),
        ];
        add_cmd(
            Some(root),
            "x",
            McpTransportArg::Stdio,
            &[],
            &[],
            &command_and_args,
        )
        .unwrap();

        let cfg = std::path::Path::new(root).join("claude-config");
        let entry = mcp_config::get_server(&cfg, "x").unwrap().unwrap();
        assert_eq!(entry["command"], "uv");
        assert_eq!(
            entry["args"],
            serde_json::json!(["run", "--", "tool"]),
            "leading `--` stripped once; the deeper `--` is preserved"
        );
    }
}
