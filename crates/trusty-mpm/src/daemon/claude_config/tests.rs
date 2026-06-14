use std::path::Path;

use serde_json::Value;

use crate::core::claude_config::ConfigRecommendation;
use crate::core::claude_config::Severity;
use crate::core::claude_config::{ClaudeConfig, ClaudeConfigPaths, ClaudeConfigReader};

use super::analyzer::ClaudeConfigAnalyzer;
use super::checkpointer::ConfigCheckpointer;
use super::profile::ProfileDeployer;

/// Build a `ClaudeConfigPaths` rooted entirely under a temp directory so a
/// test never reads or writes the operator's real `~/.claude`.
fn temp_paths(root: &Path) -> ClaudeConfigPaths {
    let project = root.join("project");
    let user = root.join("home");
    ClaudeConfigPaths {
        user_settings: user.join(".claude/settings.json"),
        user_local_settings: user.join(".claude/settings.local.json"),
        project_settings: project.join(".claude/settings.json"),
        project_local_settings: project.join(".claude/settings.local.json"),
        user_agents_dir: user.join(".claude/agents"),
        project_agents_dir: project.join(".claude/agents"),
    }
}

fn write_json(path: &Path, json: &Value) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, serde_json::to_string_pretty(json).unwrap()).unwrap();
}

#[test]
fn read_config_missing_files_is_empty() {
    // No settings files on disk → an all-default ClaudeConfig.
    let dir = tempfile::tempdir().unwrap();
    let paths = temp_paths(dir.path());
    let config = ClaudeConfigAnalyzer::read_config(&paths);
    assert_eq!(config, ClaudeConfig::default());
}

#[test]
fn read_config_detects_hooks() {
    let dir = tempfile::tempdir().unwrap();
    let paths = temp_paths(dir.path());
    write_json(
        &paths.project_settings,
        &serde_json::json!({ "hooks": { "PreToolUse": [] } }),
    );
    let config = ClaudeConfigAnalyzer::read_config(&paths);
    assert!(config.has_hooks);
}

#[test]
fn read_config_detects_wildcard_and_env() {
    let dir = tempfile::tempdir().unwrap();
    let paths = temp_paths(dir.path());
    write_json(
        &paths.project_settings,
        &serde_json::json!({
            "permissions": { "allow": ["*", "Read"] },
            "env": { "OPENROUTER_API_KEY": "sk-x" } // pragma: allowlist secret
        }),
    );
    let config = ClaudeConfigAnalyzer::read_config(&paths);
    assert!(config.allow_list_has_wildcard);
    assert_eq!(config.allow_list_entries, 2);
    assert!(config.has_openrouter_key);
}

#[test]
fn read_config_detects_agents() {
    let dir = tempfile::tempdir().unwrap();
    let paths = temp_paths(dir.path());
    std::fs::create_dir_all(&paths.project_agents_dir).unwrap();
    std::fs::write(paths.project_agents_dir.join("research.md"), "# agent").unwrap();
    let config = ClaudeConfigAnalyzer::read_config(&paths);
    assert!(config.has_agents);
}

#[test]
fn analyze_flags_missing_hooks() {
    // A default (empty) config triggers the add-trusty-hooks recommendation.
    let recs = ClaudeConfigAnalyzer::analyze(&ClaudeConfig::default());
    assert!(recs.iter().any(|r| r.id == "add-trusty-hooks"));
}

#[test]
fn analyze_flags_wildcard() {
    let config = ClaudeConfig {
        has_hooks: true,
        allow_list_has_wildcard: true,
        allow_list_entries: 1,
        has_agents: true,
        has_openrouter_key: true,
    };
    let recs = ClaudeConfigAnalyzer::analyze(&config);
    let wildcard = recs.iter().find(|r| r.id == "scope-permissions");
    assert!(wildcard.is_some());
    assert_eq!(wildcard.unwrap().severity, Severity::Critical);
}

#[test]
fn analyze_clean_config_is_empty() {
    // A fully-configured project yields no recommendations.
    let config = ClaudeConfig {
        has_hooks: true,
        allow_list_has_wildcard: false,
        allow_list_entries: 5,
        has_agents: true,
        has_openrouter_key: true,
    };
    assert!(ClaudeConfigAnalyzer::analyze(&config).is_empty());
}

#[test]
fn apply_add_hooks_writes_settings() {
    // Applying add-trusty-hooks must write a hooks block that a subsequent
    // read picks up as `has_hooks = true`.
    let dir = tempfile::tempdir().unwrap();
    let paths = temp_paths(dir.path());
    let project = dir.path().join("project");
    let rec = ClaudeConfigAnalyzer::analyze(&ClaudeConfig::default())
        .into_iter()
        .find(|r| r.id == "add-trusty-hooks")
        .expect("add-trusty-hooks recommended");
    ClaudeConfigAnalyzer::apply_recommendation(&rec, &paths, &project).expect("apply succeeds");

    let config = ClaudeConfigAnalyzer::read_config(&paths);
    assert!(config.has_hooks, "hooks block must be present after apply");
}

