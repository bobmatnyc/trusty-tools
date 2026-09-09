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
    let additions = project_managed_hook_additions(
        Some(std::path::Path::new("/usr/local/bin/tm")),
        true,
        false,
    )
    .expect("a stable hook exe resolves in the test environment");
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
    let first = project_managed_hook_additions(
        Some(std::path::Path::new("/usr/local/bin/tm")),
        true,
        false,
    )
    .expect("a stable hook exe resolves in the test environment");
    let second = project_managed_hook_additions(
        Some(std::path::Path::new("/usr/local/bin/tm")),
        true,
        false,
    )
    .expect("a stable hook exe resolves in the test environment");
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
    let enabled = project_managed_hook_additions(
        Some(std::path::Path::new("/usr/local/bin/tm")),
        true,
        false,
    )
    .expect("a stable hook exe resolves in the test environment");
    let disabled = project_managed_hook_additions(
        Some(std::path::Path::new("/usr/local/bin/tm")),
        false,
        false,
    )
    .expect("a stable hook exe resolves in the test environment");

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
        for key in project_managed_hook_additions(
            Some(std::path::Path::new("/usr/local/bin/tm")),
            enabled,
            false,
        )
        .expect("a stable hook exe resolves in the test environment")["hooks"]
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

    super::super::settings::write_project_hooks(
        project,
        Some(std::path::Path::new("/usr/local/bin/tm")),
        true,
        false,
    )
    .expect("write succeeds");

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

    super::super::settings::write_project_hooks(
        project,
        Some(std::path::Path::new("/usr/local/bin/tm")),
        true,
        false,
    )
    .expect("write succeeds");

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
    super::super::settings::write_project_hooks(
        project,
        Some(std::path::Path::new("/usr/local/bin/tm")),
        true,
        false,
    )
    .expect("second write succeeds");
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

    super::super::settings::write_project_hooks(
        project,
        Some(std::path::Path::new("/usr/local/bin/tm")),
        true,
        false,
    )
    .expect("first write succeeds");
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

    // #7244 (round 3): the second call flips `divert_enabled` so the merged
    // value actually DIFFERS. Repeating the first call's arguments now takes
    // the no-op exit, which writes nothing — and a test asserting the atomic
    // path ran would then be asserting against a call that never reached it.
    super::super::settings::write_project_hooks(
        project,
        Some(std::path::Path::new("/usr/local/bin/tm")),
        true,
        true,
    )
    .expect("second write succeeds");
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

    super::super::settings::write_project_hooks(
        project,
        Some(std::path::Path::new("/usr/local/bin/tm")),
        false,
        false,
    )
    .expect("write succeeds");

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

    super::super::settings::write_project_hooks(
        project,
        Some(std::path::Path::new("/usr/local/bin/tm")),
        true,
        false,
    )
    .expect("enabled write succeeds");
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

    super::super::settings::write_project_hooks(
        project,
        Some(std::path::Path::new("/usr/local/bin/tm")),
        false,
        false,
    )
    .expect("disabled write succeeds");

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

    super::super::settings::write_project_hooks(
        project,
        Some(std::path::Path::new("/usr/local/bin/tm")),
        true,
        false,
    )
    .expect("write 1");
    let first = std::fs::read_to_string(&settings_path).unwrap();
    super::super::settings::write_project_hooks(
        project,
        Some(std::path::Path::new("/usr/local/bin/tm")),
        false,
        false,
    )
    .expect("write 2");
    super::super::settings::write_project_hooks(
        project,
        Some(std::path::Path::new("/usr/local/bin/tm")),
        true,
        false,
    )
    .expect("write 3");
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
    let mut from_additions: Vec<String> = project_managed_hook_additions(
        Some(std::path::Path::new("/usr/local/bin/tm")),
        true,
        false,
    )
    .expect("a stable hook exe resolves in the test environment")["hooks"]
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
    super::super::settings::write_project_hooks(
        project,
        Some(std::path::Path::new("/usr/local/bin/tm")),
        true,
        false,
    )
    .expect("write 1");
    let first = std::fs::read_to_string(&settings_path).unwrap();
    super::super::settings::write_project_hooks(
        project,
        Some(std::path::Path::new("/usr/local/bin/tm")),
        true,
        false,
    )
    .expect("write 2");
    assert_eq!(
        std::fs::read_to_string(&settings_path).unwrap(),
        first,
        "the enabled write must be idempotent"
    );
}

