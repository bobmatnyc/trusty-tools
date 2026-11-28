//! Tests for the #10102 launch-time sandbox entry and the per-role record.
//!
//! #12077: the launch entry writes nothing; the #10102 tests drive the
//! retained `enforce_launch_sandbox`, which slice 2 calls behind an opt-in.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use tempfile::TempDir;

use super::super::sandbox_roles::record_path;
use super::super::{SandboxInputs, SandboxRole, SandboxSettingsError, project_policy};
use super::{
    apply_launch_sandbox, enforce_launch_sandbox, launch_sandbox_inputs, sandbox_role_for,
};
use crate::core::session_profile::SessionProfile;

/// A workspace and home under one temp dir, plus a tm-owned config dir.
struct Fx {
    _dir: TempDir,
    root: PathBuf,
    config: PathBuf,
}

impl Fx {
    fn new() -> Self {
        let dir = TempDir::new().expect("tempdir");
        let root = std::fs::canonicalize(dir.path()).expect("canonical");
        std::fs::create_dir_all(root.join("ws/.claude")).expect("mkdir ws");
        let config = root.join("home/.trusty-tools/trusty-mpm/claude-config");
        Self {
            _dir: dir,
            root,
            config,
        }
    }

    fn inputs(&self, tmux: &str) -> SandboxInputs {
        let mut inputs = SandboxInputs::new(self.root.join("ws"), self.root.join("home"));
        inputs.tmux_socket = Some(PathBuf::from(tmux));
        inputs.mpm_socket = Some(self.root.join("trusty-mpm.sock"));
        inputs
    }

    fn project(&self) -> Value {
        read(&self.root.join("ws/.claude/settings.json"))
    }
}

fn read(path: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).expect("read")).expect("json")
}

fn sockets(settings: &Value) -> Vec<String> {
    settings
        .pointer("/sandbox/network/allowUnixSockets")
        .and_then(Value::as_array)
        .expect("allowUnixSockets")
        .iter()
        .map(|v| v.as_str().expect("text").to_owned())
        .collect()
}

fn refusal(result: Result<(), SandboxSettingsError>) -> String {
    match result {
        Err(SandboxSettingsError::Refused(why)) => why,
        other => panic!("the launch must be refused, got {other:?}"),
    }
}

/// #10102 criterion 2: no config dir means no launch, and nothing is written.
#[test]
fn a_launch_with_no_config_dir_is_refused() {
    let fx = Fx::new();
    let why = refusal(enforce_launch_sandbox(
        SandboxRole::Pm,
        &fx.inputs("/t/a"),
        None,
    ));
    assert!(
        why.contains("CLAUDE_CONFIG_DIR") && why.contains("tm doctor"),
        "{why}"
    );
    assert!(
        !fx.root.join("home/.claude").exists(),
        "no `~/.claude` write"
    );
    assert!(!fx.root.join("ws/.claude/settings.json").exists());
}

/// #10102 criterion 2: the operator's own `~/.claude` is never the user tier.
#[test]
fn a_launch_into_the_operators_claude_dir_is_refused() {
    let fx = Fx::new();
    let inputs = fx.inputs("/t/a");
    let operators = inputs.home.join(".claude");
    let why = refusal(enforce_launch_sandbox(
        SandboxRole::Pm,
        &inputs,
        Some(&operators),
    ));
    assert!(why.contains("tm-owned"), "{why}");
    assert!(!operators.join("settings.json").exists());
    let why = refusal(enforce_launch_sandbox(
        SandboxRole::Pm,
        &inputs,
        Some(Path::new("rel")),
    ));
    assert!(why.contains("absolute"), "{why}");
}

/// #10102: a good launch writes both tiers and they verify.
#[test]
fn a_launch_applies_and_verifies_the_policy() {
    let fx = Fx::new();
    let inputs = fx.inputs("/t/a");
    enforce_launch_sandbox(SandboxRole::Pm, &inputs, Some(&fx.config)).expect("applies");
    assert_eq!(fx.project().pointer("/sandbox/enabled"), Some(&json!(true)));
    let user = read(&fx.config.join("settings.json"));
    assert_eq!(
        user.pointer("/sandbox/network/strictAllowlist"),
        Some(&json!(true))
    );
    assert_eq!(sandbox_role_for(SessionProfile::Pm), SandboxRole::Pm);
    assert_eq!(
        sandbox_role_for(SessionProfile::Architect),
        SandboxRole::Architect
    );
    let resolved = launch_sandbox_inputs(&inputs.workspace, &inputs.home);
    assert_eq!(resolved.workspace, inputs.workspace);
    assert!(resolved.tmux_socket.is_some(), "the workspace tmux socket");
}

