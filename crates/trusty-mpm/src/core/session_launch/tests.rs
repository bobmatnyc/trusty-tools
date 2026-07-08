use super::search_index::{
    index_is_fresh, inject_trusty_search_mcp, register_project_index, trusty_search_mcp_value,
};
use super::settings::{
    clean_global_trusty_memory_hooks, deploy_output_style, inject_trusty_memory_mcp,
    is_stale_bare_statusline_command, is_stale_statusline_command, preseed_workspace_trust,
    resolve_statusline_binary_with, trusty_memory_mcp_value, write_output_style,
    write_project_hooks, write_status_line,
};
use super::*;
use tempfile::tempdir;

/// Why: env-mutating tests previously restored the var by hand at the end of the
/// test body, so a panic between set and restore leaked process-global state
/// into sibling `#[serial]` tests. This guard restores the prior value (or
/// removes it) in `Drop`, making cleanup panic-safe.
/// What: on construction it snapshots the current value and sets the new one;
/// on drop it restores the snapshot (or removes the var if it was unset).
/// Test: used by `register_project_index_returns_derived_id`; correctness is
/// observable via that serial test passing without leaking the override env var.
struct EnvVarGuard {
    key: &'static str,
    prev: Option<String>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: &std::path::Path) -> Self {
        let prev = std::env::var(key).ok();
        // SAFETY: env-mutating tests using this guard are tagged `#[serial]`, so
        // no other thread races the set/restore. Restore happens in `Drop`.
        unsafe {
            std::env::set_var(key, value);
        }
        Self { key, prev }
    }

    /// Set a string-valued env var for the duration of a serial test (#1605).
    ///
    /// Why: the palace-pinning tests must set `TRUSTY_MEMORY_PALACE` to a plain
    /// string override; the path-based `set` would round-trip through a
    /// `PathBuf` unnecessarily.
    /// What: snapshots the prior value and sets `key=value`; restored in `Drop`.
    /// Test: used by `inject_trusty_memory_mcp_override_env_wins`.
    fn set_str(key: &'static str, value: &str) -> Self {
        let prev = std::env::var(key).ok();
        // SAFETY: serialized by `#[serial]`; restored in `Drop`.
        unsafe {
            std::env::set_var(key, value);
        }
        Self { key, prev }
    }

    /// Clear an env var for the duration of a serial test, restoring it on drop.
    ///
    /// Why: the palace-derivation tests must run with no ambient
    /// `TRUSTY_MEMORY_PALACE` override (which would otherwise win over the
    /// derived slug and make the assertions non-hermetic).
    /// What: snapshots the prior value and removes the var; restored in `Drop`.
    /// Test: used by `inject_trusty_memory_mcp_pins_palace_from_repo_url`.
    fn clear(key: &'static str) -> Self {
        let prev = std::env::var(key).ok();
        // SAFETY: serialized by `#[serial]`; restored in `Drop`.
        unsafe {
            std::env::remove_var(key);
        }
        Self { key, prev }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        // SAFETY: see `set` — serialized by `#[serial]`.
        unsafe {
            match self.prev.take() {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }
}

#[test]
fn build_system_prompt_includes_trusty_block() {
    // Why: `build_system_prompt` must always yield a prompt — generating
    // `INSTRUCTIONS.md` from the bundled assets on first run — and that
    // prompt must include the trusty tool-priority block so a launched
    // session knows to prefer `memory_recall` and `search`.
    let prompt = build_system_prompt().expect("trusty block is always present");
    assert!(prompt.contains("## Trusty Tool Priority (Non-Overridable)"));
    assert!(prompt.contains("mcp__trusty-memory__memory_recall"));
    assert!(prompt.contains("mcp__trusty-search__search"));
    // The bundled PM instructions are also part of the assembled prompt.
    assert!(prompt.contains("# PM Agent -- Trusty MPM"));
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
    assert!(prompt.contains("# PM Agent -- Trusty MPM"));
}

#[test]
fn build_system_prompt_for_no_override_matches_bundled_sections() {
    // Why: with no override files the live prompt must still carry all
    // bundled sections and the BASE_PM floor last.
    let tmp = tempdir().unwrap();
    let prompt = build_system_prompt_for(tmp.path());
    assert!(prompt.contains("# PM Agent -- Trusty MPM"));
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
    //
    // Determinism (issue #1409): HR-4 added output-style injection at the
    // `build_system_prompt_for_with_style` seam, gated on whether `claude` is
    // installed/new enough. We therefore route through the native-pinned seams
    // (`prepare_session_with_style_and_native` /
    // `build_system_prompt_for_with_style_and_native`) and assert the invariant
    // under BOTH `native_supported = true` (no injection) AND `false`
    // (injection fires). This removes the dependence on the host's `claude` that
    // made this test pass locally but FAIL on CI (where `claude` is absent → the
    // launch prompt was injected but the stash was not, so the two diverged).
    for native_supported in [true, false] {
        // A fresh tmp_home/project per iteration so the second run does not read
        // back a stash written by the first. Dedicated tmp_home keeps parallel
        // runs from racing on the shared ~/.claude/agents manifest.
        let tmp_home = tempdir().unwrap();
        let tmp = tempdir().unwrap();
        let project = tmp.path();
        let fw = crate::core::paths::FrameworkPaths::under(tmp_home.path());

        let override_dir = project.join(".trusty-mpm");
        std::fs::create_dir_all(&override_dir).unwrap();
        std::fs::write(
            override_dir.join("WORKFLOW.md"),
            "# Custom Workflow\n\nSTASH_OVERRIDE_MARKER\n",
        )
        .unwrap();

        let report = prepare_session_with_style_and_native(&fw, project, None, native_supported)
            .expect("prep succeeds");
        let stash = std::fs::read_to_string(&report.stash).expect("stash readable");

        assert!(
            stash.contains("STASH_OVERRIDE_MARKER"),
            "stash must reflect the WORKFLOW.md override (native_supported={native_supported})"
        );
        assert!(
            !stash.contains("# PM Workflow Configuration"),
            "bundled workflow heading must be replaced in the stash (native_supported={native_supported})"
        );
        assert!(
            stash.contains("# BASE_PM Framework Floor"),
            "stash must still carry the BASE_PM floor (native_supported={native_supported})"
        );
        // The CORE INVARIANT: the persisted stash must equal the exact prompt the
        // launcher would deliver under the SAME injection decision — in both
        // claude-present (native) and claude-absent (injected) environments.
        assert_eq!(
            stash,
            build_system_prompt_for_with_style_and_native(project, None, native_supported),
            "stash must equal the launch prompt (native_supported={native_supported})"
        );
        // When injection fires, the stash must actually carry the injected style
        // block; when native is supported it must NOT — proving the flag drives
        // the stash content, not the host.
        if native_supported {
            assert!(
                !stash.contains(crate::core::output_style::INJECTED_STYLE_HEADING),
                "native-capable: stash must NOT carry the injected style block"
            );
        } else {
            assert!(
                stash.contains(crate::core::output_style::INJECTED_STYLE_HEADING),
                "native-incapable: stash MUST carry the injected style block"
            );
        }
    }
}

#[test]
fn prepare_session_writes_claude_md_and_stash() {
    // Why: the launch paths rely on `prepare_session` writing the project
    // CLAUDE.md and the inspectable stash before `claude` is started.
    // Use a dedicated tmp_home so parallel tests never race on the shared
    // ~/.claude/agents manifest (each test needs its own claude_agents_dir).
    let tmp_home = tempdir().unwrap();
    let tmp = tempdir().unwrap();
    let project = tmp.path();
    let fw = crate::core::paths::FrameworkPaths::under(tmp_home.path());

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
fn prepare_session_deploys_project_tier_output_style() {
    // Why (#2125 item 2): the daemon managed-spawn path launches `claude`
    // with `--setting-sources project,local`, excluding the `user` tier the
    // home-dir deploy lands in. Without a project-tier copy the `outputStyle`
    // id written into `<project>/.claude/settings.json` cannot resolve.
    let tmp_home = tempdir().unwrap();
    let tmp = tempdir().unwrap();
    let project = tmp.path();
    let fw = crate::core::paths::FrameworkPaths::under(tmp_home.path());

    prepare_session(&fw, project).expect("prep succeeds");

    let project_style = project.join(".claude/output-styles/trusty-mpm.md");
    assert!(
        project_style.exists(),
        "project-tier output style file must be deployed: {}",
        project_style.display()
    );
}

#[test]
fn prepare_session_self_heals_missing_skill_source() {
    // #1917: `fw.skills` (the framework skill *source* dir `skill_source_dir()`
    // falls back to) starts out completely absent here — simulating a machine
    // that never ran `tm install` under the current binary. Before the fix,
    // `deploy_skills_filtered` would silently deploy zero skills from an
    // absent source with no error surfaced anywhere; session prep must now
    // self-heal it first.
    let tmp_home = tempdir().unwrap();
    let tmp = tempdir().unwrap();
    let project = tmp.path();
    let fw = crate::core::paths::FrameworkPaths::under(tmp_home.path());
    assert!(!fw.skills.exists(), "precondition: no prior tm install ran");

    let report = prepare_session(&fw, project).expect("prep succeeds");

    assert!(
        !report.skill_deploy.deployed.is_empty(),
        "session prep must self-heal the missing skill source and deploy at \
         least one skill; got {:?}",
        report.skill_deploy
    );
    assert!(fw.skills.join("tm-doctor.md").exists());
}

#[test]
fn prepare_session_self_heals_renamed_skill_source() {
    // #1917: a pre-rename `~/.trusty-mpm/framework/skills/` (stale content
    // left by an old binary, no matching bundle stamp) must be pruned and
    // refreshed automatically during session prep — not left for a manual
    // `tm install --force` to notice and fix.
    let tmp_home = tempdir().unwrap();
    let tmp = tempdir().unwrap();
    let project = tmp.path();
    let fw = crate::core::paths::FrameworkPaths::under(tmp_home.path());
    std::fs::create_dir_all(&fw.skills).unwrap();
    std::fs::write(fw.skills.join("mpm-old-skill.md"), "stale\n").unwrap();

    let report = prepare_session(&fw, project).expect("prep succeeds");

    assert!(
        !fw.skills.join("mpm-old-skill.md").exists(),
        "the stale pre-rename file must be pruned during self-heal"
    );
    assert!(
        !report.skill_deploy.deployed.is_empty(),
        "renamed/stale skill source must self-heal and deploy current skills"
    );
}

// Issue #2149 roster-deploy-failure-continues coverage lives in the sibling
// `tests_roster.rs` file (split out to keep this file under the 1500-SLOC
// test-file cap, mirroring the `doctor_output_style.rs` / `doctor_fs_checks.rs`
// split pattern already used elsewhere in this crate).

/// Why (issue #1904 stretch goal): `prepare_session_inner` emits discrete
/// `provisioning_stage` events (DeployingAgents/DeployingSkills/
/// BuildingInstructions/ConfiguringMcp) so the daemon's SSE stream can drive
/// real step-by-step progress in the `tm` CLI, instead of one opaque wait.
/// This is testable without a live daemon/tmux/git-clone: `emit()` reads a
/// `tokio::task_local` that `provisioning_stage::scoped` installs, and
/// `prepare_session` is a plain sync function we can call from inside that
/// scope in a `#[tokio::test]`.
/// What: wraps `prepare_session` in a scope backed by a fresh broadcast
/// channel, drains every event the call emitted, and asserts the four
/// session_launch-owned stages appear, IN ORDER (other stages —
/// CloningRepo/CreatingTmuxSession/LaunchingRuntime/Complete — are emitted
/// elsewhere in the call chain, not by `prepare_session_inner`, so they are
/// correctly absent here).
/// Test: this is the test.
#[tokio::test]
async fn prepare_session_emits_stage_events_in_order() {
    use crate::core::provisioning_stage::{ProvisioningStage, StageEmitter, scoped};

    let tmp_home = tempdir().unwrap();
    let tmp = tempdir().unwrap();
    let project = tmp.path();
    let fw = crate::core::paths::FrameworkPaths::under(tmp_home.path());

    let (tx, mut rx) = tokio::sync::broadcast::channel(32);
    let emitter = StageEmitter::new("test-session", "https://github.com/acme/widgets", tx);

    scoped(emitter, async {
        prepare_session(&fw, project).expect("prep succeeds");
    })
    .await;

    let mut stages = Vec::new();
    while let Ok(value) = rx.try_recv() {
        assert_eq!(value["kind"], "provisioning_stage");
        assert_eq!(value["repo_url"], "https://github.com/acme/widgets");
        stages.push(value["stage"].as_str().unwrap().to_string());
    }

    assert_eq!(
        stages,
        vec![
            ProvisioningStage::DeployingAgents.wire_name(),
            ProvisioningStage::DeployingSkills.wire_name(),
            ProvisioningStage::BuildingInstructions.wire_name(),
            ProvisioningStage::ConfiguringMcp.wire_name(),
        ],
        "prepare_session must emit exactly these four stages, in order"
    );
}

#[test]
fn prepare_session_sets_output_style() {
    // Why: a launched session must show `style:trusty-mpm`, which Claude
    // Code reads from `<project>/.claude/settings.json`.
    // Use a dedicated tmp_home so parallel tests never race on the shared
    // ~/.claude/agents manifest (each test needs its own claude_agents_dir).
    let tmp_home = tempdir().unwrap();
    let tmp = tempdir().unwrap();
    let project = tmp.path();
    let fw = crate::core::paths::FrameworkPaths::under(tmp_home.path());

    prepare_session(&fw, project).expect("prep succeeds");

    let settings_path = project.join(".claude").join("settings.json");
    assert!(settings_path.exists(), ".claude/settings.json must exist");
    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
    assert_eq!(value["outputStyle"], serde_json::json!("trusty-mpm"));
}

#[test]
fn prepare_session_writes_configured_style() {
    // Why: HR-4 — when `[style] active` is set in the framework config, the
    // launched session's settings.json must carry that id.
    let tmp_home = tempdir().unwrap();
    let tmp = tempdir().unwrap();
    let project = tmp.path();
    let fw = crate::core::paths::FrameworkPaths::under(tmp_home.path());

    // Seed `<root>/config.toml` with a teaching-mode selection.
    std::fs::create_dir_all(&fw.root).unwrap();
    std::fs::write(
        fw.config_toml(),
        "[style]\nactive = \"trusty-mpm-teacher\"\n",
    )
    .unwrap();

    prepare_session(&fw, project).expect("prep succeeds");

    let value: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(project.join(".claude").join("settings.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        value["outputStyle"],
        serde_json::json!("trusty-mpm-teacher")
    );
}

#[test]
fn prepare_session_explicit_style_overrides_config() {
    // Why: HR-4 — an explicit `--style` override beats the config `[style] active`
    // key for that launch.
    let tmp_home = tempdir().unwrap();
    let tmp = tempdir().unwrap();
    let project = tmp.path();
    let fw = crate::core::paths::FrameworkPaths::under(tmp_home.path());

    std::fs::create_dir_all(&fw.root).unwrap();
    std::fs::write(
        fw.config_toml(),
        "[style]\nactive = \"trusty-mpm-teacher\"\n",
    )
    .unwrap();

    crate::core::session_launch::prepare_session_with_style(
        &fw,
        project,
        Some("trusty-mpm-research"),
    )
    .expect("prep succeeds");

    let value: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(project.join(".claude").join("settings.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        value["outputStyle"],
        serde_json::json!("trusty-mpm-research")
    );
}

#[test]
fn prepare_session_unknown_style_falls_back_to_default() {
    // Why: DOC-17 — an unknown configured style must not fail the launch; it
    // falls back to the professional default.
    let tmp_home = tempdir().unwrap();
    let tmp = tempdir().unwrap();
    let project = tmp.path();
    let fw = crate::core::paths::FrameworkPaths::under(tmp_home.path());

    std::fs::create_dir_all(&fw.root).unwrap();
    std::fs::write(fw.config_toml(), "[style]\nactive = \"does-not-exist\"\n").unwrap();

    prepare_session(&fw, project).expect("prep succeeds");

    let value: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(project.join(".claude").join("settings.json")).unwrap(),
    )
    .unwrap();
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

    write_output_style(project, None).expect("write succeeds");

    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(claude_dir.join("settings.json")).unwrap())
            .unwrap();
    assert_eq!(value["outputStyle"], serde_json::json!("trusty-mpm"));
    assert_eq!(value["theme"], serde_json::json!("dark"));
}

#[test]
fn write_output_style_sets_active_style() {
    // Why: HR-4 — an explicitly resolved active style id must be written into
    // settings.json so a native-capable Claude Code applies it.
    let tmp = tempdir().unwrap();
    let project = tmp.path();

    write_output_style(project, Some("trusty-mpm-research")).expect("write succeeds");

    let value: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(project.join(".claude").join("settings.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        value["outputStyle"],
        serde_json::json!("trusty-mpm-research")
    );
}

#[test]
fn write_output_style_empty_falls_back_to_default() {
    // Why: a blank/whitespace id must not blank the outputStyle key; it falls
    // back to the professional default.
    let tmp = tempdir().unwrap();
    let project = tmp.path();

    write_output_style(project, Some("   ")).expect("write succeeds");

    let value: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(project.join(".claude").join("settings.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(value["outputStyle"], serde_json::json!("trusty-mpm"));
}

#[test]
fn write_output_style_sets_spinner_tips() {
    // Why: trusty-mpm sessions must override the operator's generic
    // claude-mpm spinner tips with project-specific ones; the settings.json
    // merge must enable tips and write a non-empty tips array.
    let tmp = tempdir().unwrap();
    let project = tmp.path();

    write_output_style(project, None).expect("write succeeds");

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
    // Why (#1270): the trusty-memory hooks must be scoped to the project and use
    // the canonical, real CLI surface — `UserPromptSubmit` → prompt-context and
    // `SessionStart` → inbox-check. The old `PostToolUse`/`Stop` events invoked
    // the nonexistent `hooks fire` subcommand and are gone.
    let tmp = tempdir().unwrap();
    let project = tmp.path();

    write_project_hooks(project).expect("write succeeds");

    let value: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(project.join(".claude").join("settings.json")).unwrap(),
    )
    .unwrap();
    let hooks = value["hooks"].as_object().expect("hooks must be an object");
    for event in ["UserPromptSubmit", "SessionStart"] {
        let groups = hooks[event]
            .as_array()
            .unwrap_or_else(|| panic!("{event} must be an array"));
        assert!(!groups.is_empty(), "{event} must have a handler group");
        let cmd = groups[0]["hooks"][0]["command"]
            .as_str()
            .expect("command must be a string");
        assert!(
            cmd.starts_with("trusty-memory "),
            "{event} command must invoke trusty-memory: {cmd}"
        );
    }
}

#[test]
fn write_project_hooks_uses_canonical_commands() {
    // Why (#1270): the hook commands MUST match the real trusty-memory CLI
    // (`prompt-context`, `inbox-check`) — never the bogus `hooks fire` form that
    // hard-blocked prompts with "unrecognized subcommand 'hooks'".
    let tmp = tempdir().unwrap();
    let project = tmp.path();

    write_project_hooks(project).expect("write succeeds");

    let value: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(project.join(".claude").join("settings.json")).unwrap(),
    )
    .unwrap();
    let raw = serde_json::to_string(&value).unwrap();
    assert!(
        !raw.contains("hooks fire"),
        "the broken `hooks fire` command must never be written: {raw}"
    );
    assert_eq!(
        value["hooks"]["UserPromptSubmit"][0]["hooks"][0]["command"],
        serde_json::json!("trusty-memory prompt-context")
    );
    assert_eq!(
        value["hooks"]["SessionStart"][0]["hooks"][0]["command"],
        serde_json::json!("trusty-memory inbox-check")
    );
}

#[test]
fn write_project_hooks_omits_post_tool_use_and_stop() {
    // Why (#1270): trusty-memory has no PostToolUse/Stop CLI hook surface, so
    // those events must not be registered (they previously invoked `hooks fire`,
    // which fails). Memory writes during a session flow through MCP tools.
    // NOTE (#1977): PreToolUse IS now registered — it carries the PM-enforcement
    // guard, not a trusty-memory hook — so it is intentionally excluded from
    // the absent list here (see `write_project_hooks_registers_pm_guard`).
    let tmp = tempdir().unwrap();
    let project = tmp.path();

    write_project_hooks(project).expect("write succeeds");

    let value: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(project.join(".claude").join("settings.json")).unwrap(),
    )
    .unwrap();
    for absent in ["PostToolUse", "Stop"] {
        assert!(
            value["hooks"].get(absent).is_none(),
            "{absent} hook must not be registered"
        );
    }
}

#[test]
fn write_project_hooks_registers_pm_guard() {
    // Why (#1977): managed PM sessions must register the PreToolUse enforcement
    // guard so the PM is blocked from editing code directly. The command must be
    // an absolute path (PATH-robust, per #1914) ending in `hook --pm-guard`.
    let tmp = tempdir().unwrap();
    let project = tmp.path();

    write_project_hooks(project).expect("write succeeds");

    let value: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(project.join(".claude").join("settings.json")).unwrap(),
    )
    .unwrap();
    let groups = value["hooks"]["PreToolUse"]
        .as_array()
        .expect("PreToolUse must be an array");
    assert_eq!(groups.len(), 1, "exactly one PreToolUse handler group");
    // Matcher `""` means the guard fires for every tool call.
    assert_eq!(groups[0]["matcher"], serde_json::json!(""));
    let cmd = groups[0]["hooks"][0]["command"]
        .as_str()
        .expect("command must be a string");
    assert!(
        cmd.ends_with(" hook --pm-guard"),
        "guard command must end with ' hook --pm-guard', got: {cmd}"
    );
    assert!(
        !cmd.starts_with("trusty-memory"),
        "PreToolUse must be the tm guard, not a trusty-memory hook: {cmd}"
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
    let ss = value["hooks"]["SessionStart"]
        .as_array()
        .expect("SessionStart must be an array");
    assert_eq!(
        ss.len(),
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

    inject_trusty_memory_mcp(project, None).expect("injection succeeds");

    let mcp_path = project.join(".mcp.json");
    assert!(mcp_path.exists(), ".mcp.json must be created");
    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&mcp_path).unwrap()).unwrap();
    let server = &value["mcpServers"]["trusty-memory"];
    assert_eq!(server["type"], serde_json::json!("stdio"));
    assert_eq!(server["command"], serde_json::json!("trusty-memory"));
    assert_eq!(server["args"], serde_json::json!(["serve", "--stdio"]));
}

#[test]
fn inject_trusty_memory_mcp_uses_serve_stdio() {
    // Why (#1270): the previous args `["mcp","serve"]` were invalid (no `mcp`
    // subcommand). The canonical stdio MCP invocation is `serve --stdio`.
    let tmp = tempdir().unwrap();
    let project = tmp.path();

    inject_trusty_memory_mcp(project, None).expect("injection succeeds");

    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(project.join(".mcp.json")).unwrap()).unwrap();
    let args = value["mcpServers"]["trusty-memory"]["args"]
        .as_array()
        .expect("args is an array");
    assert_eq!(
        args,
        &vec![serde_json::json!("serve"), serde_json::json!("--stdio")]
    );
    assert!(
        !args.contains(&serde_json::json!("mcp")),
        "must not use the nonexistent `mcp` subcommand"
    );
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

    inject_trusty_memory_mcp(project, None).expect("injection succeeds");

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
#[serial_test::serial]
fn inject_trusty_memory_mcp_is_idempotent() {
    // Why: `/connect` and `tm session start` may run repeatedly; a second
    // injection must not duplicate or alter the `trusty-memory` entry.
    // `#[serial]` + the guard below are required because `resolve_palace_slug`
    // reads the process-global `TRUSTY_MEMORY_PALACE` env var; a sibling serial
    // test (`inject_trusty_memory_mcp_override_env_wins`) sets it to
    // `"my-pinned-palace"`, and without serialisation + clearing the var here,
    // a race can produce different slugs on the first vs second injection, making
    // `after_first != after_second` non-deterministically (the "env-isolation
    // flake" observed on PR #1723 CI).
    let _env = EnvVarGuard::clear("TRUSTY_MEMORY_PALACE");
    let tmp = tempdir().unwrap();
    let project = tmp.path();

    inject_trusty_memory_mcp(project, None).expect("first injection succeeds");
    let after_first = std::fs::read_to_string(project.join(".mcp.json")).expect("file exists");

    inject_trusty_memory_mcp(project, None).expect("second injection succeeds");
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

// ---------------------------------------------------------------------------
// Issue #1605 — trusty-memory palace-slug pinning in managed-session injection.
// ---------------------------------------------------------------------------

/// Initialise a git repo at `dir` with `origin` pointing at `remote_url`.
///
/// Why: the palace-slug git-fallback path shells out to
/// `git -C <dir> config --get remote.origin.url`; exercising it needs a real
/// repo with a configured origin remote, not a network clone.
/// What: runs `git init`, `git remote add origin <url>` in `dir`. Skips
/// (returns `false`) when git is unavailable so the test degrades gracefully on
/// a git-less host rather than failing spuriously.
/// Test: used by `inject_trusty_memory_mcp_pins_palace_from_git_remote` and
/// `resolve_palace_slug_falls_back_to_git_remote` (it is the helper).
fn init_git_repo_with_origin(dir: &std::path::Path, remote_url: &str) -> bool {
    let init = std::process::Command::new("git")
        .arg("init")
        .arg("-q")
        .current_dir(dir)
        .status();
    match init {
        Ok(status) if status.success() => {}
        _ => return false,
    }
    let remote = std::process::Command::new("git")
        .args(["remote", "add", "origin", remote_url])
        .current_dir(dir)
        .status();
    matches!(remote, Ok(status) if status.success())
}

/// Why: `trusty_memory_mcp_value(Some(slug))` must embed
/// `env.TRUSTY_MEMORY_PALACE = <slug>` so the spawned trusty-memory resolves the
/// pinned project palace (issue #1605); the rest of the block must stay the
/// canonical `serve --stdio` stub.
/// Test: itself.
#[test]
fn trusty_memory_mcp_value_pins_palace() {
    let server = trusty_memory_mcp_value(Some("bobmatnyc-trusty-tools"));
    assert_eq!(server["type"], serde_json::json!("stdio"));
    assert_eq!(server["command"], serde_json::json!("trusty-memory"));
    assert_eq!(server["args"], serde_json::json!(["serve", "--stdio"]));
    assert_eq!(
        server["env"]["TRUSTY_MEMORY_PALACE"],
        serde_json::json!("bobmatnyc-trusty-tools")
    );
}

/// Why: when no slug can be derived the block must be byte-identical to the
/// pre-#1605 bare stub (no `env` key) so there is zero regression for sessions
/// without a resolvable project identity.
/// Test: itself.
#[test]
fn trusty_memory_mcp_value_bare() {
    let bare = trusty_memory_mcp_value(None);
    assert!(
        bare.get("env").is_none(),
        "no env key when no slug is derived"
    );
    // An empty / whitespace slug is also treated as "no pin".
    let empty = trusty_memory_mcp_value(Some("   "));
    assert!(
        empty.get("env").is_none(),
        "blank slug must not produce an env pin"
    );
}

/// Why (issue #1605 acceptance): a repo_url-cloned managed session must pin the
/// palace to the cloned-from repo's `owner-repo` slug, NOT the throwaway
/// `<session-id>` workspace basename. Passing an explicit `git_remote` (the
/// `LaunchParams`/`SessionRecord.repo_url`) must drive
/// `env.TRUSTY_MEMORY_PALACE`.
/// Test: itself — asserts the injected `.mcp.json` carries the expected slug.
#[test]
#[serial_test::serial]
fn inject_trusty_memory_mcp_pins_palace_from_repo_url() {
    // Guard: an ambient TRUSTY_MEMORY_PALACE override would win over the derived
    // slug. Clear it for the duration so the test is hermetic.
    let _guard = EnvVarGuard::clear("TRUSTY_MEMORY_PALACE");
    let tmp = tempdir().unwrap();
    // The workspace basename here is deliberately a session-id-like value; the
    // pin must come from the repo_url, not this directory name.
    let project = tmp.path().join("0c8f1a2b3c4d");
    std::fs::create_dir_all(&project).unwrap();

    inject_trusty_memory_mcp(&project, Some("git@github.com:bobmatnyc/trusty-tools.git"))
        .expect("injection succeeds");

    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(project.join(".mcp.json")).unwrap()).unwrap();
    assert_eq!(
        value["mcpServers"]["trusty-memory"]["env"]["TRUSTY_MEMORY_PALACE"],
        serde_json::json!("bobmatnyc-trusty-tools"),
        "palace must be pinned to the cloned-from owner-repo slug, not the workspace basename"
    );
}

/// Why (issue #1605): for a LOCAL-PATH session (`repo_url == None`) the injector
/// must fall back to the workspace's own `git remote get-url origin` so the
/// palace is still pinned by repo identity rather than the directory basename.
/// Test: itself — initialises a real repo with an origin remote and asserts the
/// derived slug is pinned.
#[test]
#[serial_test::serial]
fn inject_trusty_memory_mcp_pins_palace_from_git_remote() {
    let _guard = EnvVarGuard::clear("TRUSTY_MEMORY_PALACE");
    let tmp = tempdir().unwrap();
    let project = tmp.path().join("checkout-7e2");
    std::fs::create_dir_all(&project).unwrap();
    if !init_git_repo_with_origin(&project, "https://github.com/acme/widget.git") {
        eprintln!("skipping: git unavailable");
        return;
    }

    inject_trusty_memory_mcp(&project, None).expect("injection succeeds");

    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(project.join(".mcp.json")).unwrap()).unwrap();
    assert_eq!(
        value["mcpServers"]["trusty-memory"]["env"]["TRUSTY_MEMORY_PALACE"],
        serde_json::json!("acme-widget"),
        "local-path session must pin the palace from its own origin remote"
    );
}

