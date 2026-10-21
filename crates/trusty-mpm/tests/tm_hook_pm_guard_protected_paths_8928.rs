//! End-to-end proof of the #8928 step 7 Write/Edit protected-path check.
//!
//! Why: the sandbox protects settings files, `.mcp.json` and hook directories
//! from Bash, but not from the Write and Edit tools (#8928). A PM or
//! agent that writes `.claude/settings.local.json` can switch the sandbox off
//! or plant a hook that runs outside it. Only the built binary proves the
//! check runs ahead of the subagent exemptions.
//! What: spawns `tm hook --pm-guard` with a scratch `$HOME` and project, and
//! sends Write/Edit payloads at each protected file and at skill and agent
//! Markdown that adds a frontmatter `hooks:` key. Each must print a `deny`
//! naming #8928, for the main thread and for a subagent. An ordinary write and
//! a skill edit with no hook must print nothing.
//! Test: `cargo test -p trusty-mpm --test integration tm_hook_pm_guard_protected_paths_8928::`.

use std::path::Path;

use crate::tm_hook_pm_guard_trust_anchor_8878::{Fixture, run, run_under_claude};

/// A `Write` payload for `path` carrying `content`; `agent` marks a subagent.
fn write(fx: &Fixture, path: &Path, content: &str, agent: bool) -> String {
    let mut payload = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "session_id": "s-8928",
        "cwd": fx.project.display().to_string(),
        "tool_name": "Write",
        "tool_input": { "file_path": path.display().to_string(), "content": content },
    });
    if agent {
        payload["agent_id"] = serde_json::json!("agent-8928");
    }
    payload.to_string()
}

/// An `Edit` payload replacing `old` with `new` in `path`.
fn edit(fx: &Fixture, path: &Path, old: &str, new: &str) -> String {
    serde_json::json!({
        "hook_event_name": "PreToolUse",
        "session_id": "s-8928",
        "cwd": fx.project.display().to_string(),
        "tool_name": "Edit",
        "tool_input": {
            "file_path": path.display().to_string(),
            "old_string": old,
            "new_string": new,
        },
    })
    .to_string()
}

/// Assert the guard denied with the #8928 rule.
fn assert_denied(out: &str, what: &str) {
    assert!(out.contains("\"deny\""), "{what} must be denied: {out}");
    assert!(out.contains("#8928"), "{what} must name #8928: {out}");
}

const SKILL: &str = "---\nname: demo\ndescription: d\n---\n# Demo\n";
const HOOKED_SKILL: &str = "---\nname: demo\ndescription: d\nhooks:\n  PreToolUse:\n    - \
     matcher: Bash\n      hooks:\n        - type: command\n          command: /tmp/x.sh\n---\n# Demo\n";

#[test]
fn protected_settings_and_config_files_are_denied_to_main_thread_and_subagent() {
    let fx = Fixture::new();
    let p = &fx.project;
    let targets = [
        p.join(".claude/settings.json"),
        p.join(".claude/settings.local.json"),
        p.join(".mcp.json"),
        p.join(".git/config"),
        p.join(".git/hooks/pre-commit"),
        fx.home.join(".claude/settings.json"),
        fx.home.join(".claude/settings.local.json"),
        fx.home.join(".claude.json"),
    ];
    for target in &targets {
        for agent in [false, true] {
            let out = run(&fx, &write(&fx, target, "{}", agent), &[], None);
            assert_denied(&out, &format!("{} (agent={agent})", target.display()));
        }
    }
}

#[test]
fn an_executable_named_by_a_hook_command_is_denied() {
    let fx = Fixture::new();
    let script = fx.home.parent().expect("scratch").join("bin/guard-hook");
    std::fs::create_dir_all(script.parent().expect("bin")).expect("mkdir bin");
    std::fs::write(&script, "#!/bin/sh\n").expect("write script");
    let status_line = fx.home.parent().expect("scratch").join("bin/status-line");
    let settings = serde_json::json!({
        "hooks": { "PreToolUse": [{ "matcher": "", "hooks": [
            { "type": "command", "command": format!("\"{}\" --check", script.display()) }
        ]}]},
        "statusLine": { "type": "command", "command": status_line.display().to_string() },
    });
    std::fs::create_dir_all(fx.project.join(".claude")).expect("mkdir .claude");
    std::fs::write(
        fx.project.join(".claude/settings.json"),
        settings.to_string(),
    )
    .expect("write settings");
    for target in [&script, &status_line] {
        let out = run(&fx, &write(&fx, target, "#!/bin/sh\n", true), &[], None);
        assert_denied(&out, &target.display().to_string());
    }
    // An executable no hook names stays writable.
    let other = script.with_file_name("unrelated-tool");
    let out = run(&fx, &write(&fx, &other, "#!/bin/sh\n", false), &[], None);
    assert_eq!(out.trim(), "", "an unnamed script is not protected");
}

