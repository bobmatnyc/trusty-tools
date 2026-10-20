//! Unit tests for the #8928 step 7 protected-path check.

use std::path::{Path, PathBuf};

use serde_json::json;

use super::{ProtectedEnv, evaluate};
use crate::commands::pm_guard_protected_hooks::{frontmatter_hooks, hook_named_paths};

/// A scratch home and project, canonicalized.
struct Fx {
    _dir: tempfile::TempDir,
    root: PathBuf,
    home: PathBuf,
    project: PathBuf,
}

impl Fx {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = std::fs::canonicalize(dir.path()).expect("canonical");
        let home = root.join("home");
        let project = root.join("project");
        std::fs::create_dir_all(&home).expect("mkdir home");
        std::fs::create_dir_all(project.join(".claude")).expect("mkdir project");
        Self {
            _dir: dir,
            root,
            home,
            project,
        }
    }

    fn env(&self) -> ProtectedEnv {
        ProtectedEnv {
            home: Some(self.home.clone()),
            project_dir: Some(self.project.clone()),
            claude_config_dir: Some(self.root.join("ccd")),
            path: Some(self.root.join("path-bin").into_os_string()),
        }
    }

    fn write(&self, path: &Path, content: &str) -> Option<String> {
        let input = json!({ "file_path": path.display().to_string(), "content": content });
        evaluate("Write", Some(&input), Some(&self.project), &self.env())
    }
}

fn denied(verdict: Option<String>, what: &str) {
    let reason = verdict.unwrap_or_else(|| panic!("{what} must be denied"));
    assert!(reason.contains("#8928"), "{what}: {reason}");
}

#[test]
fn each_protected_file_class_is_denied() {
    let fx = Fx::new();
    let p = &fx.project;
    for target in [
        p.join(".claude/settings.json"),
        p.join(".claude/SETTINGS.LOCAL.JSON"),
        p.join("sub/.mcp.json"),
        p.join(".git/config"),
        p.join(".git/worktrees/w1/config.worktree"),
        p.join(".git/hooks/pre-push"),
        p.join(".git/modules/m/hooks/post-checkout"),
        fx.root.join("ccd/settings.json"),
        fx.root.join("ccd/.claude.json"),
        fx.home.join(".claude/settings.local.json"),
        fx.home.join(".claude.json"),
        fx.home
            .join(".trusty-tools/trusty-mpm/claude-config/settings.json"),
        // `..` is applied before the name is judged.
        p.join("docs/../.claude/settings.json"),
    ] {
        denied(fx.write(&target, "{}"), &target.display().to_string());
    }
    // Every edit tool is judged, NotebookEdit by its notebook path.
    let target = p.join(".mcp.json").display().to_string();
    for (tool, input) in [
        (
            "Edit",
            json!({ "file_path": target, "old_string": "a", "new_string": "b" }),
        ),
        ("MultiEdit", json!({ "file_path": target, "edits": [] })),
        (
            "NotebookEdit",
            json!({ "notebook_path": target, "new_source": "x" }),
        ),
    ] {
        denied(
            evaluate(tool, Some(&input), Some(p), &fx.env()),
            &format!("{tool} on .mcp.json"),
        );
    }
}

#[test]
fn a_symlink_onto_a_protected_file_is_denied() {
    let fx = Fx::new();
    let link = fx.project.join("innocent.json");
    std::os::unix::fs::symlink(fx.project.join(".claude/settings.local.json"), &link)
        .expect("symlink");
    denied(
        fx.write(&link, "{}"),
        "a dangling link onto settings.local.json",
    );
}

#[test]
fn an_ordinary_write_is_allowed() {
    let fx = Fx::new();
    let p = &fx.project;
    for target in [
        p.join("src/main.rs"),
        p.join(".claude/notes.md"),
        p.join(".claude/settings.json.bak"),
        p.join("config"),
        p.join(".github/workflows/ci.yml"),
        fx.home.join(".claude/projects/x/memory/MEMORY.md"),
        fx.root.join("ccd/plans/plan.md"),
    ] {
        assert_eq!(fx.write(&target, "x"), None, "{}", target.display());
    }
    // Tools that do not write are never judged here.
    let input = json!({ "file_path": p.join(".mcp.json").display().to_string() });
    assert_eq!(evaluate("Read", Some(&input), Some(p), &fx.env()), None);
}