/// Why (issue #1605): the operator `TRUSTY_MEMORY_PALACE` override is the
/// highest-precedence palace source; it must win over a parseable git remote so
/// an explicit pin is never silently overridden by repo identity.
/// Test: itself.
#[test]
#[serial_test::serial]
fn inject_trusty_memory_mcp_override_env_wins() {
    let _guard = EnvVarGuard::set_str("TRUSTY_MEMORY_PALACE", "my-pinned-palace");
    let tmp = tempdir().unwrap();
    let project = tmp.path();

    inject_trusty_memory_mcp(project, Some("git@github.com:bobmatnyc/trusty-tools.git"))
        .expect("injection succeeds");

    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(project.join(".mcp.json")).unwrap()).unwrap();
    assert_eq!(
        value["mcpServers"]["trusty-memory"]["env"]["TRUSTY_MEMORY_PALACE"],
        serde_json::json!("my-pinned-palace"),
        "the operator override must win over the git owner/repo slug"
    );
}

#[test]
fn prepare_session_injects_trusty_memory_mcp() {
    // Why: `prepare_session` is the single launch-prep entry point; it must
    // register the trusty-memory MCP server so launched sessions get the
    // memory tools.
    // Use a dedicated tmp_home so parallel tests never race on the shared
    // ~/.claude/agents manifest (each test needs its own claude_agents_dir).
    let tmp_home = tempdir().unwrap();
    let tmp = tempdir().unwrap();
    let project = tmp.path();
    let fw = crate::core::paths::FrameworkPaths::under(tmp_home.path());

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

// ──────────────────────────────────────────────
// trusty-search MCP injection (#1270 / step 4)
// ──────────────────────────────────────────────

#[test]
fn inject_trusty_search_mcp_adds_server() {
    // Why (#1270/step 4): spawned sessions need the code-search tools; the
    // server must be registered as `trusty-search serve` (stdio default).
    let tmp = tempdir().unwrap();
    let project = tmp.path();

    inject_trusty_search_mcp(project, None).expect("injection succeeds");

    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(project.join(".mcp.json")).unwrap()).unwrap();
    let server = &value["mcpServers"]["trusty-search"];
    assert_eq!(server["type"], serde_json::json!("stdio"));
    assert_eq!(server["command"], serde_json::json!("trusty-search"));
    assert_eq!(server["args"], serde_json::json!(["serve"]));
}

