//! Unit tests for the #8454 S2 trust anchors: `config.yaml`, the managed
//! `claude-config` settings, the delete verbs, and the project-settings
//! content rule.

use super::tests::{Fixture, allowlist, architect_env, fixture, payload, pm_env};
use super::*;
use serde_json::json;

/// The `config.yaml` anchor under the fixture home.
fn config_yaml(fx: &Fixture) -> PathBuf {
    trusty_common::crate_config::crate_config_path_at(&fx.home, "trusty-mpm")
}

/// The managed `claude-config` directory under the fixture home.
fn managed(fx: &Fixture) -> PathBuf {
    trusty_mpm::core::trusty_tools_config::managed_claude_config_dir_at(&fx.home)
}

/// [`fixture`] with the S2 anchors written.
fn egress_fixture() -> Fixture {
    let fx = fixture();
    std::fs::create_dir_all(managed(&fx)).expect("mkdir managed");
    std::fs::write(config_yaml(&fx), "workspace_root_template: ~/w\n").expect("write yaml");
    std::fs::write(managed(&fx).join("settings.json"), "{}").expect("write managed");
    std::fs::write(managed(&fx).join("settings.local.json"), "{}").expect("write local");
    fx
}

fn pm_verdict(fx: &Fixture, tool: &str, input: Value) -> Option<String> {
    evaluate(&payload(fx, tool, input), &pm_env(fx), MpmConfig::default)
}

fn pm_bash(fx: &Fixture, command: &str) -> Option<String> {
    pm_verdict(fx, "Bash", json!({ "command": command }))
}

fn write_input(path: &Path, content: &str) -> Value {
    json!({ "file_path": path.display().to_string(), "content": content })
}

/// 🔴 #8454 S2 (plan §6 `trust_anchor_denies_config_yaml_write`): every edit
/// tool, a symlink, a hard link, `~` and a case variant all land on the anchor.
#[test]
fn egress_anchor_config_yaml_writes_are_denied() {
    let fx = egress_fixture();
    let yaml = config_yaml(&fx);
    for tool in EDIT_TOOLS {
        let reason = pm_verdict(&fx, tool, write_input(&yaml, "x")).expect(tool);
        assert!(
            reason.contains("resolves to a trust anchor"),
            "{tool}: {reason}"
        );
        assert!(
            reason.contains("config.yaml"),
            "{tool}: the remedy names it: {reason}"
        );
    }
    let soft = fx.cwd.join("soft.yaml");
    std::os::unix::fs::symlink(&yaml, &soft).expect("symlink");
    assert!(pm_verdict(&fx, "Write", write_input(&soft, "x")).is_some());
    let hard = fx.cwd.join("hard.yaml");
    std::fs::hard_link(&yaml, &hard).expect("hard link");
    assert!(pm_verdict(&fx, "Write", write_input(&hard, "x")).is_some());
    for spelling in [
        "~/.trusty-tools/trusty-mpm/config.yaml",
        "~/.Trusty-Tools/Trusty-MPM/CONFIG.YAML",
    ] {
        let input = json!({ "file_path": spelling, "content": "x" });
        assert!(pm_verdict(&fx, "Write", input).is_some(), "{spelling}");
    }
    for command in [
        "echo x > ~/.trusty-tools/trusty-mpm/config.yaml",
        "cp a.txt ~/.trusty-tools/trusty-mpm/config.yaml",
        "sed -i s/a/b/ ~/.trusty-tools/trusty-mpm/config.yaml",
        "ln -sf a.txt \"$HOME/.trusty-tools/trusty-mpm/config.yaml\"",
    ] {
        assert!(pm_bash(&fx, command).is_some(), "allowed: {command}");
    }
}

/// 🔴 #8454 S2: the managed user-tier settings files are anchors for every
/// writer but the Architect's main thread.
#[test]
fn egress_anchor_managed_settings_writes_are_denied() {
    let fx = egress_fixture();
    for name in ["settings.json", "settings.local.json"] {
        let path = managed(&fx).join(name);
        assert!(
            pm_verdict(&fx, "Write", write_input(&path, "{}")).is_some(),
            "{name}"
        );
        let command = format!("echo '{{}}' > {}", path.display());
        assert!(pm_bash(&fx, &command).is_some(), "allowed: {command}");
    }
    // Writes beside them stay allowed.
    let skill = managed(&fx).join("skills").join("note.md");
    assert_eq!(pm_verdict(&fx, "Write", write_input(&skill, "x")), None);
}