/// Why (#6887): the toggle must add EXACTLY the two `PreToolUse` groups it
/// promises and touch nothing else. A writer that `insert`ed `PreToolUse`
/// instead of appending would silently drop the PM guard and the lifecycle
/// triad — the failure mode this module's doc comment exists to prevent.
/// What: builds the additions with the toggle on and off, asserts `PreToolUse`
/// grows by exactly two groups (matcher `Read`, matcher `Bash`, both invoking
/// `hook --divert-check`), and asserts every OTHER event key is byte-identical
/// between the two builds.
#[test]
fn project_managed_hook_additions_includes_divert_when_enabled() {
    let off = project_managed_hook_additions(
        Some(std::path::Path::new("/usr/local/bin/tm")),
        true,
        false,
    )
    .expect("a stable hook exe resolves in the test environment");
    let on =
        project_managed_hook_additions(Some(std::path::Path::new("/usr/local/bin/tm")), true, true)
            .expect("a stable hook exe resolves in the test environment");

    let off_pre = off["hooks"]["PreToolUse"].as_array().expect("array");
    let on_pre = on["hooks"]["PreToolUse"].as_array().expect("array");
    assert_eq!(
        on_pre.len(),
        off_pre.len() + 2,
        "the toggle must add exactly two PreToolUse groups"
    );
    assert_eq!(
        &on_pre[..off_pre.len()],
        &off_pre[..],
        "the pre-existing PreToolUse groups must be byte-identical and in place"
    );

    let added: Vec<&serde_json::Value> = on_pre[off_pre.len()..].iter().collect();
    let matchers: Vec<&str> = added
        .iter()
        .map(|g| g["matcher"].as_str().unwrap())
        .collect();
    assert_eq!(matchers, vec!["Read", "Bash"]);
    for group in &added {
        let cmd = group["hooks"][0]["command"].as_str().unwrap();
        assert!(
            cmd.ends_with(" hook --divert-check"),
            "the divert group must invoke the divert-check hook: {cmd}"
        );
        assert!(
            !cmd.contains("KEY") && !cmd.contains("TOKEN") && !cmd.contains("SECRET"),
            "no credential may appear in the hook command string: {cmd}"
        );
    }

    // Every other event key must be untouched by the toggle.
    let off_obj = off["hooks"].as_object().unwrap();
    let on_obj = on["hooks"].as_object().unwrap();
    assert_eq!(off_obj.len(), on_obj.len(), "no new event key may appear");
    for (event, value) in off_obj {
        if event == "PreToolUse" {
            continue;
        }
        assert_eq!(
            on_obj.get(event),
            Some(value),
            "{event} must be unchanged by the divert toggle"
        );
    }
}

/// Why (#6887): the feature is OPT-IN, so the default build must carry zero
/// diversion groups — a toggle that leaked one entry would divert every
/// project's reads without anyone asking.
/// What: with the toggle off, no hook command anywhere mentions
/// `--divert-check`.
#[test]
fn project_managed_hook_additions_omits_divert_when_disabled() {
    let off = project_managed_hook_additions(
        Some(std::path::Path::new("/usr/local/bin/tm")),
        true,
        false,
    )
    .expect("a stable hook exe resolves in the test environment");
    let text = off.to_string();
    assert!(
        !text.contains("--divert-check"),
        "a disabled toggle must write no diversion hook: {text}"
    );
}

/// Why (#6887, the #2948 duplication lesson): the strip predicate must claim
/// the divert command, or `write_project_hooks` appends a second copy on every
/// relaunch and never removes one when the toggle flips back off. Note
/// `is_mpm_hook_command` does NOT cover it — that predicate requires the
/// command to end with exactly ` hook`.
/// What: the predicate accepts the divert command and still rejects a foreign
/// one.
#[test]
fn is_project_managed_hook_command_recognises_divert_check() {
    assert!(is_project_managed_hook_command(
        "/opt/bin/tm hook --divert-check"
    ));
    assert!(is_project_managed_hook_command("tm hook --divert-check"));
    assert!(
        !crate::core::standalone::hooks::is_mpm_hook_command("/opt/bin/tm hook --divert-check"),
        "the triad predicate must not claim it; the divert arm is what covers it"
    );
    assert!(!is_project_managed_hook_command(
        "claude-mpm hooks fire PreToolUse --divert"
    ));
}