#[test]
fn inject_trusty_search_mcp_preserves_existing() {
    // Why: injecting trusty-search must not clobber an existing trusty-memory.
    let tmp = tempdir().unwrap();
    let project = tmp.path();
    inject_trusty_memory_mcp(project, None).expect("memory injection succeeds");

    inject_trusty_search_mcp(project, None).expect("search injection succeeds");

    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(project.join(".mcp.json")).unwrap()).unwrap();
    let servers = value["mcpServers"].as_object().expect("object");
    assert!(servers.contains_key("trusty-memory"), "memory must survive");
    assert!(
        servers.contains_key("trusty-search"),
        "search must be added"
    );
}

#[test]
fn inject_trusty_search_mcp_is_idempotent() {
    // Why: prep may run repeatedly; re-injecting must not change the file.
    let tmp = tempdir().unwrap();
    let project = tmp.path();

    inject_trusty_search_mcp(project, None).expect("first injection succeeds");
    let first = std::fs::read_to_string(project.join(".mcp.json")).unwrap();
    inject_trusty_search_mcp(project, None).expect("second injection succeeds");
    let second = std::fs::read_to_string(project.join(".mcp.json")).unwrap();

    assert_eq!(first, second, "re-injecting must leave the file unchanged");
}

#[test]
fn inject_trusty_search_mcp_pinned_is_idempotent() {
    // Why (#1373): re-running prep with the SAME pinned index id must not rewrite
    // or churn the `.mcp.json` — the pinned entry has to be stable across launches.
    let tmp = tempdir().unwrap();
    let project = tmp.path();

    inject_trusty_search_mcp(project, Some("my-project")).expect("first injection succeeds");
    let first = std::fs::read_to_string(project.join(".mcp.json")).unwrap();
    inject_trusty_search_mcp(project, Some("my-project")).expect("second injection succeeds");
    let second = std::fs::read_to_string(project.join(".mcp.json")).unwrap();

    assert_eq!(
        first, second,
        "re-injecting the same pinned id must leave the file unchanged"
    );
    // And the pin is still present/correct after the second pass.
    let value: serde_json::Value = serde_json::from_str(&second).unwrap();
    assert_eq!(
        value["mcpServers"]["trusty-search"]["args"],
        serde_json::json!(["serve", "--index", "my-project"])
    );
}

