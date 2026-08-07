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
    let additions = project_managed_hook_additions(true);
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
    let first = project_managed_hook_additions(true);
    let second = project_managed_hook_additions(true);
    assert_eq!(first, second);
}

/// Why (#5034): `[hooks] prompt_context = false` must drop the
/// `UserPromptSubmit` entry and NOTHING else. The regression this guards is a
/// toggle that also takes out `SessionStart` — the other `TRUSTY_MEMORY_HOOKS`
/// key, sharing the same constant and the same `trusty-memory ` command prefix.
/// What: builds the additions with the toggle off and asserts `UserPromptSubmit`
/// is absent while `SessionStart`, the PM guard, and all six lifecycle-triad
/// events are byte-identical to the enabled build.
#[test]
fn project_managed_hook_additions_omits_prompt_context_when_disabled() {
    let enabled = project_managed_hook_additions(true);
    let disabled = project_managed_hook_additions(false);

    let off = disabled["hooks"].as_object().expect("hooks is an object");
    assert!(
        !off.contains_key("UserPromptSubmit"),
        "UserPromptSubmit must be absent when the toggle is off: {off:?}"
    );

    // Every other event must survive with its enabled-build value untouched.
    let on = enabled["hooks"].as_object().unwrap();
    for event in [
        "SessionStart",
        "PreToolUse",
        "PostToolUse",
        "Stop",
        "SubagentStop",
        "SessionEnd",
    ] {
        assert_eq!(
            off.get(event),
            on.get(event),
            "{event} must be unchanged by the prompt_context toggle"
        );
    }
    assert_eq!(
        off.len(),
        on.len() - 1,
        "exactly one event key may differ between the two builds"
    );
}

/// Why (#5034): the strip domain must not shrink with the toggle, or the
/// opt-out silently does nothing on any project already launched once.
/// What: asserts the owned-event list covers the key set of BOTH toggle
/// variants.
#[test]
fn project_managed_hook_events_is_a_superset_of_every_variant() {
    let owned = project_managed_hook_events();
    for enabled in [true, false] {
        for key in project_managed_hook_additions(enabled)["hooks"]
            .as_object()
            .unwrap()
            .keys()
        {
            assert!(
                owned.contains(key),
                "{key} (toggle={enabled}) must be in the owned strip domain: {owned:?}"
            );
        }
    }
    assert!(
        owned.iter().any(|e| e == "UserPromptSubmit"),
        "UserPromptSubmit must stay in the strip domain even though the \
         disabled build never writes it: {owned:?}"
    );
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

    super::super::settings::write_project_hooks(project, true).expect("write succeeds");

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

    super::super::settings::write_project_hooks(project, true).expect("write succeeds");

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
    super::super::settings::write_project_hooks(project, true).expect("second write succeeds");
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

    super::super::settings::write_project_hooks(project, true).expect("first write succeeds");
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

    super::super::settings::write_project_hooks(project, true).expect("second write succeeds");
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

/// Why (#5034): the end-to-end opt-out — `[hooks] prompt_context = false` must
/// leave no `UserPromptSubmit` entry in the file the launched session reads,
/// while every other hook trusty-mpm writes is still there. The measured cost
/// this buys back is ~1,211 tokens per prompt (#4904).
/// What: writes with the toggle off into a fresh project and asserts
/// `UserPromptSubmit` is absent, `SessionStart` still carries the
/// `trusty-memory inbox-check` group, `PreToolUse` still carries the PM guard,
/// and all six lifecycle-triad events are present.
#[test]
fn write_project_hooks_omits_prompt_context_when_disabled() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path();

    super::super::settings::write_project_hooks(project, false).expect("write succeeds");

    let value: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(project.join(".claude").join("settings.json")).unwrap(),
    )
    .unwrap();
    let hooks = value["hooks"].as_object().expect("hooks must be an object");

    assert!(
        !hooks.contains_key("UserPromptSubmit"),
        "UserPromptSubmit must not be written when disabled: {hooks:?}"
    );
    let raw = serde_json::to_string(&value).unwrap();
    assert!(
        !raw.contains("prompt-context"),
        "no prompt-context command may survive anywhere in the file: {raw}"
    );

    // The SessionStart trusty-memory group is a separate hook and must stay.
    let session_start = hooks["SessionStart"].as_array().unwrap();
    let ss_commands: Vec<&str> = session_start
        .iter()
        .map(|g| g["hooks"][0]["command"].as_str().unwrap())
        .collect();
    assert!(
        ss_commands.contains(&"trusty-memory inbox-check"),
        "SessionStart must keep the inbox-check hook: {ss_commands:?}"
    );

    // The PM guard and the full lifecycle triad must be unaffected.
    let pre_commands: Vec<&str> = hooks["PreToolUse"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| g["hooks"][0]["command"].as_str().unwrap())
        .collect();
    assert!(
        pre_commands.iter().any(|c| c.ends_with(" hook --pm-guard")),
        "the PM guard must still be registered: {pre_commands:?}"
    );
    for event in [
        "PreToolUse",
        "PostToolUse",
        "Stop",
        "SubagentStop",
        "SessionStart",
        "SessionEnd",
    ] {
        assert!(
            hooks[event]
                .as_array()
                .unwrap()
                .iter()
                .any(|g| g["hooks"][0]["command"]
                    .as_str()
                    .is_some_and(|c| c.ends_with(" hook"))),
            "{event} must still carry the lifecycle-triad command"
        );
    }
}

