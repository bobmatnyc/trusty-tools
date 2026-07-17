//! Unit tests for `commands::install` — split out of `install.rs` (test-file
//! budget: 1500 SLOC).
//!
//! Why: issue #2940 rewrote `install_claude_hooks` to write ONLY into the
//! tm-owned managed `CLAUDE_CONFIG_DIR` instead of walking `$HOME` and
//! mutating every discovered project's `.claude/settings.json`. These tests
//! are the regression guard for that fix — they must fail loudly if a future
//! change reintroduces a project-directory write.
//! What: exercises `install_claude_hooks_at` (the hermetic worker) against
//! a `tempfile::TempDir` standing in for the tm-owned config dir, plus a
//! sibling "project" directory that must never be touched.
//! Test: this module IS the test suite for the hook-installation half of
//! `commands::install`. The artifact-deploy half (`install_to`) is covered by
//! `install_writes_all_artifacts` / `install_skips_existing_without_force` in
//! `tests_behavior_a.rs`, unchanged by this issue.

use super::*;

/// Why (issue #2940): the whole point of the fix — `tm install` must write
/// the MPM hook triad into `<config_dir>/settings.json` and nowhere else.
/// What: calls `install_claude_hooks_at` against a temp "managed config dir"
/// and asserts the triad landed there with an absolute-path command.
#[test]
fn install_claude_hooks_at_writes_only_the_managed_config_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let config_dir = tmp.path().join("claude-config");
    std::fs::create_dir_all(&config_dir).unwrap();

    let changed = install_claude_hooks_at(&config_dir).unwrap();
    assert_eq!(changed, 1, "first install must report one file changed");

    let settings_path = config_dir.join("settings.json");
    let val: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
    let hooks = val.get("hooks").expect("hooks key must be present");
    assert!(hooks.get("PreToolUse").is_some());
    assert!(hooks.get("PostToolUse").is_some());
    assert!(hooks.get("Stop").is_some());
    let cmd = hooks["PreToolUse"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap();
    assert!(cmd.ends_with(" hook"), "got: {cmd:?}");
}

/// Why: running `tm install` twice must produce a byte-identical file — the
/// documented idempotency requirement.
/// What: calls `install_claude_hooks_at` twice against the same temp config
/// dir and asserts the second call reports zero files changed and the file
/// content is unchanged.
#[test]
fn install_claude_hooks_at_is_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    let config_dir = tmp.path().join("claude-config");
    std::fs::create_dir_all(&config_dir).unwrap();

    let first = install_claude_hooks_at(&config_dir).unwrap();
    assert_eq!(first, 1);
    let settings_path = config_dir.join("settings.json");
    let after_first = std::fs::read_to_string(&settings_path).unwrap();

    let second = install_claude_hooks_at(&config_dir).unwrap();
    assert_eq!(second, 0, "second install must report no changes");
    let after_second = std::fs::read_to_string(&settings_path).unwrap();
    assert_eq!(
        after_first, after_second,
        "re-running install must not rewrite an already-current file"
    );
}

/// Why (issue #2940, the core regression guard): the pre-fix
/// `install_claude_hooks` discovered and mutated every `.claude/settings.json`
/// under `$HOME`, contaminating unrelated projects. `install_claude_hooks_at`
/// must now ONLY ever touch the exact `config_dir` it is given — a sibling
/// "project" directory living right next to it (as any of the operator's
/// other checkouts under `$HOME` would) must come out byte-for-byte
/// unchanged.
/// What: builds a temp tree with `claude-config/` (the managed config dir)
/// and a sibling `some-other-project/.claude/settings.json` seeded with
/// unrelated content; calls `install_claude_hooks_at(&config_dir)`; asserts
/// the sibling project's settings file is untouched and carries no `hooks`
/// key.
#[test]
fn install_claude_hooks_at_never_touches_a_sibling_project_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let config_dir = tmp.path().join("claude-config");
    std::fs::create_dir_all(&config_dir).unwrap();

    let sibling_project = tmp.path().join("some-other-project");
    let sibling_settings = sibling_project.join(".claude").join("settings.json");
    std::fs::create_dir_all(sibling_settings.parent().unwrap()).unwrap();
    let sibling_original = r#"{"outputStyle":"claude-mpm"}"#;
    std::fs::write(&sibling_settings, sibling_original).unwrap();

    install_claude_hooks_at(&config_dir).unwrap();

    let sibling_after = std::fs::read_to_string(&sibling_settings).unwrap();
    assert_eq!(
        sibling_after, sibling_original,
        "a sibling project's settings.json must be byte-for-byte unchanged \
         by `tm install` — hooks belong solely in the tm-owned config dir"
    );
    let sibling_val: serde_json::Value = serde_json::from_str(&sibling_after).unwrap();
    assert!(
        sibling_val.get("hooks").is_none(),
        "the sibling project must never gain a `hooks` key from `tm install`"
    );
}