#[test]
fn inject_both_mcp_servers_coexist() {
    // Why (#1270/step 4): the end state must have BOTH servers so the spawned
    // session gets memory AND code search.
    let tmp = tempdir().unwrap();
    let project = tmp.path();

    inject_trusty_memory_mcp(project, None).expect("memory injection succeeds");
    inject_trusty_search_mcp(project, None).expect("search injection succeeds");

    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(project.join(".mcp.json")).unwrap()).unwrap();
    let servers = value["mcpServers"].as_object().expect("object");
    assert_eq!(servers.len(), 2, "exactly memory + search");
    assert_eq!(
        servers["trusty-memory"]["args"],
        serde_json::json!(["serve", "--stdio"])
    );
    assert_eq!(
        servers["trusty-search"]["args"],
        serde_json::json!(["serve"])
    );
}

// ──────────────────────────────────────────────
// trusty-search index pin (#1373)
// ──────────────────────────────────────────────

#[test]
fn trusty_search_mcp_value_pins_index() {
    // Why (#1373): when an index id is known the stub MUST pin the session via
    // `serve --index <id>` so a bare search resolves to the project's own index.
    let v = trusty_search_mcp_value(Some("trusty-tools"));
    assert_eq!(v["command"], serde_json::json!("trusty-search"));
    assert_eq!(
        v["args"],
        serde_json::json!(["serve", "--index", "trusty-tools"])
    );
}

