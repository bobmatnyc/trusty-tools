//! Tests for the #8928 step 5 per-role sandbox settings generator.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use tempfile::TempDir;

use super::sandbox_settings::{
    DENIED_ENV_VARS, SandboxInputs, SandboxRole, SandboxSettingsError, project_policy, user_policy,
    verify_sandbox_settings, write_sandbox_settings,
};

/// A scratch workspace, home and user config dir.
struct Fx {
    _dir: TempDir,
    inputs: SandboxInputs,
    user: PathBuf,
}

impl Fx {
    fn new() -> Self {
        let dir = TempDir::new().expect("tempdir");
        let root = std::fs::canonicalize(dir.path()).expect("canonical");
        let mut inputs = SandboxInputs::new(root.join("ws"), root.join("home"));
        inputs.tmux_socket = Some(PathBuf::from("/private/tmp/tmux-502/tm-ws-abc"));
        inputs.mpm_socket = Some(root.join("Application Support/trusty-mpm.sock"));
        inputs.sccache_socket = Some(root.join("home/.trusty-mpm/sccache.sock"));
        inputs.cargo_target_dir = Some(root.join("home/.trusty-tools/cargo-target/o/r"));
        std::fs::create_dir_all(inputs.workspace.join(".claude")).expect("mkdir ws");
        let user = root.join("home/.trusty-tools/trusty-mpm/claude-config");
        Self {
            _dir: dir,
            inputs,
            user,
        }
    }

    fn file(&self, name: &str) -> PathBuf {
        self.inputs.workspace.join(".claude").join(name)
    }
}

fn read(path: &Path) -> Value {
    let text = std::fs::read_to_string(path).expect("read settings");
    serde_json::from_str(&text).expect("json")
}

fn strings(value: &Value, pointer: &str) -> Vec<String> {
    value
        .pointer(pointer)
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("{pointer} is an array in {value}"))
        .iter()
        .map(|v| v.as_str().expect("string").to_owned())
        .collect()
}

fn refused(result: Result<(), SandboxSettingsError>, what: &str) {
    match result {
        Err(SandboxSettingsError::Refused(why)) => assert!(why.contains("#8928"), "{why}"),
        other => panic!("{what} must refuse the launch, got {other:?}"),
    }
}

#[test]
fn the_pm_policy_names_every_socket_and_no_local_binding() {
    let fx = Fx::new();
    let policy = project_policy(SandboxRole::Pm, &fx.inputs);
    let sandbox = &policy["sandbox"];
    assert_eq!(sandbox["enabled"], json!(true));
    assert_eq!(sandbox["allowUnsandboxedCommands"], json!(false));
    assert_eq!(sandbox["failIfUnavailable"], json!(true));
    let sockets = strings(&policy, "/sandbox/network/allowUnixSockets");
    let want = [
        &fx.inputs.tmux_socket,
        &fx.inputs.mpm_socket,
        &fx.inputs.sccache_socket,
    ];
    for path in want.into_iter().flatten() {
        assert!(sockets.contains(&path.display().to_string()), "{sockets:?}");
    }
    // The socket path with a space is kept verbatim, as one entry.
    assert!(sockets.iter().any(|s| s.contains("Application Support")));
    assert!(sandbox["network"].get("allowLocalBinding").is_none(), "Q2");
    assert!(
        sandbox["network"].get("strictAllowlist").is_none(),
        "user tier only"
    );
    let allow_write = strings(&policy, "/sandbox/filesystem/allowWrite");
    let target = fx.inputs.cargo_target_dir.as_ref().expect("target");
    assert!(allow_write.contains(&target.display().to_string()));
    // Exact files only — never a `.claude/**` rule (Q3).
    let deny = strings(&policy, "/permissions/deny");
    assert!(deny.iter().all(|d| !d.contains(".claude/**")), "{deny:?}");
    let settings = format!("Edit(/{})", fx.file("settings.local.json").display());
    assert!(deny.contains(&settings), "{deny:?}");
    assert!(
        deny.iter().all(|d| d.contains("(//")),
        "absolute rules: {deny:?}"
    );
}

#[test]
fn the_agent_policy_has_no_tmux_socket() {
    let fx = Fx::new();
    let pm = project_policy(SandboxRole::Pm, &fx.inputs);
    let agent = project_policy(SandboxRole::Agent, &fx.inputs);
    let sockets = strings(&agent, "/sandbox/network/allowUnixSockets");
    let tmux = fx.inputs.tmux_socket.as_ref().expect("tmux");
    assert!(
        !sockets.contains(&tmux.display().to_string()),
        "{sockets:?}"
    );
    assert_eq!(sockets.len(), 2, "{sockets:?}");
    // Everything else matches the PM policy.
    let mut pm_rest = pm.clone();
    pm_rest["sandbox"]["network"]["allowUnixSockets"] = json!(null);
    let mut agent_rest = agent;
    agent_rest["sandbox"]["network"]["allowUnixSockets"] = json!(null);
    assert_eq!(pm_rest, agent_rest);
}

#[test]
fn the_architect_policy_disables_the_sandbox() {
    let fx = Fx::new();
    let policy = project_policy(SandboxRole::Architect, &fx.inputs);
    assert_eq!(policy, json!({ "sandbox": { "enabled": false } }));
    assert_eq!(user_policy(SandboxRole::Architect), None);
    write_sandbox_settings(SandboxRole::Architect, &fx.inputs, &fx.user).expect("write");
    assert!(
        !fx.user.join("settings.json").exists(),
        "no user-tier write"
    );
}

