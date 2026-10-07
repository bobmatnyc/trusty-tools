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
//! `install_writes_all_artifacts` in `tests_behavior_a_tests.rs` and by
//! `overwrite_artifact_refreshes_modified_file_without_force` /
//! `seed_once_artifact_is_not_clobbered_without_force` /
//! `seed_once_artifact_force_resets_to_shipped_default` in
//! `install_policy_tests.rs`, unchanged by this issue.

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

    let changed = install_claude_hooks_at(
        &config_dir,
        Some(std::path::PathBuf::from(
            crate::test_support::STABLE_HOOK_EXE,
        )),
        false,
    )
    .unwrap();
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

    let first = install_claude_hooks_at(
        &config_dir,
        Some(std::path::PathBuf::from(
            crate::test_support::STABLE_HOOK_EXE,
        )),
        false,
    )
    .unwrap();
    assert_eq!(first, 1);
    let settings_path = config_dir.join("settings.json");
    let after_first = std::fs::read_to_string(&settings_path).unwrap();

    let second = install_claude_hooks_at(
        &config_dir,
        Some(std::path::PathBuf::from(
            crate::test_support::STABLE_HOOK_EXE,
        )),
        false,
    )
    .unwrap();
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
/// unrelated content; calls `install_claude_hooks_at`; asserts
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

    install_claude_hooks_at(
        &config_dir,
        Some(std::path::PathBuf::from(
            crate::test_support::STABLE_HOOK_EXE,
        )),
        false,
    )
    .unwrap();

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

/// The event keys `tm install` left in `<config_dir>/settings.json`, sorted.
fn installed_events(config_dir: &std::path::Path) -> Vec<String> {
    let text = std::fs::read_to_string(config_dir.join("settings.json")).unwrap();
    let val: serde_json::Value = serde_json::from_str(&text).unwrap();
    let mut events: Vec<String> = val["hooks"].as_object().unwrap().keys().cloned().collect();
    events.sort();
    events
}

/// #8392: with the opt-in off (the default) `tm install` writes exactly the six
/// lifecycle events — no `Notification` key at all.
#[test]
fn opt_in_off_installs_exactly_the_six_lifecycle_events() {
    let tmp = tempfile::tempdir().unwrap();
    let exe = Some(std::path::PathBuf::from(
        crate::test_support::STABLE_HOOK_EXE,
    ));
    install_claude_hooks_at(tmp.path(), exe, false).unwrap();
    let mut expected = vec![
        "PostToolUse",
        "PreToolUse",
        "SessionEnd",
        "SessionStart",
        "Stop",
        "SubagentStop",
    ];
    expected.sort_unstable();
    assert_eq!(installed_events(tmp.path()), expected);
}

/// #8392: with the opt-in on, `tm install` adds one `Notification` entry that
/// runs `tm hook`; a second run changes nothing and leaves one entry.
#[test]
fn opt_in_on_installs_one_notification_entry_idempotently() {
    let tmp = tempfile::tempdir().unwrap();
    let exe = || {
        Some(std::path::PathBuf::from(
            crate::test_support::STABLE_HOOK_EXE,
        ))
    };
    assert_eq!(install_claude_hooks_at(tmp.path(), exe(), true).unwrap(), 1);
    let first = std::fs::read(tmp.path().join("settings.json")).unwrap();
    assert_eq!(install_claude_hooks_at(tmp.path(), exe(), true).unwrap(), 0);
    assert_eq!(
        std::fs::read(tmp.path().join("settings.json")).unwrap(),
        first
    );

    assert!(installed_events(tmp.path()).contains(&"Notification".to_string()));
    let val: serde_json::Value = serde_json::from_slice(&first).unwrap();
    let groups = val["hooks"]["Notification"].as_array().unwrap();
    assert_eq!(groups.len(), 1, "exactly one Notification group");
    assert_eq!(
        groups[0]["hooks"][0]["command"],
        format!("{} hook", crate::test_support::STABLE_HOOK_EXE)
    );
}

/// Why (#9018): with `[pm_guard] enabled = false`, `tm install` must take a
/// guard entry out of the settings file it writes, not leave it to fire.
/// What: seeds a guard entry and a foreign entry, installs with the guard off,
/// and asserts the guard is gone while the lifecycle `tm hook` group and the
/// foreign entry remain.
#[test]
fn install_with_the_guard_off_strips_an_existing_guard_entry() {
    let tmp = tempfile::tempdir().unwrap();
    let guard = format!("{} hook --pm-guard", crate::test_support::STABLE_HOOK_EXE);
    let seeded = serde_json::json!({
        "hooks": { "PreToolUse": [
            { "matcher": "", "hooks": [{ "type": "command", "command": guard }] },
            { "matcher": "Bash", "hooks": [{ "type": "command", "command": "/opt/foreign/x" }] }
        ] }
    });
    std::fs::write(tmp.path().join("settings.json"), seeded.to_string()).unwrap();

    let exe = Some(std::path::PathBuf::from(
        crate::test_support::STABLE_HOOK_EXE,
    ));
    assert_eq!(
        install_claude_hooks_at_with_pm_guard(tmp.path(), exe, false, false).unwrap(),
        1
    );

    let val: serde_json::Value =
        serde_json::from_slice(&std::fs::read(tmp.path().join("settings.json")).unwrap()).unwrap();
    let commands: Vec<&str> = val["hooks"]["PreToolUse"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|g| g["hooks"][0]["command"].as_str())
        .collect();
    assert!(!commands.contains(&guard.as_str()), "{commands:?}");
    assert!(commands.contains(&"/opt/foreign/x"), "{commands:?}");
    assert!(
        commands.iter().any(|c| c.ends_with(" hook")),
        "{commands:?}"
    );
}