#[test]
fn trusty_search_mcp_value_unpinned() {
    // Why (#1373 back-compat): a `None` (or blank) id yields the legacy bare
    // `serve` stub so the session still gets the tools.
    assert_eq!(
        trusty_search_mcp_value(None)["args"],
        serde_json::json!(["serve"])
    );
    assert_eq!(
        trusty_search_mcp_value(Some("   "))["args"],
        serde_json::json!(["serve"]),
        "a blank id must not pin"
    );
}

#[test]
fn inject_trusty_search_mcp_pins_index() {
    // Why (#1373): the injected `.mcp.json` entry must carry the pin so the
    // launched Claude session is scoped to its project index.
    let tmp = tempdir().unwrap();
    let project = tmp.path();

    inject_trusty_search_mcp(project, Some("my-project")).expect("injection succeeds");

    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(project.join(".mcp.json")).unwrap()).unwrap();
    assert_eq!(
        value["mcpServers"]["trusty-search"]["args"],
        serde_json::json!(["serve", "--index", "my-project"])
    );
}

#[test]
#[serial_test::serial]
fn register_project_index_returns_derived_id() {
    // Why (#1373): registration must derive the project's index id (git-root
    // basename, via the shared `trusty_common::derive_index_id`) AND remain
    // graceful when the trusty-search daemon is unreachable — it still returns
    // the id so the stub can be pinned. We force the daemon-down path by
    // pointing the data dir at an empty temp dir so `read_daemon_addr` finds no
    // `http_addr` file (and thus issues no HTTP POST). `#[serial]` because the
    // override env var is process-global.
    let data_dir = tempdir().unwrap();
    // Panic-safe restore: the guard restores/removes the override env var in its
    // `Drop`, so a panic in the assertions below never leaks it to sibling
    // serial tests.
    let _env = EnvVarGuard::set(
        trusty_common::data_dir::DATA_DIR_OVERRIDE_ENV,
        data_dir.path(),
    );

    // A git-rooted project: id == the git-root basename, even from a nested dir.
    let project = tempdir().unwrap();
    std::fs::create_dir_all(project.path().join(".git")).unwrap();
    let nested = project.path().join("crates/inner");
    std::fs::create_dir_all(&nested).unwrap();

    let id = register_project_index(&nested);
    let expected = trusty_common::derive_index_id(project.path());
    assert_eq!(id, Some(expected), "id is the git-root basename");
}

// ── index_is_fresh (#1908) ────────────────────────────────────────────────────
// Pure predicate behind `best_effort_trigger_reindex`'s freshness skip: these
// tests exercise it directly against synthetic `GET .../status` bodies so the
// rule is verified without a live trusty-search daemon.

#[test]
fn index_is_fresh_true_when_recently_indexed_with_chunks() {
    // Why: the whole point of the optimisation is to skip a redundant reindex
    // when the index already has content and was built recently.
    let now = chrono::Utc::now();
    let status = serde_json::json!({
        "chunk_count": 42,
        "last_indexed": now.to_rfc3339(),
    });
    assert!(index_is_fresh(&status));
}

#[test]
fn index_is_fresh_false_when_no_chunks() {
    // Why: a zero-chunk index is empty regardless of how recent `last_indexed`
    // claims to be — it must always be reindexed.
    let now = chrono::Utc::now();
    let status = serde_json::json!({
        "chunk_count": 0,
        "last_indexed": now.to_rfc3339(),
    });
    assert!(!index_is_fresh(&status));
}

#[test]
fn index_is_fresh_false_when_stale() {
    // Why: an index last built more than an hour ago should be refreshed, even
    // though it has chunks.
    let stale = chrono::Utc::now() - chrono::Duration::hours(2);
    let status = serde_json::json!({
        "chunk_count": 10,
        "last_indexed": stale.to_rfc3339(),
    });
    assert!(!index_is_fresh(&status));
}

#[test]
fn index_is_fresh_false_when_last_indexed_missing_or_malformed() {
    // Why: fail-open toward reindexing — a missing or unparsable timestamp
    // must never be treated as "fresh".
    assert!(!index_is_fresh(&serde_json::json!({ "chunk_count": 10 })));
    assert!(!index_is_fresh(&serde_json::json!({
        "chunk_count": 10,
        "last_indexed": "not-a-timestamp",
    })));
    assert!(!index_is_fresh(&serde_json::json!({})));
}

#[test]
fn prepare_session_injects_both_mcp_servers() {
    // Why (#1270/step 4): the single launch-prep entry point must wire BOTH the
    // memory and the search MCP servers.
    let tmp_home = tempdir().unwrap();
    let tmp = tempdir().unwrap();
    let project = tmp.path();
    let fw = crate::core::paths::FrameworkPaths::under(tmp_home.path());

    prepare_session(&fw, project).expect("prep succeeds");

    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(project.join(".mcp.json")).unwrap()).unwrap();
    assert_eq!(
        value["mcpServers"]["trusty-memory"]["command"],
        serde_json::json!("trusty-memory")
    );
    assert_eq!(
        value["mcpServers"]["trusty-search"]["command"],
        serde_json::json!("trusty-search")
    );
}

// ──────────────────────────────────────────────
// Workspace trust pre-seed (#1269)
// ──────────────────────────────────────────────

#[test]
fn preseed_trust_marks_directory() {
    // Why (#1269): the interactive session must not stall on the trust dialog;
    // seeding the per-dir entry in ~/.claude.json suppresses it.
    let tmp = tempdir().unwrap();
    let claude_json = tmp.path().join(".claude.json");
    let workspace = tmp.path().join("ws");

    preseed_workspace_trust(&claude_json, &workspace).expect("seed succeeds");

    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&claude_json).unwrap()).unwrap();
    let key = workspace.to_string_lossy().to_string();
    let entry = &value["projects"][&key];
    assert_eq!(entry["hasTrustDialogAccepted"], serde_json::json!(true));
    assert_eq!(
        entry["hasCompletedProjectOnboarding"],
        serde_json::json!(true)
    );
    assert!(
        entry["projectOnboardingSeenCount"].as_u64().unwrap() >= 1,
        "onboarding counter must be >= 1"
    );
}

#[test]
fn preseed_trust_preserves_other_keys() {
    // Why: the file holds OAuth/login data; seeding trust must not drop it.
    let tmp = tempdir().unwrap();
    let claude_json = tmp.path().join(".claude.json");
    let workspace = tmp.path().join("ws");
    std::fs::write(
        &claude_json,
        r#"{"oauthAccount":{"emailAddress":"r@1mc.io"},"projects":{"/other":{"hasTrustDialogAccepted":true}}}"#,
    )
    .unwrap();

    preseed_workspace_trust(&claude_json, &workspace).expect("seed succeeds");

    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&claude_json).unwrap()).unwrap();
    // OAuth survives untouched (issue #1269: OAuth must be preserved).
    assert_eq!(
        value["oauthAccount"]["emailAddress"],
        serde_json::json!("r@1mc.io")
    );
    // Pre-existing project survives.
    assert_eq!(
        value["projects"]["/other"]["hasTrustDialogAccepted"],
        serde_json::json!(true)
    );
    // New workspace is trusted.
    let key = workspace.to_string_lossy().to_string();
    assert_eq!(
        value["projects"][&key]["hasTrustDialogAccepted"],
        serde_json::json!(true)
    );
}

#[test]
fn preseed_trust_is_idempotent() {
    // Why: prep may run repeatedly; a second seed must not change the file.
    let tmp = tempdir().unwrap();
    let claude_json = tmp.path().join(".claude.json");
    let workspace = tmp.path().join("ws");

    preseed_workspace_trust(&claude_json, &workspace).expect("first seed");
    let first = std::fs::read_to_string(&claude_json).unwrap();
    preseed_workspace_trust(&claude_json, &workspace).expect("second seed");
    let second = std::fs::read_to_string(&claude_json).unwrap();

    assert_eq!(first, second, "re-seeding must leave the file unchanged");
}

#[test]
fn preseed_trust_leaves_malformed_file() {
    // Why (#1269): a malformed ~/.claude.json likely still holds OAuth state;
    // clobbering it would force a re-login. Seeding must bail out untouched.
    let tmp = tempdir().unwrap();
    let claude_json = tmp.path().join(".claude.json");
    let workspace = tmp.path().join("ws");
    let garbage = "{ this is not valid json ";
    std::fs::write(&claude_json, garbage).unwrap();

    preseed_workspace_trust(&claude_json, &workspace).expect("soft-fails to Ok");

    let after = std::fs::read_to_string(&claude_json).unwrap();
    assert_eq!(after, garbage, "malformed file must be left untouched");
}

