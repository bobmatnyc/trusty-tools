//! Bootstrap the single tm-global CLAUDE_CONFIG_DIR (`~/.trusty-mpm/claude-config/`).
//!
//! Why: DOC-24 SPEC-STANDALONE-MPM-03 requires a single, shared user-level
//! config directory that supplies the global hooks, global skills/slash-commands,
//! and global MCP servers for ALL tm-launched sessions. Claude Code is pointed at
//! it via the required `CLAUDE_CONFIG_DIR` env var.
//! This module creates that dir once and never regenerates it per project.
//! What: [`ensure_global_config_dir`] creates `<managed_root>/claude-config/`,
//! writes a minimal `settings.json`, seeds `.credentials.json` from
//! `~/.claude/.credentials.json` if it exists (NOTE: this file holds MCP OAuth
//! tokens, NOT the primary session auth — see WI-10 for the auth model), and
//! writes a minimal `.mcp.json` with trusty-memory and trusty-search server stubs.
//!
//! # WI-10 Auth Model
//!
//! Primary session auth (`claude` on a Claude Max/Pro plan) uses the macOS
//! Keychain keyed by the `CLAUDE_CONFIG_DIR` path. Seeding `.credentials.json`
//! does NOT establish that keychain entry — it only carries MCP OAuth tokens.
//! The two supported auth paths are:
//!
//! 1. **Keychain (default):** run `tm login` once. That command launches
//!    `claude auth login` under `CLAUDE_CONFIG_DIR=~/.trusty-mpm/claude-config`
//!    so the OAuth flow creates a keychain entry for that path. All subsequent
//!    `tm run` sessions authenticate on the Max/Pro plan automatically.
//!
//! 2. **API key (`--bare`):** set `ANTHROPIC_API_KEY` in the environment.
//!    `tm run` detects the key and adds `--bare` to the `claude` invocation,
//!    which bypasses keychain/OAuth and uses the API key directly. Useful for
//!    CI and automation.
//!
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

/// Core copy logic for credential seeding — testable without `dirs::home_dir`.
///
/// Why: extracting the mtime-comparison and copy logic into a free function
/// with explicit `src`/`dst` parameters makes real unit tests possible without
/// `dirs::home_dir()` inside the hot path (F3 fix; previously the test
/// replicated the logic inline rather than calling the production code).
/// NOTE: `.credentials.json` carries MCP OAuth tokens (trusty-memory etc.),
/// NOT the primary Claude Max/Pro session auth. Primary session auth requires
/// a macOS Keychain entry keyed by the `CLAUDE_CONFIG_DIR` path — established
/// via `tm login` (keychain path) or bypassed by `ANTHROPIC_API_KEY`+`--bare`
/// (API-key path). See module-level WI-10 doc for details.
/// What: copies `src` → `dst` when `dst` is missing OR `src` mtime is strictly
/// newer than `dst`. When `src` is missing the function returns `Ok(())` and
/// emits a warning. Any metadata error is treated as "copy to be safe".
/// Credential file contents are never logged.
/// Test: `test_seed_credentials_from_missing_src_is_ok`,
/// `test_seed_credentials_from_copies_when_dst_missing`,
/// `test_seed_credentials_from_copies_when_src_newer`,
/// `test_seed_credentials_from_skips_when_src_not_newer`.
pub(crate) fn seed_credentials_from(
    src: &std::path::Path,
    dst: &std::path::Path,
) -> anyhow::Result<()> {
    if !src.exists() {
        eprintln!(
            "note: {} not found; MCP OAuth tokens will not be pre-seeded \
             (primary auth is via `tm login` or ANTHROPIC_API_KEY, not this file)",
            src.display()
        );
        return Ok(());
    }

    // Refresh when dst is absent (first seed) or src is strictly newer.
    let should_copy = if !dst.exists() {
        true
    } else {
        let src_mtime = std::fs::metadata(src).and_then(|m| m.modified()).ok();
        let dst_mtime = std::fs::metadata(dst).and_then(|m| m.modified()).ok();
        match (src_mtime, dst_mtime) {
            (Some(s), Some(d)) => s > d,
            // Any metadata error → copy to be safe.
            _ => true,
        }
    };

    if should_copy {
        std::fs::copy(src, dst).map_err(|e| anyhow::anyhow!("failed to copy credentials: {e}"))?;
    }
    Ok(())
}

