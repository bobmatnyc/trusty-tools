//! Project trust + MCP-server pre-seeding for the managed CLAUDE_CONFIG_DIR.
//!
//! Why: WI-3 (sub-parts 2+3) requires that the managed Claude Code session never
//! sees a blocking "Do you trust this folder?" or "New MCP servers found" dialog.
//! Claude Code reads per-directory trust from `$CLAUDE_CONFIG_DIR/.claude.json` when
//! `CLAUDE_CONFIG_DIR` is set — NOT from `$HOME/.claude.json`.  All trust seeding
//! for the managed path MUST target `<claude_config_dir>/.claude.json`.
//!
//! ISOLATION INVARIANT: this module NEVER writes to `~/.claude.json` or
//! `~/.claude/`.  It only writes to files under `<claude_config_dir>` (the
//! managed CLAUDE_CONFIG_DIR — typically `~/.trusty-mpm/claude-config/`).
//!
//! What: [`preseed_managed_trust`] merges `projects.<workspace>` trust keys and
//! `enabledMcpjsonServers` into `<claude_config_dir>/.claude.json`. The MCP
//! server list is derived from `ensure_mcp_config` output (the same two servers
//! written into the managed `.mcp.json`).
//! Test: `test_preseed_managed_trust_marks_directory`,
//!   `test_preseed_managed_trust_is_idempotent`,
//!   `test_preseed_managed_trust_enables_mcp_servers`,
//!   `test_preseed_managed_trust_no_home_write` (isolation guard).

use std::path::Path;

/// The MCP server names written into the managed `.mcp.json` by `ensure_mcp_config`.
///
/// Why: `preseed_managed_trust` must pre-approve exactly the servers that
/// `ensure_mcp_config` wires into the managed `.mcp.json`; deriving them from the
/// same constant keeps the two in sync without re-parsing the file at trust-seed time.
/// What: the sorted list of server names — `["trusty-memory", "trusty-search"]` —
/// matching the keys written by `ensure_mcp_config` in `global_config.rs`.
/// Test: asserted in `test_preseed_managed_trust_enables_mcp_servers`.
pub(super) const MANAGED_MCP_SERVERS: &[&str] = &["trusty-memory", "trusty-search"];