#[test]
fn every_fail_closed_arm_denies() {
    let fx = Fx::new();
    let env = fx.env();
    let p = fx.project.as_path();
    // No target path, and an empty one.
    denied(
        evaluate("Write", Some(&json!({})), Some(p), &env),
        "no target",
    );
    denied(evaluate("Write", None, Some(p), &env), "no input");
    let empty = json!({ "file_path": "" });
    denied(
        evaluate("Write", Some(&empty), Some(p), &env),
        "empty target",
    );
    // A relative target with no working directory, and with an empty one.
    let rel = json!({ "file_path": "notes.md", "content": "x" });
    denied(
        evaluate("Write", Some(&rel), None, &env),
        "relative, no cwd",
    );
    denied(
        evaluate("Write", Some(&rel), Some(Path::new("")), &env),
        "relative, empty cwd",
    );
    // A symlink loop does not resolve.
    let a = fx.project.join("loop-a");
    std::os::unix::fs::symlink(fx.project.join("loop-b"), &a).expect("link a");
    std::os::unix::fs::symlink(&a, fx.project.join("loop-b")).expect("link b");
    denied(fx.write(&a.join("x.md"), "x"), "a symlink loop");
    // A settings file that does not parse hides the paths its hooks name.
    let settings = fx.project.join(".claude/settings.local.json");
    std::fs::write(&settings, "{ not json").expect("write malformed");
    denied(
        fx.write(&fx.project.join("notes.md"), "x"),
        "malformed settings",
    );
    std::fs::write(&settings, "[1]").expect("write non-object");
    denied(
        fx.write(&fx.project.join("notes.md"), "x"),
        "non-object settings",
    );
    std::fs::remove_file(&settings).expect("rm settings");
    // Skill Markdown: unreadable, unreproducible edit, unparseable frontmatter.
    let skill_dir = fx.project.join(".claude/skills/demo");
    std::fs::create_dir_all(skill_dir.join("DIR.md")).expect("mkdir DIR.md");
    denied(
        fx.write(&skill_dir.join("DIR.md"), "x"),
        "unreadable markdown",
    );
    let skill = skill_dir.join("SKILL.md");
    std::fs::write(&skill, "---\nname: s\n---\n").expect("write skill");
    let input = json!({
        "file_path": skill.display().to_string(),
        "old_string": "absent text",
        "new_string": "hooks: {}",
    });
    denied(
        evaluate("Edit", Some(&input), Some(p), &env),
        "an edit that cannot be reproduced",
    );
    denied(
        fx.write(&skill, "---\nname: [unclosed\nhooks: x\n---\n"),
        "unparseable frontmatter naming hooks",
    );
}

#[test]
fn hook_and_status_line_paths_are_listed() {
    let fx = Fx::new();
    let settings = json!({
        "hooks": { "PreToolUse": [{ "matcher": "", "hooks": [
            { "type": "command", "command": "\"$CLAUDE_PROJECT_DIR\"/.claude/hooks/a.sh --x" },
            { "type": "command", "command": "FOO=~/bin/b node /opt/tool/c.js" },
            { "type": "prompt", "prompt": "/not/a/command" },
        ]}]},
        "statusLine": { "type": "command", "command": "$HOME/bin/status" },
    });
    std::fs::write(
        fx.project.join(".claude/settings.json"),
        settings.to_string(),
    )
    .expect("write settings");
    let named = hook_named_paths(&fx.env(), Some(&fx.project)).expect("readable");
    for want in [
        fx.project.join(".claude/hooks/a.sh"),
        fx.home.join("bin/b"),
        PathBuf::from("/opt/tool/c.js"),
        fx.home.join("bin/status"),
    ] {
        assert!(named.contains(&want), "{} in {named:?}", want.display());
    }
    assert!(
        !named.contains(&PathBuf::from("/not/a/command")),
        "{named:?}"
    );
    denied(
        fx.write(&fx.project.join(".claude/hooks/a.sh"), "x"),
        "a hook script",
    );
    denied(fx.write(&fx.home.join("bin/status"), "x"), "the statusLine");
}