/// Why (#6887): the end-to-end write, and the property N relaunches must hold —
/// exactly two divert groups, never 2N.
/// What: writes with the toggle on three times and asserts the file is stable
/// and carries exactly two `--divert-check` groups.
#[test]
fn write_project_hooks_writes_divert_groups_when_enabled() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path();
    let settings_path = project.join(".claude").join("settings.json");

    super::super::settings::write_project_hooks(
        project,
        Some(std::path::Path::new("/usr/local/bin/tm")),
        true,
        true,
    )
    .expect("write 1");
    let first = std::fs::read_to_string(&settings_path).unwrap();
    super::super::settings::write_project_hooks(
        project,
        Some(std::path::Path::new("/usr/local/bin/tm")),
        true,
        true,
    )
    .expect("write 2");
    super::super::settings::write_project_hooks(
        project,
        Some(std::path::Path::new("/usr/local/bin/tm")),
        true,
        true,
    )
    .expect("write 3");
    assert_eq!(
        std::fs::read_to_string(&settings_path).unwrap(),
        first,
        "repeated launches must not duplicate the divert groups"
    );

    let value: serde_json::Value = serde_json::from_str(&first).unwrap();
    let pre = value["hooks"]["PreToolUse"].as_array().expect("array");
    let divert: Vec<&serde_json::Value> = pre
        .iter()
        .filter(|g| {
            g["hooks"][0]["command"]
                .as_str()
                .is_some_and(|c| c.ends_with(" hook --divert-check"))
        })
        .collect();
    assert_eq!(divert.len(), 2, "exactly two divert groups: {pre:?}");
    // The credential-free rule, asserted against what actually lands on disk.
    assert!(
        !first.contains("TRUSTY_DIVERT_WORKER_MODEL") && !first.contains("api_key"),
        "settings.json must carry no divert config and no credential: {first}"
    );
}

/// Why (#6887, the #5034 lesson applied to this toggle): flipping `[divert]
/// enabled` back to false must REMOVE what a prior launch wrote. Deriving the
/// strip domain from the toggled-down additions would leave the groups firing
/// forever.
/// What: writes with the toggle on, then off, and asserts no `--divert-check`
/// command survives while the PM guard and the lifecycle triad do.
#[test]
fn write_project_hooks_strips_stale_divert_when_disabled() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path();
    let settings_path = project.join(".claude").join("settings.json");

    super::super::settings::write_project_hooks(
        project,
        Some(std::path::Path::new("/usr/local/bin/tm")),
        true,
        true,
    )
    .expect("enabled write");
    assert!(
        std::fs::read_to_string(&settings_path)
            .unwrap()
            .contains("--divert-check"),
        "precondition: the enabled write must land the groups"
    );

    super::super::settings::write_project_hooks(
        project,
        Some(std::path::Path::new("/usr/local/bin/tm")),
        true,
        false,
    )
    .expect("disabled write");
    let after = std::fs::read_to_string(&settings_path).unwrap();
    assert!(
        !after.contains("--divert-check"),
        "the disabled write must strip the stale divert groups: {after}"
    );

    // And it must strip ONLY those: the rest of the enabled-off build survives.
    let value: serde_json::Value = serde_json::from_str(&after).unwrap();
    let pre = value["hooks"]["PreToolUse"].as_array().expect("array");
    assert_eq!(
        pre.len(),
        2,
        "PM guard + lifecycle triad must remain: {pre:?}"
    );
}

/// The pinned installed-looking binary every test in this module writes with.
const TEST_EXE: &str = "/usr/local/bin/tm";

/// Every timestamped snapshot of `settings.json` in `dir`, name-sorted.
///
/// Shares the prune's own inclusion rule rather than re-deriving it, so a
/// count here can never include a file the prune would not have touched — the
/// atomic writer's single-slot `settings.json.bak` in particular.
fn snapshot_names(dir: &std::path::Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .expect("readable dir")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| crate::core::standalone::hooks::backup::is_snapshot_of("settings.json", n))
        .collect();
    names.sort();
    names
}

/// Call the project-tier writer with the pinned test binary.
fn write(project: &std::path::Path, prompt_context: bool, divert: bool) {
    super::super::settings::write_project_hooks(
        project,
        Some(std::path::Path::new(TEST_EXE)),
        prompt_context,
        divert,
    )
    .expect("write succeeds");
}

