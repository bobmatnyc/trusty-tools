//! Managed-session hook definitions and idempotent merge into settings.json.
//!
//! Why: WI-3 requires the managed CLAUDE_CONFIG_DIR (`~/.trusty-mpm/claude-config/`)
//! to ship the full trusty-mpm hook triad (PreToolUse / PostToolUse / Stop) so
//! the daemon receives Claude Code lifecycle events from managed sessions. Without
//! hooks the circuit breaker, audit log, and dashboard are blind to managed sessions.
//! Centralising the definition here (rather than duplicating it in the `tm install`
//! binary) lets `ensure_global_config_dir` call it from the library crate, and
//! lets the binary re-use the same literal for `tm install`.
//! What: [`mpm_hook_additions`] returns the hook triad JSON block;
//! [`ensure_managed_hooks`] reads `<claude_config_dir>/settings.json`, deep-merges
//! the triad idempotently, and writes back.
//! Test: `test_ensure_managed_hooks_writes_triad`,
//! `test_ensure_managed_hooks_is_idempotent` in this module.

use std::path::Path;

/// Build the MPM lifecycle hook additions JSON block (five events).
///
/// Why: every call site — the managed global config writer AND `tm install` — must
/// use the exact same shape so [`trusty_common::claude_config::merge_hook_entries`]
/// can dedup by deep equality without producing duplicates. Centralising the literal
/// here means the managed path and the `install` path can never silently diverge.
/// `SessionStart` and `SessionEnd` (#1744) are required so the daemon can capture
/// the Claude Code internal session UUID (for `--resume`) and immediately mark a
/// session Stopped when Claude Code exits, even on ungraceful exits. Without these
/// two events wired, `correlate_session_start` and `handle_session_end` in
/// `daemon/api.rs` receive no traffic and `claude_session_id` stays `None` forever.
/// The `merge_hook_entries` dedup logic preserves any existing `SessionStart` entry
/// (e.g. `trusty-memory inbox-check` from the project-level config) — adding the
/// `trusty-mpm hook` entry alongside it does NOT clobber the memory hook.
/// What: returns a JSON object with five `hooks` arrays:
/// `PreToolUse`, `PostToolUse` (async), `Stop`, `SessionStart`, `SessionEnd`.
/// `PostToolUse` is marked `async: true` so Claude Code does not block waiting for
/// the daemon to ingest tool results; the other four use short synchronous timeouts.
/// Test: `test_mpm_hook_additions_has_five_events`,
/// covered by `test_ensure_managed_hooks_writes_triad`.
pub fn mpm_hook_additions() -> serde_json::Value {
    serde_json::json!({
        "hooks": {
            "PreToolUse": [{
                "matcher": "*",
                "hooks": [{
                    "type": "command",
                    "command": "trusty-mpm hook",
                    "timeout": 5
                }]
            }],
            "PostToolUse": [{
                "matcher": "*",
                "hooks": [{
                    "type": "command",
                    "command": "trusty-mpm hook",
                    "timeout": 60,
                    "async": true
                }]
            }],
            "Stop": [{
                "matcher": "*",
                "hooks": [{
                    "type": "command",
                    "command": "trusty-mpm hook",
                    "timeout": 5
                }]
            }],
            "SessionStart": [{
                "matcher": "*",
                "hooks": [{
                    "type": "command",
                    "command": "trusty-mpm hook",
                    "timeout": 5
                }]
            }],
            "SessionEnd": [{
                "matcher": "*",
                "hooks": [{
                    "type": "command",
                    "command": "trusty-mpm hook",
                    "timeout": 5
                }]
            }]
        }
    })
}