/// 🔴 #8454 S2: anchoring `config.yaml` also denies `rm` and `mv` of the
/// directories above it; deletes elsewhere stay allowed.
#[test]
fn egress_anchor_rm_and_mv_of_the_config_dir_are_denied() {
    let fx = egress_fixture();
    for command in [
        "rm -rf ~/.trusty-tools/trusty-mpm",
        "rm -rf ~/.trusty-tools",
        "rm ~/.trusty-tools/trusty-mpm/config.yaml",
        "rm -rf ~/.trusty-tools/trusty-mpm/*",
        "rm -rf $HOME/.trusty-tools/*",
        "rmdir ~/.trusty-tools/trusty-mpm/claude-config",
        "unlink ~/.trusty-tools/trusty-mpm/claude-config/settings.json",
        "sudo rm -r -- ~/.trusty-tools/trusty-mpm",
        "mv ~/.trusty-tools/trusty-mpm /tmp/gone",
        "mv ~/.trusty-tools /tmp/gone",
        "rm ~/.trusty-mpm/config.toml",
    ] {
        assert!(pm_bash(&fx, command).is_some(), "allowed: {command}");
    }
    for command in [
        "rm ~/.trusty-tools/trusty-mpm/logs/x.log",
        "rm -f a.txt",
        "rm -rf build/*",
        "rm *.log",
        "cd /tmp && rm -rf *",
        "mv ~/.trusty-tools/trusty-mpm/logs/a /tmp/a",
        "echo x > ~/.trusty-tools/trusty-mpm/notes.md",
    ] {
        assert_eq!(pm_bash(&fx, command), None, "denied: {command}");
    }
}

/// 🔴 #8454 S2: a `config.yaml` location that does not resolve refuses every
/// write, as the #8878 config anchor does.
#[test]
fn egress_anchor_unresolvable_location_denies_every_write() {
    let fx = egress_fixture();
    let yaml = config_yaml(&fx);
    std::fs::remove_file(&yaml).expect("remove yaml");
    std::os::unix::fs::symlink(&yaml, &yaml).expect("self symlink");
    let reason = pm_bash(&fx, "echo x > notes.md").expect("denied");
    assert!(reason.contains("cannot locate the"), "{reason}");
    assert_eq!(pm_bash(&fx, "ls -la"), None);
}

/// 🔴 #8454 S2: a write the guard cannot place, whose file name could be a new
/// anchor, is denied.
#[test]
fn egress_anchor_unplaceable_bash_write_is_denied() {
    let fx = egress_fixture();
    for command in [
        "cd ~/.trusty-tools/trusty-mpm && echo x > config.yaml",
        "cd claude-config && cp a.txt settings.json",
        "cp a.txt $DIR/config.yaml",
        "cd somewhere && rm -rf .trusty-tools",
    ] {
        assert!(pm_bash(&fx, command).is_some(), "allowed: {command}");
    }
}

/// 🔴 #8454 S2 (Architect D6): the Architect's main thread may write the new
/// anchors; its subagent and a subagent payload may not.
#[test]
fn egress_anchor_architect_main_thread_only() {
    let fx = egress_fixture();
    let env = architect_env(&fx);
    for path in [config_yaml(&fx), managed(&fx).join("settings.json")] {
        let write = payload(&fx, "Write", write_input(&path, "{}"));
        assert_eq!(evaluate(&write, &env, || allowlist(&fx)), None, "{path:?}");
        let mut dispatched = write.clone();
        dispatched["agent_id"] = json!("agent-7");
        assert!(evaluate(&dispatched, &env, || allowlist(&fx)).is_some());
        let nested = HookEnv {
            sub_agent: true,
            ..architect_env(&fx)
        };
        assert!(evaluate(&write, &nested, || allowlist(&fx)).is_some());
    }
}