#[test]
fn preseed_trust_enables_mcp_servers_from_mcp_json() {
    // Why (#1296): a spawned workspace ships a `.mcp.json` with multiple MCP
    // servers; Claude Code shows a blocking "new MCP servers found" dialog
    // unless the server names are pre-approved via `enabledMcpjsonServers` in
    // the project entry. Seeding trust must also seed that approval list.
    let tmp = tempdir().unwrap();
    let claude_json = tmp.path().join(".claude.json");
    let workspace = tmp.path().join("ws");
    std::fs::create_dir_all(&workspace).unwrap();
    // The project ships a `.mcp.json` with two servers.
    std::fs::write(
        workspace.join(".mcp.json"),
        r#"{"mcpServers":{"trusty-search":{"command":"trusty-search"},"trusty-memory":{"command":"trusty-memory"}}}"#,
    )
    .unwrap();

    preseed_workspace_trust(&claude_json, &workspace).expect("seed succeeds");

    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&claude_json).unwrap()).unwrap();
    let key = workspace.to_string_lossy().to_string();
    let enabled = value["projects"][&key]["enabledMcpjsonServers"]
        .as_array()
        .expect("enabledMcpjsonServers is an array");
    let mut names: Vec<&str> = enabled.iter().filter_map(|v| v.as_str()).collect();
    names.sort_unstable();
    assert_eq!(
        names,
        vec!["trusty-memory", "trusty-search"],
        "all .mcp.json server names must be pre-approved"
    );
}

#[test]
fn preseed_trust_enables_empty_when_no_mcp_json() {
    // Why (#1296): when the workspace has no `.mcp.json`, seeding must never
    // crash; it writes an empty approval list so the key is present and Claude
    // Code does not prompt.
    let tmp = tempdir().unwrap();
    let claude_json = tmp.path().join(".claude.json");
    let workspace = tmp.path().join("ws");
    std::fs::create_dir_all(&workspace).unwrap();

    preseed_workspace_trust(&claude_json, &workspace).expect("seed succeeds");

    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&claude_json).unwrap()).unwrap();
    let key = workspace.to_string_lossy().to_string();
    let enabled = value["projects"][&key]["enabledMcpjsonServers"]
        .as_array()
        .expect("enabledMcpjsonServers is an array");
    assert!(
        enabled.is_empty(),
        "no .mcp.json yields an empty approval list, not a crash"
    );
}

#[test]
fn prepare_session_preseeds_enabled_mcp_servers() {
    // Why (#1296): the single launch-prep entry point injects trusty-memory and
    // trusty-search into `.mcp.json`, then seeds trust. The pre-seeded trust
    // entry in ~/.claude.json must list BOTH injected servers under
    // `enabledMcpjsonServers` so the spawned session runs non-interactively.
    let tmp_home = tempdir().unwrap();
    let tmp = tempdir().unwrap();
    let project = tmp.path();
    let fw = crate::core::paths::FrameworkPaths::under(tmp_home.path());

    prepare_session(&fw, project).expect("prep succeeds");

    // prepare_session seeds the operator's real ~/.claude.json via
    // preseed_workspace_trust_home, so re-derive the per-workspace approval list
    // by reading .mcp.json directly and asserting both injected servers landed.
    let mcp: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(project.join(".mcp.json")).unwrap()).unwrap();
    let servers = mcp["mcpServers"].as_object().expect("mcpServers object");
    assert!(servers.contains_key("trusty-memory"));
    assert!(servers.contains_key("trusty-search"));

    // Seed an isolated ~/.claude.json to assert the approval list is derived
    // from the now-populated .mcp.json.
    let claude_json = tmp_home.path().join(".claude-iso.json");
    preseed_workspace_trust(&claude_json, project).expect("seed succeeds");
    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&claude_json).unwrap()).unwrap();
    let key = project.to_string_lossy().to_string();
    let enabled = value["projects"][&key]["enabledMcpjsonServers"]
        .as_array()
        .expect("enabledMcpjsonServers is an array");
    let mut names: Vec<&str> = enabled.iter().filter_map(|v| v.as_str()).collect();
    names.sort_unstable();
    assert_eq!(names, vec!["trusty-memory", "trusty-search"]);
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
fn deploy_output_style_writes_all_styles() {
    // Why: HR-4 — the operator may select any of the three bundled styles, so
    // ALL of them must land in ~/.claude/output-styles/ for the selection to
    // resolve in Claude Code.
    let home = tempdir().unwrap();
    deploy_output_style(home.path()).expect("deploy succeeds");

    let dir = home.path().join(".claude").join("output-styles");
    for style in crate::core::bundle::OUTPUT_STYLES {
        let path = dir.join(style.file_name);
        assert!(path.exists(), "{} must be deployed", style.file_name);
        let written = std::fs::read_to_string(&path).expect("style file readable");
        assert_eq!(written, style.content, "{} content matches", style.id);
    }
    // Sanity: exactly the three bundled styles are written.
    assert_eq!(crate::core::bundle::OUTPUT_STYLES.len(), 3);
}

#[test]
fn prepare_session_reports_output_style() {
    // Why: callers report the deployed style path; `prepare_session` must
    // populate `PrepReport.output_style` with the file it deployed.
    // Use a dedicated tmp_home so parallel tests never race on the shared
    // ~/.claude/agents manifest (each test needs its own claude_agents_dir).
    let tmp_home = tempdir().unwrap();
    let tmp = tempdir().unwrap();
    let project = tmp.path();
    let fw = crate::core::paths::FrameworkPaths::under(tmp_home.path());

    let report = prepare_session(&fw, project).expect("prep succeeds");

    let style = report
        .output_style
        .expect("output style deployed when home is resolvable");
    assert!(style.ends_with("trusty-mpm.md"));
    assert!(style.exists());
    // Issue #1860: the deploy must target the scoped `tmp_home`, never the
    // real `$HOME` — this is the regression the leaking test exposed.
    assert!(
        style.starts_with(tmp_home.path()),
        "output style must deploy under the injected FrameworkPaths base, got {}",
        style.display()
    );
}

#[test]
fn prepare_session_reports_skill_deploy() {
    // Why: `prepare_session` must run the skill deploy step so launched
    // sessions see trusty-mpm skills; the report must carry its stats.
    // Use a dedicated tmp_home so parallel tests never race on the shared
    // ~/.claude/agents manifest (each test needs its own claude_agents_dir).
    let tmp_home = tempdir().unwrap();
    let tmp = tempdir().unwrap();
    let project = tmp.path();
    let fw = crate::core::paths::FrameworkPaths::under(tmp_home.path());

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
    // Use a dedicated tmp_home so parallel tests never race on the shared
    // ~/.claude/agents manifest (each test needs its own claude_agents_dir).
    let tmp_home = tempdir().unwrap();
    let tmp = tempdir().unwrap();
    let project = tmp.path();
    let fw = crate::core::paths::FrameworkPaths::under(tmp_home.path());

    let first = prepare_session(&fw, project).expect("first prep succeeds");
    assert!(first.instructions.claude_md_created);

    let second = prepare_session(&fw, project).expect("second prep succeeds");
    assert!(
        !second.instructions.claude_md_created,
        "CLAUDE.md already exists on the second run"
    );
}

// ──────────────────────────────────────────────
// HR-2 — Manifest-driven harness provisioning
// ──────────────────────────────────────────────

/// Seed two bundled agent source files (a base + a leaf) under `fw.agents` so
/// the deploy step has deterministic content to filter.
///
/// Why: the manifest integration tests must assert WHICH agents deploy; seeding
/// a known two-agent set makes the include/exclude assertions deterministic
/// regardless of whether the host has the optional `agents/` submodule.
/// What: writes `base-engineer.md` and `rust-engineer.md` (the latter extends
/// the former) into the framework's bundled agent source dir.
/// Test: used by `prepare_session_manifest_*`.
fn seed_bundled_agents(fw: &crate::core::paths::FrameworkPaths) {
    std::fs::create_dir_all(&fw.agents).unwrap();
    std::fs::write(
        fw.agents.join("base-engineer.md"),
        "---\nname: base-engineer\nrole: base-engineer\n---\n\n# Base Eng\n\nBASE.\n",
    )
    .unwrap();
    std::fs::write(
        fw.agents.join("rust-engineer.md"),
        "---\nname: rust-engineer\nrole: engineer\nextends: base-engineer\n---\n\n# Rust\n\nLEAF.\n",
    )
    .unwrap();
}

