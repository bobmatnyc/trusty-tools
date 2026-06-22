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
//! server list is derived from `ensure_mcp_config` output (the same three servers
//! written into the managed `.mcp.json`).
//! Test: `test_preseed_managed_trust_marks_directory`,
//!   `test_preseed_managed_trust_is_idempotent`,
//!   `test_preseed_managed_trust_enables_mcp_servers`,
//!   `test_preseed_managed_trust_no_home_write` (isolation guard),
//!   `test_preseed_managed_trust_quarantines_malformed_json` (corrupt-file quarantine).

use std::path::Path;

/// The MCP server names written into the managed `.mcp.json` by `ensure_mcp_config`.
///
/// Why: `preseed_managed_trust` must pre-approve exactly the servers that
/// `ensure_mcp_config` wires into the managed `.mcp.json`; deriving them from the
/// same constant keeps the two in sync without re-parsing the file at trust-seed time.
/// What: the sorted list of server names —
/// `["trusty-memory", "trusty-review", "trusty-search"]` — matching the keys
/// written by `ensure_mcp_config` in `global_config.rs`.
/// `trusty-review` was added in WI-8 (refs #1548).
/// Test: asserted in `test_preseed_managed_trust_enables_mcp_servers`.
pub(super) const MANAGED_MCP_SERVERS: &[&str] =
    &["trusty-memory", "trusty-review", "trusty-search"];

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
/// when the file exists but contains malformed JSON the corrupt file is renamed
/// to `.claude.json.corrupt` (preserving bytes for post-mortem) and seeding
/// proceeds from a fresh `{}`), then ensures `projects.<workspace>` carries
/// `hasTrustDialogAccepted: true`, `hasCompletedProjectOnboarding: true`,
/// `projectOnboardingSeenCount: 1` (if not already ≥ 1), and
/// `enabledMcpjsonServers: ["trusty-memory","trusty-review","trusty-search"]`.
/// Writes back pretty-printed; idempotent: if all fields already match, the
/// file is NOT rewritten. All other keys in the file are preserved.
/// Test: `test_preseed_managed_trust_marks_directory`,
///   `test_preseed_managed_trust_is_idempotent`,
///   `test_preseed_managed_trust_enables_mcp_servers`,
///   `test_preseed_managed_trust_no_home_write`,
///   `test_preseed_managed_trust_quarantines_malformed_json`.
pub fn preseed_managed_trust(claude_config_dir: &Path, workspace: &Path) -> anyhow::Result<()> {
    use serde_json::Value;

    let claude_json = claude_config_dir.join(".claude.json");

    // Read existing config. If the file exists but contains malformed JSON,
    // quarantine it by renaming to `.claude.json.corrupt` (the file holds no
    // valid OAuth state when it cannot be parsed) and proceed from `{}`.
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
                // Malformed JSON holds no valid OAuth state — quarantine the
                // corrupt file so trust seeding can proceed from a fresh `{}`.
                // A malformed .claude.json would otherwise permanently block
                // trust seeding (managed sessions see the trust dialog forever).
                let corrupt_path = claude_json.with_extension("json.corrupt");
                match std::fs::rename(&claude_json, &corrupt_path) {
                    Ok(()) => tracing::warn!(
                        "managed trust pre-seed: {} is not valid JSON ({err}); \
                         quarantined to {} — proceeding with fresh config",
                        claude_json.display(),
                        corrupt_path.display()
                    ),
                    Err(rename_err) => tracing::warn!(
                        "managed trust pre-seed: {} is not valid JSON ({err}); \
                         quarantine rename failed ({rename_err}) — proceeding with fresh config",
                        claude_json.display()
                    ),
                }
                Value::Object(serde_json::Map::new())
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

    // WI-3 MCP-ENABLE / WI-8: preseed_managed_trust must pre-approve the managed MCP
    // servers, including trusty-review added in WI-8 (refs #1548).
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
        // WI-8: trusty-review must appear in the pre-approved server list so
        // managed sessions do not see the "New MCP servers found" dialog for it.
        assert!(
            names.contains(&"trusty-review"),
            "trusty-review must be in enabledMcpjsonServers (WI-8); got {names:?}"
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
    // `<claude_config_dir>`.
    //
    // This test uses two complementary strategies:
    //
    // 1. Sentinel-root check: `cfg` is a subdirectory of `root`; we assert
    //    nothing lands at the `root` level (outside `cfg`). This proves the
    //    function only writes through the `claude_config_dir` argument.
    //
    // 2. Fake-HOME redirect (serial_test): we redirect `HOME` to a second
    //    empty temp dir and assert it remains empty after the call. Because
    //    the test is serialised with `#[serial]`, the env mutation is sound —
    //    parallel Rust test threads cannot observe the changed `HOME`.
    //
    // Together the two checks cover both "writes through argument" and
    // "never writes through $HOME" without unsound parallel env mutation.
    #[serial_test::serial]
    #[test]
    fn test_preseed_managed_trust_no_home_write() {
        /// RAII guard that restores $HOME to its original value on drop (including panic).
        struct HomeGuard(Option<String>);
        impl Drop for HomeGuard {
            fn drop(&mut self) {
                match self.0 {
                    Some(ref p) => unsafe { std::env::set_var("HOME", p) },
                    None => unsafe { std::env::remove_var("HOME") },
                }
            }
        }

        // Strategy 1: sentinel-root guard.
        let root = TempDir::new().unwrap();
        let cfg = root.path().join("claude-config");
        std::fs::create_dir_all(&cfg).unwrap();
        let workspace = root.path().join("repo");
        std::fs::create_dir_all(&workspace).unwrap();

        // Strategy 2: redirect $HOME to an empty temp dir; assert it stays empty.
        let fake_home = TempDir::new().unwrap();
        // SAFETY: test is serial (#[serial_test::serial]), so no other test thread
        // reads HOME concurrently during this function body. The HomeGuard restores
        // HOME even if an assertion below panics.
        let _home_guard = {
            let prev = std::env::var("HOME").ok();
            unsafe { std::env::set_var("HOME", fake_home.path()) };
            HomeGuard(prev)
        };

        preseed_managed_trust(&cfg, &workspace).unwrap();

        // --- Strategy 1 assertions (sentinel-root) ---

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

        // --- Strategy 2 assertions (fake-HOME stays empty) ---

        // The fake home directory must contain no .claude.json and no .claude/
        // sub-directory (the two locations Claude Code reads global config from).
        assert!(
            !fake_home.path().join(".claude.json").exists(),
            "preseed_managed_trust must NOT write .claude.json to $HOME \
             (isolation invariant)"
        );
        assert!(
            !fake_home.path().join(".claude").exists(),
            "preseed_managed_trust must NOT create $HOME/.claude/ \
             (isolation invariant)"
        );

        // _home_guard drops here and restores HOME.
    }

    // WI-3 TRUST-SEED robustness: a pre-existing malformed .claude.json must be
    // quarantined (renamed to .claude.json.corrupt) and seeding must proceed from
    // a fresh `{}` — so the managed session never gets stuck in a permanent
    // "trust dialog" loop because of a corrupt file.
    #[test]
    fn test_preseed_managed_trust_quarantines_malformed_json() {
        let tmp = TempDir::new().unwrap();
        let cfg = tmp.path().join("claude-config");
        std::fs::create_dir_all(&cfg).unwrap();
        let workspace = tmp.path().join("repo");
        std::fs::create_dir_all(&workspace).unwrap();

        // Write deliberately malformed JSON into .claude.json.
        std::fs::write(cfg.join(".claude.json"), b"{ this is not valid json !!!").unwrap();

        // preseed_managed_trust must succeed (no error).
        preseed_managed_trust(&cfg, &workspace).unwrap();

        // The quarantine sibling must exist (corrupt file was renamed, not deleted).
        assert!(
            cfg.join(".claude.json.corrupt").exists(),
            ".claude.json.corrupt quarantine file must exist after malformed-JSON quarantine"
        );

        // The new .claude.json must be valid JSON containing the seeded trust keys.
        let text = std::fs::read_to_string(cfg.join(".claude.json"))
            .expect(".claude.json must exist after quarantine + fresh seed");
        let val: serde_json::Value =
            serde_json::from_str(&text).expect(".claude.json must be valid JSON after fresh seed");

        let key = workspace.to_string_lossy().to_string();
        let proj = val["projects"][&key]
            .as_object()
            .expect("projects.<workspace> must be an object");
        assert_eq!(
            proj.get("hasTrustDialogAccepted"),
            Some(&serde_json::Value::Bool(true)),
            "hasTrustDialogAccepted must be true after quarantine + fresh seed"
        );
        assert_eq!(
            proj.get("hasCompletedProjectOnboarding"),
            Some(&serde_json::Value::Bool(true)),
            "hasCompletedProjectOnboarding must be true after quarantine + fresh seed"
        );
    }
}
