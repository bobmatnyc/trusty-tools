//! Tests for [`super`] — `tm doctor --fix`'s session-scope repair (#7678).
//!
//! Why: this repair writes into a project the operator can see, so its guard,
//! its idempotence, and above all its behaviour on a FAILED write have to be
//! pinned rather than described. Before #7678 there was no repair at all: the
//! check reported the decision and nothing re-applied it.
//! What: the `.trusty-mpm/` guard, the dry-run/apply pair for the plugin map,
//! foreign-key preservation, the no-write-when-current case (mtime included),
//! the unwritable-file failure, and the composed-MCP half.
//! Test: this file.

use std::path::Path;

use serde_json::json;
use tempfile::TempDir;

use super::*;

/// A managed project directory: the `.trusty-mpm/` marker and nothing else.
fn managed_project(root: &Path, name: &str) -> std::path::PathBuf {
    let project = root.join(name);
    std::fs::create_dir_all(project.join(FRAMEWORK_DIR_NAME)).unwrap();
    project
}

/// A managed config dir whose installed-plugin index names `keys`.
fn managed_config(root: &Path, keys: &[&str]) -> std::path::PathBuf {
    let config = root.join("cfg");
    let mut plugins = serde_json::Map::new();
    for key in keys {
        plugins.insert((*key).to_string(), json!([]));
    }
    std::fs::create_dir_all(config.join("plugins")).unwrap();
    std::fs::write(
        config.join("plugins").join("installed_plugins.json"),
        serde_json::to_string_pretty(&json!({ "plugins": plugins })).unwrap(),
    )
    .unwrap();
    config
}

/// Read a project's `.claude/settings.json` as JSON.
fn read_settings(project: &Path) -> serde_json::Value {
    let text = std::fs::read_to_string(project.join(".claude").join("settings.json")).unwrap();
    serde_json::from_str(&text).unwrap()
}

#[test]
fn session_scope_repair_skips_an_unregistered_project() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("not-mine");
    std::fs::create_dir_all(&project).unwrap();
    let config = managed_config(tmp.path(), &["aws-core@market"]);

    let steps =
        repair_session_scope_with_trust(&project, Some(&config), None, RepairMode::Apply, false);

    assert!(steps.is_empty(), "{steps:?}");
    assert!(
        !project.join(".claude").exists(),
        "a project with no `.trusty-mpm/` marker must not be touched at all"
    );
}

#[test]
fn session_scope_repair_steps_name_the_check() {
    let tmp = TempDir::new().unwrap();
    let project = managed_project(tmp.path(), "repo");
    let config = managed_config(tmp.path(), &["aws-core@market"]);

    let steps =
        repair_session_scope_with_trust(&project, Some(&config), None, RepairMode::DryRun, false);

    assert_eq!(steps.len(), 1, "{steps:?}");
    assert_eq!(steps[0].check, CHECK_NAME);
    assert_eq!(steps[0].status, StepStatus::Planned);
    assert!(
        steps[0].what.contains("aws-core@market"),
        "the preview must name the key: {}",
        steps[0].what
    );
}

#[test]
fn session_scope_repair_dry_run_writes_nothing() {
    let tmp = TempDir::new().unwrap();
    let project = managed_project(tmp.path(), "repo");
    let config = managed_config(tmp.path(), &["aws-core@market"]);

    let steps =
        repair_session_scope_with_trust(&project, Some(&config), None, RepairMode::DryRun, false);

    assert_eq!(steps.len(), 1);
    assert!(!steps[0].changed());
    assert!(
        !project.join(".claude").join("settings.json").exists(),
        "a dry run must not create the settings file"
    );
}

#[test]
fn session_scope_repair_applies_the_default_deny_map() {
    let tmp = TempDir::new().unwrap();
    let project = managed_project(tmp.path(), "repo");
    let config = managed_config(tmp.path(), &["aws-agents@market", "aws-core@market"]);

    let steps =
        repair_session_scope_with_trust(&project, Some(&config), None, RepairMode::Apply, false);

    assert_eq!(steps.len(), 1, "{steps:?}");
    assert_eq!(steps[0].status, StepStatus::Applied { backup: None });
    let settings = read_settings(&project);
    assert_eq!(
        settings["enabledPlugins"]["aws-agents@market"],
        json!(false)
    );
    assert_eq!(settings["enabledPlugins"]["aws-core@market"], json!(false));
}

#[test]
fn session_scope_repair_preserves_foreign_settings_keys() {
    let tmp = TempDir::new().unwrap();
    let project = managed_project(tmp.path(), "repo");
    let config = managed_config(tmp.path(), &["aws-core@market"]);
    std::fs::create_dir_all(project.join(".claude")).unwrap();
    std::fs::write(
        project.join(".claude").join("settings.json"),
        serde_json::to_string_pretty(&json!({
            "outputStyle": "trusty-mpm",
            "hooks": {"PreToolUse": []},
            "enabledPlugins": {"hand-installed@elsewhere": true},
        }))
        .unwrap(),
    )
    .unwrap();

    let steps =
        repair_session_scope_with_trust(&project, Some(&config), None, RepairMode::Apply, false);

    assert_eq!(steps[0].status, StepStatus::Applied { backup: None });
    let settings = read_settings(&project);
    assert_eq!(settings["outputStyle"], json!("trusty-mpm"));
    assert_eq!(settings["hooks"]["PreToolUse"], json!([]));
    assert_eq!(
        settings["enabledPlugins"]["hand-installed@elsewhere"],
        json!(true),
        "a key tm never enumerated stays the operator's"
    );
    assert_eq!(settings["enabledPlugins"]["aws-core@market"], json!(false));
}