#[test]
fn apply_manual_rec_errors() {
    // A non-auto-applicable recommendation cannot be applied programmatically.
    let dir = tempfile::tempdir().unwrap();
    let paths = temp_paths(dir.path());
    let project = dir.path().join("project");
    let rec = ConfigRecommendation {
        id: "scope-permissions".into(),
        severity: Severity::Critical,
        title: "x".into(),
        description: "x".into(),
        auto_applicable: false,
    };
    assert!(ClaudeConfigAnalyzer::apply_recommendation(&rec, &paths, &project).is_err());
}

// ---- deterministic analysis coverage --------------------------------

/// A fully-configured `ClaudeConfig` — the baseline a test mutates to
/// trigger exactly one recommendation.
fn healthy_config() -> ClaudeConfig {
    ClaudeConfig {
        has_hooks: true,
        allow_list_has_wildcard: false,
        allow_list_entries: 5,
        has_agents: true,
        has_openrouter_key: true,
    }
}

#[test]
fn analyze_missing_hooks_flags_warning() {
    // No hooks → exactly the add-trusty-hooks rec, at Warning severity.
    let config = ClaudeConfig {
        has_hooks: false,
        ..healthy_config()
    };
    let recs = ClaudeConfigAnalyzer::analyze(&config);
    let rec = recs
        .iter()
        .find(|r| r.id == "add-trusty-hooks")
        .expect("add-trusty-hooks flagged");
    assert_eq!(rec.severity, Severity::Warning);
    assert_eq!(recs.len(), 1, "only the missing-hooks issue");
}

#[test]
fn analyze_wildcard_permission_flags_critical() {
    // A `*` in the allow list → scope-permissions at Critical severity.
    let config = ClaudeConfig {
        allow_list_has_wildcard: true,
        ..healthy_config()
    };
    let recs = ClaudeConfigAnalyzer::analyze(&config);
    let rec = recs
        .iter()
        .find(|r| r.id == "scope-permissions")
        .expect("scope-permissions flagged");
    assert_eq!(rec.severity, Severity::Critical);
    assert_eq!(recs.len(), 1, "only the wildcard issue");
}

#[test]
fn analyze_no_agents_flags_info() {
    // No agent files → deploy-agents at Info severity.
    let config = ClaudeConfig {
        has_agents: false,
        ..healthy_config()
    };
    let recs = ClaudeConfigAnalyzer::analyze(&config);
    let rec = recs
        .iter()
        .find(|r| r.id == "deploy-agents")
        .expect("deploy-agents flagged");
    assert_eq!(rec.severity, Severity::Info);
    assert_eq!(recs.len(), 1, "only the missing-agents issue");
}

#[test]
fn analyze_missing_openrouter_key_flags_warning() {
    // No OPENROUTER_API_KEY → add-openrouter-key at Warning severity.
    let config = ClaudeConfig {
        has_openrouter_key: false,
        ..healthy_config()
    };
    let recs = ClaudeConfigAnalyzer::analyze(&config);
    let rec = recs
        .iter()
        .find(|r| r.id == "add-openrouter-key")
        .expect("add-openrouter-key flagged");
    assert_eq!(rec.severity, Severity::Warning);
    assert_eq!(recs.len(), 1, "only the missing-key issue");
}

#[test]
fn analyze_fully_configured_is_empty() {
    // Hooks + scoped perms + agents + key → zero recommendations.
    assert!(ClaudeConfigAnalyzer::analyze(&healthy_config()).is_empty());
}

#[test]
fn analyze_partial_config_multiple_recs() {
    // Missing hooks AND a wildcard → two recommendations, Critical first.
    let config = ClaudeConfig {
        has_hooks: false,
        allow_list_has_wildcard: true,
        ..healthy_config()
    };
    let recs = ClaudeConfigAnalyzer::analyze(&config);
    assert_eq!(recs.len(), 2, "exactly the two flagged issues");
    assert_eq!(
        recs[0].severity,
        Severity::Critical,
        "Critical sorts before Warning"
    );
    assert_eq!(recs[0].id, "scope-permissions");
    assert_eq!(recs[1].severity, Severity::Warning);
    assert_eq!(recs[1].id, "add-trusty-hooks");
}

// ---- checkpointing & backup/restore ---------------------------------