/// Idempotently merge the MPM hook triad into `<claude_config_dir>/settings.json`.
///
/// Why: the managed CLAUDE_CONFIG_DIR starts with an empty `settings.json` (`{}`),
/// so without this call managed sessions have NO hooks and the daemon is blind to
/// their lifecycle events. Called from [`super::global_config::ensure_global_config_dir`]
/// after the initial settings.json seed so the file is always wired on every
/// managed launch.
/// What: reads `<claude_config_dir>/settings.json` (tolerates missing / empty /
/// malformed by starting from `{}`), deep-merges [`mpm_hook_additions`] using
/// [`trusty_common::claude_config::merge_hook_entries`], and writes back only when
/// the merged value differs — so calling this twice produces identical files
/// (idempotency requirement).
/// Test: `test_ensure_managed_hooks_writes_triad`, `test_ensure_managed_hooks_is_idempotent`.
pub fn ensure_managed_hooks(claude_config_dir: &Path) -> anyhow::Result<()> {
    use trusty_common::claude_config::{merge_hook_entries, write_json_atomic};

    let settings_path = claude_config_dir.join("settings.json");

    let original: serde_json::Value = match std::fs::read_to_string(&settings_path) {
        Ok(s) if s.trim().is_empty() => serde_json::Value::Object(serde_json::Map::new()),
        Ok(s) => serde_json::from_str::<serde_json::Value>(&s)
            .ok()
            .filter(|v| v.is_object())
            .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new())),
        Err(_) => serde_json::Value::Object(serde_json::Map::new()),
    };

    let additions = mpm_hook_additions();
    let merged = merge_hook_entries(&original, &additions);

    // Write back only when something actually changed (idempotency guard).
    if merged != original {
        write_json_atomic(&settings_path, &merged)
            .map_err(|e| anyhow::anyhow!("failed to write managed hooks to settings.json: {e}"))?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_mpm_hook_additions_has_five_events() {
        // #1744: SessionStart/SessionEnd must be present so the daemon receives
        // them for claude_session_id capture and immediate-Stopped marking.
        let v = mpm_hook_additions();
        let hooks = v.get("hooks").expect("missing 'hooks' key");
        assert!(hooks.get("PreToolUse").is_some(), "missing PreToolUse");
        assert!(hooks.get("PostToolUse").is_some(), "missing PostToolUse");
        assert!(hooks.get("Stop").is_some(), "missing Stop");
        assert!(
            hooks.get("SessionStart").is_some(),
            "missing SessionStart (#1744)"
        );
        assert!(
            hooks.get("SessionEnd").is_some(),
            "missing SessionEnd (#1744)"
        );
    }

    // WI-3 HOOK-CLEAN test: after ensure_managed_hooks, settings.json contains
    // the full PreToolUse/PostToolUse/Stop trusty-mpm hook entries.
    #[test]
    fn test_ensure_managed_hooks_writes_triad() {
        let tmp = TempDir::new().unwrap();
        let cfg = tmp.path().to_path_buf();

        // Write a minimal settings.json (simulating the initial seed).
        std::fs::write(cfg.join("settings.json"), "{}\n").unwrap();

        ensure_managed_hooks(&cfg).unwrap();

        let text = std::fs::read_to_string(cfg.join("settings.json")).unwrap();
        let val: serde_json::Value = serde_json::from_str(&text).unwrap();

        let hooks = val
            .get("hooks")
            .expect("settings.json must contain 'hooks'");
        assert!(
            hooks.get("PreToolUse").is_some(),
            "settings.json must contain hooks.PreToolUse after ensure_managed_hooks"
        );
        assert!(
            hooks.get("PostToolUse").is_some(),
            "settings.json must contain hooks.PostToolUse after ensure_managed_hooks"
        );
        assert!(
            hooks.get("Stop").is_some(),
            "settings.json must contain hooks.Stop after ensure_managed_hooks"
        );
        assert!(
            hooks.get("SessionStart").is_some(),
            "settings.json must contain hooks.SessionStart after ensure_managed_hooks (#1744)"
        );
        assert!(
            hooks.get("SessionEnd").is_some(),
            "settings.json must contain hooks.SessionEnd after ensure_managed_hooks (#1744)"
        );

        // Verify the command in at least one event references trusty-mpm.
        let pre = hooks["PreToolUse"].as_array().unwrap();
        let cmd = pre[0]["hooks"][0]["command"].as_str().unwrap();
        assert_eq!(
            cmd, "trusty-mpm hook",
            "hook command must be 'trusty-mpm hook'"
        );
    }

    // WI-3 HOOK-CLEAN idempotency test: calling ensure_managed_hooks twice must
    // NOT duplicate hook entries.
    #[test]
    fn test_ensure_managed_hooks_is_idempotent() {
        let tmp = TempDir::new().unwrap();
        let cfg = tmp.path().to_path_buf();

        std::fs::write(cfg.join("settings.json"), "{}\n").unwrap();

        ensure_managed_hooks(&cfg).unwrap();
        let after_first = std::fs::read_to_string(cfg.join("settings.json")).unwrap();

        ensure_managed_hooks(&cfg).unwrap();
        let after_second = std::fs::read_to_string(cfg.join("settings.json")).unwrap();

        assert_eq!(
            after_first, after_second,
            "ensure_managed_hooks must be idempotent: calling twice must not change settings.json"
        );

        // Also verify entries are not duplicated.
        let val: serde_json::Value = serde_json::from_str(&after_second).unwrap();
        let pre = val["hooks"]["PreToolUse"].as_array().unwrap();
        let pre_hook_count = pre
            .iter()
            .filter(|g| {
                g.get("hooks")
                    .and_then(|h| h.as_array())
                    .is_some_and(|cmds| {
                        cmds.iter().any(|c| {
                            c.get("command")
                                .and_then(|v| v.as_str())
                                .is_some_and(|s| s == "trusty-mpm hook")
                        })
                    })
            })
            .count();
        assert_eq!(
            pre_hook_count, 1,
            "PreToolUse must have exactly one trusty-mpm hook group after two calls"
        );
    }

    // WI-3 HOOK-CLEAN: existing non-hook keys must be preserved after ensure_managed_hooks.
    #[test]
    fn test_ensure_managed_hooks_preserves_existing_keys() {
        let tmp = TempDir::new().unwrap();
        let cfg = tmp.path().to_path_buf();

        std::fs::write(
            cfg.join("settings.json"),
            r#"{"outputStyle":"trusty-mpm","someOtherKey":42}"#,
        )
        .unwrap();

        ensure_managed_hooks(&cfg).unwrap();

        let text = std::fs::read_to_string(cfg.join("settings.json")).unwrap();
        let val: serde_json::Value = serde_json::from_str(&text).unwrap();

        assert_eq!(
            val.get("outputStyle").and_then(|v| v.as_str()),
            Some("trusty-mpm"),
            "outputStyle key must be preserved"
        );
        assert_eq!(
            val.get("someOtherKey").and_then(|v| v.as_i64()),
            Some(42),
            "someOtherKey must be preserved"
        );
        // Hooks must also be present.
        assert!(val.get("hooks").is_some(), "hooks must be present");
    }
}
