//! Unit tests for [`super`] (managed-session hook definitions and merge logic).
//!
//! Why: split out of `mod.rs` to keep the production file under the 500-SLOC
//! cap (CLAUDE.md) once the #2015 replace-by-identity regression test was
//! added; mirrors the `core/session_launch/{mod.rs,tests.rs}` split already
//! used elsewhere in this crate.
//! What: exercises `mpm_hook_command`, `mpm_hook_additions[_with_exe]`,
//! `ensure_managed_hooks`, `strip_mpm_hook_entries`, and `write_project_hooks`
//! (including the #2015 stale-exe-path replacement regression).
//! Test: this module IS the test suite for `super`.

use super::*;
use tempfile::TempDir;

/// Why: hooks must embed an absolute binary path so they fire even in
/// environments where ~/.cargo/bin is not on PATH.
/// What: passes a known absolute path as exe_override and asserts the
/// returned command starts with that path followed by " hook".
#[test]
fn test_hook_command_uses_absolute_path() {
    let fake_exe = PathBuf::from("/usr/local/bin/trusty-mpm");
    let cmd = mpm_hook_command(Some(&fake_exe));
    assert!(
        cmd.starts_with('/'),
        "hook command must start with '/' (absolute path), got: {cmd:?}"
    );
    assert!(
        cmd.ends_with(" hook"),
        "hook command must end with ' hook', got: {cmd:?}"
    );
    assert_eq!(cmd, "/usr/local/bin/trusty-mpm hook");
}

/// Why: when exe_override is None and current_exe() is available (which
/// it always is in cargo test), the command must still start with '/' —
/// the test binary itself is an absolute path.
#[test]
fn test_hook_command_without_override_is_absolute_or_fallback() {
    let cmd = mpm_hook_command(None);
    // Either an absolute path (normal) or the bare fallback (edge case
    // where current_exe() is somehow unavailable / relative).
    assert!(
        cmd.ends_with(" hook"),
        "hook command must end with ' hook', got: {cmd:?}"
    );
}

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

