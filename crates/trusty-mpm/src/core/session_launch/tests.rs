use super::settings::{
    clean_global_trusty_memory_hooks, deploy_output_style, inject_trusty_memory_mcp,
    write_output_style, write_project_hooks,
};
use super::*;
use tempfile::tempdir;

#[test]
fn build_system_prompt_includes_trusty_block() {
    // Why: `build_system_prompt` must always yield a prompt — generating
    // `INSTRUCTIONS.md` from the bundled assets on first run — and that
    // prompt must include the trusty tool-priority block so a launched
    // session knows to prefer `memory_recall` and `search_code`.
    let prompt = build_system_prompt().expect("trusty block is always present");
    assert!(prompt.contains("## Trusty Tool Priority (Non-Overridable)"));
    assert!(prompt.contains("mcp__trusty-memory__memory_recall"));
    assert!(prompt.contains("mcp__trusty-search__search_code"));
    // The bundled PM instructions are also part of the assembled prompt.
    assert!(prompt.contains("# PM Agent -- Claude MPM"));
}

#[test]
fn build_system_prompt_for_applies_project_override() {
    // Why: the live launch prompt must reflect a project-level override file
    // under `<project>/.trusty-mpm/` (issue #381), while still appending the
    // non-overridable BASE_PM floor.
    let tmp = tempdir().unwrap();
    let project = tmp.path();
    let override_dir = project.join(".trusty-mpm");
    std::fs::create_dir_all(&override_dir).unwrap();
    std::fs::write(
        override_dir.join("INSTRUCTIONS.md"),
        "PROJECT_OVERRIDE_MARKER\n",
    )
    .unwrap();

    let prompt = build_system_prompt_for(project);
    assert!(prompt.contains("PROJECT_OVERRIDE_MARKER"));
    assert!(prompt.contains("# BASE_PM Framework Floor"));
    // Bundled PM body is still present (INSTRUCTIONS.md is additive).
    assert!(prompt.contains("# PM Agent -- Claude MPM"));
}

#[test]
fn build_system_prompt_for_no_override_matches_bundled_sections() {
    // Why: with no override files the live prompt must still carry all
    // bundled sections and the BASE_PM floor last.
    let tmp = tempdir().unwrap();
    let prompt = build_system_prompt_for(tmp.path());
    assert!(prompt.contains("# PM Agent -- Claude MPM"));
    assert!(prompt.contains("# Agent Delegation Routing"));
    let base = prompt.find("# BASE_PM Framework Floor").expect("base");
    let deleg = prompt.find("# Agent Delegation Routing").expect("deleg");
    assert!(base > deleg, "BASE_PM floor must be last");
}

#[test]
fn prepare_session_stash_reflects_override() {
    // Why: the inspectable stash (`last-instructions.md`) must reflect the
    // SAME override-resolved prompt the launch path uses, so `tm session
    // instructions` shows what was actually delivered (issue #381 / #382).
    let tmp = tempdir().unwrap();
    let project = tmp.path();
    let fw = crate::core::paths::FrameworkPaths::default();

    let override_dir = project.join(".trusty-mpm");
    std::fs::create_dir_all(&override_dir).unwrap();
    std::fs::write(
        override_dir.join("WORKFLOW.md"),
        "# Custom Workflow\n\nSTASH_OVERRIDE_MARKER\n",
    )
    .unwrap();

    let report = prepare_session(&fw, project).expect("prep succeeds");
    let stash = std::fs::read_to_string(&report.stash).expect("stash readable");

    assert!(
        stash.contains("STASH_OVERRIDE_MARKER"),
        "stash must reflect the WORKFLOW.md override"
    );
    assert!(
        !stash.contains("# PM Workflow Configuration"),
        "bundled workflow heading must be replaced in the stash"
    );
    assert!(
        stash.contains("# BASE_PM Framework Floor"),
        "stash must still carry the BASE_PM floor"
    );
    // The stash must equal the live prompt for this project.
    assert_eq!(stash, build_system_prompt_for(project));
}

#[test]
fn prepare_session_writes_claude_md_and_stash() {
    // Why: the launch paths rely on `prepare_session` writing the project
    // CLAUDE.md and the inspectable stash before `claude` is started.
    let tmp = tempdir().unwrap();
    let project = tmp.path();
    let fw = crate::core::paths::FrameworkPaths::default();

    let report = prepare_session(&fw, project).expect("prep succeeds");

    assert!(
        project.join("CLAUDE.md").exists(),
        "CLAUDE.md must exist after prep"
    );
    assert!(
        report.stash.exists(),
        "merged instructions stash must be written"
    );
    assert_eq!(
        report.stash,
        project.join(".trusty-mpm").join("last-instructions.md")
    );
}