#[test]
fn a_markdown_hook_is_judged_by_the_result() {
    let fx = Fx::new();
    let agent = fx.home.join(".claude/agents/a.md");
    let hooked = "---\nname: a\nhooks:\n  Stop: []\n---\nbody\n";
    denied(fx.write(&agent, hooked), "a user-tier agent gaining hooks");
    let ccd_skill = fx.root.join("ccd/skills/s/SKILL.md");
    denied(
        fx.write(&ccd_skill, hooked),
        "a CLAUDE_CONFIG_DIR skill gaining hooks",
    );
    // A file that already carries the same hooks may change its body.
    std::fs::create_dir_all(agent.parent().expect("dir")).expect("mkdir");
    std::fs::write(&agent, hooked).expect("write agent");
    let body = json!({
        "file_path": agent.display().to_string(),
        "old_string": "body",
        "new_string": "new body",
    });
    assert_eq!(
        evaluate("Edit", Some(&body), Some(&fx.project), &fx.env()),
        None
    );
    // Changing the hooks is denied; removing them is not.
    denied(
        fx.write(&agent, "---\nname: a\nhooks:\n  Stop: [x]\n---\nbody\n"),
        "changed hooks",
    );
    assert_eq!(fx.write(&agent, "---\nname: a\n---\nbody\n"), None);
    // MultiEdit applies every edit before judging.
    std::fs::write(&agent, "---\nname: a\n---\nbody\n").expect("write plain agent");
    let multi = json!({
        "file_path": agent.display().to_string(),
        "edits": [
            { "old_string": "body", "new_string": "b2" },
            { "old_string": "name: a\n", "new_string": "name: a\nhooks: {Stop: []}\n" },
        ],
    });
    denied(
        evaluate("MultiEdit", Some(&multi), Some(&fx.project), &fx.env()),
        "a MultiEdit adding hooks",
    );
    // Markdown elsewhere is never parsed for hooks.
    assert_eq!(fx.write(&fx.project.join("docs/a.md"), hooked), None);
    assert_eq!(frontmatter_hooks("no frontmatter"), Ok(None));
    assert_eq!(frontmatter_hooks("---\nname: x\n---\n"), Ok(None));
    assert!(frontmatter_hooks("---\nhooks: {}\n---\n").is_ok_and(|h| h.is_some()));
}

const HOOKED: &str = "---\nname: s\nhooks:\n  Stop: []\n---\n";

/// #8928 fix round: APFS folds `ſ` (U+017F) to `s` and `K` (U+212A) to `k`,
/// so those spellings open the protected file and must be denied.
#[test]
fn unicode_case_folds_cannot_dodge_the_rule() {
    let fx = Fx::new();
    let p = &fx.project;
    for target in [
        p.join(".claude/\u{17f}ettings.local.json"),
        p.join(".claude/\u{17f}ettings.json"),
        p.join(".mcp.j\u{17f}on"),
        p.join(".git/hoo\u{212a}s/pre-push"),
        p.join(".git/config"),
        fx.home.join(".claude/\u{17f}ettings.json"),
    ] {
        denied(fx.write(&target, "{}"), &target.display().to_string());
    }
    let skill = p.join(".claude/\u{17f}kills/x/SKILL.md");
    denied(
        fx.write(&skill, HOOKED),
        "a hooked skill under .claude/ſkills",
    );
}

/// #8928 fix round: plugin manifests and the scripts their hooks run live
/// under `<user config dir>/plugins/`.
#[test]
fn plugin_files_are_protected() {
    let fx = Fx::new();
    for target in [
        fx.root.join("ccd/plugins/p/hooks/hooks.json"),
        fx.home.join(".claude/plugins/p/scripts/run.sh"),
        fx.home
            .join(".trusty-tools/trusty-mpm/claude-config/plugins/cache/x.json"),
    ] {
        denied(fx.write(&target, "{}"), &target.display().to_string());
    }
}