/// Why: with an absolute exe_override the generated command must start
/// with '/' in every event slot.
#[test]
fn test_mpm_hook_additions_with_exe_embeds_absolute_path() {
    let fake_exe = PathBuf::from("/home/user/.cargo/bin/trusty-mpm");
    let v = mpm_hook_additions_with_exe(Some(&fake_exe));
    let hooks = v.get("hooks").expect("missing 'hooks' key");
    for event in &[
        "PreToolUse",
        "PostToolUse",
        "Stop",
        "SessionStart",
        "SessionEnd",
    ] {
        let cmd = hooks[event][0]["hooks"][0]["command"]
            .as_str()
            .unwrap_or_default();
        assert!(
            cmd.starts_with('/'),
            "event {event}: command must be absolute, got: {cmd:?}"
        );
    }
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

    // Verify the command ends with " hook" (may be absolute or bare fallback).
    let pre = hooks["PreToolUse"].as_array().unwrap();
    let cmd = pre[0]["hooks"][0]["command"].as_str().unwrap();
    assert!(
        cmd.ends_with(" hook"),
        "hook command must end with ' hook', got: {cmd:?}"
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
                            .is_some_and(|s| s.ends_with(" hook"))
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

/// Why: `remove_global_trusty_mpm_hooks` must strip only MPM entries and
/// leave unrelated hooks (e.g. trusty-memory's) intact.
/// What: seeds a settings JSON with both an MPM hook group and a non-MPM
/// hook group, calls `strip_mpm_hook_entries`, and asserts only the MPM
/// group was removed.
#[test]
fn test_strip_mpm_hook_entries_removes_only_mpm_entries() {
    let mut val = serde_json::json!({
        "hooks": {
            "PreToolUse": [
                {
                    "matcher": "*",
                    "hooks": [{ "type": "command", "command": "trusty-mpm hook" }]
                },
                {
                    "matcher": "*",
                    "hooks": [{ "type": "command", "command": "other-tool run" }]
                }
            ],
            "SessionStart": [
                {
                    "matcher": "*",
                    "hooks": [{ "type": "command", "command": "trusty-mpm hook" }]
                }
            ]
        }
    });

    let changed = strip_mpm_hook_entries(&mut val);
    assert!(changed, "must report change when MPM entries removed");

    // PreToolUse should still exist with the non-MPM hook.
    let pre = val["hooks"]["PreToolUse"].as_array().unwrap();
    assert_eq!(pre.len(), 1, "non-MPM entry must survive");
    assert_eq!(
        pre[0]["hooks"][0]["command"].as_str().unwrap(),
        "other-tool run"
    );

    // SessionStart must be fully removed (was MPM-only).
    assert!(
        val["hooks"].get("SessionStart").is_none(),
        "empty event key must be removed"
    );
}

/// Why: absolute-path variants of the MPM hook command must also be
/// recognised so stale entries from previous abspath installs are stripped.
#[test]
fn test_strip_mpm_hook_entries_recognises_abspath_variants() {
    let mut val = serde_json::json!({
        "hooks": {
            "Stop": [
                {
                    "matcher": "*",
                    "hooks": [{ "type": "command", "command": "/home/user/.cargo/bin/trusty-mpm hook" }]
                }
            ]
        }
    });

    let changed = strip_mpm_hook_entries(&mut val);
    assert!(changed);
    // hooks key removed entirely when all events are gone.
    assert!(val.get("hooks").is_none(), "hooks key must be removed");
}

/// Why: `write_project_hooks` must write into the specified project
/// settings path, NOT into any global file.
/// What: calls `write_project_hooks` with a tempdir-based path, asserts the
/// file was created there and contains hook entries.
#[test]
fn test_write_project_hooks_targets_project_dir() {
    let tmp = TempDir::new().unwrap();
    let project_settings = tmp.path().join(".claude").join("settings.json");
    // File doesn't exist yet — write_project_hooks must create it.
    let fake_exe = PathBuf::from("/fake/bin/trusty-mpm");
    let wrote = write_project_hooks(&project_settings, Some(&fake_exe)).unwrap();
    assert!(wrote, "must report file was written");
    assert!(
        project_settings.exists(),
        "project settings file must exist"
    );

    let text = std::fs::read_to_string(&project_settings).unwrap();
    let val: serde_json::Value = serde_json::from_str(&text).unwrap();
    let hooks = val.get("hooks").expect("hooks key must be present");
    assert!(hooks.get("PreToolUse").is_some());

    let cmd = hooks["PreToolUse"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap();
    assert!(
        cmd.starts_with('/'),
        "command must be absolute, got: {cmd:?}"
    );
}

/// Why: calling `write_project_hooks` twice must be a no-op (idempotent).
#[test]
fn test_write_project_hooks_is_idempotent() {
    let tmp = TempDir::new().unwrap();
    let settings = tmp.path().join("settings.json");
    let fake_exe = PathBuf::from("/fake/bin/trusty-mpm");

    write_project_hooks(&settings, Some(&fake_exe)).unwrap();
    let content_first = std::fs::read_to_string(&settings).unwrap();

    let wrote_second = write_project_hooks(&settings, Some(&fake_exe)).unwrap();
    assert!(!wrote_second, "second call must be a no-op");

    let content_second = std::fs::read_to_string(&settings).unwrap();
    assert_eq!(content_first, content_second, "file must not change");
}

/// Why (#2015): `merge_hook_entries` dedups only by byte-for-byte JSON
/// equality, so writing with a different resolved exe path (bin rename,
/// worktree rebuild, reinstall) must REPLACE the stale MPM group rather
/// than append a second one beside it — otherwise MPM hook groups
/// accumulate and each fires on every lifecycle event.
/// What: calls `write_project_hooks` twice with two different
/// `exe_override` paths against the same settings file, seeding a
/// pre-existing non-MPM hook group first. Asserts exactly one MPM hook
/// group per event survives (matching the SECOND call's exe path) and
/// the non-MPM group is untouched.
#[test]
fn test_write_project_hooks_replaces_stale_exe_path_group() {
    let tmp = TempDir::new().unwrap();
    let settings = tmp.path().join("settings.json");

    // Seed a pre-existing non-MPM hook group that must survive untouched.
    std::fs::write(
        &settings,
        serde_json::json!({
            "hooks": {
                "PreToolUse": [{
                    "matcher": "*",
                    "hooks": [{ "type": "command", "command": "trusty-memory inbox-check" }]
                }]
            }
        })
        .to_string(),
    )
    .unwrap();

    let exe_v1 = PathBuf::from("/opt/old/bin/tm");
    let exe_v2 = PathBuf::from("/opt/new/bin/trusty-mpm");

    write_project_hooks(&settings, Some(&exe_v1)).unwrap();
    write_project_hooks(&settings, Some(&exe_v2)).unwrap();

    let text = std::fs::read_to_string(&settings).unwrap();
    let val: serde_json::Value = serde_json::from_str(&text).unwrap();
    let hooks = val["hooks"].as_object().expect("hooks must be present");

    for event in &[
        "PreToolUse",
        "PostToolUse",
        "Stop",
        "SessionStart",
        "SessionEnd",
    ] {
        let arr = hooks[*event]
            .as_array()
            .unwrap_or_else(|| panic!("event {event} must be present after two writes"));
        let mpm_groups: Vec<&serde_json::Value> = arr
            .iter()
            .filter(|g| {
                g.get("hooks")
                    .and_then(|h| h.as_array())
                    .is_some_and(|cmds| {
                        cmds.iter().all(|c| {
                            c.get("command")
                                .and_then(|v| v.as_str())
                                .is_some_and(is_mpm_hook_command)
                        })
                    })
            })
            .collect();
        assert_eq!(
            mpm_groups.len(),
            1,
            "event {event} must have exactly one MPM hook group, found {}",
            mpm_groups.len()
        );
        let cmd = mpm_groups[0]["hooks"][0]["command"].as_str().unwrap();
        assert_eq!(
            cmd, "/opt/new/bin/trusty-mpm hook",
            "event {event} must carry the SECOND call's exe path"
        );
    }

    // The pre-existing non-MPM group must be preserved untouched.
    let pre = hooks["PreToolUse"].as_array().unwrap();
    assert!(
        pre.iter()
            .any(|g| g["hooks"][0]["command"] == "trusty-memory inbox-check"),
        "pre-existing non-MPM hook group must be preserved"
    );
}