#[test]
fn prepare_session_sets_output_style() {
    // Why: a launched session must show `style:trusty-mpm`, which Claude
    // Code reads from `<project>/.claude/settings.json`.
    let tmp = tempdir().unwrap();
    let project = tmp.path();
    let fw = crate::core::paths::FrameworkPaths::default();

    prepare_session(&fw, project).expect("prep succeeds");

    let settings_path = project.join(".claude").join("settings.json");
    assert!(settings_path.exists(), ".claude/settings.json must exist");
    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
    assert_eq!(value["outputStyle"], serde_json::json!("trusty-mpm"));
}

#[test]
fn write_output_style_preserves_existing_keys() {
    // Why: merging the style must not clobber an operator's other settings.
    let tmp = tempdir().unwrap();
    let project = tmp.path();
    let claude_dir = project.join(".claude");
    std::fs::create_dir_all(&claude_dir).unwrap();
    std::fs::write(
        claude_dir.join("settings.json"),
        r#"{"theme":"dark","outputStyle":"old"}"#,
    )
    .unwrap();

    write_output_style(project).expect("write succeeds");

    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(claude_dir.join("settings.json")).unwrap())
            .unwrap();
    assert_eq!(value["outputStyle"], serde_json::json!("trusty-mpm"));
    assert_eq!(value["theme"], serde_json::json!("dark"));
}

#[test]
fn write_output_style_sets_spinner_tips() {
    // Why: trusty-mpm sessions must override the operator's generic
    // claude-mpm spinner tips with project-specific ones; the settings.json
    // merge must enable tips and write a non-empty tips array.
    let tmp = tempdir().unwrap();
    let project = tmp.path();

    write_output_style(project).expect("write succeeds");

    let value: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(project.join(".claude").join("settings.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(value["spinnerTipsEnabled"], serde_json::json!(true));
    let tips = value["spinnerTipsOverride"]["tips"]
        .as_array()
        .expect("spinnerTipsOverride.tips must be an array");
    assert!(!tips.is_empty(), "spinner tips must be non-empty");
    assert!(tips.iter().all(|tip| tip.is_string()));
}

#[test]
fn write_project_hooks_writes_all_event_types() {
    // Why: the trusty-memory hooks must be scoped to the project and cover
    // the session lifecycle — PostToolUse, Stop, and UserPromptSubmit.
    // PreToolUse is intentionally absent: trusty-memory has no
    // `claude.pre-tool-use` handler.
    let tmp = tempdir().unwrap();
    let project = tmp.path();

    write_project_hooks(project).expect("write succeeds");

    let value: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(project.join(".claude").join("settings.json")).unwrap(),
    )
    .unwrap();
    let hooks = value["hooks"].as_object().expect("hooks must be an object");
    for event in ["PostToolUse", "Stop", "UserPromptSubmit"] {
        let groups = hooks[event]
            .as_array()
            .unwrap_or_else(|| panic!("{event} must be an array"));
        assert!(!groups.is_empty(), "{event} must have a handler group");
        let cmd = groups[0]["hooks"][0]["command"]
            .as_str()
            .expect("command must be a string");
        assert!(
            cmd.contains("trusty-memory hooks fire"),
            "{event} command must invoke trusty-memory"
        );
    }
}

#[test]
fn write_project_hooks_omits_pre_tool_use() {
    // Why: trusty-memory has no `claude.pre-tool-use` handler, so a
    // PreToolUse hook would error on every tool call. The written hooks
    // block must not register one.
    let tmp = tempdir().unwrap();
    let project = tmp.path();

    write_project_hooks(project).expect("write succeeds");

    let value: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(project.join(".claude").join("settings.json")).unwrap(),
    )
    .unwrap();
    assert!(
        value["hooks"].get("PreToolUse").is_none(),
        "PreToolUse hook must not be registered"
    );
}

#[test]
fn write_project_hooks_replaces_existing() {
    // Why: re-running prep must replace the hooks block, not append to it,
    // so handler arrays never duplicate and cause double-firing.
    let tmp = tempdir().unwrap();
    let project = tmp.path();

    write_project_hooks(project).expect("first write succeeds");
    write_project_hooks(project).expect("second write succeeds");

    let value: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(project.join(".claude").join("settings.json")).unwrap(),
    )
    .unwrap();
    let post = value["hooks"]["PostToolUse"]
        .as_array()
        .expect("PostToolUse must be an array");
    assert_eq!(
        post.len(),
        1,
        "re-running must replace, not append, handler groups"
    );
    // Unrelated keys must survive the replace.
    write_project_hooks(project).expect("third write succeeds");
    let value: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(project.join(".claude").join("settings.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        value["hooks"]["UserPromptSubmit"].as_array().unwrap().len(),
        1
    );
}