/// #9012: with a roster but no skills and instructions resolvable, `tm
/// install` fails before writing anything, and the error names `tm content
/// install`. The roster resolves, so the content arm is the one refusing.
#[test]
fn install_without_content_fails_naming_tm_content_install() {
    use trusty_mpm::core::content_source::DevOverride;

    let cache = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let paths = trusty_mpm::core::paths::FrameworkPaths::under(home.path());
    let err = install_to_resolving(
        &paths,
        false,
        || Ok(test_roster()),
        || trusty_mpm::core::content_source::framework_content_in(cache.path(), DevOverride::Off),
    )
    .expect_err("no content installed");
    assert!(err.to_string().contains("tm content install"), "{err}");
    assert!(err.to_string().contains("tm content update"), "{err}");
    assert!(
        !paths.framework.exists(),
        "a failed install must leave the framework tree unwritten"
    );
}

/// #9396: the `tm install` gate fetches on first use; when that fetch fails
/// it fails closed — nothing written, nothing pinned — naming `tm content
/// update` and the offline `--from` install.
#[test]
fn install_with_a_failed_first_use_fetch_fails_closed() {
    use trusty_mpm::content::bundle_cache::{CacheError, Fallback};
    use trusty_mpm::content::first_use::resolve_or_fetch_in;
    use trusty_mpm::core::content_source::{AgentRoster, DevOverride};

    let cache = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let paths = trusty_mpm::core::paths::FrameworkPaths::under(home.path());
    let mut fetches = 0;
    let err = install_to_resolving(
        &paths,
        false,
        || {
            let content = resolve_or_fetch_in(cache.path(), DevOverride::Off, |_| {
                fetches += 1;
                Err(CacheError::Network {
                    url: "https://api.github.com".to_owned(),
                    reason: "network is unreachable".to_owned(),
                    fallback: Fallback::None,
                })
            })?;
            AgentRoster::load(&content)
        },
        || unreachable!("the roster gate refuses first"),
    )
    .expect_err("the fetch failed");
    let msg = err.to_string();
    assert_eq!(fetches, 1, "one fetch attempt");
    assert!(msg.contains("network is unreachable"), "{msg}");
    assert!(msg.contains("run `tm content update`"), "{msg}");
    assert!(msg.contains("tm content install --from"), "{msg}");
    assert!(!paths.framework.exists(), "nothing written");
    assert!(
        !cache
            .path()
            .join(trusty_common::content::LOCK_FILE_NAME)
            .exists()
    );
}

/// #9012: a content source without the bundled docs fails `tm install`,
/// naming the missing doc, before any framework file is written.
#[test]
fn install_without_the_bundled_docs_fails_naming_the_doc() {
    use trusty_mpm::core::framework_content::REQUIRED_INSTRUCTIONS;

    let checkout = tempfile::tempdir().unwrap();
    let root = checkout.path();
    for (_, rel) in trusty_common::content::DEV_CLASS_SOURCES {
        std::fs::create_dir_all(root.join(rel)).unwrap();
    }
    std::fs::create_dir_all(root.join(".git")).unwrap();
    std::fs::write(root.join("Cargo.toml"), "[workspace]\n").unwrap();
    // Every required instruction and one skill, but no `docs/`.
    for rel in REQUIRED_INSTRUCTIONS {
        let path = root.join("content/instructions").join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, test_content_ref().required(rel)).unwrap();
    }
    std::fs::write(root.join("content/skills/tm.md"), "skill").unwrap();
    let content = trusty_mpm::core::content_source::FrameworkContent::load(
        &trusty_agents_common::agent_content::checkout_content(root).unwrap(),
    )
    .expect("a source without docs still loads");

    let home = tempfile::tempdir().unwrap();
    let paths = trusty_mpm::core::paths::FrameworkPaths::under(home.path());
    let err =
        install_to_with(&paths, false, test_roster_ref(), &content).expect_err("no bundled docs");
    assert!(
        err.to_string()
            .contains("instructions/docs/WHAT-IS-TRUSTY-MPM.md"),
        "{err}"
    );
    assert!(
        !paths.framework.exists(),
        "a failed install must leave the framework tree unwritten"
    );
}