/// Why (#7244, round 3): this is the writer that damaged a real project's
/// `.claude/settings.json`. Its prior state has to survive the rewrite that
/// replaces it, and the atomic writer's one `.bak` slot cannot carry that —
/// the next launch overwrites it.
/// What: creates the file, rewrites it with a different toggle, and asserts one
/// snapshot exists holding the pre-rewrite bytes.
#[test]
fn write_project_hooks_snapshots_the_file_it_replaces() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path();
    let claude_dir = project.join(".claude");
    let settings_path = claude_dir.join("settings.json");

    write(project, true, false);
    let before = std::fs::read_to_string(&settings_path).unwrap();
    assert!(
        snapshot_names(&claude_dir).is_empty(),
        "nothing existed before the first write, so nothing was snapshotted"
    );

    write(project, true, true);

    let snapshots = snapshot_names(&claude_dir);
    assert_eq!(
        snapshots.len(),
        1,
        "expected one snapshot, got {snapshots:?}"
    );
    assert_eq!(
        std::fs::read_to_string(claude_dir.join(&snapshots[0])).unwrap(),
        before,
        "the snapshot must hold the file as it was before the rewrite"
    );
}

/// Why (#7244): every managed launch that changes the file adds a snapshot, so
/// an unbounded set would turn a long-lived project's `.claude/` into an
/// archive. Three is the kept depth.
/// What: four rewrites that each change the file, then asserts exactly three
/// snapshots survive and the FIRST one taken is the one gone — pruning the
/// newest would bound the set while discarding the copy an operator wants.
#[test]
fn write_project_hooks_prunes_snapshots_to_three() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path();
    let claude_dir = project.join(".claude");

    // Creates the file; no snapshot (nothing existed).
    write(project, true, false);
    // Four rewrites, each differing from the one before it.
    write(project, true, true);
    let oldest = snapshot_names(&claude_dir);
    assert_eq!(oldest.len(), 1, "the first rewrite snapshots once");
    write(project, false, true);
    write(project, false, false);
    write(project, true, false);

    let snapshots = snapshot_names(&claude_dir);
    assert_eq!(
        snapshots.len(),
        3,
        "four rewrites must leave exactly three snapshots, got {snapshots:?}"
    );
    assert!(
        !snapshots.contains(&oldest[0]),
        "the oldest snapshot must be the one pruned, still present in {snapshots:?}"
    );
}

/// Why (#7244): `prepare_session` calls this on EVERY managed launch, and
/// almost every call reproduces the bytes already on disk. Snapshotting those
/// would fill all three kept slots with copies of the current file within
/// three launches, evicting the one prior state worth keeping.
/// What: writes twice with identical arguments and asserts no snapshot, no
/// change to the file, and — the assertion that separates "returned early" from
/// "rewrote the same bytes" — no `settings.json.bak`, which
/// `write_json_atomic` creates on any call that reaches it.
#[test]
fn write_project_hooks_takes_no_snapshot_when_nothing_changes() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path();
    let claude_dir = project.join(".claude");
    let settings_path = claude_dir.join("settings.json");

    write(project, true, false);
    let before = std::fs::read_to_string(&settings_path).unwrap();

    write(project, true, false);

    assert!(
        !claude_dir.join("settings.json.bak").exists(),
        "an identical rewrite must return before the atomic write, which would \
         have left its own backup"
    );
    assert!(
        snapshot_names(&claude_dir).is_empty(),
        "an identical rewrite must take no snapshot"
    );
    assert_eq!(
        std::fs::read_to_string(&settings_path).unwrap(),
        before,
        "an identical rewrite must leave the file alone"
    );
}

/// Why (#7244): a refusal writes nothing, so there is no prior state at risk.
/// Snapshotting anyway would let a machine that cannot resolve `tm` evict a
/// real prior state, three launches at a time, while changing nothing.
/// What: seeds a settings file, hands the writer a refusal through the
/// resolution seam (a host with `tm` installed would otherwise have the PATH
/// fallback rescue any refused `exe_override`), and asserts the refusal
/// surfaced, no snapshot appeared, and the file is unchanged.
#[test]
fn write_project_hooks_takes_no_snapshot_when_the_exe_is_refused() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path();
    let claude_dir = project.join(".claude");

    write(project, true, false);
    let before = std::fs::read_to_string(claude_dir.join("settings.json")).unwrap();

    let refusal = Err(
        crate::core::standalone::hooks::StableHookExeError::Ephemeral(std::path::PathBuf::from(
            "/x/target-7244/debug/deps/some_test-cd3ba8f0",
        )),
    );
    super::super::settings::write_project_hooks_with(project, refusal)
        .expect_err("a refusal must reach the caller, not be written");

    assert!(
        snapshot_names(&claude_dir).is_empty(),
        "a refused write must take no snapshot"
    );
    assert_eq!(
        std::fs::read_to_string(claude_dir.join("settings.json")).unwrap(),
        before,
        "a refused write must leave the file alone"
    );
}