#[test]
fn apply_creates_checkpoint_before_change() {
    // Applying a recommendation must leave a checkpoint file behind in
    // `.trusty-mpm/checkpoints/`.
    let dir = tempfile::tempdir().unwrap();
    let paths = temp_paths(dir.path());
    let project = dir.path().join("project");
    let rec = ClaudeConfigAnalyzer::analyze(&ClaudeConfig::default())
        .into_iter()
        .find(|r| r.id == "add-trusty-hooks")
        .expect("add-trusty-hooks recommended");
    let checkpoint_id =
        ClaudeConfigAnalyzer::apply_recommendation(&rec, &paths, &project).expect("apply succeeds");

    let cp_file = crate::core::claude_config::CheckpointPaths::for_id(&project, &checkpoint_id);
    assert!(cp_file.exists(), "checkpoint JSON must exist after apply");
    let checkpoints = ConfigCheckpointer::list(&project).unwrap();
    assert_eq!(checkpoints.len(), 1);
    assert_eq!(checkpoints[0].id, checkpoint_id);
    assert_eq!(
        checkpoints[0].label.as_deref(),
        Some("before-add-trusty-hooks")
    );
}

#[test]
fn restore_reverts_to_pre_apply_state() {
    // Apply a change, then restore the pre-apply checkpoint: the project
    // settings.json must return to its original (absent) state-equivalent.
    let dir = tempfile::tempdir().unwrap();
    let paths = temp_paths(dir.path());
    let project = dir.path().join("project");

    // Start from a project settings.json with no hooks.
    write_json(&paths.project_settings, &serde_json::json!({ "x": 1 }));

    let rec = ConfigRecommendation {
        id: "add-trusty-hooks".into(),
        severity: Severity::Warning,
        title: "x".into(),
        description: "x".into(),
        auto_applicable: true,
    };
    let checkpoint_id =
        ClaudeConfigAnalyzer::apply_recommendation(&rec, &paths, &project).expect("apply succeeds");
    assert!(
        ClaudeConfigAnalyzer::read_config(&paths).has_hooks,
        "hooks present after apply"
    );

    ConfigCheckpointer::restore(&project, &checkpoint_id).expect("restore succeeds");
    let restored: Value =
        serde_json::from_str(&std::fs::read_to_string(&paths.project_settings).unwrap()).unwrap();
    assert_eq!(restored, serde_json::json!({ "x": 1 }), "original content");
    assert!(
        !ClaudeConfigAnalyzer::read_config(&paths).has_hooks,
        "hooks gone after restore"
    );
}

#[test]
fn checkpoint_list_newest_first() {
    // Three checkpoints created in sequence list newest-first.
    let dir = tempfile::tempdir().unwrap();
    let paths = temp_paths(dir.path());
    let project = dir.path().join("project");

    let mut ids = Vec::new();
    for label in ["first", "second", "third"] {
        let id = ConfigCheckpointer::create(&paths, &project, Some(label)).unwrap();
        ids.push(id);
        // Sleep past the 1-second timestamp resolution so ordering is
        // deterministic regardless of the random suffix.
        std::thread::sleep(std::time::Duration::from_millis(1100));
    }

    let listed = ConfigCheckpointer::list(&project).unwrap();
    assert_eq!(listed.len(), 3);
    assert_eq!(listed[0].id, ids[2], "newest first");
    assert_eq!(listed[1].id, ids[1]);
    assert_eq!(listed[2].id, ids[0], "oldest last");
}

#[test]
fn safe_restore_does_not_delete_new_files() {
    // A config file created AFTER the checkpoint must survive a restore —
    // restore only rewrites files that were captured in the checkpoint.
    let dir = tempfile::tempdir().unwrap();
    let paths = temp_paths(dir.path());
    let project = dir.path().join("project");

    // Checkpoint with only the project settings.json present.
    write_json(&paths.project_settings, &serde_json::json!({ "a": 1 }));
    let checkpoint_id = ConfigCheckpointer::create(&paths, &project, Some("snapshot")).unwrap();

    // After the checkpoint, a new local-settings file appears.
    write_json(
        &paths.project_local_settings,
        &serde_json::json!({ "new": true }),
    );

    ConfigCheckpointer::restore(&project, &checkpoint_id).expect("restore succeeds");
    assert!(
        paths.project_local_settings.exists(),
        "file created after the checkpoint must not be deleted by restore"
    );
    let still: Value =
        serde_json::from_str(&std::fs::read_to_string(&paths.project_local_settings).unwrap())
            .unwrap();
    assert_eq!(still, serde_json::json!({ "new": true }));
}

