//! Tests for the #7849 toggle-driven hook-group diff.
//!
//! Why: the incident was a settings file that carried every base group and
//! neither `--prompt-feedback` group, in a project whose committed flag said
//! the capture was on. Every test here is written against that shape, plus the
//! two ways the diff must stay quiet — a file tm's project tier never wrote,
//! and a user-tier file that only ever carried the lifecycle triad.
//! What: the expected-set resolution, the pure group comparison, and the two
//! quiet arms.
//! Test: this file IS the test suite.

use super::*;
use crate::core::paths::FrameworkPaths;
use crate::test_support::STABLE_HOOK_EXE;

/// A hermetic project whose committed flag is stated, never inherited.
///
/// Why: `prompt_self_improvement::enabled_for` falls back to the operator's
/// `~/.trusty-mpm/config.toml`, so a fixture that omitted the key would read
/// the host. The project layer outranks it, so stating it here removes the
/// `$HOME` dependency in both directions.
/// What: `(fw, workspace)` with `.trusty-mpm.toml` carrying the flag.
fn project_with_flag(base: &std::path::Path, on: bool) -> (FrameworkPaths, std::path::PathBuf) {
    let workspace = base.join("workspace");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    std::fs::write(
        workspace.join(crate::core::project_config::PROJECT_CONFIG_FILE),
        format!("prompt_self_improvement = {on}\n"),
    )
    .expect("write project config");
    let mut fw = FrameworkPaths::for_managed_project(base, &workspace);
    fw.trusty_mpm_root = None;
    (fw, workspace)
}

/// The settings shape the launch-path writer produces, as a plain value.
fn settings_from(additions: &Value) -> Value {
    serde_json::json!({ "hooks": additions["hooks"].clone() })
}

/// Every `--prompt-feedback` command in `val`, sorted by event.
fn capture_events(val: &Value) -> Vec<String> {
    let mut events = event_names_matching(val, |c| c.ends_with(" hook --prompt-feedback"));
    events.sort();
    events
}

#[test]
fn expected_additions_carry_the_capture_when_the_flag_is_on() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (fw, workspace) = project_with_flag(tmp.path(), true);

    let additions =
        project_hook_additions_for(&fw, &workspace, Some(std::path::Path::new(STABLE_HOOK_EXE)))
            .expect("a pinned installed binary resolves");

    assert_eq!(
        capture_events(&settings_from(&additions)),
        vec!["Stop".to_string(), "SubagentStop".to_string()],
        "the committed flag must reach the additions builder"
    );
}

#[test]
fn expected_additions_omit_the_capture_when_the_flag_is_off() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (fw, workspace) = project_with_flag(tmp.path(), false);

    let additions =
        project_hook_additions_for(&fw, &workspace, Some(std::path::Path::new(STABLE_HOOK_EXE)))
            .expect("a pinned installed binary resolves");

    assert!(
        capture_events(&settings_from(&additions)).is_empty(),
        "the flag is off, so no capture group may be asked for"
    );
}

#[test]
fn diff_reports_nothing_for_an_exact_match() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (fw, workspace) = project_with_flag(tmp.path(), true);
    let additions =
        project_hook_additions_for(&fw, &workspace, Some(std::path::Path::new(STABLE_HOOK_EXE)))
            .expect("resolve");

    let gaps = diff_hook_groups(&additions, &settings_from(&additions));

    assert!(gaps.is_empty(), "an exact match has no gaps: {gaps:?}");
}

#[test]
fn diff_reports_a_group_the_file_lacks() {
    let expected = serde_json::json!({
        "hooks": {
            "Stop": [{
                "matcher": "",
                "hooks": [{ "type": "command", "command": "/usr/local/bin/tm hook --prompt-feedback" }]
            }]
        }
    });
    let actual = serde_json::json!({ "hooks": { "Stop": [] } });

    let gaps = diff_hook_groups(&expected, &actual);

    assert_eq!(
        gaps.missing,
        vec![(
            "Stop".to_string(),
            "/usr/local/bin/tm hook --prompt-feedback".to_string()
        )]
    );
    assert!(gaps.stale.is_empty(), "nothing is stale here: {gaps:?}");
}

#[test]
fn diff_reports_a_tm_owned_command_the_config_no_longer_asks_for() {
    let expected = serde_json::json!({ "hooks": {} });
    let actual = serde_json::json!({
        "hooks": {
            "Stop": [{
                "matcher": "",
                "hooks": [
                    { "type": "command", "command": "/usr/local/bin/tm hook --prompt-feedback" },
                    { "type": "command", "command": "/opt/other-harness/run --stop" }
                ]
            }]
        }
    });

    let gaps = diff_hook_groups(&expected, &actual);

    assert_eq!(
        gaps.stale,
        vec![(
            "Stop".to_string(),
            "/usr/local/bin/tm hook --prompt-feedback".to_string()
        )],
        "only the tm-owned command is stale; the foreign one is never counted"
    );
}

