//! Bootstrap the single tm-global CLAUDE_CONFIG_DIR (`~/.trusty-mpm/claude-config/`).
//!
//! Why: DOC-24 SPEC-STANDALONE-MPM-03 requires a single, shared user-level
//! config directory that supplies the global hooks, global skills/slash-commands,
//! and global MCP servers for ALL tm-launched sessions. Claude Code is pointed at
//! it via the required `CLAUDE_CONFIG_DIR` env var; without it every session
//! would be unauthenticated ("Not logged in"). This module creates that dir once
//! and never regenerates it per project.
//! What: [`ensure_global_config_dir`] creates `<managed_root>/claude-config/`,
//! writes a minimal `settings.json`, seeds `.credentials.json` from
//! `~/.claude/.credentials.json` if it exists, and writes a minimal `.mcp.json`
//! with trusty-memory and trusty-search server stubs.
//! Test: `test_global_config_dir_ensure_idempotent` in this module.

use std::path::{Path, PathBuf};

/// Ensure the single tm-global config dir exists, seeding minimal content.
///
/// Why: `tm run` calls this before launching `claude` so the CLAUDE_CONFIG_DIR
/// always contains a valid `settings.json` and MCP config. Idempotent — safe to
/// call on every launch.
/// What: creates `<managed_root>/claude-config/`, writes `settings.json` (`{}`
/// when absent), copies `~/.claude/.credentials.json` if found (silently skips
/// otherwise), and writes `.mcp.json` with trusty-memory + trusty-search stubs
/// (idempotent via the inject pattern).
/// Test: `test_global_config_dir_ensure_idempotent`.
pub fn ensure_global_config_dir(
    managed_root: &Path,
    claude_config_dir: &Path,
) -> anyhow::Result<PathBuf> {
    std::fs::create_dir_all(claude_config_dir)?;

    let settings_path = claude_config_dir.join("settings.json");
    if !settings_path.exists() {
        std::fs::write(&settings_path, "{}\n")?;
    }

    seed_credentials(claude_config_dir);
    ensure_mcp_config(claude_config_dir)?;

    let _ = managed_root;
    Ok(claude_config_dir.to_path_buf())
}

/// Copy `~/.claude/.credentials.json` into the tm-global config dir.
///
/// Why: CLAUDE_CONFIG_DIR relocates the entire user-level layer including
/// `.credentials.json` (validated 2026-06-22 / v2.1.185, A9). Without seeding
/// the file the session is unauthenticated.
/// What: copies `~/.claude/.credentials.json` → `<claude_config_dir>/.credentials.json`
/// when the source exists. Silently skips (logs a warning) when missing so that
/// `ANTHROPIC_API_KEY` + `--bare` is still a valid credential path.
/// Test: tested indirectly by `test_global_config_dir_ensure_idempotent`.
fn seed_credentials(claude_config_dir: &Path) {
    let Some(home) = dirs::home_dir() else {
        eprintln!("warning: cannot resolve home directory; skipping credential seed");
        return;
    };
    let src = home.join(".claude").join(".credentials.json");
    if !src.exists() {
        eprintln!(
            "warning: ~/.claude/.credentials.json not found; \
             set ANTHROPIC_API_KEY or run `tm install` to seed credentials"
        );
        return;
    }
    let dst = claude_config_dir.join(".credentials.json");
    if dst.exists() {
        return;
    }
    if let Err(err) = std::fs::copy(&src, &dst) {
        eprintln!("warning: failed to copy credentials: {err}");
    }
}

/// Write a minimal `.mcp.json` with trusty-memory and trusty-search stubs.
///
/// Why: the tm-global config dir must carry the global MCP server definitions
/// so every tm-launched session can use memory and search tools without
/// per-project wiring.
/// What: reads `<claude_config_dir>/.mcp.json` (starts from `{}` when absent),
/// injects `trusty-memory` and `trusty-search` entries under `mcpServers` using
/// the same idempotent merge used by `inject_mcp_server` in settings.rs.
/// Test: `test_global_config_dir_ensure_idempotent`.
fn ensure_mcp_config(claude_config_dir: &Path) -> anyhow::Result<()> {
    let mcp_path = claude_config_dir.join(".mcp.json");
    let mut config = match std::fs::read_to_string(&mcp_path) {
        Ok(text) => serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .filter(|v| v.is_object())
            .unwrap_or_else(|| serde_json::json!({})),
        Err(_) => serde_json::json!({}),
    };

    let servers = config
        .as_object_mut()
        .expect("config is an object")
        .entry("mcpServers")
        .or_insert_with(|| serde_json::json!({}));
    if !servers.is_object() {
        *servers = serde_json::json!({});
    }
    let servers = servers.as_object_mut().expect("mcpServers is an object");

    let memory_entry = serde_json::json!({
        "type": "stdio",
        "command": "trusty-memory",
        "args": ["serve", "--stdio"]
    });
    let search_entry = serde_json::json!({
        "type": "stdio",
        "command": "trusty-search",
        "args": ["serve"]
    });

    servers
        .entry("trusty-memory")
        .or_insert(memory_entry.clone());
    servers
        .entry("trusty-search")
        .or_insert(search_entry.clone());

    let serialized = serde_json::to_string_pretty(&config)?;
    std::fs::write(&mcp_path, serialized)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_global_config_dir_ensure_idempotent() {
        let tmp = TempDir::new().unwrap();
        let managed_root = tmp.path().join("managed");
        let claude_config = managed_root.join("claude-config");

        ensure_global_config_dir(&managed_root, &claude_config).unwrap();
        assert!(claude_config.join("settings.json").exists());
        assert!(claude_config.join(".mcp.json").exists());

        // Second call must not fail (idempotent).
        ensure_global_config_dir(&managed_root, &claude_config).unwrap();
        assert!(claude_config.join("settings.json").exists());
    }

    #[test]
    fn test_mcp_config_contains_both_servers() {
        let tmp = TempDir::new().unwrap();
        let cfg = tmp.path().to_path_buf();
        ensure_mcp_config(&cfg).unwrap();
        let text = std::fs::read_to_string(cfg.join(".mcp.json")).unwrap();
        let val: serde_json::Value = serde_json::from_str(&text).unwrap();
        let servers = val["mcpServers"].as_object().unwrap();
        assert!(servers.contains_key("trusty-memory"));
        assert!(servers.contains_key("trusty-search"));
    }
}
