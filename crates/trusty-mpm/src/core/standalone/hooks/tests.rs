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

/// Why (#2229): an ephemeral build/worktree `exe_override` (e.g.
/// `target/debug/deps/...`) must NOT be baked into the hook command — it 404s
/// once the artifact is rebuilt away. The command must instead resolve a stable
/// installed binary via PATH, or degrade to the bare fallback — never the
/// worktree path.
/// What: passes a `target/debug/deps` path as exe_override and asserts the
/// resulting command never contains that ephemeral path and still ends with
/// " hook".
#[test]
fn test_hook_command_rejects_ephemeral_exe_override() {
    let ephemeral = PathBuf::from("/Users/x/trusty-tools/target/debug/deps/trusty_mpm-deadbeef");
    let cmd = mpm_hook_command(Some(&ephemeral));
    assert!(
        !cmd.contains("target/debug/deps"),
        "ephemeral build path must never be baked into the hook command, got: {cmd:?}"
    );
    assert!(
        cmd.ends_with(" hook"),
        "hook command must still end with ' hook', got: {cmd:?}"
    );
}

#[test]
fn test_mpm_hook_additions_has_six_events() {
    // #1744: SessionStart/SessionEnd must be present so the daemon receives
    // them for claude_session_id capture and immediate-Stopped marking.
    // #2610: SubagentStop must be present so the hook handler sees a delegated
    // subagent's turn-end and can flag an idle-parking final message.
    let v = mpm_hook_additions();
    let hooks = v.get("hooks").expect("missing 'hooks' key");
    assert!(hooks.get("PreToolUse").is_some(), "missing PreToolUse");
    assert!(hooks.get("PostToolUse").is_some(), "missing PostToolUse");
    assert!(hooks.get("Stop").is_some(), "missing Stop");
    assert!(
        hooks.get("SubagentStop").is_some(),
        "missing SubagentStop (#2610)"
    );
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
        "SubagentStop",
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

/// Why (#2015): the crate ships the SAME binary under two `[[bin]]` names —
/// `trusty-mpm` and the short `tm` alias used by `tm run`/`tm load`/`tm
/// login`. `is_mpm_hook_command` must treat both names as the same owner so
/// [`strip_mpm_hook_entries_for_events`] recognises a stale `tm`-named group
/// when the next write resolves to `trusty-mpm` (or vice versa).
/// What: asserts bare `"tm hook"`, absolute `"/opt/bin/tm hook"`, bare
/// `"trusty-mpm hook"`, and absolute `"/opt/bin/trusty-mpm hook"` are all
/// recognised, while an unrelated binary that merely shares the `tm` stem
/// with a *different* trailing subcommand (not literally `hook`) is not.
#[test]
fn test_is_mpm_hook_command_recognises_tm_bin_name() {
    assert!(is_mpm_hook_command("tm hook"), "bare 'tm' must match");
    assert!(
        is_mpm_hook_command("/opt/old/bin/tm hook"),
        "absolute path ending in '/tm' must match"
    );
    assert!(
        is_mpm_hook_command("trusty-mpm hook"),
        "bare 'trusty-mpm' must still match"
    );
    assert!(
        is_mpm_hook_command("/opt/new/bin/trusty-mpm hook"),
        "absolute path ending in '/trusty-mpm' must still match"
    );
    assert!(
        !is_mpm_hook_command("tm status"),
        "must stay scoped to the ' hook' subcommand, not any 'tm' invocation"
    );
    assert!(
        !is_mpm_hook_command("other-tool hook"),
        "unrelated binaries named neither 'tm' nor 'trusty-mpm' must not match"
    );
}

/// Why (#2235): dedup that keyed identity on the exact file name ∈
/// {`trusty-mpm`,`tm`} could never strip a stale entry whose command carried a
/// Cargo build-artifact path (`.../deps/trusty_mpm-<hash> hook`,
/// `.../tm-<hash> hook`) or the retired `session_manager_mvp-<hash>` name, so
/// those groups accumulated forever and bloated `settings.json`. The predicate
/// must recognise the whole binary family so the strip collapses them.
/// What: asserts hash-suffixed artifacts (underscore + dash spellings, the `tm`
/// alias) and the defunct MVP name — bare and hash-suffixed — are all
/// recognised, while look-alikes that merely share a stem prefix (`tm-cli`) or
/// carry a non-hex suffix are NOT, so unrelated tools are never stripped.
#[test]
fn test_is_mpm_hook_command_recognises_stale_hash_and_mvp_variants() {
    // Cargo build-artifact (deps) forms — the recurring #2235 stale patterns.
    assert!(
        is_mpm_hook_command("/x/target/debug/deps/trusty_mpm-1a2b3c4d5e6f7a8b hook"),
        "underscore dep artifact with hex hash must match"
    );
    assert!(
        is_mpm_hook_command("/x/target/debug/deps/tm-1a2b3c4d5e6f7a8b hook"),
        "tm alias dep artifact with hex hash must match"
    );
    assert!(
        is_mpm_hook_command("/x/target/debug/deps/trusty-mpm-1a2b3c4d hook"),
        "dash-spelled artifact with hex hash must match"
    );
    // Defunct pre-rename binary — bare and hash-suffixed.
    assert!(
        is_mpm_hook_command("session_manager_mvp hook"),
        "bare defunct MVP name must match"
    );
    assert!(
        is_mpm_hook_command("/x/deps/session_manager_mvp-deadbeef hook"),
        "hash-suffixed defunct MVP name must match"
    );
    // Negative: unrelated binaries sharing a stem prefix must NOT match.
    assert!(
        !is_mpm_hook_command("/usr/bin/tm-cli hook"),
        "look-alike 'tm-cli' (non-hex suffix) must not be mis-identified as mpm"
    );
    assert!(
        !is_mpm_hook_command("/usr/bin/trusty-mpmx hook"),
        "unrelated 'trusty-mpmx' must not match"
    );
}

/// Why (issue #2940): `tm doctor` and `tm hooks clean` must recognise a
/// project's pre-existing claude-mpm hook wiring so they can warn about the
/// conflict without mistaking it for a tm entry (which would delete someone
/// else's hooks) or missing it entirely (which would under-report a real
/// conflict).
/// What: asserts a bare `claude-mpm hooks fire ...` command, an absolute
/// `.claude-mpm/`-rooted script path, and an underscore-spelled `claude_mpm`
/// variant are all recognised, case-insensitively, while an unrelated
/// command (and any tm-owned command) is not.
#[test]
fn test_is_claude_mpm_hook_command_recognises_foreign_signatures() {
    assert!(is_claude_mpm_hook_command(
        "claude-mpm hooks fire PreToolUse"
    ));
    assert!(is_claude_mpm_hook_command(
        "/Users/x/.claude-mpm/scripts/hook_handler.sh PreToolUse"
    ));
    assert!(is_claude_mpm_hook_command("claude_mpm hooks fire Stop"));
    assert!(is_claude_mpm_hook_command("CLAUDE-MPM HOOKS FIRE STOP"));
    assert!(!is_claude_mpm_hook_command("trusty-memory prompt-context"));
    assert!(!is_claude_mpm_hook_command("some-other-tool run"));
}

/// Why (issue #2940): the two predicates gate mutually exclusive actions
/// (strip vs. warn-only) — if they ever overlapped, `tm hooks clean` could
/// delete a foreign harness's hooks, which is strictly the operator's call.
/// What: for every command string [`is_mpm_hook_command`] recognises, asserts
/// [`is_claude_mpm_hook_command`] does NOT also recognise it, and vice versa.
#[test]
fn test_is_claude_mpm_hook_command_never_overlaps_tm() {
    let tm_commands = [
        "tm hook",
        "/opt/bin/trusty-mpm hook",
        "/x/target/debug/deps/trusty_mpm-1a2b3c4d5e6f7a8b hook",
        "session_manager_mvp hook",
    ];
    for cmd in tm_commands {
        assert!(is_mpm_hook_command(cmd), "sanity: {cmd} must be tm-owned");
        assert!(
            !is_claude_mpm_hook_command(cmd),
            "tm-owned command {cmd} must never also be classified as claude-mpm"
        );
    }

    let foreign_commands = [
        "claude-mpm hooks fire PreToolUse",
        "/Users/x/.claude-mpm/scripts/hook_handler.sh PreToolUse",
    ];
    for cmd in foreign_commands {
        assert!(
            is_claude_mpm_hook_command(cmd),
            "sanity: {cmd} must be claude-mpm-owned"
        );
        assert!(
            !is_mpm_hook_command(cmd),
            "claude-mpm command {cmd} must never also be classified as tm-owned"
        );
    }
}

/// Why (#2235): the durable fix must make provisioning self-compacting — a
/// config pre-seeded with duplicate managed entries from stale binary paths
/// (hash-suffixed worktree builds AND the defunct `session_manager_mvp` name)
/// must collapse to exactly ONE canonical group per event on the next
/// `write_project_hooks`, instead of the observed unbounded accumulation
/// (~6900-line settings.json). Also proves N repeated writes stay bounded.
/// What: seeds an event array with FOUR stale MPM groups (two hash-suffixed
/// path variants + one `session_manager_mvp` variant + one bare `tm`) plus one
/// unrelated non-MPM group, then calls `write_project_hooks` and asserts the
/// event collapses to exactly 2 groups (1 preserved non-MPM + 1 canonical MPM)
/// with no stale path surviving. Then writes 5 more times and asserts the raw
/// group count never grows.
#[test]
fn test_write_project_hooks_collapses_stale_hash_and_mvp_entries() {
    let tmp = TempDir::new().unwrap();
    let settings = tmp.path().join("settings.json");

    // Pre-seed PreToolUse with a pile of stale MPM variants + one non-MPM group.
    std::fs::write(
        &settings,
        serde_json::json!({
            "hooks": {
                "PreToolUse": [
                    { "matcher": "*", "hooks": [{ "type": "command",
                        "command": "/a/target/debug/deps/trusty_mpm-1111111111111111 hook" }] },
                    { "matcher": "*", "hooks": [{ "type": "command",
                        "command": "/b/target/debug/deps/tm-2222222222222222 hook" }] },
                    { "matcher": "*", "hooks": [{ "type": "command",
                        "command": "/c/deps/session_manager_mvp-deadbeef hook" }] },
                    { "matcher": "*", "hooks": [{ "type": "command",
                        "command": "tm hook" }] },
                    { "matcher": "*", "hooks": [{ "type": "command",
                        "command": "trusty-memory inbox-check" }] }
                ]
            }
        })
        .to_string(),
    )
    .unwrap();

    let exe = PathBuf::from("/home/user/.cargo/bin/trusty-mpm");
    write_project_hooks(&settings, Some(&exe)).unwrap();

    let text = std::fs::read_to_string(&settings).unwrap();
    let val: serde_json::Value = serde_json::from_str(&text).unwrap();
    let pre = val["hooks"]["PreToolUse"].as_array().unwrap();

    // Raw length (NOT predicate-filtered): 1 preserved non-MPM + 1 canonical MPM.
    assert_eq!(
        pre.len(),
        2,
        "all stale MPM variants must collapse to one canonical group, found {}: {pre:?}",
        pre.len()
    );
    let cmds: Vec<&str> = pre
        .iter()
        .filter_map(|g| g["hooks"][0]["command"].as_str())
        .collect();
    assert!(
        cmds.contains(&"trusty-memory inbox-check"),
        "non-MPM group must survive, got: {cmds:?}"
    );
    for stale in [
        "trusty_mpm-1111111111111111",
        "tm-2222222222222222",
        "session_manager_mvp",
    ] {
        assert!(
            !cmds.iter().any(|c| c.contains(stale)),
            "stale variant {stale:?} must be stripped, got: {cmds:?}"
        );
    }
    assert!(
        cmds.iter().any(|c| c.contains("/.cargo/bin/trusty-mpm")),
        "canonical resolved exe path must be present, got: {cmds:?}"
    );

    // N-times idempotency: 5 more writes must never grow the group count.
    for _ in 0..5 {
        write_project_hooks(&settings, Some(&exe)).unwrap();
    }
    let text2 = std::fs::read_to_string(&settings).unwrap();
    let val2: serde_json::Value = serde_json::from_str(&text2).unwrap();
    for event in &[
        "PreToolUse",
        "PostToolUse",
        "Stop",
        "SubagentStop",
        "SessionStart",
        "SessionEnd",
    ] {
        let arr = val2["hooks"][*event].as_array().unwrap();
        let mpm_groups = arr
            .iter()
            .filter(|g| {
                g["hooks"].as_array().is_some_and(|hs| {
                    hs.iter()
                        .any(|h| h["command"].as_str().is_some_and(|c| c.ends_with(" hook")))
                })
            })
            .count();
        assert_eq!(
            mpm_groups, 1,
            "event {event} must have exactly one MPM group after N writes, found {mpm_groups}"
        );
    }
}

/// Why (#2015): `merge_hook_entries` dedups only by byte-for-byte JSON
/// equality, so writing with a different resolved exe path (bin rename —
/// including the `tm` vs `trusty-mpm` [[bin]]-name switch, not just a
/// directory change — worktree rebuild, reinstall) must REPLACE the stale
/// MPM group rather than append a second one beside it — otherwise MPM hook
/// groups accumulate and each fires on every lifecycle event.
/// What: calls `write_project_hooks` twice — first with the `tm` bin name,
/// then with the `trusty-mpm` bin name — against the same settings file,
/// seeding a pre-existing non-MPM hook group first. Deliberately asserts on
/// the RAW array length and literal command strings (NOT filtered through
/// `is_mpm_hook_command`, the predicate under test) so a stale group that a
/// narrow/buggy predicate fails to recognise cannot hide from the
/// assertion: PreToolUse must have exactly 2 groups (1 preserved non-MPM +
/// 1 MPM), and no surviving entry anywhere may reference the first (`tm`)
/// exe path.
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

    // PreToolUse: the preserved non-MPM group + exactly one MPM group.
    // Raw length, independent of `is_mpm_hook_command` — a stale group the
    // predicate fails to recognise would otherwise inflate this to 3 while
    // still passing a predicate-filtered assertion.
    let pre = hooks["PreToolUse"]
        .as_array()
        .expect("PreToolUse must be present");
    assert_eq!(
        pre.len(),
        2,
        "PreToolUse must have exactly 2 groups (1 non-MPM + 1 MPM), found {}: {pre:?}",
        pre.len()
    );
    let pre_commands: Vec<&str> = pre
        .iter()
        .filter_map(|g| g["hooks"][0]["command"].as_str())
        .collect();
    assert!(
        pre_commands.contains(&"trusty-memory inbox-check"),
        "pre-existing non-MPM hook group must be preserved, got: {pre_commands:?}"
    );
    assert!(
        pre_commands
            .iter()
            .any(|c| c.contains("/opt/new/bin/trusty-mpm")),
        "must contain the SECOND call's exe path, got: {pre_commands:?}"
    );
    assert!(
        !pre_commands.iter().any(|c| c.contains("/opt/old/bin/tm")),
        "must NOT contain the FIRST call's stale exe path, got: {pre_commands:?}"
    );

    // Every other event started empty, so exactly one (MPM) group must survive.
    for event in &[
        "PostToolUse",
        "Stop",
        "SubagentStop",
        "SessionStart",
        "SessionEnd",
    ] {
        let arr = hooks[*event]
            .as_array()
            .unwrap_or_else(|| panic!("event {event} must be present after two writes"));
        assert_eq!(
            arr.len(),
            1,
            "event {event} must have exactly 1 group, found {}: {arr:?}",
            arr.len()
        );
        let cmd = arr[0]["hooks"][0]["command"].as_str().unwrap();
        assert!(
            cmd.contains("/opt/new/bin/trusty-mpm"),
            "event {event} must carry the SECOND call's exe path, got: {cmd:?}"
        );
        assert!(
            !cmd.contains("/opt/old/bin/tm"),
            "event {event} must NOT carry the stale FIRST exe path, got: {cmd:?}"
        );
    }
}
