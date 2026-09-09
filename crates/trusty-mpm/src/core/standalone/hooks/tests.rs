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

/// An absolute, installed-looking `tm` path that always resolves.
///
/// Why (#7244): `resolve_stable_hook_exe` now refuses the running `cargo test`
/// binary on two independent grounds — it lives in a build tree, and its stem
/// is not one this crate ships — so every test that used to lean on
/// `current_exe()` or on the machine having `tm` on PATH would read a refusal
/// instead of the behaviour it is asserting. CI runners have no `tm`.
/// What: outside every build tree and named `tm`, so both halves of the guard
/// accept it without touching the filesystem.
const STABLE_TEST_EXE: &str = "/usr/local/bin/tm";

/// The refusal a `cargo test` binary must produce, or the installed binary a
/// host that happens to have `tm` falls back to — never the test binary.
///
/// Why (#7244): the three "must not bake X" tests below run on hosts with and
/// without `tm` installed and must assert the same thing on both. What matters
/// is not WHICH answer comes back but that the rejected path is never in it.
/// What: asserts `result` either refused, or produced a `" hook"` command that
/// does not mention `rejected`.
/// Test: this IS a test helper; see its three callers.
fn assert_never_baked(result: &Result<String, StableHookExeError>, rejected: &Path) {
    let Ok(cmd) = result else {
        return;
    };
    assert!(
        cmd.ends_with(" hook"),
        "a resolved hook command must end with ' hook', got: {cmd:?}"
    );
    assert!(
        !cmd.contains(&rejected.display().to_string()),
        "{} must never be baked into a hook command, got: {cmd:?}",
        rejected.display()
    );
}