#[test]
fn checkpoint_delete_removes_file() {
    // Deleting a checkpoint removes its JSON file.
    let dir = tempfile::tempdir().unwrap();
    let paths = temp_paths(dir.path());
    let project = dir.path().join("project");
    let id = ConfigCheckpointer::create(&paths, &project, None).unwrap();
    assert_eq!(ConfigCheckpointer::list(&project).unwrap().len(), 1);
    ConfigCheckpointer::delete(&project, &id).expect("delete succeeds");
    assert!(ConfigCheckpointer::list(&project).unwrap().is_empty());
}

// ---- deployment profiles --------------------------------------------

#[test]
fn builtin_profiles_are_present() {
    let names: Vec<String> = ProfileDeployer::builtin_profiles()
        .into_iter()
        .map(|p| p.name)
        .collect();
    assert!(names.contains(&"trusty-mpm-oversight".to_string()));
    assert!(names.contains(&"read-only-review".to_string()));
    assert!(names.contains(&"minimal".to_string()));
}

#[test]
fn deploy_trusty_oversight_profile_writes_hooks() {
    // Deploying the oversight profile must write a hooks block into the
    // project settings.json.
    let dir = tempfile::tempdir().unwrap();
    let paths = temp_paths(dir.path());
    let project = dir.path().join("project");
    let profile = ProfileDeployer::builtin_profiles()
        .into_iter()
        .find(|p| p.name == "trusty-mpm-oversight")
        .expect("oversight profile exists");
    ProfileDeployer::deploy(&profile, &paths, &project).expect("deploy succeeds");

    let settings: Value =
        serde_json::from_str(&std::fs::read_to_string(&paths.project_settings).unwrap()).unwrap();
    assert!(
        settings["hooks"]["PreToolUse"].is_array(),
        "PreToolUse hooks written"
    );
    assert!(
        settings["hooks"]["PostToolUse"].is_array(),
        "PostToolUse hooks written"
    );
    assert!(
        ClaudeConfigAnalyzer::read_config(&paths).has_hooks,
        "deployed hooks are detected by the analyzer"
    );
}

#[test]
fn deploy_readonly_profile_writes_deny_list() {
    // Deploying the read-only profile must write a permissions deny list.
    let dir = tempfile::tempdir().unwrap();
    let paths = temp_paths(dir.path());
    let project = dir.path().join("project");
    let profile = ProfileDeployer::builtin_profiles()
        .into_iter()
        .find(|p| p.name == "read-only-review")
        .expect("read-only profile exists");
    ProfileDeployer::deploy(&profile, &paths, &project).expect("deploy succeeds");

    let settings: Value =
        serde_json::from_str(&std::fs::read_to_string(&paths.project_settings).unwrap()).unwrap();
    let deny = settings["permissions"]["deny"]
        .as_array()
        .expect("deny list present");
    assert!(deny.iter().any(|v| v == "Bash"));
    assert!(deny.iter().any(|v| v == "Write"));
    assert!(deny.iter().any(|v| v == "Edit"));
}

#[test]
fn list_applied_detects_deployed_profile() {
    // After deploying the read-only profile, list_applied reports it.
    let dir = tempfile::tempdir().unwrap();
    let paths = temp_paths(dir.path());
    let project = dir.path().join("project");
    let profile = ProfileDeployer::builtin_profiles()
        .into_iter()
        .find(|p| p.name == "read-only-review")
        .expect("read-only profile exists");
    ProfileDeployer::deploy(&profile, &paths, &project).expect("deploy succeeds");

    let applied = ProfileDeployer::list_applied(&paths).unwrap();
    assert!(applied.contains(&"read-only-review".to_string()));
}

#[test]
fn deploy_creates_checkpoint() {
    // deploy must checkpoint before writing, returning the checkpoint id.
    let dir = tempfile::tempdir().unwrap();
    let paths = temp_paths(dir.path());
    let project = dir.path().join("project");
    let profile = ProfileDeployer::builtin_profiles()
        .into_iter()
        .find(|p| p.name == "minimal")
        .expect("minimal profile exists");
    let checkpoint_id =
        ProfileDeployer::deploy(&profile, &paths, &project).expect("deploy succeeds");
    let listed = ConfigCheckpointer::list(&project).unwrap();
    assert!(listed.iter().any(|c| c.id == checkpoint_id));
}

#[test]
fn find_claude_processes_does_not_panic() {
    // Whether or not Claude Code is running, this returns a Vec without
    // panicking — the count is environment-dependent.
    let _pids = super::restarter::ClaudeCodeRestarter::find_claude_processes();
}

#[test]
fn paths_for_project_is_usable() {
    // The core resolver and the analyzer agree on the path shape.
    let paths = ClaudeConfigReader::paths_for_project(Path::new("/work/demo"));
    assert!(paths.project_settings.ends_with(".claude/settings.json"));
}