/// The same hook under a different binary path is not a gap.
///
/// Why (#7849): this crate installs two binaries, `tm` and `trusty-mpm`, and
/// `resolve_stable_hook_exe` answers with whichever one is running. Comparing
/// the whole command string would make the daemon call every group a `tm`
/// launch wrote stale, and each would rewrite what the other wrote on every
/// launch.
/// What: the same argv under two absolute paths compares equal.
/// Test: itself.
#[test]
fn diff_ignores_a_different_binary_path_for_the_same_hook() {
    let expected = serde_json::json!({
        "hooks": {
            "Stop": [{
                "matcher": "",
                "hooks": [{ "type": "command", "command": "/a/bin/tm hook --prompt-feedback" }]
            }]
        }
    });
    let actual = serde_json::json!({
        "hooks": {
            "Stop": [{
                "matcher": "",
                "hooks": [{ "type": "command", "command": "/b/bin/trusty-mpm hook --prompt-feedback" }]
            }]
        }
    });

    let gaps = diff_hook_groups(&expected, &actual);

    assert!(
        gaps.is_empty(),
        "the same hook under another path is the same hook: {gaps:?}"
    );
}

#[test]
fn a_file_tm_never_provisioned_has_no_gaps() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (fw, workspace) = project_with_flag(tmp.path(), true);
    let foreign = serde_json::json!({
        "hooks": {
            "Stop": [{
                "matcher": "",
                "hooks": [{ "type": "command", "command": "/opt/other-harness/run --stop" }]
            }]
        }
    });

    let gaps = project_hook_group_gaps(
        &fw,
        &workspace,
        &foreign,
        Some(std::path::Path::new(STABLE_HOOK_EXE)),
    );

    assert!(
        gaps.is_empty(),
        "a project tm never provisioned owes tm no hook group: {gaps:?}"
    );
}

#[test]
fn a_user_tier_file_has_no_project_tier_gaps() {
    // The user tier carries the lifecycle triad and nothing else — no PM guard,
    // no `trusty-memory` block. Reporting the project-tier set missing from it
    // would make `tm doctor` fail on every machine.
    let tmp = tempfile::tempdir().expect("tempdir");
    let (fw, workspace) = project_with_flag(tmp.path(), true);
    let user_tier = serde_json::json!({
        "hooks": {
            "SessionStart": [{
                "matcher": "",
                "hooks": [{ "type": "command", "command": format!("{STABLE_HOOK_EXE} hook") }]
            }]
        }
    });

    let gaps = project_hook_group_gaps(
        &fw,
        &workspace,
        &user_tier,
        Some(std::path::Path::new(STABLE_HOOK_EXE)),
    );

    assert!(
        gaps.is_empty(),
        "the user tier is not the project tier: {gaps:?}"
    );
}

#[test]
fn a_flipped_on_flag_is_a_missing_group() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let exe = Some(std::path::Path::new(STABLE_HOOK_EXE));

    // Written while the flag was off...
    let (off_fw, workspace) = project_with_flag(tmp.path(), false);
    let written = settings_from(
        &project_hook_additions_for(&off_fw, &workspace, exe).expect("resolve with the flag off"),
    );

    // ...then the committed flag flips on.
    std::fs::write(
        workspace.join(crate::core::project_config::PROJECT_CONFIG_FILE),
        "prompt_self_improvement = true\n",
    )
    .expect("flip the flag");

    let gaps = project_hook_group_gaps(&off_fw, &workspace, &written, exe);

    let events: Vec<&str> = gaps.missing.iter().map(|(e, _)| e.as_str()).collect();
    assert_eq!(
        events,
        vec!["Stop", "SubagentStop"],
        "exactly the two capture groups must be reported: {gaps:?}"
    );
    assert!(gaps.stale.is_empty(), "nothing is stale: {gaps:?}");
}

#[test]
fn a_flipped_off_flag_is_a_stale_group() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let exe = Some(std::path::Path::new(STABLE_HOOK_EXE));

    let (on_fw, workspace) = project_with_flag(tmp.path(), true);
    let written = settings_from(
        &project_hook_additions_for(&on_fw, &workspace, exe).expect("resolve with the flag on"),
    );

    std::fs::write(
        workspace.join(crate::core::project_config::PROJECT_CONFIG_FILE),
        "prompt_self_improvement = false\n",
    )
    .expect("flip the flag");

    let gaps = project_hook_group_gaps(&on_fw, &workspace, &written, exe);

    let events: Vec<&str> = gaps.stale.iter().map(|(e, _)| e.as_str()).collect();
    assert_eq!(
        events,
        vec!["Stop", "SubagentStop"],
        "the groups the flag no longer asks for must be reported: {gaps:?}"
    );
    assert!(gaps.missing.is_empty(), "nothing is missing: {gaps:?}");
}