#[test]
fn prepare_session_default_deploys_all_seeded_agents() {
    // Why: HR-2 must be regression-safe — with NO manifest present, the
    // compiled-in default reproduces today's behavior, deploying every bundled
    // agent. This proves "absent manifest = unchanged provisioning".
    let tmp_home = tempdir().unwrap();
    let tmp = tempdir().unwrap();
    let project = tmp.path();
    let fw = crate::core::paths::FrameworkPaths::under(tmp_home.path());
    // Force the bundled source so the test does not depend on an `agents/`
    // submodule resolved from the running binary's location.
    let mut fw = fw;
    fw.trusty_mpm_root = None;
    seed_bundled_agents(&fw);

    let report = prepare_session(&fw, project).expect("prep succeeds");

    // Both seeded agents deploy (default manifest selects all).
    assert!(
        report
            .deploy
            .deployed
            .contains(&"base-engineer.md".to_string())
    );
    assert!(
        report
            .deploy
            .deployed
            .contains(&"rust-engineer.md".to_string())
    );
    assert!(fw.claude_agents_dir().join("rust-engineer.md").exists());
}

#[test]
fn prepare_session_manifest_filters_agent_set() {
    // Why: HR-2 — a project manifest's `[agents] include` must restrict WHICH
    // agents the harness deploys.
    let tmp_home = tempdir().unwrap();
    let tmp = tempdir().unwrap();
    let project = tmp.path();
    let mut fw = crate::core::paths::FrameworkPaths::under(tmp_home.path());
    fw.trusty_mpm_root = None;
    seed_bundled_agents(&fw);

    // Project override manifest: only deploy rust-engineer.
    let manifest_dir = project.join(".trusty-mpm");
    std::fs::create_dir_all(&manifest_dir).unwrap();
    std::fs::write(
        manifest_dir.join("manifest.toml"),
        "[agents]\ninclude = [\"rust-engineer\"]\n",
    )
    .unwrap();

    let report = prepare_session(&fw, project).expect("prep succeeds");

    assert!(
        report
            .deploy
            .deployed
            .contains(&"rust-engineer.md".to_string()),
        "included agent must deploy"
    );
    assert!(
        !report
            .deploy
            .deployed
            .contains(&"base-engineer.md".to_string()),
        "excluded-by-omission agent must NOT deploy"
    );
    assert!(!fw.claude_agents_dir().join("base-engineer.md").exists());
}

#[test]
fn prepare_session_manifest_disables_mcp_server() {
    // Why: HR-2 — a manifest `[mcp] trusty_search = false` must suppress the
    // trusty-search MCP injection while leaving trusty-memory intact.
    let tmp_home = tempdir().unwrap();
    let tmp = tempdir().unwrap();
    let project = tmp.path();
    let mut fw = crate::core::paths::FrameworkPaths::under(tmp_home.path());
    fw.trusty_mpm_root = None;

    let manifest_dir = project.join(".trusty-mpm");
    std::fs::create_dir_all(&manifest_dir).unwrap();
    std::fs::write(
        manifest_dir.join("manifest.toml"),
        "[mcp]\ntrusty_search = false\n",
    )
    .unwrap();

    prepare_session(&fw, project).expect("prep succeeds");

    let mcp_path = project.join(".mcp.json");
    assert!(
        mcp_path.exists(),
        ".mcp.json must exist (trusty-memory wrote it)"
    );
    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&mcp_path).unwrap()).unwrap();
    assert!(
        value["mcpServers"].get("trusty-search").is_none(),
        "manifest disabled trusty-search; it must not be injected"
    );
    assert!(
        value["mcpServers"].get("trusty-memory").is_some(),
        "trusty-memory stays injected"
    );
}

