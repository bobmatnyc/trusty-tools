//! Managed-session hook definitions and idempotent merge into settings.json.
//!
//! Why: WI-3 requires the managed CLAUDE_CONFIG_DIR (`~/.trusty-mpm/claude-config/`)
//! to ship the full trusty-mpm hook triad (PreToolUse / PostToolUse / Stop) so
//! the daemon receives Claude Code lifecycle events from managed sessions. Without
//! hooks the circuit breaker, audit log, and dashboard are blind to managed sessions.
//! Centralising the definition here (rather than duplicating it in the `tm install`
//! binary) lets `ensure_global_config_dir` call it from the library crate, and
//! lets the binary re-use the same literal for `tm install`.
//! What: [`mpm_hook_command`] resolves the absolute binary path (falling back to
//! the bare name when resolution fails); [`mpm_hook_additions`] returns the hook
//! triad JSON block; [`ensure_managed_hooks`] reads `<claude_config_dir>/settings.json`,
//! deep-merges the triad idempotently, and writes back; [`remove_global_trusty_mpm_hooks`]
//! strips MPM hook entries from all global settings files; [`write_project_hooks`]
//! writes project-scoped hooks into a given settings file.
//! Test: `test_ensure_managed_hooks_writes_triad`,
//! `test_ensure_managed_hooks_is_idempotent`,
//! `test_hook_command_uses_absolute_path`,
//! `test_remove_global_trusty_mpm_hooks_removes_only_mpm_entries`,
//! `test_write_project_hooks_targets_project_dir` in this module.

use std::path::{Path, PathBuf};

/// The bare fallback command name when the binary path cannot be resolved.
///
/// Why: if `current_exe()` fails (e.g. under `cargo test` without a real
/// install), we must still produce a working hook definition rather than
/// panicking. The bare name is what Claude Code would previously use
/// (resolved via PATH), so it degrades gracefully.
/// What: `"trusty-mpm hook"`.
const BARE_HOOK_COMMAND: &str = "trusty-mpm hook";

/// Resolve the absolute path for the `trusty-mpm hook` command.
///
/// Why: using a bare binary name means Claude Code resolves the hook via
/// `$PATH` at fire-time. In build environments or fresh shells where
/// `~/.cargo/bin` is absent from `PATH`, this silently fails to launch the
/// process — the hook executor throws a not-found error and the event is
/// lost. Using the absolute path of the running binary eliminates the PATH
/// dependency entirely and is correct by construction: the binary being
/// installed is the same binary running `tm install`.
/// What: calls `std::env::current_exe()`, canonicalizes the path, and
/// returns `"<abs-path> hook"`. Falls back to `BARE_HOOK_COMMAND` if
/// resolution or canonicalization fails so the install never panics.
/// If a caller already knows the exe path (e.g. from `current_exe()` cached
/// at startup), they can pass it via `exe_override` to skip the syscall.
/// Test: `test_hook_command_uses_absolute_path`.
pub fn mpm_hook_command(exe_override: Option<&Path>) -> String {
    let resolved = exe_override
        .map(|p| p.canonicalize().unwrap_or_else(|_| p.to_path_buf()))
        .or_else(|| {
            std::env::current_exe()
                .ok()
                .and_then(|p| p.canonicalize().ok().or(Some(p)))
        });

    match resolved {
        Some(p) if p.is_absolute() => format!("{} hook", p.display()),
        _ => BARE_HOOK_COMMAND.to_string(),
    }
}

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
/// `exe_override` pins the binary path for the hook command; pass `None` to resolve
/// via `mpm_hook_command(None)`.
/// Test: `test_mpm_hook_additions_has_five_events`,
/// covered by `test_ensure_managed_hooks_writes_triad`.
pub fn mpm_hook_additions_with_exe(exe_override: Option<&Path>) -> serde_json::Value {
    let cmd = mpm_hook_command(exe_override);
    serde_json::json!({
        "hooks": {
            "PreToolUse": [{
                "matcher": "*",
                "hooks": [{
                    "type": "command",
                    "command": cmd,
                    "timeout": 5
                }]
            }],
            "PostToolUse": [{
                "matcher": "*",
                "hooks": [{
                    "type": "command",
                    "command": cmd,
                    "timeout": 60,
                    "async": true
                }]
            }],
            "Stop": [{
                "matcher": "*",
                "hooks": [{
                    "type": "command",
                    "command": cmd,
                    "timeout": 5
                }]
            }],
            "SessionStart": [{
                "matcher": "*",
                "hooks": [{
                    "type": "command",
                    "command": cmd,
                    "timeout": 5
                }]
            }],
            "SessionEnd": [{
                "matcher": "*",
                "hooks": [{
                    "type": "command",
                    "command": cmd,
                    "timeout": 5
                }]
            }]
        }
    })
}