/// The egress group tm writes, as a literal so the fixture does not depend on
/// the writer under test.
fn egress_group() -> Value {
    json!({ "matcher": "", "hooks": [
        { "type": "command", "command": "/usr/local/bin/tm hook --egress-guard", "timeout": 10 }
    ]})
}

/// A project settings body carrying the egress group, `extra` merged in.
fn guarded_settings(extra: Value) -> Value {
    let mut settings = json!({
        "permissions": { "allow": ["Bash(ls:*)"] },
        "hooks": { "PreToolUse": [egress_group()] },
    });
    if let (Some(base), Some(extra)) = (settings.as_object_mut(), extra.as_object()) {
        base.extend(extra.clone());
    }
    settings
}

/// The fixture's project `.claude/` directory, with `settings.json` holding
/// `body`; returns the settings path.
fn project_settings(fx: &Fixture, body: &str) -> PathBuf {
    let dir = fx.cwd.join(".claude");
    std::fs::create_dir_all(&dir).expect("mkdir .claude");
    let path = dir.join("settings.json");
    std::fs::write(&path, body).expect("write project settings");
    path
}

fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).expect("json")
}

/// 🔴 #8454 S2 (Architect D2): a routine edit that keeps the egress entry is
/// allowed — engineers edit committed settings.
#[test]
fn egress_project_settings_routine_edit_is_allowed() {
    let fx = egress_fixture();
    let path = project_settings(&fx, &pretty(&guarded_settings(json!({}))));
    let edit = json!({
        "file_path": path.display().to_string(),
        "old_string": "Bash(ls:*)",
        "new_string": "Bash(git status:*)",
    });
    assert_eq!(pm_verdict(&fx, "Edit", edit), None);
    let rewrite = guarded_settings(json!({ "model": "opus" }));
    assert_eq!(
        pm_verdict(&fx, "Write", write_input(&path, &pretty(&rewrite))),
        None
    );
    // A file that never carried the entry is the engineer's to rewrite.
    let plain = project_settings(&fx, "{\"model\": \"opus\"}");
    assert_eq!(pm_verdict(&fx, "Write", write_input(&plain, "{}")), None);
    assert_eq!(pm_bash(&fx, "echo '{}' > .claude/settings.json"), None);
}

/// 🔴 #8454 S2 (Architect D2): an Edit, Write or MultiEdit whose result drops
/// or weakens the egress entry is denied.
#[test]
fn egress_project_settings_edit_dropping_the_entry_is_denied() {
    let fx = egress_fixture();
    let body = pretty(&guarded_settings(json!({})));
    let path = project_settings(&fx, &body);
    let file = path.display().to_string();
    let drop = json!({ "file_path": file, "old_string": "hook --egress-guard",
                       "new_string": "hook" });
    assert!(pm_verdict(&fx, "Edit", drop).is_some());
    let weaken = json!({ "file_path": file, "old_string": "\"timeout\": 10",
                         "new_string": "\"timeout\": 1" });
    assert!(pm_verdict(&fx, "Edit", weaken).is_some());
    let multi = json!({ "file_path": file, "edits": [
        { "old_string": "Bash(ls:*)", "new_string": "Bash(pwd)" },
        { "old_string": "--egress-guard", "new_string": "--nothing" },
    ]});
    assert!(pm_verdict(&fx, "MultiEdit", multi).is_some());
    let reason = pm_verdict(&fx, "Write", write_input(&path, "{}")).expect("denied");
    assert!(reason.contains("#8454"), "{reason}");
    // An edit the guard cannot reproduce on a guarded file is denied.
    let unmatched = json!({ "file_path": file, "old_string": "absent text",
                            "new_string": "x" });
    assert!(pm_verdict(&fx, "Edit", unmatched).is_some());
}

/// 🔴 #8454 S2 (Architect D2): setting `disableAllHooks` is denied in either
/// project file, whether or not that file carries the entry.
#[test]
fn egress_project_settings_disable_all_hooks_is_denied() {
    let fx = egress_fixture();
    let path = project_settings(&fx, &pretty(&guarded_settings(json!({}))));
    let disabled = guarded_settings(json!({ "disableAllHooks": true }));
    assert!(pm_verdict(&fx, "Write", write_input(&path, &pretty(&disabled))).is_some());
    let local = fx.cwd.join(".claude/settings.local.json");
    let input = write_input(&local, "{\"disableAllHooks\": true}");
    assert!(pm_verdict(&fx, "Write", input).is_some());
    let edit = json!({ "file_path": path.display().to_string(),
                       "old_string": "\"permissions\"",
                       "new_string": "\"disableAllHooks\": true, \"permissions\"" });
    assert!(pm_verdict(&fx, "Edit", edit).is_some());
}