#[test]
fn prepare_session_manifest_sets_default_style() {
    // Why: HR-2 — a manifest `[style] active` sets the default output style when
    // no `--style` flag and no `[style] active` config key override it.
    let tmp_home = tempdir().unwrap();
    let tmp = tempdir().unwrap();
    let project = tmp.path();
    let mut fw = crate::core::paths::FrameworkPaths::under(tmp_home.path());
    fw.trusty_mpm_root = None;

    let manifest_dir = project.join(".trusty-mpm");
    std::fs::create_dir_all(&manifest_dir).unwrap();
    std::fs::write(
        manifest_dir.join("manifest.toml"),
        "[style]\nactive = \"trusty-mpm-research\"\n",
    )
    .unwrap();

    prepare_session(&fw, project).expect("prep succeeds");

    let value: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(project.join(".claude").join("settings.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        value["outputStyle"],
        serde_json::json!("trusty-mpm-research")
    );
}

#[test]
fn prepare_session_config_style_overrides_manifest() {
    // Why: HR-2 precedence — the `[style] active` CONFIG key must win over the
    // manifest's `[style] active` (config > manifest > default).
    let tmp_home = tempdir().unwrap();
    let tmp = tempdir().unwrap();
    let project = tmp.path();
    let mut fw = crate::core::paths::FrameworkPaths::under(tmp_home.path());
    fw.trusty_mpm_root = None;

    // Config selects teacher; manifest selects research. Config must win.
    std::fs::create_dir_all(&fw.root).unwrap();
    std::fs::write(
        fw.config_toml(),
        "[style]\nactive = \"trusty-mpm-teacher\"\n",
    )
    .unwrap();
    let manifest_dir = project.join(".trusty-mpm");
    std::fs::create_dir_all(&manifest_dir).unwrap();
    std::fs::write(
        manifest_dir.join("manifest.toml"),
        "[style]\nactive = \"trusty-mpm-research\"\n",
    )
    .unwrap();

    prepare_session(&fw, project).expect("prep succeeds");

    let value: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(project.join(".claude").join("settings.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        value["outputStyle"],
        serde_json::json!("trusty-mpm-teacher"),
        "config [style] active must override the manifest's style"
    );
}

// ── write_status_line tests ───────────────────────────────────────────────────

/// Assert `cmd` is `"<absolute-path> statusline"` where the path is exactly
/// the current test binary's `current_exe()` (#1914: `write_status_line`
/// prefers `current_exe()` over a bare command).
///
/// Why the `canonicalize()` call: `resolve_statusline_binary` canonicalizes
/// `current_exe()` best-effort (symlink resolution, #1914 review finding 1)
/// before returning it, so the expected value here must apply the identical
/// transform — otherwise this assertion would be flaky on any platform where
/// the test binary's path traverses a symlink (e.g. macOS `/tmp` ->
/// `/private/tmp`).
fn assert_resolved_statusline_command(cmd: &str) {
    // #2229: the resolved statusline command must invoke the `statusline`
    // subcommand and must NOT bake an ephemeral build/worktree path (the test
    // process's own `current_exe()` is `target/debug/deps/...`, exactly the path
    // that must be rejected). The binary is either a stable PATH-resolved
    // install (`~/.cargo/bin/tm`) or the bare `tm`/`trusty-mpm` fallback — never
    // the transient artifact — so we assert the invariant, not an exact path
    // (which would depend on whether `tm` is installed in the test env).
    let binary = cmd
        .strip_suffix(" statusline")
        .unwrap_or_else(|| panic!("command must end with ' statusline', got {cmd}"));
    assert!(
        !trusty_common::bin_resolve::is_ephemeral_build_path(std::path::Path::new(binary)),
        "resolved statusline binary must not be an ephemeral build path, got {binary}"
    );
}

#[test]
fn write_status_line_injects_when_absent() {
    // When no settings.json exists, write_status_line creates it with statusLine
    // resolved to the absolute current-exe path (#1914), not a bare command.
    let tmp = tempdir().unwrap();
    write_status_line(tmp.path()).expect("write succeeds");
    let raw = std::fs::read_to_string(tmp.path().join(".claude").join("settings.json")).unwrap();
    let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(v["statusLine"]["type"], "command", "type must be command");
    assert_resolved_statusline_command(v["statusLine"]["command"].as_str().unwrap());
    assert_eq!(v["statusLine"]["padding"], 0, "padding must be 0");
}

#[test]
fn write_status_line_skips_when_already_set() {
    // When statusLine already exists (a genuine user customization), write_status_line
    // must not overwrite it.
    let tmp = tempdir().unwrap();
    let claude_dir = tmp.path().join(".claude");
    std::fs::create_dir_all(&claude_dir).unwrap();
    let existing =
        serde_json::json!({"statusLine": {"type": "command", "command": "my custom cmd"}});
    std::fs::write(
        claude_dir.join("settings.json"),
        serde_json::to_string_pretty(&existing).unwrap(),
    )
    .unwrap();
    write_status_line(tmp.path()).expect("write succeeds without modifying");
    let raw = std::fs::read_to_string(claude_dir.join("settings.json")).unwrap();
    let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(
        v["statusLine"]["command"], "my custom cmd",
        "existing statusLine must not be overwritten"
    );
}

#[test]
fn write_status_line_preserves_user_config() {
    // #1914 review finding 3: this test must seed a GENUINELY custom
    // statusLine.command (not one of the bare defaults write_status_line
    // itself would have written) so it actually exercises "leave user
    // customizations alone", rather than the absent-key injection path
    // covered separately by `write_status_line_injects_when_absent` or the
    // stale-default heal path covered by `write_status_line_heals_stale_*`.
    let tmp = tempdir().unwrap();
    let claude_dir = tmp.path().join(".claude");
    std::fs::create_dir_all(&claude_dir).unwrap();
    let existing = serde_json::json!({
        "outputStyle": "trusty-mpm-research",
        "someKey": true,
        "statusLine": {"type": "command", "command": "my-custom-statusline", "padding": 2}
    });
    std::fs::write(
        claude_dir.join("settings.json"),
        serde_json::to_string_pretty(&existing).unwrap(),
    )
    .unwrap();
    write_status_line(tmp.path()).expect("write succeeds");
    let raw = std::fs::read_to_string(claude_dir.join("settings.json")).unwrap();
    let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(
        v["outputStyle"], "trusty-mpm-research",
        "outputStyle must be preserved"
    );
    assert_eq!(v["someKey"], true, "arbitrary keys must be preserved");
    assert_eq!(
        v["statusLine"]["command"], "my-custom-statusline",
        "a genuinely custom statusLine.command must never be overwritten"
    );
    assert_eq!(
        v["statusLine"]["padding"], 2,
        "the rest of a custom statusLine entry must also survive untouched"
    );
}

#[test]
fn write_status_line_heals_stale_tm_default() {
    // #1914 self-heal: a pre-#1914 bare "tm statusline" default on disk (the
    // literal fingerprint this module used to write) is upgraded IN PLACE to
    // the resolved absolute path, so `ensure_status_line`'s resume self-heal
    // (#1913) also fixes the PATH-resolution risk without a separate hook.
    let tmp = tempdir().unwrap();
    let claude_dir = tmp.path().join(".claude");
    std::fs::create_dir_all(&claude_dir).unwrap();
    let existing = serde_json::json!({"statusLine": {"type": "command", "command": "tm statusline", "padding": 0}});
    std::fs::write(
        claude_dir.join("settings.json"),
        serde_json::to_string_pretty(&existing).unwrap(),
    )
    .unwrap();
    write_status_line(tmp.path()).expect("write succeeds");
    let raw = std::fs::read_to_string(claude_dir.join("settings.json")).unwrap();
    let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_resolved_statusline_command(v["statusLine"]["command"].as_str().unwrap());
    assert_eq!(
        v["statusLine"]["padding"], 0,
        "padding must survive the in-place upgrade"
    );
}

#[test]
fn write_status_line_heals_stale_trusty_mpm_default() {
    // Same self-heal, exercised against the `trusty-mpm` binary-name fingerprint
    // (the second `[[bin]]` this crate produces).
    let tmp = tempdir().unwrap();
    let claude_dir = tmp.path().join(".claude");
    std::fs::create_dir_all(&claude_dir).unwrap();
    let existing = serde_json::json!({
        "statusLine": {"type": "command", "command": "trusty-mpm statusline", "padding": 0}
    });
    std::fs::write(
        claude_dir.join("settings.json"),
        serde_json::to_string_pretty(&existing).unwrap(),
    )
    .unwrap();
    write_status_line(tmp.path()).expect("write succeeds");
    let raw = std::fs::read_to_string(claude_dir.join("settings.json")).unwrap();
    let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_resolved_statusline_command(v["statusLine"]["command"].as_str().unwrap());
}

// ── is_stale_bare_statusline_command tests ────────────────────────────────────

#[test]
fn is_stale_bare_statusline_command_matches_known_defaults() {
    assert!(is_stale_bare_statusline_command(
        &serde_json::json!({"type": "command", "command": "tm statusline"})
    ));
    assert!(is_stale_bare_statusline_command(
        &serde_json::json!({"type": "command", "command": "trusty-mpm statusline"})
    ));
}

#[test]
fn is_stale_bare_statusline_command_ignores_custom_command() {
    // A user's own custom command must never be flagged for in-place upgrade,
    // even if it happens to invoke `tm` with extra arguments.
    assert!(!is_stale_bare_statusline_command(
        &serde_json::json!({"type": "command", "command": "tm statusline --compact"})
    ));
    assert!(!is_stale_bare_statusline_command(
        &serde_json::json!({"type": "command", "command": "my custom cmd"})
    ));
    assert!(!is_stale_bare_statusline_command(
        &serde_json::json!({"type": "command", "command": "/opt/homebrew/bin/tm statusline"})
    ));
}

#[test]
fn is_stale_bare_statusline_command_ignores_non_command_type() {
    assert!(!is_stale_bare_statusline_command(
        &serde_json::json!({"type": "text", "command": "tm statusline"})
    ));
}

// ── is_stale_statusline_command tests (#2229) ─────────────────────────────────

#[test]
fn is_stale_statusline_command_flags_missing_and_ephemeral() {
    // Bare pre-#1914 defaults are stale (superset of is_stale_bare).
    assert!(is_stale_statusline_command(
        &serde_json::json!({"type": "command", "command": "tm statusline"})
    ));
    // An ephemeral build path is stale even though it is absolute.
    assert!(is_stale_statusline_command(&serde_json::json!({
        "type": "command",
        "command": "/repo/target/debug/deps/trusty_mpm-abc statusline"
    })));
    // A worktree path is stale.
    assert!(is_stale_statusline_command(&serde_json::json!({
        "type": "command",
        "command": "/repo/.claude/worktrees/fix-1/target/release/tm statusline"
    })));
    // An absolute path that does not exist on disk is stale.
    assert!(is_stale_statusline_command(&serde_json::json!({
        "type": "command",
        "command": "/no/such/dir/definitely-missing-2229 statusline"
    })));
}

#[cfg(unix)]
#[test]
fn is_stale_statusline_command_respects_existing_custom_binary() {
    // An absolute binary that EXISTS and is not ephemeral is a genuine
    // customization — never flagged. `/bin/sh` exists on every supported unix.
    assert!(!is_stale_statusline_command(&serde_json::json!({
        "type": "command",
        "command": "/bin/sh statusline"
    })));
    // A command that is not our "<binary> statusline" shape is never flagged.
    assert!(!is_stale_statusline_command(
        &serde_json::json!({"type": "command", "command": "my-custom-line"})
    ));
    // Non-command entries are ignored.
    assert!(!is_stale_statusline_command(&serde_json::json!({
        "type": "text",
        "command": "/no/such/dir/x statusline"
    })));
}

// ── resolve_statusline_binary_with tests ──────────────────────────────────────

#[test]
fn resolve_statusline_binary_with_prefers_current_exe() {
    let resolved = resolve_statusline_binary_with(
        || Ok(PathBuf::from("/abs/path/to/tm")),
        |_name| Some(PathBuf::from("/should/not/be/used/tm")),
    );
    assert_eq!(resolved, "/abs/path/to/tm");
}

#[test]
fn resolve_statusline_binary_with_rejects_ephemeral_current_exe() {
    // #2229: when current_exe() is an ephemeral build/worktree path it must be
    // rejected so the PATH-lookup fallback (a stable installed binary) is used
    // instead — never the transient artifact.
    let resolved = resolve_statusline_binary_with(
        || Ok(PathBuf::from("/repo/target/debug/deps/trusty_mpm-abc123")),
        |name| {
            assert_eq!(
                name, "tm",
                "path lookup must search for the bare 'tm' name first"
            );
            Some(PathBuf::from("/opt/homebrew/bin/tm"))
        },
    );
    assert_eq!(
        resolved, "/opt/homebrew/bin/tm",
        "an ephemeral current_exe must be rejected in favour of the PATH-resolved install"
    );
}

#[test]
fn resolve_statusline_binary_with_falls_back_to_path_lookup() {
    let resolved = resolve_statusline_binary_with(
        || Err(std::io::Error::other("current_exe unavailable")),
        |name| {
            assert_eq!(name, "tm", "path lookup must search for the bare 'tm' name");
            Some(PathBuf::from("/opt/homebrew/bin/tm"))
        },
    );
    assert_eq!(resolved, "/opt/homebrew/bin/tm");
}

#[test]
fn resolve_statusline_binary_with_falls_back_to_trusty_mpm_name() {
    // #1914 review finding 1: a machine with ONLY `trusty-mpm` on PATH (not
    // the `tm` alias) must still resolve when current_exe() is unavailable —
    // a bare single-name "tm" PATH lookup would silently degrade to the bare
    // literal here, reproducing the exact bug this module fixes.
    let resolved = resolve_statusline_binary_with(
        || Err(std::io::Error::other("current_exe unavailable")),
        |name| match name {
            "tm" => None,
            "trusty-mpm" => Some(PathBuf::from("/opt/homebrew/bin/trusty-mpm")),
            other => panic!("unexpected binary name looked up: {other}"),
        },
    );
    assert_eq!(resolved, "/opt/homebrew/bin/trusty-mpm");
}

#[test]
fn resolve_statusline_binary_with_falls_back_to_bare_name() {
    let resolved = resolve_statusline_binary_with(
        || Err(std::io::Error::other("current_exe unavailable")),
        |_name| None,
    );
    assert_eq!(
        resolved, "tm",
        "must degrade to the bare literal when both sources fail"
    );
}