/// Build the MPM lifecycle hook additions JSON block with the default exe resolution.
///
/// Why: convenience wrapper that calls `mpm_hook_additions_with_exe(None)` so
/// existing call sites that do not need to pin the exe path stay concise.
/// What: delegates to [`mpm_hook_additions_with_exe`] with `None`.
/// Test: covered by `test_mpm_hook_additions_has_five_events`.
pub fn mpm_hook_additions() -> serde_json::Value {
    mpm_hook_additions_with_exe(None)
}

/// Check if a command string is one of the trusty-mpm hook command variants.
///
/// Why: when stripping global hooks we need to match both bare and absolute-
/// path variants so we do not leave stale bare-name entries behind.
/// What: returns `true` when `cmd` ends with ` hook` and its prefix (the
/// binary path) either is the literal `"trusty-mpm"` or ends with `/trusty-mpm`.
/// Test: `test_remove_global_trusty_mpm_hooks_removes_only_mpm_entries`.
fn is_mpm_hook_command(cmd: &str) -> bool {
    // The command must end with " hook" (with exactly one trailing sub-command word).
    let Some(binary) = cmd.strip_suffix(" hook") else {
        return false;
    };
    binary == "trusty-mpm" || binary.ends_with("/trusty-mpm")
}

/// Strip trusty-mpm hook entries from every global Claude settings file.
///
/// Why: after switching from global hooks to project-scoped hooks, any
/// previously installed global hook triad must be cleaned up so it does not
/// fire in unrelated projects. This mirrors the `remove_global_trusty_memory_hooks`
/// pattern from trusty-memory — strip first, then write project-scoped hooks.
/// What: discovers all `.claude/settings*.json` files under `$HOME` via
/// [`trusty_common::claude_config::discover_claude_settings`], and for each file
/// removes any hook group whose `command` field matches a trusty-mpm hook pattern
/// (see [`is_mpm_hook_command`]). Writes back atomically only when something
/// actually changed. Returns the count of files modified.
/// Test: `test_remove_global_trusty_mpm_hooks_removes_only_mpm_entries`.
pub fn remove_global_trusty_mpm_hooks() -> anyhow::Result<usize> {
    use trusty_common::claude_config::{
        default_settings_max_depth, discover_claude_settings, write_json_atomic,
    };

    let home =
        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("could not resolve home directory"))?;
    let files = discover_claude_settings(&home, default_settings_max_depth());

    let mut changed = 0usize;
    for path in &files {
        let text = match std::fs::read_to_string(path) {
            Ok(s) if s.trim().is_empty() => continue,
            Ok(s) => s,
            Err(_) => continue,
        };
        let mut val: serde_json::Value = match serde_json::from_str::<serde_json::Value>(&text) {
            Ok(v) if v.is_object() => v,
            _ => continue,
        };

        if strip_mpm_hook_entries(&mut val) {
            if let Err(e) = write_json_atomic(path, &val) {
                eprintln!(
                    "warning: could not remove MPM hooks from {}: {e}",
                    path.display()
                );
            } else {
                changed += 1;
            }
        }
    }
    Ok(changed)
}

