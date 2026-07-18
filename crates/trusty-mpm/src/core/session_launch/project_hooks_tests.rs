//! Unit tests for [`super`] (project-tier trusty-owned hook combination,
//! issue #2003).
//!
//! Why: split out of `project_hooks.rs` to keep the production file focused,
//! mirroring the `hooks/{mod.rs,tests.rs}` split already used elsewhere in
//! this crate.
//! What: exercises `project_managed_hook_additions` (all three sources
//! combine, stable across repeated calls) and
//! `is_project_managed_hook_command` (recognises all three sources, rejects
//! foreign commands), plus end-to-end coverage of
//! `super::super::settings::write_project_hooks` proving the daemon
//! managed-launch path now writes the full lifecycle triad AND preserves a
//! pre-existing foreign hook.
//! Test: this module IS the test suite for `super`.

use super::*;
use tempfile::TempDir;

#[test]
fn project_managed_hook_additions_combines_all_three_sources() {
    let additions = project_managed_hook_additions();
    let hooks = additions["hooks"]
        .as_object()
        .expect("hooks must be an object");

    // trusty-memory source.
    assert_eq!(
        hooks["UserPromptSubmit"][0]["hooks"][0]["command"],
        serde_json::json!("trusty-memory prompt-context")
    );

    // PreToolUse must carry BOTH the PM-guard group and the lifecycle-triad
    // group — neither source may clobber the other.
    let pre = hooks["PreToolUse"]
        .as_array()
        .expect("PreToolUse must be an array");
    assert_eq!(pre.len(), 2, "PreToolUse must carry guard + triad groups");
    let pre_commands: Vec<&str> = pre
        .iter()
        .map(|g| g["hooks"][0]["command"].as_str().unwrap())
        .collect();
    assert!(
        pre_commands.iter().any(|c| c.ends_with(" hook --pm-guard")),
        "PM-guard command must be present: {pre_commands:?}"
    );
    assert!(
        pre_commands.iter().any(|c| c.ends_with(" hook")),
        "lifecycle-triad command must be present: {pre_commands:?}"
    );

    // SessionStart must carry BOTH trusty-memory and the lifecycle triad.
    let session_start = hooks["SessionStart"]
        .as_array()
        .expect("SessionStart must be an array");
    assert_eq!(session_start.len(), 2);

    // Lifecycle-triad-only events (no trusty-memory / PM-guard overlap) must
    // still be present.
    for event in ["PostToolUse", "Stop", "SubagentStop", "SessionEnd"] {
        assert!(
            hooks
                .get(event)
                .and_then(serde_json::Value::as_array)
                .is_some_and(|a| !a.is_empty()),
            "{event} must carry the lifecycle-triad group"
        );
    }
}

#[test]
fn project_managed_hook_additions_is_stable_across_calls() {
    // Why: two independent calls must produce byte-identical output so the
    // caller's merge is idempotent across repeated `write_project_hooks` runs.
    let first = project_managed_hook_additions();
    let second = project_managed_hook_additions();
    assert_eq!(first, second);
}

#[test]
fn is_project_managed_hook_command_recognises_all_three_sources() {
    assert!(is_project_managed_hook_command("trusty-mpm hook"));
    assert!(is_project_managed_hook_command("/opt/bin/tm hook"));
    assert!(is_project_managed_hook_command(
        "trusty-memory prompt-context"
    ));
    assert!(is_project_managed_hook_command("trusty-memory inbox-check"));
    assert!(is_project_managed_hook_command(
        "/opt/bin/tm hook --pm-guard"
    ));
    assert!(
        !is_project_managed_hook_command("claude-mpm hooks fire PreToolUse"),
        "a foreign command must never be recognised as project-managed"
    );
    assert!(
        !is_project_managed_hook_command("some-other-tool run"),
        "an unrelated command must never be recognised as project-managed"
    );
}

/// Why (issue #2003): this is the regression test for the bug report — a
/// managed session launched via `write_project_hooks` (the daemon path) must
/// end up with the lifecycle triad in the PROJECT-tier settings file, since
/// `--setting-sources project,local` never loads the user-tier file
/// `ensure_managed_hooks` provisions.
/// What: calls `write_project_hooks` and asserts all six triad events are
/// present with a `... hook` command.
#[test]
fn write_project_hooks_writes_lifecycle_triad() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path();

    super::super::settings::write_project_hooks(project).expect("write succeeds");

    let value: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(project.join(".claude").join("settings.json")).unwrap(),
    )
    .unwrap();
    let hooks = value["hooks"].as_object().expect("hooks must be an object");
    for event in [
        "PreToolUse",
        "PostToolUse",
        "Stop",
        "SubagentStop",
        "SessionStart",
        "SessionEnd",
    ] {
        let groups = hooks
            .get(event)
            .and_then(serde_json::Value::as_array)
            .unwrap_or_else(|| panic!("{event} must be present"));
        assert!(
            groups.iter().any(|g| g["hooks"][0]["command"]
                .as_str()
                .is_some_and(|c| c.ends_with(" hook"))),
            "{event} must carry the lifecycle-triad `... hook` command"
        );
    }
}