/// 🔴 #8454 S2 (Architect D2): a Bash write to either project file is denied
/// while the project's settings carry the entry, and allowed once they do not.
#[test]
fn egress_project_settings_bash_write_is_denied_when_the_entry_is_present() {
    let fx = egress_fixture();
    project_settings(&fx, &pretty(&guarded_settings(json!({}))));
    for command in [
        "echo '{}' > .claude/settings.json",
        "sed -i s/egress/x/ .claude/settings.json",
        "cp a.txt .claude/settings.json",
        "mv .claude/settings.json /tmp/s.json",
        "rm .claude/settings.json",
        "echo '{\"disableAllHooks\":true}' > .claude/settings.local.json",
        "cd .claude && echo x > settings.json",
    ] {
        let reason = pm_bash(&fx, command).unwrap_or_else(|| panic!("allowed: {command}"));
        assert!(reason.contains("#8454"), "{command}: {reason}");
    }
    project_settings(&fx, "{\"model\": \"opus\"}");
    assert_eq!(pm_bash(&fx, "echo '{}' > .claude/settings.json"), None);
}

/// 🔴 #8454 S2 (Architect D2): an unreadable or malformed project settings file
/// denies every write to it.
#[test]
fn egress_project_settings_unreadable_or_malformed_is_denied() {
    let fx = egress_fixture();
    let path = project_settings(&fx, "{ not json");
    assert!(pm_verdict(&fx, "Write", write_input(&path, "{}")).is_some());
    assert!(pm_bash(&fx, "echo '{}' > .claude/settings.json").is_some());
    let looped = fx.cwd.join(".claude/settings.local.json");
    std::os::unix::fs::symlink(&looped, &looped).expect("self symlink");
    assert!(pm_verdict(&fx, "Write", write_input(&looped, "{}")).is_some());
}

/// 🔴 #8454 S2: the content rule binds the Architect's main thread too — D6
/// exempts it from the `config.yaml` and managed-settings anchors only.
#[test]
fn egress_project_settings_rule_binds_the_architect() {
    let fx = egress_fixture();
    let path = project_settings(&fx, &pretty(&guarded_settings(json!({}))));
    let write = payload(&fx, "Write", write_input(&path, "{}"));
    assert!(evaluate(&write, &architect_env(&fx), || allowlist(&fx)).is_some());
}

/// 🔴 #8454 S2: a write through a symlink onto a guarded project settings
/// file is judged as a write to that file.
#[test]
fn egress_project_settings_symlink_is_followed() {
    let fx = egress_fixture();
    let path = project_settings(&fx, &pretty(&guarded_settings(json!({}))));
    let link = fx.cwd.join("alias.json");
    std::os::unix::fs::symlink(&path, &link).expect("symlink");
    assert!(pm_verdict(&fx, "Write", write_input(&link, "{}")).is_some());
    assert!(pm_bash(&fx, "echo '{}' > alias.json").is_some());
}

/// `command` as a Bash call, judged for a PM, a subagent payload and a
/// session with the subagent marker set.
fn verdicts_for_every_caller(fx: &Fixture, command: &str) -> [Option<String>; 3] {
    let call = payload(fx, "Bash", json!({ "command": command }));
    let mut dispatched = call.clone();
    dispatched["agent_id"] = json!("agent-9");
    let marked = HookEnv {
        sub_agent: true,
        ..pm_env(fx)
    };
    [
        evaluate(&call, &pm_env(fx), MpmConfig::default),
        evaluate(&dispatched, &pm_env(fx), MpmConfig::default),
        evaluate(&call, &marked, MpmConfig::default),
    ]
}