/// Remove every trusty-mpm hook entry from a settings JSON value in-place.
///
/// Why: shared logic between `remove_global_trusty_mpm_hooks` and tests.
/// What: iterates over every event key under `hooks`, filters out matcher
/// groups whose `hooks[*].command` matches [`is_mpm_hook_command`], and
/// removes the entire event key when its array becomes empty after filtering.
/// Returns `true` if anything was removed.
/// Test: `test_remove_global_trusty_mpm_hooks_removes_only_mpm_entries`.
fn strip_mpm_hook_entries(val: &mut serde_json::Value) -> bool {
    let Some(hooks_map) = val.get_mut("hooks").and_then(|h| h.as_object_mut()) else {
        return false;
    };

    let mut any_changed = false;
    let mut events_to_remove: Vec<String> = Vec::new();

    for (event_key, event_val) in hooks_map.iter_mut() {
        let Some(arr) = event_val.as_array_mut() else {
            continue;
        };
        let before_len = arr.len();
        arr.retain(|group| {
            // Retain groups that do NOT contain exclusively mpm hook commands.
            let Some(inner_hooks) = group.get("hooks").and_then(|h| h.as_array()) else {
                return true; // keep unknown shapes
            };
            !inner_hooks.iter().all(|entry| {
                entry
                    .get("command")
                    .and_then(|c| c.as_str())
                    .is_some_and(is_mpm_hook_command)
            })
        });
        if arr.len() != before_len {
            any_changed = true;
            if arr.is_empty() {
                events_to_remove.push(event_key.clone());
            }
        }
    }

    // Remove now-empty event keys entirely.
    for key in events_to_remove {
        hooks_map.remove(&key);
        any_changed = true;
    }

    // Remove the `hooks` key itself if the map is now empty.
    if hooks_map.is_empty() {
        val.as_object_mut().unwrap().remove("hooks");
    }

    any_changed
}

/// Write project-scoped MPM hooks into a single Claude settings file.
///
/// Why: global hooks fire in every project and can break unrelated build
/// environments that have a stripped PATH. Project-scoped hooks only fire
/// inside the specific project directory, matching trusty-memory's approach.
/// What: reads `settings_path` (tolerates missing/empty — starts from `{}`),
/// deep-merges the MPM hook additions, and writes back atomically only when
/// something actually changed. Returns `true` when the file was updated.
/// `exe_override` is forwarded to [`mpm_hook_additions_with_exe`] so the
/// caller can pin the binary path at install time.
/// Test: `test_write_project_hooks_targets_project_dir`.
pub fn write_project_hooks(
    settings_path: &Path,
    exe_override: Option<&Path>,
) -> anyhow::Result<bool> {
    use trusty_common::claude_config::{merge_hook_entries, write_json_atomic};

    let original: serde_json::Value = match std::fs::read_to_string(settings_path) {
        Ok(s) if s.trim().is_empty() => serde_json::Value::Object(serde_json::Map::new()),
        Ok(s) => serde_json::from_str::<serde_json::Value>(&s)
            .ok()
            .filter(|v| v.is_object())
            .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            serde_json::Value::Object(serde_json::Map::new())
        }
        Err(e) => {
            return Err(anyhow::Error::new(e))
                .map_err(|e| anyhow::anyhow!("read {}: {e}", settings_path.display()));
        }
    };

    let additions = mpm_hook_additions_with_exe(exe_override);
    let merged = merge_hook_entries(&original, &additions);

    if merged == original {
        return Ok(false);
    }

    if let Some(parent) = settings_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    write_json_atomic(settings_path, &merged)
        .map_err(|e| anyhow::anyhow!("write {}: {e}", settings_path.display()))?;
    Ok(true)
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
/// (idempotency requirement). Uses [`mpm_hook_additions_with_exe`] to embed the
/// absolute binary path rather than a bare name.
/// Test: `test_ensure_managed_hooks_writes_triad`, `test_ensure_managed_hooks_is_idempotent`.
pub fn ensure_managed_hooks(claude_config_dir: &Path) -> anyhow::Result<()> {
    let settings_path = claude_config_dir.join("settings.json");
    write_project_hooks(&settings_path, None).map(|_| ())
}

/// Resolve the resolved-exe path from `current_exe()` for use at install time.
///
/// Why: callers that run `tm install` should pin the absolute path once
/// (at install entry, before any `cd`) and pass it through. This helper
/// centralises the `current_exe → canonicalize → Option<PathBuf>` dance.
/// What: returns `Some(canonicalized_path)` or `None` if resolution fails.
/// Test: covered indirectly by `test_hook_command_uses_absolute_path`.
pub fn resolve_current_exe() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.canonicalize().ok().or(Some(p)))
}

#[cfg(test)]
mod tests {
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
}