/// #10102 Fail-Open Check: a read-back that does not verify refuses the launch.
#[test]
fn a_verify_failure_refuses_the_launch() {
    let fx = Fx::new();
    std::fs::write(
        fx.root.join("ws/.claude/settings.local.json"),
        r#"{"sandbox":{"enabled":false}}"#,
    )
    .expect("write local");
    let why = refusal(enforce_launch_sandbox(
        SandboxRole::Pm,
        &fx.inputs("/t/a"),
        Some(&fx.config),
    ));
    assert!(why.contains("settings.local.json"), "{why}");
}

/// #10102 criterion 3: a PM and an agent launched into one workspace each
/// keep the other's entries — the PM's tmux socket survives the agent launch,
/// and the agent's target dir survives a PM relaunch.
#[test]
fn two_sandboxed_roles_in_one_workspace_keep_each_others_entries() {
    let fx = Fx::new();
    let pm = fx.inputs("/t/pm-socket");
    enforce_launch_sandbox(SandboxRole::Pm, &pm, Some(&fx.config)).expect("pm");
    let mut agent = fx.inputs("/t/unused");
    agent.cargo_target_dir = Some(fx.root.join("agent-target"));
    enforce_launch_sandbox(SandboxRole::Agent, &agent, Some(&fx.config)).expect("agent");
    enforce_launch_sandbox(SandboxRole::Pm, &pm, Some(&fx.config)).expect("pm again");
    let settings = fx.project();
    assert!(
        sockets(&settings).contains(&"/t/pm-socket".to_owned()),
        "{settings}"
    );
    let writes = settings
        .pointer("/sandbox/filesystem/allowWrite")
        .expect("allowWrite");
    assert!(
        writes
            .as_array()
            .expect("list")
            .contains(&json!(fx.root.join("agent-target").display().to_string())),
        "{writes}"
    );
    let record = read(&record_path(&pm.workspace));
    assert_eq!(record["pm"], project_policy(SandboxRole::Pm, &pm));
    assert_eq!(record["agent"], project_policy(SandboxRole::Agent, &agent));
}

/// #10102 criterion 3: an operator allow-list entry is still replaced — the
/// union is over tm's recorded roles, never the file's own lists.
#[test]
fn the_role_union_never_keeps_an_operator_socket() {
    let fx = Fx::new();
    std::fs::write(
        fx.root.join("ws/.claude/settings.json"),
        r#"{"sandbox":{"network":{"allowUnixSockets":["/op/sccache.sock"]}}}"#,
    )
    .expect("seed");
    let pm = fx.inputs("/t/pm-socket");
    enforce_launch_sandbox(SandboxRole::Pm, &pm, Some(&fx.config)).expect("pm");
    assert!(!sockets(&fx.project()).contains(&"/op/sccache.sock".to_owned()));
}

/// #10102 criterion 3: the Architect and a sandboxed role need opposite
/// `sandbox.enabled` values, so the second launch is refused and the first
/// role's policy is left intact, in either order.
#[test]
fn the_architect_and_a_sandboxed_role_never_overwrite_each_other() {
    let fx = Fx::new();
    let pm = fx.inputs("/t/pm-socket");
    enforce_launch_sandbox(SandboxRole::Pm, &pm, Some(&fx.config)).expect("pm");
    let why = refusal(enforce_launch_sandbox(
        SandboxRole::Architect,
        &pm,
        Some(&fx.config),
    ));
    assert!(
        why.contains("`pm` role") && why.contains("tm fleet init"),
        "{why}"
    );
    assert_eq!(fx.project().pointer("/sandbox/enabled"), Some(&json!(true)));

    let other = Fx::new();
    let arch = other.inputs("/t/a");
    enforce_launch_sandbox(SandboxRole::Architect, &arch, Some(&other.config)).expect("arch");
    refusal(enforce_launch_sandbox(
        SandboxRole::Agent,
        &arch,
        Some(&other.config),
    ));
    assert_eq!(
        other.project().pointer("/sandbox/enabled"),
        Some(&json!(false))
    );
}