/// 🔴 #8454 S2 fix round (critic HIGH): an expansion after a literal parent
/// is judged by that parent, and one in the first segment by a literal last
/// component. Only an operand with no placeable parent stays allowed.
#[test]
fn egress_anchor_delete_with_an_expansion_is_judged_by_its_parent() {
    let fx = egress_fixture();
    for command in [
        "rm ~/.trusty-tools/trusty-mpm/$F",
        "rm ~/.trusty-mpm/$X",
        "rm \"$D/config.yaml\"",
        "rm \"$HOME/.trusty-mpm/twin/armed/$R\"",
        "rm -f ~/.trusty-tools/trusty-mpm/`cat f`",
    ] {
        for (caller, verdict) in verdicts_for_every_caller(&fx, command)
            .into_iter()
            .enumerate()
        {
            assert!(verdict.is_some(), "caller {caller} allowed: {command}");
        }
    }
    for command in [
        "rm $F",
        "rm \"$D/$X\"",
        "rm build/$F",
        "rm -f \"$TMPDIR/x.log\"",
    ] {
        assert_eq!(pm_bash(&fx, command), None, "denied: {command}");
    }
}

/// 🔴 #8454 S2 fix round (Fail-Open Check): a glob whose literal parent does
/// not resolve could delete an anchor, so it is denied.
#[test]
fn egress_anchor_delete_through_a_looped_glob_parent_is_denied() {
    let fx = egress_fixture();
    let looped = fx.cwd.join("loop");
    std::os::unix::fs::symlink(&looped, &looped).expect("self symlink");
    let reason = pm_bash(&fx, "rm -rf loop/*").expect("denied");
    assert!(reason.contains("does not resolve"), "{reason}");
}

/// 🔴 #8454 S2 fix round (critic MEDIUM): deleting, moving or copying over a
/// guarded project's `.claude` directory is a write to both settings files;
/// edits inside it that leave them alone stay allowed.
#[test]
fn egress_project_settings_claude_dir_operations_are_denied() {
    let fx = egress_fixture();
    project_settings(&fx, &pretty(&guarded_settings(json!({}))));
    std::fs::write(fx.cwd.join(".claude/notes.md"), "n").expect("write note");
    for command in [
        "rm -rf .claude",
        "rm -f .claude/*",
        "rm .claude/settings*.json",
        "mv .claude old-claude",
        "cp -r /tmp/x/.claude .",
        "cp -rT /tmp/x .claude",
    ] {
        let reason = pm_bash(&fx, command).unwrap_or_else(|| panic!("allowed: {command}"));
        assert!(reason.contains("#8454"), "{command}: {reason}");
    }
    for command in [
        "rm .claude/notes.md",
        "rm -rf .claude/worktrees/x",
        "cp a.txt .claude/notes.md",
        "cp a.txt .claude/",
    ] {
        assert_eq!(pm_bash(&fx, command), None, "denied: {command}");
    }
}

/// 🔴 #8454 S2 fix round: a `.claude` tree moved in that sets
/// `disableAllHooks` is denied even where the project carries no entry yet.
#[test]
fn egress_project_settings_incoming_claude_tree_disabling_hooks_is_denied() {
    let fx = egress_fixture();
    let evil = fx.cwd.join("evil/.claude");
    std::fs::create_dir_all(&evil).expect("mkdir evil");
    std::fs::write(
        evil.join("settings.local.json"),
        "{\"disableAllHooks\": true}",
    )
    .expect("write evil");
    assert!(pm_bash(&fx, "mv evil/.claude .").is_some());
    std::fs::write(evil.join("settings.local.json"), "{}").expect("rewrite evil");
    assert_eq!(pm_bash(&fx, "mv evil/.claude ."), None);
}

/// 🔴 #8454 S2 (Bob ruling Ded): a write or delete denied only because a
/// protected name sits in a directory the guard cannot place says so, and
/// tells the caller to retry with the absolute path.
#[test]
fn egress_anchor_protected_name_in_an_unknown_directory_names_the_retry() {
    let fx = egress_fixture();
    let reason = pm_bash(&fx, "cd deploy && rm config.yaml").expect("denied");
    assert!(
        reason.contains("matches a protected name in a directory the guard cannot place"),
        "{reason}"
    );
    assert!(reason.contains("Retry with the absolute path"), "{reason}");
}