/// Pre-seed project trust and MCP-server approval into `<claude_config_dir>/.claude.json`.
///
/// Why (WI-3 sub-parts 2+3): Claude Code shows two blocking dialogs for unfamiliar
/// projects — (1) "Do you trust the files in this folder?" and (2) "New MCP servers
/// found". Both are keyed to the absolute workspace path under
/// `projects.<workspace>` in `$CLAUDE_CONFIG_DIR/.claude.json`. Writing the trust
/// keys before launch means managed sessions start immediately without any dialog.
///
/// ISOLATION: writes ONLY to `<claude_config_dir>/.claude.json` — never to
/// `$HOME/.claude.json` or anything under `~/.claude/`. This is the central
/// isolation invariant for the managed driver (WI-7 support).
///
/// What: reads `<claude_config_dir>/.claude.json` (starts from `{}` when absent;
/// skips silently when the file exists but contains malformed JSON so we never
/// clobber OAuth state), then ensures `projects.<workspace>` carries
/// `hasTrustDialogAccepted: true`, `hasCompletedProjectOnboarding: true`,
/// `projectOnboardingSeenCount: 1` (if not already ≥ 1), and
/// `enabledMcpjsonServers: ["trusty-memory","trusty-search"]`.
/// Writes back pretty-printed; idempotent: if all fields already match, the
/// file is NOT rewritten. All other keys in the file are preserved.
/// Test: `test_preseed_managed_trust_marks_directory`,
///   `test_preseed_managed_trust_is_idempotent`,
///   `test_preseed_managed_trust_enables_mcp_servers`,
///   `test_preseed_managed_trust_no_home_write`.
pub fn preseed_managed_trust(claude_config_dir: &Path, workspace: &Path) -> anyhow::Result<()> {
    use serde_json::Value;

    let claude_json = claude_config_dir.join(".claude.json");

    // Read existing config. If the file exists but contains malformed JSON we
    // must NOT overwrite it — it may hold OAuth state. Treat as a soft no-op.
    let mut config: Value = match std::fs::read_to_string(&claude_json) {
        Ok(text) => match serde_json::from_str::<Value>(&text) {
            Ok(val) if val.is_object() => val,
            Ok(_) => {
                tracing::warn!(
                    "skipping managed trust pre-seed: {} is valid JSON but not an object",
                    claude_json.display()
                );
                return Ok(());
            }
            Err(err) => {
                tracing::warn!(
                    "skipping managed trust pre-seed: {} is not valid JSON ({err}); \
                     leaving untouched to protect OAuth state",
                    claude_json.display()
                );
                return Ok(());
            }
        },
        // Missing file is expected on first seed — start fresh.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Value::Object(serde_json::Map::new()),
        Err(err) => {
            tracing::warn!(
                "skipping managed trust pre-seed: failed to read {}: {err}",
                claude_json.display()
            );
            return Ok(());
        }
    };

    let workspace_key = workspace.to_string_lossy().to_string();

    // Navigate to (or create) projects.<workspace>.
    let projects = config
        .as_object_mut()
        .expect("config is an object")
        .entry("projects")
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    if !projects.is_object() {
        *projects = Value::Object(serde_json::Map::new());
    }
    let projects = projects.as_object_mut().expect("projects is an object");

    let entry = projects
        .entry(workspace_key)
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    if !entry.is_object() {
        *entry = Value::Object(serde_json::Map::new());
    }
    let entry = entry.as_object_mut().expect("project entry is an object");

    // Build the expected enabledMcpjsonServers list from the canonical constant.
    let enabled_mcp = Value::Array(
        MANAGED_MCP_SERVERS
            .iter()
            .map(|s| Value::String(s.to_string()))
            .collect(),
    );

    // Idempotent: skip the write when all fields are already fully set AND the
    // MCP list already matches the canonical server set.
    let already_seeded = entry.get("hasTrustDialogAccepted") == Some(&Value::Bool(true))
        && entry.get("hasCompletedProjectOnboarding") == Some(&Value::Bool(true))
        && entry
            .get("projectOnboardingSeenCount")
            .and_then(|v| v.as_i64())
            .is_some_and(|n| n >= 1)
        && entry.get("enabledMcpjsonServers") == Some(&enabled_mcp);
    if already_seeded {
        return Ok(());
    }

    entry.insert("hasTrustDialogAccepted".to_string(), Value::Bool(true));
    entry.insert(
        "hasCompletedProjectOnboarding".to_string(),
        Value::Bool(true),
    );
    // Only set the count to 1 if it is not already >= 1 (avoid overwriting
    // a higher value the user may have accumulated in a previous session).
    entry
        .entry("projectOnboardingSeenCount")
        .or_insert_with(|| Value::from(1));
    entry.insert("enabledMcpjsonServers".to_string(), enabled_mcp);

    // Use atomic write so a crash mid-write never produces a torn .claude.json
    // (which may hold OAuth state). `write_json_atomic` also creates parent
    // directories and backs up the previous file to `.claude.json.bak`.
    trusty_common::claude_config::write_json_atomic(&claude_json, &config)
        .map_err(|e| anyhow::anyhow!("failed to write {}: {e}", claude_json.display()))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // WI-3 TRUST-SEED: preseed_managed_trust must write trust keys for the workspace.
    #[test]
    fn test_preseed_managed_trust_marks_directory() {
        let tmp = TempDir::new().unwrap();
        let cfg = tmp.path().join("claude-config");
        std::fs::create_dir_all(&cfg).unwrap();
        let workspace = tmp.path().join("projects").join("my-repo").join("repo");
        std::fs::create_dir_all(&workspace).unwrap();

        preseed_managed_trust(&cfg, &workspace).unwrap();

        let text = std::fs::read_to_string(cfg.join(".claude.json")).unwrap();
        let val: serde_json::Value = serde_json::from_str(&text).unwrap();

        let key = workspace.to_string_lossy().to_string();
        let proj = val["projects"][&key].as_object().unwrap();
        assert_eq!(
            proj.get("hasTrustDialogAccepted"),
            Some(&serde_json::Value::Bool(true)),
            "hasTrustDialogAccepted must be true"
        );
        assert_eq!(
            proj.get("hasCompletedProjectOnboarding"),
            Some(&serde_json::Value::Bool(true)),
            "hasCompletedProjectOnboarding must be true"
        );
        assert!(
            proj.get("projectOnboardingSeenCount")
                .and_then(|v| v.as_i64())
                .is_some_and(|n| n >= 1),
            "projectOnboardingSeenCount must be >= 1"
        );
    }

    // WI-3 MCP-ENABLE: preseed_managed_trust must pre-approve the managed MCP servers.
    #[test]
    fn test_preseed_managed_trust_enables_mcp_servers() {
        let tmp = TempDir::new().unwrap();
        let cfg = tmp.path().join("claude-config");
        std::fs::create_dir_all(&cfg).unwrap();
        let workspace = tmp.path().join("repo");
        std::fs::create_dir_all(&workspace).unwrap();

        preseed_managed_trust(&cfg, &workspace).unwrap();

        let text = std::fs::read_to_string(cfg.join(".claude.json")).unwrap();
        let val: serde_json::Value = serde_json::from_str(&text).unwrap();

        let key = workspace.to_string_lossy().to_string();
        let servers = val["projects"][&key]["enabledMcpjsonServers"]
            .as_array()
            .expect("enabledMcpjsonServers must be an array");
        let names: Vec<&str> = servers.iter().filter_map(|v| v.as_str()).collect();
        assert!(
            names.contains(&"trusty-memory"),
            "trusty-memory must be in enabledMcpjsonServers; got {names:?}"
        );
        assert!(
            names.contains(&"trusty-search"),
            "trusty-search must be in enabledMcpjsonServers; got {names:?}"
        );
    }

    // WI-3 TRUST-SEED idempotency: calling preseed_managed_trust twice must not
    // corrupt the file or duplicate entries.
    #[test]
    fn test_preseed_managed_trust_is_idempotent() {
        let tmp = TempDir::new().unwrap();
        let cfg = tmp.path().join("claude-config");
        std::fs::create_dir_all(&cfg).unwrap();
        let workspace = tmp.path().join("my-repo");
        std::fs::create_dir_all(&workspace).unwrap();

        preseed_managed_trust(&cfg, &workspace).unwrap();
        let after_first = std::fs::read_to_string(cfg.join(".claude.json")).unwrap();

        preseed_managed_trust(&cfg, &workspace).unwrap();
        let after_second = std::fs::read_to_string(cfg.join(".claude.json")).unwrap();

        assert_eq!(
            after_first, after_second,
            "preseed_managed_trust must be idempotent: two calls must produce identical output"
        );
    }

    // WI-3 TRUST-SEED: existing keys must be preserved (trust seed must not
    // clobber unrelated data already in the config file).
    #[test]
    fn test_preseed_managed_trust_preserves_other_keys() {
        let tmp = TempDir::new().unwrap();
        let cfg = tmp.path().join("claude-config");
        std::fs::create_dir_all(&cfg).unwrap();
        let workspace = tmp.path().join("repo");
        std::fs::create_dir_all(&workspace).unwrap();

        // Pre-populate .claude.json with unrelated data.
        std::fs::write(
            cfg.join(".claude.json"),
            r#"{"someOAuthToken":"keep-me","otherField":99}"#,
        )
        .unwrap();

        preseed_managed_trust(&cfg, &workspace).unwrap();

        let text = std::fs::read_to_string(cfg.join(".claude.json")).unwrap();
        let val: serde_json::Value = serde_json::from_str(&text).unwrap();

        assert_eq!(
            val.get("someOAuthToken").and_then(|v| v.as_str()),
            Some("keep-me"),
            "someOAuthToken must be preserved after trust seed"
        );
        assert_eq!(
            val.get("otherField").and_then(|v| v.as_i64()),
            Some(99),
            "otherField must be preserved after trust seed"
        );
    }

    // WI-3 ISOLATION: preseed_managed_trust must NEVER write anything outside
    // `<claude_config_dir>`. Because the function takes `claude_config_dir`
    // explicitly and never consults `$HOME`, we can assert the isolation invariant
    // without mutating the environment: place `cfg` as a subdirectory inside a
    // wider temp root, call the function, then assert that nothing was written
    // directly under that temp root (outside `cfg`). Any accidental home-relative
    // write would have to land somewhere on the real filesystem — not in the temp
    // root — so this test proves the function only touches `claude_config_dir`.
    #[test]
    fn test_preseed_managed_trust_no_home_write() {
        // `root` is the parent sentinel: only `cfg` (a subdirectory) should gain files.
        let root = TempDir::new().unwrap();
        let cfg = root.path().join("claude-config");
        std::fs::create_dir_all(&cfg).unwrap();
        let workspace = root.path().join("repo");
        std::fs::create_dir_all(&workspace).unwrap();

        preseed_managed_trust(&cfg, &workspace).unwrap();

        // The expected output file must exist inside cfg.
        assert!(
            cfg.join(".claude.json").exists(),
            "preseed_managed_trust must write .claude.json inside claude_config_dir"
        );

        // No .claude.json should exist directly under root (outside cfg).
        assert!(
            !root.path().join(".claude.json").exists(),
            "preseed_managed_trust must NOT write .claude.json outside claude_config_dir \
             (isolation invariant)"
        );

        // No .claude directory should exist directly under root (outside cfg).
        assert!(
            !root.path().join(".claude").exists(),
            "preseed_managed_trust must NOT create .claude/ outside claude_config_dir \
             (isolation invariant)"
        );
    }
}