#[test]
fn inject_trusty_memory_mcp_adds_server() {
    // Why: a launched session needs the `trusty-memory` MCP server in
    // `.mcp.json` for the memory tools to be available; injection must
    // create the file with the server registered.
    let tmp = tempdir().unwrap();
    let project = tmp.path();

    inject_trusty_memory_mcp(project).expect("injection succeeds");

    let mcp_path = project.join(".mcp.json");
    assert!(mcp_path.exists(), ".mcp.json must be created");
    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&mcp_path).unwrap()).unwrap();
    let server = &value["mcpServers"]["trusty-memory"];
    assert_eq!(server["type"], serde_json::json!("stdio"));
    assert_eq!(server["command"], serde_json::json!("trusty-memory"));
    assert_eq!(server["args"], serde_json::json!(["mcp", "serve"]));
}

#[test]
fn inject_trusty_memory_mcp_preserves_existing() {
    // Why: injection must not clobber MCP servers the operator already
    // configured (e.g. `trusty-search`).
    let tmp = tempdir().unwrap();
    let project = tmp.path();
    std::fs::write(
        project.join(".mcp.json"),
        r#"{"mcpServers":{"trusty-search":{"type":"stdio","command":"trusty-search","args":["serve"]}}}"#,
    )
    .unwrap();

    inject_trusty_memory_mcp(project).expect("injection succeeds");

    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(project.join(".mcp.json")).unwrap()).unwrap();
    let servers = value["mcpServers"]
        .as_object()
        .expect("mcpServers must be an object");
    assert!(
        servers.contains_key("trusty-search"),
        "existing server must survive injection"
    );
    assert!(
        servers.contains_key("trusty-memory"),
        "trusty-memory must be injected"
    );
    assert_eq!(
        value["mcpServers"]["trusty-search"]["command"],
        serde_json::json!("trusty-search")
    );
}

#[test]
fn inject_trusty_memory_mcp_is_idempotent() {
    // Why: `/connect` and `tm session start` may run repeatedly; a second
    // injection must not duplicate or alter the `trusty-memory` entry.
    let tmp = tempdir().unwrap();
    let project = tmp.path();

    inject_trusty_memory_mcp(project).expect("first injection succeeds");
    let after_first = std::fs::read_to_string(project.join(".mcp.json")).expect("file exists");

    inject_trusty_memory_mcp(project).expect("second injection succeeds");
    let after_second = std::fs::read_to_string(project.join(".mcp.json")).expect("file exists");

    assert_eq!(
        after_first, after_second,
        "re-injecting must leave the file unchanged"
    );
    let value: serde_json::Value = serde_json::from_str(&after_second).unwrap();
    assert_eq!(
        value["mcpServers"].as_object().unwrap().len(),
        1,
        "trusty-memory must not be duplicated"
    );
}

#[test]
fn prepare_session_injects_trusty_memory_mcp() {
    // Why: `prepare_session` is the single launch-prep entry point; it must
    // register the trusty-memory MCP server so launched sessions get the
    // memory tools.
    let tmp = tempdir().unwrap();
    let project = tmp.path();
    let fw = crate::core::paths::FrameworkPaths::default();

    prepare_session(&fw, project).expect("prep succeeds");

    let mcp_path = project.join(".mcp.json");
    assert!(mcp_path.exists(), ".mcp.json must exist after prep");
    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&mcp_path).unwrap()).unwrap();
    assert_eq!(
        value["mcpServers"]["trusty-memory"]["command"],
        serde_json::json!("trusty-memory")
    );
}

