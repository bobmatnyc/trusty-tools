//! An Architect launch gets a sandbox payload only when opted in, and writes
//! no sandbox key (#12077 (a)/(b), part 4c).
//!
//! Why: the Architect arm of the bin's
//! `launch_files_carry_the_sandbox_only_when_opted_in_12077`. The carrier
//! probes `claude --version` for an Architect, so the test puts a fake
//! `claude` at the floor first on `PATH`. That is a process-global write, and
//! the bin target allows none for it, so the arm runs here.
//! What: the library calls the bin's `launch_home::launch_files_with` makes,
//! in its order, under a scratch home: the role's sandbox, the session MCP
//! config, then the carrier settings. Off gives no payload; on gives one with
//! the sandbox disabled. No settings file and no `sandbox-roles.json` carries
//! a sandbox key.
//! Test: this file IS the test (mounted in the `env_serial` target).

use std::path::Path;

use trusty_mpm::core::session_profile::SessionProfile;

// #12077 part 4c: the lib's `fake_claude_on_path`, the same source file.
#[path = "../src/test_fake_claude_path.rs"]
mod fake_claude_path;
use fake_claude_path::fake_claude_on_path;
// The floor version `fake_claude_path`'s stub reports.
use trusty_mpm::core::architect_claude_version::ARCHITECT_MIN_CLAUDE_VERSION;

/// A `tm-test-` scratch dir under `/tmp`, which the lib's sweep reaps.
fn hermetic_temp_dir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("tm-test-")
        .tempdir_in("/tmp")
        .expect("create a scratch dir")
}

/// An Architect launch's sandbox payload, after the same provisioning steps
/// as the bin's `launch_files_with` with a home.
fn architect_launch_sandbox(
    workspace: &Path,
    config: &Path,
    home: &Path,
    opted_in: bool,
) -> Option<serde_json::Value> {
    let profile = SessionProfile::Architect;
    let role = trusty_mpm::core::session_launch::sandbox_role_for(profile);
    let sandbox = trusty_mpm::core::claude_sandbox::opted_in_launch_sandbox(
        opted_in,
        role,
        workspace,
        Some(home),
    )
    .expect("the role's sandbox");
    let root = trusty_mpm::core::paths::FrameworkPaths::under_home(home).crate_config_root();
    trusty_mpm::core::session_mcp_scope::provision_for_spawn_at(&root, workspace, Some(config))
        .expect("the session MCP config");
    trusty_mpm::core::launch_policy::provision_settings(&root, workspace, Some(config), profile)
        .expect("the carrier settings");
    sandbox
}

/// #12077 part 4c: the Architect arm, with a fake `claude` at the floor.
#[test]
fn an_architect_launch_carries_the_sandbox_only_when_opted_in_12077() {
    let _claude = fake_claude_on_path();
    let home = hermetic_temp_dir();
    let workspace = hermetic_temp_dir();
    let config = home.path().join("cfg");

    let off = architect_launch_sandbox(workspace.path(), &config, home.path(), false);
    assert_eq!(off, None, "Architect: default is off");
    let on = architect_launch_sandbox(workspace.path(), &config, home.path(), true);
    let enabled = on
        .as_ref()
        .and_then(|v| v.pointer("/sandbox/enabled"))
        .cloned();
    assert_eq!(enabled, Some(serde_json::json!(false)), "Architect");

    for file in [
        workspace.path().join(".claude/settings.json"),
        workspace.path().join(".claude/settings.local.json"),
        home.path().join(".claude/settings.json"),
        config.join("settings.json"),
    ] {
        let Ok(raw) = std::fs::read_to_string(&file) else {
            continue;
        };
        assert!(!raw.contains("\"sandbox\""), "{}: {raw}", file.display());
    }
    assert!(
        !workspace
            .path()
            .join(".trusty-mpm/sandbox-roles.json")
            .exists()
    );
}