/// Why (#7244, fail-closed): a rewrite whose prior state cannot be preserved
/// must not run. Writing anyway and warning about the snapshot repeats the
/// original defect — a writer proceeding past a step it could not complete.
/// What: makes `.claude/` unwritable so the snapshot's exclusive create fails,
/// then asserts `PrepError::HookSnapshot` came back naming the file, and the
/// file is byte-identical. Unix-only: the read-only directory bit is the
/// portable way to deny file creation.
#[cfg(unix)]
#[test]
fn write_project_hooks_aborts_when_the_snapshot_fails() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = TempDir::new().unwrap();
    let project = tmp.path();
    let claude_dir = project.join(".claude");
    let settings_path = claude_dir.join("settings.json");

    write(project, true, false);
    let before = std::fs::read_to_string(&settings_path).unwrap();

    std::fs::set_permissions(&claude_dir, std::fs::Permissions::from_mode(0o500)).unwrap();
    let result = super::super::settings::write_project_hooks(
        project,
        Some(std::path::Path::new(TEST_EXE)),
        true,
        true,
    );
    // Restore before asserting, so a failed assertion still leaves a removable
    // temp dir behind.
    std::fs::set_permissions(&claude_dir, std::fs::Permissions::from_mode(0o700)).unwrap();

    let err = result.expect_err("an unsnapshottable rewrite must fail");
    assert!(
        matches!(
            err,
            crate::core::session_launch::PrepError::HookSnapshot { .. }
        ),
        "expected HookSnapshot, got {err:?}"
    );
    assert!(
        err.to_string().contains("settings.json"),
        "the error must name the file whose rewrite was abandoned, got: {err}"
    );
    assert_eq!(
        std::fs::read_to_string(&settings_path).unwrap(),
        before,
        "the settings file must be untouched when its snapshot could not be taken"
    );
}

/// A build-tree executable path the #7262 classifier must always reject.
const EPHEMERAL_EXE: &str = "/repo/target-7247/debug/deps/test_session_lifecycle-cd3ba8f03938239b";

/// The argv shapes this crate's writers produce must stay inside the list
/// [`crate::core::standalone::hooks::build_tree`] classifies (#7262).
///
/// Why: the classifier keeps its own copy of the argv vocabulary, and a writer
/// that grows a new sub-flag without adding it there goes back to being
/// invisible to `tm doctor` and unstrippable by the writer — the exact #7244
/// failure, one flag later. This test derives each shape from the writer's OWN
/// output rather than restating the literal, so the two cannot drift.
/// What: strips the resolved binary off each writer-produced command, re-attaches
/// the same argv to a build-tree binary, and asserts the classifier claims it.
#[test]
fn pm_guard_and_divert_commands_end_in_a_known_argv_tail() {
    use crate::core::standalone::hooks::{
        is_build_tree_hook_command, is_build_tree_statusline_command,
    };

    let bin = super::super::settings::resolve_statusline_binary();
    let mut produced: Vec<String> = vec![
        super::super::settings::pm_guard_hook_value()[0]["hooks"][0]["command"]
            .as_str()
            .expect("the PM-guard command is a string")
            .to_string(),
    ];
    for group in super::super::divert_hooks::divert_hook_groups() {
        produced.push(
            group["hooks"][0]["command"]
                .as_str()
                .expect("a divert command is a string")
                .to_string(),
        );
    }

    for cmd in &produced {
        let argv = cmd
            .strip_prefix(&bin)
            .unwrap_or_else(|| panic!("{cmd} must start with the resolved binary {bin}"));
        assert!(
            is_build_tree_hook_command(&format!("{EPHEMERAL_EXE}{argv}")),
            "argv {argv:?} is not in the #7262 classifier's vocabulary"
        );
    }

    let statusline = super::super::settings::resolve_statusline_command();
    let argv = statusline
        .strip_prefix(&bin)
        .unwrap_or_else(|| panic!("{statusline} must start with the resolved binary {bin}"));
    assert!(
        is_build_tree_statusline_command(&format!("{EPHEMERAL_EXE}{argv}")),
        "statusLine argv {argv:?} is not in the #7262 classifier's vocabulary"
    );
}