#[test]
fn the_user_policy_carries_strict_allowlist_and_credentials() {
    let policy = user_policy(SandboxRole::Agent).expect("agent user policy");
    assert_eq!(
        policy.pointer("/sandbox/network/strictAllowlist"),
        Some(&json!(true))
    );
    let env = policy
        .pointer("/sandbox/credentials/envVars")
        .and_then(Value::as_array)
        .expect("envVars");
    assert_eq!(env.len(), DENIED_ENV_VARS.len());
    assert!(env.iter().all(|e| e["mode"] == json!("deny")));
}

#[test]
fn writing_preserves_unrelated_keys_and_verifies() {
    let fx = Fx::new();
    let existing = json!({
        "hooks": { "Stop": [] },
        "permissions": { "allow": ["Bash(ls:*)"], "deny": ["Read(//x)"] },
        "sandbox": { "excludedCommands": ["docker"] },
    });
    std::fs::write(fx.file("settings.json"), existing.to_string()).expect("seed");
    std::fs::create_dir_all(&fx.user).expect("mkdir user");
    std::fs::write(fx.user.join("settings.json"), r#"{"model":"opus"}"#).expect("seed user");
    write_sandbox_settings(SandboxRole::Pm, &fx.inputs, &fx.user).expect("write");
    let project = read(&fx.file("settings.json"));
    assert_eq!(project["hooks"], json!({ "Stop": [] }));
    assert_eq!(project["permissions"]["allow"], json!(["Bash(ls:*)"]));
    let deny = strings(&project, "/permissions/deny");
    assert_eq!(deny[0], "Read(//x)", "operator entries kept first");
    assert_eq!(project["sandbox"]["excludedCommands"], json!(["docker"]));
    assert_eq!(project["sandbox"]["enabled"], json!(true));
    let user = read(&fx.user.join("settings.json"));
    assert_eq!(user["model"], json!("opus"));
    assert_eq!(
        user.pointer("/sandbox/network/strictAllowlist"),
        Some(&json!(true))
    );
    // A second write adds no duplicate deny entry.
    write_sandbox_settings(SandboxRole::Pm, &fx.inputs, &fx.user).expect("rewrite");
    let again = strings(&read(&fx.file("settings.json")), "/permissions/deny");
    assert_eq!(again, deny);
}

#[test]
fn a_local_override_refuses_the_launch() {
    let fx = Fx::new();
    std::fs::write(
        fx.file("settings.local.json"),
        r#"{"sandbox":{"enabled":false}}"#,
    )
    .expect("seed local");
    refused(
        write_sandbox_settings(SandboxRole::Pm, &fx.inputs, &fx.user),
        "a local sandbox override",
    );
    // A local file with no sandbox key is fine.
    std::fs::write(fx.file("settings.local.json"), r#"{"model":"x"}"#).expect("local");
    write_sandbox_settings(SandboxRole::Agent, &fx.inputs, &fx.user).expect("allowed");
}

#[test]
fn a_weakened_project_file_refuses_the_launch() {
    let fx = Fx::new();
    write_sandbox_settings(SandboxRole::Pm, &fx.inputs, &fx.user).expect("write");
    let path = fx.file("settings.json");
    for (pointer, value) in [
        ("/sandbox/enabled", json!(false)),
        ("/sandbox/allowUnsandboxedCommands", json!(true)),
    ] {
        let mut project = read(&path);
        *project.pointer_mut(pointer).expect("key") = value;
        std::fs::write(&path, project.to_string()).expect("weaken");
        refused(
            verify_sandbox_settings(SandboxRole::Pm, &fx.inputs.workspace, &fx.user),
            pointer,
        );
        write_sandbox_settings(SandboxRole::Pm, &fx.inputs, &fx.user).expect("restore");
    }
    // The user tier without strictAllowlist refuses too.
    std::fs::write(fx.user.join("settings.json"), "{}").expect("weaken user");
    refused(
        verify_sandbox_settings(SandboxRole::Agent, &fx.inputs.workspace, &fx.user),
        "no strictAllowlist",
    );
    // The Architect's file must disable the sandbox.
    refused(
        verify_sandbox_settings(SandboxRole::Architect, &fx.inputs.workspace, &fx.user),
        "an Architect with the sandbox on",
    );
}

#[test]
fn unreadable_settings_refuse_the_launch() {
    let fx = Fx::new();
    // No project file at all.
    let missing = verify_sandbox_settings(SandboxRole::Pm, &fx.inputs.workspace, &fx.user);
    assert!(
        matches!(missing, Err(SandboxSettingsError::Unreadable { .. })),
        "{missing:?}"
    );
    write_sandbox_settings(SandboxRole::Pm, &fx.inputs, &fx.user).expect("write");
    for (file, body) in [
        (fx.file("settings.local.json"), "{ not json"),
        (fx.file("settings.local.json"), "[]"),
        (fx.user.join("settings.json"), "{ not json"),
    ] {
        let before = std::fs::read_to_string(&file).ok();
        std::fs::write(&file, body).expect("corrupt");
        let result = verify_sandbox_settings(SandboxRole::Pm, &fx.inputs.workspace, &fx.user);
        assert!(
            matches!(result, Err(SandboxSettingsError::Unreadable { .. })),
            "{} = {body:?}: {result:?}",
            file.display()
        );
        match before {
            Some(text) => std::fs::write(&file, text).expect("restore"),
            None => std::fs::remove_file(&file).expect("remove"),
        }
    }
}