/// Why: hooks must embed an absolute binary path so they fire even in
/// environments where ~/.cargo/bin is not on PATH.
/// What: passes a known absolute path as exe_override and asserts the
/// returned command starts with that path followed by " hook".
#[test]
fn test_hook_command_uses_absolute_path() {
    let fake_exe = PathBuf::from("/usr/local/bin/trusty-mpm");
    let cmd = mpm_hook_command(Some(&fake_exe)).expect("an installed-looking override resolves");
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

/// Why (#7244): with no override the resolver reads `current_exe()`, which
/// under `cargo test` is the libtest harness — a build artifact whose stem is
/// not one this crate ships. That is exactly the path that got written into a
/// real project's `settings.json` as the command for pm-guard, Read/Bash
/// diversion, `PostToolUse` and `SessionEnd`. It must never come back from
/// here, on any host: one with `tm` installed answers with the installed
/// binary, one without answers with a refusal.
/// What: asserts the running test binary's own path is absent from whichever
/// of those two answers this host produces.
#[test]
fn test_hook_command_without_override_never_names_the_test_binary() {
    let exe = std::env::current_exe().expect("current_exe resolvable under cargo test");
    assert_never_baked(&mpm_hook_command(None), &exe);
}

/// Why (#2229): an ephemeral build/worktree `exe_override` (e.g.
/// `target/debug/deps/...`) must NOT be baked into the hook command — it 404s
/// once the artifact is rebuilt away. The command must instead resolve a stable
/// installed binary via PATH, or refuse (#7244) — never the worktree path.
/// What: passes each ephemeral shape as exe_override and asserts the resulting
/// command never contains it.
#[test]
fn test_hook_command_rejects_ephemeral_exe_override() {
    // #7244: the second path uses this repo's per-worktree `target-<issue>`
    // build root, which contains neither `target/debug` nor `target/release`
    // and so passed the pre-fix guard outright.
    for ephemeral in [
        PathBuf::from("/Users/x/trusty-tools/target/debug/deps/trusty_mpm-deadbeef"),
        PathBuf::from("/Users/x/trusty-tools/target-7224/debug/deps/trusty_mpm-deadbeef"),
    ] {
        assert_never_baked(&mpm_hook_command(Some(&ephemeral)), &ephemeral);
    }
}

/// Why (#4485): the #2229 guard listed build/worktree layouts only, so a
/// binary under a system temp root — the shape the Claude Code agent harness
/// produces at `/private/tmp/claude-<uid>/<session>/<uuid>/scratchpad/…` —
/// looked like an ordinary installed path and WAS baked into the hook command.
/// Sixteen settings files across ten projects ended up running a dead
/// `cargo test --no-run` libtest harness on every hook event.
/// What: passes each system-temp shape as `exe_override` and asserts the
/// resulting command never contains it, while still ending in " hook" (a stable
/// PATH-resolved install, or the bare fallback).
#[test]
fn test_hook_command_rejects_system_temp_exe_override() {
    for ephemeral in [
        PathBuf::from("/private/tmp/claude-502/-Users-x-proj/9f1c/scratchpad/base-bins/trusty-mpm"),
        PathBuf::from("/tmp/trusty-mpm"),
        std::env::temp_dir().join("claude-4485/base-bins/trusty-mpm"),
    ] {
        assert_never_baked(&mpm_hook_command(Some(&ephemeral)), &ephemeral);
    }
}

/// Why (#7244): the running executable was judged purely by WHERE it lived, so
/// the moment the path guard missed a build layout — this repo's per-worktree
/// `target-<issue>` directories — a `cargo test` harness passed as "the
/// installed tm binary". `deps/test_session_lifecycle-cd3ba8f03938239b` was
/// written into a real project's `settings.json` as the command for pm-guard,
/// Read/Bash diversion, `PostToolUse` and `SessionEnd`; enforcement was dead
/// until the file was regenerated. Asking WHAT the binary is stops the class
/// even where the path guard is blind.
/// What: the exact offending stem, at a path with nothing ephemeral about it,
/// with the PATH fallback injected as empty so the refusal is the ONLY possible
/// answer on every host. Asserts the variant, not just that it failed.
#[test]
fn resolve_stable_hook_exe_with_refuses_a_foreign_binary_stem() {
    let foreign = PathBuf::from("/usr/local/bin/test_session_lifecycle-cd3ba8f03938239b");
    let err = resolve_stable_hook_exe_with(Some(foreign.clone()), |_| None)
        .expect_err("a non-tm binary must never become the hook command");
    assert!(
        matches!(&err, StableHookExeError::ForeignBinary(p) if *p == foreign),
        "expected ForeignBinary({}), got {err:?}",
        foreign.display()
    );
}

/// Why (#7244): the ephemeral half of the guard must keep refusing even when
/// the name check would have accepted — the two reasons are independent, and a
/// `deps/trusty_mpm-<hash>` artifact passes the name check by design (it is our
/// binary, just a transient copy of it).
/// What: injects an empty PATH fallback so the refusal is the only answer, and
/// pins the variant so a future change cannot silently reclassify it.
#[test]
fn resolve_stable_hook_exe_with_refuses_an_ephemeral_exe() {
    let ephemeral =
        PathBuf::from("/Users/x/trusty-tools/target-7224/debug/deps/trusty_mpm-cd3ba8f0");
    let err = resolve_stable_hook_exe_with(Some(ephemeral.clone()), |_| None)
        .expect_err("a build artifact must never become the hook command");
    assert!(
        matches!(&err, StableHookExeError::Ephemeral(p) if *p == ephemeral),
        "expected Ephemeral({}), got {err:?}",
        ephemeral.display()
    );
}

/// Why (#7244 round 2): `$PATH` is not a trust boundary. The running-exe branch
/// refuses a build-tree binary, then the fallback resolved `tm` from `$PATH`
/// and accepted whatever came back — so a `target/debug/tm` (or one of this
/// repo's `target-<issue>/debug/tm`) ahead of the installed one on `$PATH` was
/// written into `settings.json` as the hook command, which is the same bad
/// write #7244 reports, through the side door instead of the front one.
/// What: injects an ephemeral hit from `path_lookup` with a running path that
/// is itself refused, and asserts the whole call refuses rather than persisting
/// the lookup's answer. `deps` sits under `<profile>` so the guard reads it as
/// the Cargo artifact it is. The variant is the running path's refusal, which
/// is the actionable one for the operator.
#[test]
fn resolve_stable_hook_exe_with_refuses_an_ephemeral_path_lookup_hit() {
    let from_path = PathBuf::from("/Users/x/trusty-tools/target-7244/debug/tm");
    let refused = PathBuf::from("/Users/x/trusty-tools/target-7224/debug/deps/tm-cd3ba8f0");
    let err = resolve_stable_hook_exe_with(Some(refused.clone()), |_| Some(from_path.clone()))
        .expect_err("a build-tree binary on PATH must never become the hook command");
    assert!(
        matches!(&err, StableHookExeError::Ephemeral(p) if *p == refused),
        "expected Ephemeral({}), got {err:?}",
        refused.display()
    );
}

/// Why (#7244 round 2): the fallback's second gate is the stem check — a
/// `$PATH` entry that resolves to something outside a build tree but is not a
/// binary this crate ships must be refused for the same reason the running-exe
/// branch refuses one.
/// What: injects a foreign, non-ephemeral hit from `path_lookup` and asserts
/// the refusal, so the two gates are pinned independently rather than one
/// masking the other.
#[test]
fn resolve_stable_hook_exe_with_refuses_a_foreign_path_lookup_hit() {
    let from_path = PathBuf::from("/usr/local/bin/session_manager_mvp-cd3ba8f0");
    let err = resolve_stable_hook_exe_with(None, |_| Some(from_path.clone()))
        .expect_err("a non-tm binary on PATH must never become the hook command");
    assert!(
        matches!(&err, StableHookExeError::Unresolved),
        "with no running path to blame the refusal is Unresolved, got {err:?}"
    );
}

/// Why (#7244): refusing the running binary must not mean refusing outright —
/// a developer running a debug build still gets working hooks, pointed at the
/// installed binary. The fallback is what keeps the hard refusal from being a
/// regression for every debug-build launch.
/// What: injects a refused running path AND a PATH hit, and asserts the
/// installed path wins while the refused one appears nowhere.
#[test]
fn resolve_stable_hook_exe_with_falls_back_to_the_installed_binary() {
    let installed = PathBuf::from(STABLE_TEST_EXE);
    let refused = PathBuf::from("/Users/x/trusty-tools/target-7224/debug/deps/tm-cd3ba8f0");
    let resolved = resolve_stable_hook_exe_with(Some(refused.clone()), |_| Some(installed.clone()))
        .expect("an installed binary on PATH resolves");
    assert_eq!(
        resolved, installed,
        "the PATH-resolved install must win over a refused running path"
    );
}

/// Why (#7244): the name check is the second, independent reason a binary is
/// accepted, so it must accept every name this crate actually ships — a false
/// refusal here would stop hooks being written at all. Both `[[bin]]` names and
/// the underscore crate-name spelling Cargo uses for dep artifacts identify the
/// same hook owner.
///
/// Round 2 removes `session_manager_mvp` from the accepted set.
/// `crates/trusty-mpm/tests/session_manager_mvp.rs` compiles to
/// `session_manager_mvp-<hash>` on every `cargo test`, so while that name was
/// accepted the two supposedly independent checks collapsed to one for exactly
/// the shape #7244 is about: a running test harness whose only remaining
/// obstacle was the path guard. The cleanup side still recognises the retired
/// name (`MPM_STALE_BIN_STEMS`) — recognising a name for REMOVAL is safe,
/// recognising it for PERSISTENCE is not.
/// What: asserts each shipped name (bare and hash-suffixed) is recognised and
/// that a lookalike is not. `trusty-mpm` is why the hash-strip rule checks that
/// the suffix is hex: its own trailing `-mpm` must not be taken for a hash.
#[test]
fn is_mpm_bin_stem_path_accepts_the_shipped_names() {
    for name in [
        "tm",
        "trusty-mpm",
        "trusty_mpm",
        "tm-1a2b3c4d5e6f7a8b",
        "trusty_mpm-1a2b3c4d",
    ] {
        assert!(
            is_mpm_bin_stem_path(&PathBuf::from("/usr/local/bin").join(name)),
            "{name} is a binary this crate ships and must be recognised"
        );
    }
    for name in [
        "test_session_lifecycle-cd3ba8f03938239b",
        // The `cargo test` harness for this crate's own integration suite. A
        // plain `/usr/local/bin/` prefix means the path guard says nothing —
        // the stem check is the only thing refusing it (#7244 round 2).
        "session_manager_mvp",
        "session_manager_mvp-deadbeef",
        "tm-cli",
        "trusty-mpmx",
        "claude",
    ] {
        assert!(
            !is_mpm_bin_stem_path(&PathBuf::from("/usr/local/bin").join(name)),
            "{name} is not a binary this crate ships and must be refused"
        );
    }
}

/// Why (#7244) — the Fail-Open Check: a refusal must write NOTHING. The whole
/// incident is a writer that produced a hook command it could not vouch for and
/// wrote it anyway, over a settings file that already held correct ones. A
/// settings file with no tm hooks is recoverable on the next launch; one wired
/// to a dead `cargo test` harness silently disables pm-guard enforcement and
/// looks fine.
/// What: hands the writer a refusal directly (the resolution seam — on a host
/// with `tm` installed the PATH fallback would otherwise rescue the call and
/// this could never be observed) and asserts three things: the error surfaces,
/// an EXISTING settings file is byte-identical afterwards, and a MISSING one is
/// still missing — no `.claude/` directory conjured, no empty `{}` left behind.
#[test]
fn write_project_hooks_writes_nothing_when_the_exe_cannot_be_resolved() {
    let tmp = TempDir::new().expect("tempdir");

    let existing = tmp.path().join("settings.json");
    let before = r#"{"hooks":{"PreToolUse":[{"matcher":"*","hooks":[{"type":"command","command":"/usr/local/bin/tm hook --pm-guard"}]}]}}"#;
    std::fs::write(&existing, before).expect("seed settings");

    let refusal = Err(StableHookExeError::ForeignBinary(PathBuf::from(
        "/x/target-7224/debug/deps/test_session_lifecycle-cd3ba8f0",
    )));
    let err = write_project_hooks_with(&existing, refusal)
        .expect_err("a refusal must reach the caller, not be written");
    assert!(
        err.to_string().contains("test_session_lifecycle"),
        "the error must name the binary it refused, got: {err}"
    );
    assert_eq!(
        std::fs::read_to_string(&existing).expect("read back"),
        before,
        "an existing settings file must be byte-identical after a refusal"
    );

    let missing = tmp.path().join("absent").join("settings.json");
    let refusal = Err(StableHookExeError::Unresolved);
    write_project_hooks_with(&missing, refusal).expect_err("a refusal must reach the caller");
    assert!(
        !missing.exists() && !missing.parent().expect("parent").exists(),
        "a refusal must not create the settings file or its directory"
    );
}

#[test]
fn test_mpm_hook_additions_has_six_events() {
    // #1744: SessionStart/SessionEnd must be present so the daemon receives
    // them for claude_session_id capture and immediate-Stopped marking.
    // #2610: SubagentStop must be present so the hook handler sees a delegated
    // subagent's turn-end and can flag an idle-parking final message.
    // #7244: pinned rather than resolved — see `STABLE_TEST_EXE`.
    let v = mpm_hook_additions_with_exe(Some(Path::new(STABLE_TEST_EXE)))
        .expect("a pinned installed-looking exe always resolves");
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
    let v = mpm_hook_additions_with_exe(Some(&fake_exe))
        .expect("an installed-looking override resolves");
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

    ensure_managed_hooks_with_exe(&cfg, Some(std::path::Path::new("/usr/local/bin/tm"))).unwrap();

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

    ensure_managed_hooks_with_exe(&cfg, Some(std::path::Path::new("/usr/local/bin/tm"))).unwrap();
    let after_first = std::fs::read_to_string(cfg.join("settings.json")).unwrap();

    ensure_managed_hooks_with_exe(&cfg, Some(std::path::Path::new("/usr/local/bin/tm"))).unwrap();
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

    ensure_managed_hooks_with_exe(&cfg, Some(std::path::Path::new("/usr/local/bin/tm"))).unwrap();

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

/// Why (issue #2948): a hand-mixed group whose `hooks[]` array carries ONE
/// tm-owned entry alongside ONE foreign entry must have ONLY the tm entry
/// removed — the group (and the foreign entry) survive. The pre-fix `.all()`
/// group-level filter left the whole group untouched in this shape.
/// What: seeds a `PreToolUse` group with a tm entry + a foreign entry, calls
/// `strip_mpm_hook_entries`, and asserts the group still exists with exactly
/// the foreign entry remaining.
#[test]
fn test_strip_mpm_hook_entries_removes_only_tm_entry_from_mixed_group() {
    let mut val = serde_json::json!({
        "hooks": {
            "PreToolUse": [
                {
                    "matcher": "*",
                    "hooks": [
                        { "type": "command", "command": "trusty-mpm hook" },
                        { "type": "command", "command": "claude-mpm hooks fire PreToolUse" }
                    ]
                }
            ]
        }
    });

    let changed = strip_mpm_hook_entries(&mut val);
    assert!(changed, "must report change when the tm entry is stripped");

    let groups = val["hooks"]["PreToolUse"]
        .as_array()
        .expect("the mixed group must survive — it still carries a foreign entry");
    assert_eq!(groups.len(), 1, "the group must not be dropped wholesale");
    let inner = groups[0]["hooks"].as_array().unwrap();
    assert_eq!(inner.len(), 1, "only the tm entry must be removed");
    assert_eq!(
        inner[0]["command"].as_str().unwrap(),
        "claude-mpm hooks fire PreToolUse"
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

/// Why (#4058 review round 1 MEDIUM finding 2): `MPM_BIN_NAMES` intentionally
/// keeps its own array (order `trusty-mpm` then `tm`, load-bearing for
/// [`resolve_stable_hook_exe`]'s first-hit-wins PATH lookup) rather than
/// aliasing [`crate::core::own_binary_names::OWN_BINARY_NAMES`] (order `tm`
/// then `trusty-mpm`, load-bearing for a *different* consumer). Pinning the
/// SET (order-insensitive) here means a future third `[[bin]]` target added
/// to one array without the other trips this test instead of drifting silently.
/// What: asserts both arrays contain exactly the same two names, ignoring order.
#[test]
fn test_mpm_bin_names_matches_own_binary_names_set() {
    let mut mpm_bin_names: Vec<&str> = MPM_BIN_NAMES.to_vec();
    mpm_bin_names.sort_unstable();
    let mut own_binary_names: Vec<&str> = crate::core::own_binary_names::OWN_BINARY_NAMES.to_vec();
    own_binary_names.sort_unstable();
    assert_eq!(
        mpm_bin_names, own_binary_names,
        "MPM_BIN_NAMES and OWN_BINARY_NAMES must stay set-equal even though their order differs"
    );
}

/// Why (#4058 review round 1 MEDIUM finding 2): the SET test above cannot see
/// an order flip, and an order flip at this site is exactly what shipped
/// unnoticed in round 1 — [`resolve_stable_hook_exe`] consumes `MPM_BIN_NAMES`
/// via `.find_map(resolve_binary)`, so the FIRST entry that resolves on `PATH`
/// becomes the hook exe. `resolve_stable_hook_exe` calls the real
/// `resolve_binary` and is not injectable, so no behavioural test can observe
/// that preference; pinning the array's order is the only mechanical guard.
/// The order is `"trusty-mpm"` first deliberately: it is the unambiguous full
/// crate/binary name, whereas `"tm"` is a short alias a user may well have
/// shadowed on `PATH` with an unrelated tool, and a hook command is persisted
/// into `settings.json` where a wrong resolution survives across sessions.
/// What: asserts `MPM_BIN_NAMES` is exactly `["trusty-mpm", "tm"]`, in order.
#[test]
fn test_mpm_bin_names_prefers_full_name_over_short_alias() {
    assert_eq!(
        MPM_BIN_NAMES,
        &["trusty-mpm", "tm"],
        "resolve_stable_hook_exe takes the first PATH hit — 'trusty-mpm' must stay first"
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

/// Why (#2940 review round 1, MEDIUM): `is_mpm_hook_command` drives
/// `tm hooks clean --force`'s DESTRUCTIVE deletion path. Before this test's
/// fix, a foreign binary that happened to be named `<stem>-<hexhash>`
/// anywhere on disk (e.g. an unrelated tool at `/usr/local/bin/tm-a1b2c3d4`)
/// would be silently deleted from a project's settings even though it has
/// nothing to do with tm — `resolve_stable_hook_exe`/`current_exe()` can only
/// ever produce a hash-suffixed path under a Cargo `deps/` build-artifact
/// directory, so the predicate must never trust the hash-suffixed shape
/// outside one.
/// What: asserts a `<stem>-<hexhash>` command whose path has NO `deps`
/// component is rejected, while the same stem+hash under a `deps/` directory
/// (any depth) still matches.
#[test]
fn test_is_mpm_hook_command_rejects_hash_suffixed_binary_outside_deps_dir() {
    assert!(
        !is_mpm_hook_command("/usr/local/bin/tm-a1b2c3d4 hook"),
        "hash-suffixed binary outside any deps/ dir must NOT be treated as tm-owned"
    );
    assert!(
        !is_mpm_hook_command("/opt/foreign-tool/trusty_mpm-deadbeef hook"),
        "coincidental stem+hash match outside deps/ must NOT be treated as tm-owned"
    );
    // Sanity: the same hash-suffixed shape under ANY deps/ directory still matches.
    assert!(
        is_mpm_hook_command("/some/other/deps/tm-a1b2c3d4 hook"),
        "hash-suffixed binary under a deps/ dir must still match"
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

/// Plant a settings file at `path` carrying one tm hook group and one foreign
/// group under `PreToolUse`.
///
/// Why: both `remove_global_trusty_mpm_hooks_at` tests need the same shape — a
/// file where a correct strip is visibly distinguishable both from a wholesale
/// wipe and from no write at all.
/// What: creates the parent directory and writes the two-group JSON.
/// Test: used by `remove_global_hooks_at_strips_only_the_two_global_files` and
/// `remove_global_hooks_at_ignores_project_settings_below_home`.
fn plant_mixed_settings(path: &Path) {
    std::fs::create_dir_all(path.parent().expect("settings path has a parent"))
        .expect("settings parent dir creatable");
    let json = serde_json::json!({
        "hooks": {
            "PreToolUse": [
                { "hooks": [ { "type": "command", "command": "/opt/bin/trusty-mpm hook" } ] },
                { "hooks": [ { "type": "command", "command": "/usr/bin/other-tool run" } ] }
            ]
        }
    });
    std::fs::write(
        path,
        serde_json::to_string_pretty(&json).expect("json serialises"),
    )
    .expect("settings file writable");
}

/// Every `PreToolUse` command still present in the settings file at `path`.
fn pre_tool_use_commands(path: &Path) -> Vec<String> {
    let text = std::fs::read_to_string(path).expect("settings file readable");
    let val: serde_json::Value = serde_json::from_str(&text).expect("settings file parses");
    val["hooks"]["PreToolUse"]
        .as_array()
        .expect("PreToolUse is an array")
        .iter()
        .filter_map(|g| g["hooks"][0]["command"].as_str().map(str::to_string))
        .collect()
}

/// Why (#5875, #6070): the removal path must strip tm's own hook groups from
/// both global settings files and leave every foreign group in place.
/// What: plants a mixed file at `<home>/.claude/settings.json` and another at
/// `<home>/.claude/settings.local.json`, runs the strip against that home, and
/// asserts two files changed and only the foreign command survives in each.
#[test]
fn remove_global_hooks_at_strips_only_the_two_global_files() {
    let home = TempDir::new().expect("tempdir");
    let global = home.path().join(".claude").join("settings.json");
    let global_local = home.path().join(".claude").join("settings.local.json");
    plant_mixed_settings(&global);
    plant_mixed_settings(&global_local);

    let changed = remove_global_trusty_mpm_hooks_at(home.path()).expect("strip succeeds");

    assert_eq!(changed, 2, "both global settings files carried a tm group");
    for path in [&global, &global_local] {
        assert_eq!(
            pre_tool_use_commands(path),
            vec!["/usr/bin/other-tool run".to_string()],
            "only the foreign group may survive in {}",
            path.display()
        );
    }
}

/// Why (#5875): this is the regression test for the hang. Before the fix the
/// removal path reached its targets by a depth-8 recursive walk of the whole
/// home tree, so it rewrote every PROJECT settings file on the machine and paid
/// an unbounded scan to find them — the `opendir()` that blocked `tm launch`,
/// and with it the eight `guided_fallback_*` tests, forever. `tm hooks clean`
/// owns the machine-wide sweep (#2940); this path must not.
/// What: plants a project settings file three levels below `home` carrying the
/// same tm hook group as the global file, strips, and asserts the project file
/// is byte-identical afterwards while the global file was still cleaned.
#[test]
fn remove_global_hooks_at_ignores_project_settings_below_home() {
    let home = TempDir::new().expect("tempdir");
    let global = home.path().join(".claude").join("settings.json");
    let project = home
        .path()
        .join("code")
        .join("acme")
        .join("widget")
        .join(".claude")
        .join("settings.json");
    plant_mixed_settings(&global);
    plant_mixed_settings(&project);
    let project_before = std::fs::read_to_string(&project).expect("project settings readable");

    let changed = remove_global_trusty_mpm_hooks_at(home.path()).expect("strip succeeds");

    assert_eq!(changed, 1, "only the global settings file may be rewritten");
    assert_eq!(
        std::fs::read_to_string(&project).expect("project settings still readable"),
        project_before,
        "a project settings file below home must be left untouched: {}",
        project.display()
    );
    assert_eq!(
        pre_tool_use_commands(&global),
        vec!["/usr/bin/other-tool run".to_string()],
        "the global file must still have its tm group stripped"
    );
}

/// Why (#5875): a home with no `.claude` directory is the common case on a
/// machine that never carried legacy global hooks. `launch()` calls this on
/// every session start, so it must be a silent no-op rather than an error.
/// What: strips against an empty tempdir and asserts `Ok(0)`.
#[test]
fn remove_global_hooks_at_reports_zero_when_no_global_settings_exist() {
    let home = TempDir::new().expect("tempdir");
    assert_eq!(
        remove_global_trusty_mpm_hooks_at(home.path()).expect("strip succeeds"),
        0
    );
}

/// Every timestamped snapshot of `settings.json` in `dir`, name-sorted.
///
/// Deliberately narrower than "every `.bak`": the atomic writer keeps its own
/// single-slot `settings.json.bak`, and counting that as a snapshot would make
/// the no-snapshot assertions below pass for the wrong reason.
fn snapshot_names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .expect("readable dir")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| backup::is_snapshot_of("settings.json", n))
        .collect();
    names.sort();
    names
}

/// Why (#7244, round 3): the incident's only recovery path was reconstructing
/// `.claude/settings.json` by hand, because the writer that broke it also
/// consumed the one `.bak` slot the atomic writer keeps. A snapshot taken
/// before the replacing rename is what makes the prior state readable
/// afterwards.
/// What: writes once to create the file, rewrites with a different toggle, and
/// asserts exactly one snapshot exists whose bytes are the pre-rewrite file's.
#[test]
fn write_project_hooks_snapshots_the_file_it_replaces() {
    let tmp = TempDir::new().expect("tempdir");
    let settings = tmp.path().join("settings.json");

    write_project_hooks(&settings, Some(Path::new(STABLE_TEST_EXE))).expect("first write");
    let before = std::fs::read_to_string(&settings).expect("read back");
    assert!(
        snapshot_names(tmp.path()).is_empty(),
        "the file did not exist before the first write, so nothing was snapshotted"
    );

    // A different exe path is what makes the merged value differ, so the
    // no-op exit does not swallow this rewrite.
    write_project_hooks(&settings, Some(Path::new("/opt/homebrew/bin/tm"))).expect("second write");

    let snapshots = snapshot_names(tmp.path());
    assert_eq!(
        snapshots.len(),
        1,
        "expected one snapshot, got {snapshots:?}"
    );
    assert_eq!(
        std::fs::read_to_string(tmp.path().join(&snapshots[0])).expect("read snapshot"),
        before,
        "the snapshot must hold the file as it was before the rewrite"
    );
    assert_ne!(
        std::fs::read_to_string(&settings).expect("read back"),
        before,
        "the rewrite itself must still have happened"
    );
}

/// Why (#7244): a refusal writes nothing, so there is nothing to preserve. A
/// snapshot taken anyway would push a real prior state out of the kept three
/// on a machine whose `tm` cannot be resolved — every launch adding a copy of
/// a file no launch is changing.
/// What: hands the writer a refusal over an existing file and asserts no
/// snapshot appeared.
#[test]
fn write_project_hooks_takes_no_snapshot_when_the_exe_is_refused() {
    let tmp = TempDir::new().expect("tempdir");
    let settings = tmp.path().join("settings.json");
    std::fs::write(&settings, "{}").expect("seed settings");

    let refusal = Err(StableHookExeError::Unresolved);
    write_project_hooks_with(&settings, refusal).expect_err("a refusal must reach the caller");

    assert!(
        snapshot_names(tmp.path()).is_empty(),
        "a refused write must take no snapshot"
    );
}

/// Why (#7244): `ensure_managed_hooks` runs on every managed launch and almost
/// always produces the same bytes. Snapshotting an unchanged file would fill
/// the three kept slots with copies of the current state, evicting the one
/// prior state worth keeping.
/// What: writes twice with identical arguments and asserts the second call
/// reports no change and left no snapshot.
#[test]
fn write_project_hooks_takes_no_snapshot_when_nothing_changes() {
    let tmp = TempDir::new().expect("tempdir");
    let settings = tmp.path().join("settings.json");

    assert!(
        write_project_hooks(&settings, Some(Path::new(STABLE_TEST_EXE))).expect("first write"),
        "the first write creates the file"
    );
    assert!(
        !write_project_hooks(&settings, Some(Path::new(STABLE_TEST_EXE))).expect("second write"),
        "an identical rewrite must report no change"
    );
    assert!(
        snapshot_names(tmp.path()).is_empty(),
        "a no-op rewrite must take no snapshot"
    );
}

/// Why (#7244, fail-closed): if the prior state cannot be preserved, the
/// rewrite that would destroy it must not run. The alternative — write anyway,
/// warn about the snapshot — is exactly the shape of the original bug: a
/// writer proceeding past a step it could not complete.
/// What: makes the settings directory unwritable so the snapshot's exclusive
/// create fails, then asserts the error names the snapshot stage (not the
/// write, which never ran) and the file is byte-identical. Unix-only: the
/// read-only directory bit is the portable way to deny file creation.
#[cfg(unix)]
#[test]
fn write_project_hooks_aborts_the_rewrite_when_the_snapshot_fails() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = TempDir::new().expect("tempdir");
    let dir = tmp.path().join("claude");
    std::fs::create_dir(&dir).expect("create dir");
    let settings = dir.join("settings.json");

    write_project_hooks(&settings, Some(Path::new(STABLE_TEST_EXE))).expect("first write");
    let before = std::fs::read_to_string(&settings).expect("read back");

    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).expect("chmod");
    let result = write_project_hooks(&settings, Some(Path::new("/opt/homebrew/bin/tm")));
    // Restore before asserting, so a failed assertion still leaves a removable
    // temp dir behind.
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).expect("chmod back");

    let err = result.expect_err("an unsnapshottable rewrite must fail");
    assert!(
        err.to_string().contains("snapshot"),
        "the error must name the stage that refused, got: {err}"
    );
    assert_eq!(
        std::fs::read_to_string(&settings).expect("read back"),
        before,
        "the settings file must be untouched when its snapshot could not be taken"
    );
}