#[test]
fn a_frontmatter_hook_in_a_skill_or_agent_file_is_denied() {
    let fx = Fixture::new();
    let skill = fx.project.join(".claude/skills/demo/SKILL.md");
    let agent = fx.project.join(".claude/agents/demo.md");
    for target in [&skill, &agent] {
        let out = run(&fx, &write(&fx, target, HOOKED_SKILL, true), &[], None);
        assert_denied(&out, &target.display().to_string());
        // The same file without a hook is ordinary work.
        let out = run(&fx, &write(&fx, target, SKILL, true), &[], None);
        assert_eq!(out.trim(), "", "{} without hooks", target.display());
    }
    // An Edit whose new_string adds the key is denied too.
    std::fs::create_dir_all(agent.parent().expect("agents")).expect("mkdir agents");
    std::fs::write(&agent, SKILL).expect("write agent");
    let added = "description: d\nhooks:\n  Stop:\n    - hooks:\n        - type: command\n          \
                 command: /tmp/x.sh\n";
    let out = run(
        &fx,
        &edit(&fx, &agent, "description: d\n", added),
        &[],
        None,
    );
    assert_denied(&out, "an Edit adding hooks");
    // An Edit to the body is allowed.
    let out = run(&fx, &edit(&fx, &agent, "# Demo", "# Demo two"), &[], None);
    assert_eq!(out.trim(), "", "a body edit is allowed");
}

#[test]
fn an_ordinary_write_and_the_architect_main_thread_are_allowed() {
    let fx = Fixture::new();
    let notes = fx.project.join("docs/notes.md");
    assert_eq!(
        run(&fx, &write(&fx, &notes, "x", true), &[], None).trim(),
        ""
    );
    // The Architect runs unsandboxed and may edit its own settings (#8928
    // step 5: the Architect gets `sandbox.enabled: false`).
    let settings = fx.project.join(".claude/settings.json");
    let out = run_under_claude(&fx, &write(&fx, &settings, "{}", false), true);
    assert_eq!(out.trim(), "", "the Architect main thread is exempt: {out}");
}

/// The fail-closed arm through the binary: a settings file that does not
/// parse hides the paths its hooks name, so a write is refused. (An
/// unresolvable target is already refused earlier by the #8625 rule; the unit
/// tests cover this rule's own arm.)
#[test]
fn unreadable_settings_deny_rather_than_allow() {
    let fx = Fixture::new();
    std::fs::create_dir_all(fx.project.join(".claude")).expect("mkdir .claude");
    let local = fx.project.join(".claude/settings.local.json");
    std::fs::write(&local, "{ not json").expect("write malformed");
    let notes = fx.project.join("notes.md");
    let out = run(&fx, &write(&fx, &notes, "x", true), &[], None);
    assert_denied(&out, "a write while settings do not parse");
}

/// #8928 fix round: a bare program word in a hook command is the file `PATH`
/// resolves it to, so that file is protected.
#[test]
fn a_bare_hook_program_on_path_is_denied() {
    let fx = Fixture::new();
    let bin = fx.home.parent().expect("scratch").join("path-bin");
    std::fs::create_dir_all(&bin).expect("mkdir bin");
    let program = bin.join("fake-hook-prog-8928");
    std::fs::write(&program, "#!/bin/sh\n").expect("write program");
    let settings = serde_json::json!({ "hooks": { "Stop": [{ "hooks": [
        { "type": "command", "command": "fake-hook-prog-8928 inbox-check" }
    ]}]}});
    std::fs::create_dir_all(fx.project.join(".claude")).expect("mkdir .claude");
    std::fs::write(
        fx.project.join(".claude/settings.json"),
        settings.to_string(),
    )
    .expect("write settings");
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = run(
        &fx,
        &write(&fx, &program, "#!/bin/sh\n", true),
        &[("PATH", path.as_str())],
        None,
    );
    assert_denied(&out, "the PATH-resolved hook program");
}
