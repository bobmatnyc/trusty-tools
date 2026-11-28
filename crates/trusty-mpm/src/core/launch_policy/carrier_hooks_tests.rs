//! Tests for tm's hooks on the carrier body (#12077 part 4c).

use std::collections::BTreeMap;

use super::*;
use crate::core::launch_policy::carrier::{LaunchSettings, provision_settings_at};
use crate::core::session_profile::SessionProfile;
use crate::core::standalone::hooks::egress::{EgressGuardState, egress_guard_state};
use crate::core::standalone::hooks::{StableHookExeError, hook_entry_command_line};
use crate::test_support::STABLE_HOOK_EXE;
use serde_json::json;

/// A project, a framework base and a state root, all scratch.
struct Fx {
    _tmp: tempfile::TempDir,
    repos: PathBuf,
    state: PathBuf,
    project: PathBuf,
    fw: FrameworkPaths,
}

impl Fx {
    fn new() -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().canonicalize().expect("canonical");
        let repos = root.join("repos");
        let project = repos.join("acme").join("widget");
        std::fs::create_dir_all(project.join(".claude")).expect("mkdir");
        let fw = FrameworkPaths::for_managed_workspace_under(root.join("base"), &project);
        Self {
            state: root.join("state"),
            _tmp: tmp,
            repos,
            project,
            fw,
        }
    }
    fn provision(&self, profile: SessionProfile) -> Result<LaunchSettings, ProvisionError> {
        self.provision_at(&self.state, profile)
    }
    fn provision_at(
        &self,
        state: &Path,
        profile: SessionProfile,
    ) -> Result<LaunchSettings, ProvisionError> {
        provision_settings_at(&CarrierInputs {
            cwd: &self.project,
            repos_root: &self.repos,
            state_root: state,
            claude_json_dir: None,
            profile,
            hooks: Some(LaunchHooks {
                fw: &self.fw,
                exe: Path::new(STABLE_HOOK_EXE),
            }),
        })
    }
}

fn body(settings: &LaunchSettings) -> Value {
    let file = settings.file.as_ref().expect("a body");
    serde_json::from_slice(&std::fs::read(file).expect("read body")).expect("JSON body")
}

/// `event → command lines` for every entry under `val`'s `hooks`.
fn lines_by_event(val: &Value) -> BTreeMap<String, Vec<String>> {
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (event, groups) in val["hooks"].as_object().into_iter().flatten() {
        for group in groups.as_array().into_iter().flatten() {
            for entry in group["hooks"].as_array().into_iter().flatten() {
                if let Some(line) = hook_entry_command_line(entry) {
                    out.entry(event.clone())
                        .or_default()
                        .push(line.into_owned());
                }
            }
        }
    }
    out
}

#[test]
fn the_carrier_body_carries_the_hooks_12077() {
    let fx = Fx::new();
    let launch = fx.provision(SessionProfile::Pm).expect("provisions");
    let body = body(&launch);
    assert_eq!(
        egress_guard_state(&body),
        EgressGuardState::Present,
        "{body}"
    );
    let pre = &lines_by_event(&body)["PreToolUse"];
    assert!(
        pre.iter().any(|l| l.ends_with(" hook --pm-guard")),
        "{pre:?}"
    );
    // Never in the stored record the divert worker reads.
    let record = crate::core::session_launch::launch_keys_record_path(&fx.project);
    assert!(!record.exists(), "the carrier writes no record");
    // And never into the project tier.
    assert!(!fx.project.join(".claude").join("settings.json").exists());
}

#[test]
fn an_architect_carrier_body_keeps_its_exec_form_guard_12077() {
    let fx = Fx::new();
    let launch = fx.provision(SessionProfile::Architect).expect("provisions");
    let pre = lines_by_event(&body(&launch))["PreToolUse"].clone();
    assert!(
        pre.contains(&format!("{STABLE_HOOK_EXE} hook --pm-guard")),
        "{pre:?}"
    );
}