/// #8928 fix round: a relative path in a hook command resolves against the
/// project root, and so does a bare argument naming a file there.
#[test]
fn relative_hook_paths_resolve_against_the_project() {
    let fx = Fx::new();
    std::fs::create_dir_all(fx.project.join("scripts")).expect("mkdir scripts");
    std::fs::write(fx.project.join("hook-arg.sh"), "#!/bin/sh\n").expect("write arg");
    let settings = json!({ "hooks": { "Stop": [{ "hooks": [
        { "type": "command", "command": "./scripts/hook.sh --x" },
        { "type": "command", "command": "bash hook-arg.sh" },
        { "type": "command", "command": "sh .claude/hooks/rel.sh" },
    ]}]}});
    std::fs::write(
        fx.project.join(".claude/settings.json"),
        settings.to_string(),
    )
    .expect("write settings");
    for target in [
        fx.project.join("scripts/hook.sh"),
        fx.project.join("hook-arg.sh"),
        fx.project.join(".claude/hooks/rel.sh"),
    ] {
        denied(fx.write(&target, "x"), &target.display().to_string());
    }
}

/// #8928 fix round: a command named by a skill's or agent's frontmatter
/// `hooks:` runs outside the sandbox like a settings hook.
#[test]
fn frontmatter_hook_commands_are_protected() {
    let fx = Fx::new();
    let skill = fx.project.join(".claude/skills/s/SKILL.md");
    std::fs::create_dir_all(skill.parent().expect("dir")).expect("mkdir skill");
    let script = fx.root.join("bin/fm-hook.sh");
    std::fs::write(
        &skill,
        format!(
            "---\nname: s\nhooks:\n  PreToolUse:\n    - matcher: Bash\n      hooks:\n        - \
             type: command\n          command: {} --check\n---\nbody\n",
            script.display()
        ),
    )
    .expect("write skill");
    let agent = fx.root.join("ccd/agents/a.md");
    std::fs::create_dir_all(agent.parent().expect("dir")).expect("mkdir agent");
    let agent_script = fx.root.join("bin/agent-hook.sh");
    std::fs::write(
        &agent,
        format!(
            "---\nname: a\nhooks:\n  Stop:\n    - hooks:\n        - type: command\n          \
             command: {}\n---\n",
            agent_script.display()
        ),
    )
    .expect("write agent");
    for target in [&script, &agent_script] {
        denied(fx.write(target, "x"), &target.display().to_string());
    }
}

/// #8928 fix round: the fold maps the APFS-equivalent spellings onto ASCII.
#[test]
fn case_folding_matches_apfs() {
    use crate::commands::pm_guard_trust_anchor_paths::{eq_fold, fold_case, starts_with_ci};
    assert_eq!(fold_case("\u{17f}ettings.JSON"), "settings.json");
    assert_eq!(fold_case("hoo\u{212a}s"), "hooks");
    assert_eq!(fold_case("\u{fb06}ate"), "state");
    assert!(eq_fold(".mcp.j\u{17f}on", ".MCP.json"));
    assert!(!eq_fold("settings.json", "settings.jsonx"));
    assert!(starts_with_ci(
        Path::new("/h/.CLAUDE/\u{17f}kills/x"),
        Path::new("/h/.claude/skills")
    ));
}

/// #8928 fix round: a bare hook program is the file `PATH` resolves.
#[test]
fn a_bare_hook_program_is_resolved_on_path() {
    let fx = Fx::new();
    let bin = fx.root.join("path-bin");
    std::fs::create_dir_all(&bin).expect("mkdir bin");
    std::fs::write(bin.join("hook-prog"), "#!/bin/sh\n").expect("write prog");
    std::fs::write(bin.join("wrapped-prog"), "#!/bin/sh\n").expect("write prog");
    let settings = json!({ "hooks": { "Stop": [{ "hooks": [
        { "type": "command", "command": "hook-prog inbox-check" },
        { "type": "command", "command": "true && env A=1 wrapped-prog" },
    ]}]}});
    std::fs::write(
        fx.project.join(".claude/settings.json"),
        settings.to_string(),
    )
    .expect("write settings");
    denied(fx.write(&bin.join("hook-prog"), "x"), "a PATH program");
    denied(
        fx.write(&bin.join("wrapped-prog"), "x"),
        "a program after env",
    );
    // An argument is not looked up on PATH.
    std::fs::write(bin.join("inbox-check"), "x").expect("write arg");
    assert_eq!(fx.write(&bin.join("inbox-check"), "x"), None);
}