/// Why (#5034): this is the test that fails if the strip domain is derived from
/// the additions instead of the owned set. Every project in the wild has
/// already been launched with the hook enabled, so the entry is ALREADY in
/// `.claude/settings.json`; an opt-out that only stops re-writing it would
/// leave it firing forever and the config key would appear to do nothing.
/// What: writes once enabled (seeding the entry), then writes disabled, and
/// asserts the stale entry is gone while a foreign `UserPromptSubmit` entry
/// added by the operator survives.
#[test]
fn write_project_hooks_strips_stale_prompt_context_when_disabled() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path();
    let settings_path = project.join(".claude").join("settings.json");

    super::super::settings::write_project_hooks(project, true).expect("enabled write succeeds");
    let seeded: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
    assert_eq!(
        seeded["hooks"]["UserPromptSubmit"][0]["hooks"][0]["command"],
        serde_json::json!("trusty-memory prompt-context"),
        "precondition: the enabled write must have seeded the entry"
    );

    // The operator also has a hook of their own on the same event.
    let mut with_foreign = seeded.clone();
    with_foreign["hooks"]["UserPromptSubmit"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({
            "matcher": "",
            "hooks": [{ "type": "command", "command": "my-own-tool inject", "timeout": 5 }]
        }));
    std::fs::write(
        &settings_path,
        serde_json::to_string_pretty(&with_foreign).unwrap(),
    )
    .unwrap();

    super::super::settings::write_project_hooks(project, false).expect("disabled write succeeds");

    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
    let raw = serde_json::to_string(&value).unwrap();
    assert!(
        !raw.contains("trusty-memory prompt-context"),
        "the stale entry from the earlier enabled launch must be stripped: {raw}"
    );

    let remaining: Vec<&str> = value["hooks"]["UserPromptSubmit"]
        .as_array()
        .expect("the operator's own group keeps the event key alive")
        .iter()
        .map(|g| g["hooks"][0]["command"].as_str().unwrap())
        .collect();
    assert_eq!(
        remaining,
        vec!["my-own-tool inject"],
        "a foreign UserPromptSubmit entry must survive the strip"
    );
}

/// Why (#5034): the opt-out must be reversible — clearing the config key has to
/// restore the hook, not leave the project permanently stripped.
/// What: enabled → disabled → enabled, asserting the final file is
/// byte-identical to the first enabled write.
#[test]
fn write_project_hooks_re_enabling_restores_the_hook() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path();
    let settings_path = project.join(".claude").join("settings.json");

    super::super::settings::write_project_hooks(project, true).expect("write 1");
    let first = std::fs::read_to_string(&settings_path).unwrap();
    super::super::settings::write_project_hooks(project, false).expect("write 2");
    super::super::settings::write_project_hooks(project, true).expect("write 3");
    let third = std::fs::read_to_string(&settings_path).unwrap();

    assert_eq!(
        third, first,
        "re-enabling must reproduce the enabled write byte for byte"
    );
}

/// Why (#5034): the default path must not move. `#[hooks] prompt_context`
/// defaults to `true`, and the enabled write must be exactly what the
/// pre-toggle code produced — which, for the strip half of the change, means
/// the widened strip domain has to be a no-op when the hook is enabled.
/// What: asserts the config default is `true`, and that the enabled write is
/// unchanged whether the strip runs over the owned event set or over the
/// additions' own keys (the pre-#5034 derivation), on both a fresh project and
/// one already carrying a prior write.
#[test]
fn write_project_hooks_enabled_output_is_unchanged_by_the_toggle() {
    assert!(
        crate::core::config::MpmConfig::default()
            .hooks
            .prompt_context,
        "the shipped default must keep the hook enabled"
    );

    // The pre-#5034 strip domain was the additions' own key set. With the hook
    // enabled the two derivations must be identical, which is what makes the
    // default path byte-identical to before.
    let mut from_additions: Vec<String> = project_managed_hook_additions(true)["hooks"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    let mut owned = project_managed_hook_events();
    from_additions.sort();
    owned.sort();
    assert_eq!(
        owned, from_additions,
        "with the hook enabled the widened strip domain must be a no-op"
    );

    // And the write itself is stable across repeats, on a project that already
    // carries a prior launch's entries.
    let tmp = TempDir::new().unwrap();
    let project = tmp.path();
    let settings_path = project.join(".claude").join("settings.json");
    super::super::settings::write_project_hooks(project, true).expect("write 1");
    let first = std::fs::read_to_string(&settings_path).unwrap();
    super::super::settings::write_project_hooks(project, true).expect("write 2");
    assert_eq!(
        std::fs::read_to_string(&settings_path).unwrap(),
        first,
        "the enabled write must be idempotent"
    );
}