/// #10102 criterion 4: the sccache client env reaches the Architect's settings
/// `env` block, and a sandboxed role keeps `RUSTC_WRAPPER` empty without it.
#[test]
fn only_the_architect_gets_the_sccache_client_env() {
    let fx = Fx::new();
    let mut inputs = fx.inputs("/t/a");
    let socket = fx.root.join("home/.trusty-mpm/sccache.sock");
    inputs.sccache_socket = Some(socket.clone());
    enforce_launch_sandbox(SandboxRole::Architect, &inputs, Some(&fx.config)).expect("arch");
    let env = fx.project()["env"].clone();
    assert_eq!(
        env["SCCACHE_SERVER_UDS"],
        json!(socket.display().to_string())
    );
    assert_eq!(env["SCCACHE_CLIENT_SIDE"], json!("1"));

    let pm = project_policy(SandboxRole::Pm, &inputs);
    assert_eq!(pm["env"], json!({ "RUSTC_WRAPPER": "" }));
}

/// #10102 fix round, Fail-Open Check: a corrupt role record refuses the
/// launch, naming the file, and never lets an Architect flip a PM's sandbox.
#[test]
fn a_corrupt_role_record_refuses_the_launch() {
    let fx = Fx::new();
    let pm = fx.inputs("/t/pm-socket");
    enforce_launch_sandbox(SandboxRole::Pm, &pm, Some(&fx.config)).expect("pm");
    let record = record_path(&pm.workspace);
    for corrupt in ["{not json", "[]"] {
        std::fs::write(&record, corrupt).expect("corrupt the record");
        match enforce_launch_sandbox(SandboxRole::Architect, &pm, Some(&fx.config)) {
            Err(SandboxSettingsError::Record { path, source }) => {
                assert_eq!(path, record);
                assert!(
                    source.to_string().contains("sandbox-roles.json"),
                    "{source}"
                );
            }
            other => panic!("{corrupt}: a corrupt record must refuse, got {other:?}"),
        }
        assert_eq!(fx.project().pointer("/sandbox/enabled"), Some(&json!(true)));
    }
}

/// #12077: a default launch, in every role, writes no sandbox key anywhere —
/// the project tier, the local override, the role record and the user tier
/// are left exactly as they were.
#[test]
fn a_default_launch_writes_no_sandbox_key_12077() {
    let fx = Fx::new();
    let project = fx.root.join("ws/.claude/settings.json");
    let local = fx.root.join("ws/.claude/settings.local.json");
    let user = fx.config.join("settings.json");
    std::fs::create_dir_all(&fx.config).expect("mkdir config");
    for (path, body) in [
        (&project, r#"{"theme":"dark"}"#),
        (&local, r#"{"model":"x"}"#),
        (&user, r#"{"theme":"light"}"#),
    ] {
        std::fs::write(path, body).expect("seed");
    }
    let inputs = fx.inputs("/t/a");
    for role in [SandboxRole::Pm, SandboxRole::Agent, SandboxRole::Architect] {
        apply_launch_sandbox(role, &inputs, Some(&fx.config)).expect("a launch is never refused");
    }
    assert_eq!(fx.project(), json!({ "theme": "dark" }));
    assert_eq!(read(&local), json!({ "model": "x" }));
    assert_eq!(read(&user), json!({ "theme": "light" }));
    assert!(
        !record_path(&inputs.workspace).exists(),
        "no sandbox-roles.json"
    );
}

/// #12077: the #10102 refusal is gone — no config dir, or the operator's own
/// `~/.claude`, no longer refuses the launch, and nothing lands in `~/.claude`.
#[test]
fn a_launch_with_no_config_dir_is_not_refused_12077() {
    let fx = Fx::new();
    let inputs = fx.inputs("/t/a");
    let operators = inputs.home.join(".claude");
    for dir in [None, Some(operators.as_path()), Some(Path::new("rel"))] {
        apply_launch_sandbox(SandboxRole::Pm, &inputs, dir).expect("not refused");
    }
    assert!(!operators.exists(), "no `~/.claude` write");
    assert!(!fx.root.join("ws/.claude/settings.json").exists());
}