#[test]
fn session_scope_repair_writes_nothing_when_the_settings_already_match() {
    let tmp = TempDir::new().unwrap();
    let project = managed_project(tmp.path(), "repo");
    let config = managed_config(tmp.path(), &["aws-core@market"]);
    repair_session_scope_with_trust(&project, Some(&config), None, RepairMode::Apply, false);

    let settings_path = project.join(".claude").join("settings.json");
    let before_bytes = std::fs::read(&settings_path).unwrap();
    let before_mtime = std::fs::metadata(&settings_path)
        .unwrap()
        .modified()
        .unwrap();

    let steps =
        repair_session_scope_with_trust(&project, Some(&config), None, RepairMode::Apply, false);

    assert!(steps.is_empty(), "a matching file owes no step: {steps:?}");
    assert_eq!(std::fs::read(&settings_path).unwrap(), before_bytes);
    assert_eq!(
        std::fs::metadata(&settings_path)
            .unwrap()
            .modified()
            .unwrap(),
        before_mtime,
        "an in-sync project must take no write at all"
    );
}

#[test]
fn session_scope_repair_enables_an_opted_in_plugin_for_a_trusted_project() {
    let tmp = TempDir::new().unwrap();
    let project = managed_project(tmp.path(), "repo");
    let config = managed_config(tmp.path(), &["aws-core@market"]);
    std::fs::write(
        project.join(crate::core::project_config::PROJECT_CONFIG_FILE),
        "[session]\nplugins = [\"aws-core\"]\n",
    )
    .unwrap();

    let steps =
        repair_session_scope_with_trust(&project, Some(&config), None, RepairMode::Apply, true);

    assert_eq!(steps[0].status, StepStatus::Applied { backup: None });
    assert_eq!(
        read_settings(&project)["enabledPlugins"]["aws-core@market"],
        json!(true),
        "a trusted project's own opt-in is honoured by the repair"
    );
}

/// A write failure must be `Failed`, never a step that reports success.
///
/// The unwritable fixture is a DIRECTORY standing where `settings.json` belongs
/// rather than a permission bit: a mode-based fixture is a no-op for uid 0, and
/// these tests run wherever CI puts them. See #7678.
#[test]
fn session_scope_repair_reports_an_unwritable_settings_file_as_failed() {
    let tmp = TempDir::new().unwrap();
    let project = managed_project(tmp.path(), "repo");
    let config = managed_config(tmp.path(), &["aws-core@market"]);
    std::fs::create_dir_all(project.join(".claude").join("settings.json")).unwrap();

    let steps =
        repair_session_scope_with_trust(&project, Some(&config), None, RepairMode::Apply, false);

    assert_eq!(steps.len(), 1, "{steps:?}");
    assert!(
        matches!(steps[0].status, StepStatus::Failed(_)),
        "an unwritable settings file is an error, never a success: {:?}",
        steps[0].status
    );
    assert!(
        !steps[0].changed(),
        "a failed write must not count as an applied repair"
    );
}

#[test]
fn session_scope_repair_provisions_an_absent_mcp_file() {
    let tmp = TempDir::new().unwrap();
    let project = managed_project(tmp.path(), "repo");
    let config = managed_config(tmp.path(), &[]);
    let mcp = tmp.path().join("state").join("session-mcp").join("k.json");

    let steps = repair_session_scope_with_trust(
        &project,
        Some(&config),
        Some(&mcp),
        RepairMode::Apply,
        false,
    );

    assert_eq!(steps.len(), 1, "{steps:?}");
    assert_eq!(steps[0].status, StepStatus::Applied { backup: None });
    assert_eq!(
        std::fs::read_to_string(&mcp).unwrap(),
        crate::core::session_mcp_scope::composed_body(&project, &config).unwrap(),
        "the repair writes exactly what a launch composes"
    );
}

#[test]
fn session_scope_repair_leaves_a_current_mcp_file_alone() {
    let tmp = TempDir::new().unwrap();
    let project = managed_project(tmp.path(), "repo");
    let config = managed_config(tmp.path(), &[]);
    let mcp = tmp.path().join("state").join("session-mcp").join("k.json");
    crate::core::session_mcp_scope::provision_at(&mcp, &project, &config).unwrap();
    let before = std::fs::metadata(&mcp).unwrap().modified().unwrap();

    let steps = repair_session_scope_with_trust(
        &project,
        Some(&config),
        Some(&mcp),
        RepairMode::Apply,
        false,
    );

    assert!(steps.is_empty(), "{steps:?}");
    assert_eq!(
        std::fs::metadata(&mcp).unwrap().modified().unwrap(),
        before,
        "a current composed file must not be rewritten"
    );
}

#[test]
fn composed_body_matches_the_provisioned_file() {
    let tmp = TempDir::new().unwrap();
    let project = managed_project(tmp.path(), "repo");
    let config = managed_config(tmp.path(), &[]);
    let mcp = tmp.path().join("session-mcp").join("k.json");

    crate::core::session_mcp_scope::provision_at(&mcp, &project, &config).unwrap();

    assert_eq!(
        std::fs::read_to_string(&mcp).unwrap(),
        crate::core::session_mcp_scope::composed_body(&project, &config).unwrap()
    );
}