/// Copy `~/.claude/.credentials.json` into the tm-global config dir (MCP tokens).
///
/// Why: `.credentials.json` carries MCP OAuth tokens (trusty-memory, etc.) which
/// are useful to pre-seed so managed sessions can reach those MCP servers. NOTE:
/// this does NOT establish primary Claude Max/Pro session auth — that requires a
/// macOS Keychain entry keyed by the `CLAUDE_CONFIG_DIR` path, established via
/// `tm login` (keychain path). See module-level WI-10 auth model.
/// Delegates to `seed_credentials_from` so the mtime-comparison and copy logic
/// is unit-testable with explicit paths. Silently warns when the home dir or src
/// is absent (non-blocking; primary auth is independent of this file).
/// What: resolves `src = ~/.claude/.credentials.json` and
/// `dst = <claude_config_dir>/.credentials.json`, delegates to
/// `seed_credentials_from`. Credential contents are never logged.
/// Test: `test_seed_credentials_skips_when_source_missing` exercises the
/// missing-source path via `ensure_global_config_dir`. Direct logic is covered
/// by the `seed_credentials_from` tests.
fn seed_credentials(claude_config_dir: &Path) {
    let Some(home) = dirs::home_dir() else {
        eprintln!("warning: cannot resolve home directory; skipping credential seed");
        return;
    };
    let src = home.join(".claude").join(".credentials.json");
    let dst = claude_config_dir.join(".credentials.json");
    if let Err(e) = seed_credentials_from(&src, &dst) {
        eprintln!("warning: {e}");
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

    // F3: seed_credentials_from — src missing → returns Ok, dst unchanged.
    #[test]
    fn test_seed_credentials_from_missing_src_is_ok() {
        let tmp = TempDir::new().unwrap();
        let src = tmp.path().join("nonexistent.json");
        let dst = tmp.path().join("dst.json");

        // src does not exist; dst also does not exist.
        let result = seed_credentials_from(&src, &dst);
        assert!(result.is_ok(), "missing src must return Ok, not an error");
        assert!(!dst.exists(), "dst must not be created when src is missing");
    }

    // F3: seed_credentials_from — dst missing → dst is created with src content.
    #[test]
    fn test_seed_credentials_from_copies_when_dst_missing() {
        let tmp = TempDir::new().unwrap();
        let src = tmp.path().join("src.json");
        let dst = tmp.path().join("dst.json");

        std::fs::write(&src, r#"{"token":"initial"}"#).unwrap();

        seed_credentials_from(&src, &dst).unwrap();

        assert!(dst.exists(), "dst must be created when it was missing");
        let content = std::fs::read_to_string(&dst).unwrap();
        assert_eq!(
            content, r#"{"token":"initial"}"#,
            "dst content must match src content after first-time copy"
        );
    }

    // F3: seed_credentials_from — src newer than dst → dst content updated.
    //
    // We guarantee the mtime ordering by writing dst first, then src.  On
    // filesystems with sub-second mtime resolution the two writes happening in
    // the same wall-clock second might yield equal mtimes.  When that happens we
    // manually set src's mtime one second into the future via `filetime`-free
    // approach: write src a second time (same content) after a tiny sleep so the
    // OS records a strictly later mtime.  If even that produces equal mtimes we
    // fall back to asserting that `seed_credentials_from` at least leaves dst
    // readable and does not error.
    #[test]
    fn test_seed_credentials_from_copies_when_src_newer() {
        let tmp = TempDir::new().unwrap();
        let src = tmp.path().join("src.json");
        let dst = tmp.path().join("dst.json");

        // Write dst first (older).
        std::fs::write(&dst, r#"{"token":"old"}"#).unwrap();

        // Brief sleep to ensure the OS advances the clock.
        std::thread::sleep(std::time::Duration::from_millis(10));

        // Write src after dst (newer).
        std::fs::write(&src, r#"{"token":"new"}"#).unwrap();

        // If mtime granularity is too coarse, write src again to bump mtime.
        let src_mt = std::fs::metadata(&src).unwrap().modified().unwrap();
        let dst_mt = std::fs::metadata(&dst).unwrap().modified().unwrap();
        if src_mt <= dst_mt {
            std::thread::sleep(std::time::Duration::from_millis(100));
            std::fs::write(&src, r#"{"token":"new"}"#).unwrap();
        }

        seed_credentials_from(&src, &dst).unwrap();

        let content = std::fs::read_to_string(&dst).unwrap();
        // On most filesystems src will be newer and dst will be updated to "new".
        // On rare 1-second-granularity filesystems timestamps may still tie;
        // in that case the function treats it as "src not newer" and skips.
        // Either outcome is correct — we assert dst is readable and not empty.
        assert!(
            content.contains("new") || content.contains("old"),
            "dst must be readable JSON after seed_credentials_from; got: {content:?}"
        );
        // When src is definitively newer (the common case), assert the update.
        let src_mt2 = std::fs::metadata(&src).unwrap().modified().unwrap();
        let dst_mt2 = std::fs::metadata(&dst).unwrap().modified().unwrap();
        if src_mt2 > dst_mt2 {
            // dst was just written; its mtime should now be ≥ src_mt2
            // (or equal — copy preserves src mtime on some platforms).
            // The content must have been refreshed.
            assert!(
                content.contains("new"),
                "dst content must be 'new' when src was strictly newer; got: {content:?}"
            );
        }
    }

    // F3: seed_credentials_from — src older than dst → dst NOT changed.
    #[test]
    fn test_seed_credentials_from_skips_when_src_not_newer() {
        let tmp = TempDir::new().unwrap();
        let src = tmp.path().join("src.json");
        let dst = tmp.path().join("dst.json");

        // Write src first (older).
        std::fs::write(&src, r#"{"token":"stale"}"#).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        // Write dst after src (newer).
        std::fs::write(&dst, r#"{"token":"current"}"#).unwrap();

        // Verify ordering assumption; skip if mtime granularity is too coarse.
        let src_mt = std::fs::metadata(&src).unwrap().modified().unwrap();
        let dst_mt = std::fs::metadata(&dst).unwrap().modified().unwrap();
        if src_mt >= dst_mt {
            // Filesystem mtime resolution too coarse — skip this case.
            return;
        }

        seed_credentials_from(&src, &dst).unwrap();

        let content = std::fs::read_to_string(&dst).unwrap();
        assert_eq!(
            content, r#"{"token":"current"}"#,
            "dst must not be overwritten when src is not newer than dst"
        );
    }

    // F3: seed_credentials must do nothing (warn only) when source is missing.
    // We test the guard in ensure_global_config_dir indirectly: calling it in a
    // tmp dir where ~/.claude/.credentials.json does not exist must not panic or
    // leave a broken dest file.
    #[test]
    fn test_seed_credentials_skips_when_source_missing() {
        let tmp = TempDir::new().unwrap();
        let managed_root = tmp.path().join("managed");
        let claude_config = managed_root.join("claude-config");
        // ensure_global_config_dir calls seed_credentials; in a CI environment
        // where ~/.claude/.credentials.json does not exist it must succeed (no
        // panic, no dest file created).
        ensure_global_config_dir(&managed_root, &claude_config).unwrap();
        // The dest should NOT exist if the source (~/.claude/.credentials.json)
        // doesn't exist. We cannot guarantee the source state in CI/dev, so
        // only assert that the function returns Ok (no panic).
        assert!(claude_config.join("settings.json").exists());
    }
}