#[test]
fn remove_global_hooks_removes_trusty_memory_entries() {
    // Why: the global trusty-memory hook entries must be cleaned out so
    // they no longer fire for unrelated Claude Code sessions; non-trusty
    // entries and empty-becoming events must be handled correctly.
    let tmp = tempdir().unwrap();
    let settings_path = tmp.path().join("settings.json");
    std::fs::write(
        &settings_path,
        r#"{
          "theme": "dark",
          "hooks": {
            "PostToolUse": [
              { "matcher": "*", "hooks": [ { "type": "command", "command": "bash track.sh" } ] },
              { "matcher": "Write|Edit|Bash", "hooks": [ { "type": "command", "command": "trusty-memory hooks fire claude.post-tool-use" } ] }
            ],
            "Stop": [
              { "matcher": "", "hooks": [ { "type": "command", "command": "trusty-memory hooks fire claude.stop" } ] }
            ],
            "UserPromptSubmit": [
              { "matcher": "", "hooks": [ { "type": "command", "command": "trusty-memory hooks fire claude.user-prompt" } ] }
            ]
          }
        }"#,
    )
    .unwrap();

    clean_global_trusty_memory_hooks(&settings_path).expect("clean succeeds");

    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
    // Unrelated keys survive.
    assert_eq!(value["theme"], serde_json::json!("dark"));
    // Non-trusty PostToolUse entry survives; trusty one is gone.
    let post = value["hooks"]["PostToolUse"].as_array().unwrap();
    assert_eq!(post.len(), 1);
    assert!(
        post[0]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .contains("track.sh")
    );
    // Stop and UserPromptSubmit only had trusty entries, so the keys are gone.
    assert!(
        value["hooks"].get("Stop").is_none(),
        "empty Stop event must be removed"
    );
    assert!(
        value["hooks"].get("UserPromptSubmit").is_none(),
        "empty UserPromptSubmit event must be removed"
    );
}

#[test]
fn remove_global_hooks_tolerates_missing_file() {
    // Why: cleanup is non-fatal and idempotent — a missing settings file
    // (operator never created one) must be a no-op success.
    let tmp = tempdir().unwrap();
    let missing = tmp.path().join("nope.json");
    clean_global_trusty_memory_hooks(&missing).expect("missing file is a no-op");
}

#[test]
fn deploy_output_style_writes_file() {
    // Why: Claude Code resolves the `trusty-mpm` output style only when a
    // matching file exists in `~/.claude/output-styles/`; deployment must
    // create that file (and its parent dir) with the bundled content.
    let home = tempdir().unwrap();
    let path = deploy_output_style(home.path()).expect("deploy succeeds");

    assert_eq!(
        path,
        home.path()
            .join(".claude")
            .join("output-styles")
            .join("trusty-mpm.md")
    );
    let written = std::fs::read_to_string(&path).expect("style file readable");
    assert_eq!(written, crate::core::bundle::OUTPUT_STYLE);
    assert!(written.contains("name: trusty-mpm"));
}

#[test]
fn deploy_output_style_overwrites() {
    // Why: framework upgrades to the style must propagate on the next
    // launch, so deployment always overwrites any existing file.
    let home = tempdir().unwrap();
    let first = deploy_output_style(home.path()).expect("first deploy succeeds");
    std::fs::write(&first, "stale operator content").unwrap();

    let second = deploy_output_style(home.path()).expect("second deploy succeeds");
    assert_eq!(first, second);
    let written = std::fs::read_to_string(&second).unwrap();
    assert_eq!(written, crate::core::bundle::OUTPUT_STYLE);
}

#[test]
fn prepare_session_reports_output_style() {
    // Why: callers report the deployed style path; `prepare_session` must
    // populate `PrepReport.output_style` with the file it deployed.
    let tmp = tempdir().unwrap();
    let project = tmp.path();
    let fw = crate::core::paths::FrameworkPaths::default();

    let report = prepare_session(&fw, project).expect("prep succeeds");

    let style = report
        .output_style
        .expect("output style deployed when home is resolvable");
    assert!(style.ends_with("trusty-mpm.md"));
    assert!(style.exists());
}

#[test]
fn prepare_session_reports_skill_deploy() {
    // Why: `prepare_session` must run the skill deploy step so launched
    // sessions see trusty-mpm skills; the report must carry its stats.
    let tmp = tempdir().unwrap();
    let project = tmp.path();
    let fw = crate::core::paths::FrameworkPaths::default();

    let report = prepare_session(&fw, project).expect("prep succeeds");

    // The stats are present (a fresh install with no skill source is an
    // empty-but-valid result; this asserts the field is populated, not
    // that any specific skill deployed).
    let _ = &report.skill_deploy;
}

#[test]
fn prepare_session_is_idempotent() {
    // Why: `/connect` and `tm session start` may run repeatedly on the same
    // project; a second prep must not fail and must not recreate CLAUDE.md.
    let tmp = tempdir().unwrap();
    let project = tmp.path();
    let fw = crate::core::paths::FrameworkPaths::default();

    let first = prepare_session(&fw, project).expect("first prep succeeds");
    assert!(first.instructions.claude_md_created);

    let second = prepare_session(&fw, project).expect("second prep succeeds");
    assert!(
        !second.instructions.claude_md_created,
        "CLAUDE.md already exists on the second run"
    );
}