#[test]
fn the_carrier_strips_tms_old_project_hooks_12077() {
    let fx = Fx::new();
    let path = fx.project.join(".claude").join("settings.json");
    let old = json!({ "model": "opus", "hooks": { "Stop": [
        { "matcher": "*", "hooks": [{ "type": "command", "command": "/old/bin/tm hook" }] }
    ]}});
    std::fs::write(&path, old.to_string()).expect("seed");
    fx.provision(SessionProfile::Pm).expect("provisions");
    let after: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(after, json!({ "model": "opus" }));
}

/// Fail-Open Check (Q1 refusal): a body carrying hooks that cannot be written
/// refuses the launch, even with no launch keys and no owner policy.
#[test]
fn an_unwritable_hook_body_refuses_the_launch_12077() {
    let fx = Fx::new();
    let state = fx.repos.join("state-file");
    std::fs::write(&state, b"not a directory").expect("seed");
    let err = fx
        .provision_at(&state, SessionProfile::Pm)
        .expect_err("a hook body that cannot be written refuses");
    assert!(matches!(err, ProvisionError::Write { .. }), "{err}");
}

/// Fail-Open Check: a project file the strip cannot read refuses the launch.
#[test]
fn an_unwritable_project_file_refuses_the_carrier_12077() {
    let fx = Fx::new();
    std::fs::create_dir_all(fx.project.join(".claude").join("settings.local.json")).unwrap();
    let err = fx
        .provision(SessionProfile::Pm)
        .expect_err("an unreadable project file refuses");
    assert!(matches!(err, ProvisionError::Strip(_)), "{err}");
}

/// Fail-Open Check (Q2): no stable hook binary refuses every profile's launch.
#[test]
fn an_unresolvable_hook_binary_refuses_the_launch_12077() {
    let err: ProvisionError = StableHookExeError::Unresolved.into();
    assert!(matches!(err, ProvisionError::HookExe(_)));
    assert!(err.to_string().contains("refusing the launch"), "{err}");
    // A build-tree override with no installed fallback is refused by the
    // resolver itself; on a host with `tm` installed PATH rescues it, so the
    // arm is asserted through the conversion above.
    let ok = resolve_launch_hooks(Path::new("/tmp"), Some(Path::new(STABLE_HOOK_EXE)));
    assert_eq!(
        ok.expect("a pinned exe resolves").1,
        PathBuf::from(STABLE_HOOK_EXE)
    );
}

/// No hook fires twice: every tm command the managed `claude-config` file
/// carries is in the body byte for byte, or its executable is absent from the
/// body for that event, and the `Notification` hook is never in the body.
#[test]
fn the_body_and_the_managed_file_never_differ_on_a_tm_command_12077() {
    let fx = Fx::new();
    let managed = fx.repos.join("claude-config");
    std::fs::create_dir_all(&managed).unwrap();
    crate::core::standalone::hooks::ensure_managed_hooks_with_exe(
        &managed,
        Some(Path::new(STABLE_HOOK_EXE)),
    )
    .expect("managed hooks");
    let managed_value: Value =
        serde_json::from_slice(&std::fs::read(managed.join("settings.json")).unwrap()).unwrap();
    let body_lines = lines_by_event(&body(&fx.provision(SessionProfile::Pm).unwrap()));
    let managed_lines = lines_by_event(&managed_value);
    assert!(!body_lines.contains_key("Notification"), "{body_lines:?}");
    let exe_of = |line: &str| line.split_whitespace().next().unwrap_or("").to_owned();
    for (event, lines) in &managed_lines {
        let Some(in_body) = body_lines.get(event) else {
            continue;
        };
        for line in lines {
            let same_exe: Vec<&String> = in_body
                .iter()
                .filter(|b| exe_of(b) == exe_of(line))
                .collect();
            assert!(
                same_exe.is_empty() || in_body.contains(line),
                "{event}: managed `{line}` differs from the body's {same_exe:?}"
            );
        }
    }
}