/// Why (issue #2003, consistent with #2948's entry-level guarantee): a
/// project's own pre-existing, genuinely foreign hook must survive
/// `write_project_hooks` — the old REPLACE-the-whole-`hooks`-key behaviour
/// clobbered it.
/// What: seeds `settings.json` with a foreign `PreToolUse` group, calls
/// `write_project_hooks`, and asserts the foreign group is still present
/// alongside the newly-added PM-guard/triad groups.
#[test]
fn write_project_hooks_preserves_foreign_hooks() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path();
    let claude_dir = project.join(".claude");
    std::fs::create_dir_all(&claude_dir).unwrap();
    std::fs::write(
        claude_dir.join("settings.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "hooks": {
                "PreToolUse": [{
                    "matcher": "*",
                    "hooks": [{ "type": "command", "command": "claude-mpm hooks fire PreToolUse", "timeout": 5 }]
                }]
            }
        }))
        .unwrap(),
    )
    .unwrap();

    super::super::settings::write_project_hooks(project).expect("write succeeds");

    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(claude_dir.join("settings.json")).unwrap())
            .unwrap();
    let pre = value["hooks"]["PreToolUse"]
        .as_array()
        .expect("PreToolUse must be an array");
    let commands: Vec<&str> = pre
        .iter()
        .map(|g| g["hooks"][0]["command"].as_str().unwrap())
        .collect();
    assert!(
        commands.contains(&"claude-mpm hooks fire PreToolUse"),
        "the foreign entry must survive: {commands:?}"
    );
    assert!(
        commands.iter().any(|c| c.ends_with(" hook --pm-guard")),
        "the PM-guard entry must also be present: {commands:?}"
    );

    // Re-running must not duplicate the foreign entry or our own groups.
    super::super::settings::write_project_hooks(project).expect("second write succeeds");
    let value2: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(claude_dir.join("settings.json")).unwrap())
            .unwrap();
    let pre2 = value2["hooks"]["PreToolUse"].as_array().unwrap();
    assert_eq!(
        pre2.len(),
        pre.len(),
        "re-running must not duplicate any group"
    );
}

/// Why (issue #2972): the final write must go through
/// `trusty_common::claude_config::write_json_atomic` (temp-file + rename), not
/// a plain `fs::write`, so a crash mid-write can never leave a
/// torn/truncated `settings.json`. `write_json_atomic` backs the prior file up
/// to `<path>.bak` before replacing it — that backup only appears when the
/// atomic path actually ran, so its presence after a second write (and
/// absence after the first) is an observable proxy for "the atomic
/// temp-file+rename mechanism was used" without reaching into private
/// trusty-common internals.
/// What: writes twice, asserting no `.bak`/`.tmp` after the first write (file
/// didn't exist yet), a `.bak` byte-identical to the pre-second-write content
/// after the second, no leftover `.tmp` file (the rename is what makes the
/// write atomic), and that the JSON payload itself is unchanged by the write
/// mechanism swap.
#[test]
fn write_project_hooks_writes_via_atomic_path() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path();
    let claude_dir = project.join(".claude");
    let settings_path = claude_dir.join("settings.json");
    let bak_path = claude_dir.join("settings.json.bak");
    let tmp_path = claude_dir.join("settings.json.tmp");

    super::super::settings::write_project_hooks(project).expect("first write succeeds");
    assert!(settings_path.exists(), "settings.json must be created");
    assert!(
        !bak_path.exists(),
        "no backup expected on first write (file did not previously exist)"
    );
    assert!(
        !tmp_path.exists(),
        "the .tmp file must be renamed away, never left behind"
    );
    let first_content = std::fs::read_to_string(&settings_path).unwrap();

    super::super::settings::write_project_hooks(project).expect("second write succeeds");
    assert!(
        bak_path.exists(),
        "write_json_atomic must back up the prior file before replacing it"
    );
    assert!(
        !tmp_path.exists(),
        "the .tmp file must be renamed away, never left behind"
    );
    let backup_content = std::fs::read_to_string(&bak_path).unwrap();
    assert_eq!(
        backup_content, first_content,
        "the backup must be a byte-identical copy of the pre-second-write content"
    );

    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
    assert!(value["hooks"]["SessionStart"].is_array());
}
