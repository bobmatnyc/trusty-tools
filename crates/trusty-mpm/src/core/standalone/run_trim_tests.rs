//! Tests for `tm run`'s #8992 tool trim.

use super::run_launch_command;
use crate::core::session_profile::SessionProfile;

/// The `(argv, env)` of the `tm run` command for `profile`.
fn launched(profile: SessionProfile) -> (Vec<String>, Vec<(String, Option<String>)>) {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let cmd = run_launch_command(
        None,
        &tmp.path().join("repo"),
        &tmp.path().join("cfg"),
        None,
        &Default::default(),
        &tmp.path().join("p.txt"),
        profile,
    )
    .expect("the command builds");
    let args = cmd
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let env = cmd
        .get_envs()
        .map(|(k, v)| {
            (
                k.to_string_lossy().into_owned(),
                v.map(|v| v.to_string_lossy().into_owned()),
            )
        })
        .collect();
    (args, env)
}

/// #8992: `tm run` of a PM carries the Artifact switch and the deny flag, and
/// stamps `pm`; a supervisor run carries neither and stamps `supervisor`.
/// Before the fix `run_launch_command` took no profile and trimmed nothing.
#[test]
fn run_launch_command_trims_a_pm_and_not_an_architect() {
    let deny = ["--disallowedTools", "Workflow,DesignSync"];
    let artifact_off = (
        "CLAUDE_CODE_DISABLE_ARTIFACT".to_owned(),
        Some("1".to_owned()),
    );
    let (args, env) = launched(SessionProfile::Pm);
    assert!(args.windows(2).any(|w| w == deny), "{args:?}");
    assert!(env.contains(&artifact_off), "{env:?}");
    assert!(env.contains(&(
        "TRUSTY_MPM_SESSION_PROFILE".to_owned(),
        Some("pm".to_owned())
    )));

    let (args, env) = launched(SessionProfile::Architect);
    assert!(!args.iter().any(|a| a == "--disallowedTools"), "{args:?}");
    assert!(!env.iter().any(|(k, _)| k == "CLAUDE_CODE_DISABLE_ARTIFACT"));
    assert!(env.contains(&(
        "TRUSTY_MPM_SESSION_PROFILE".to_owned(),
        Some("supervisor".to_owned())
    )));
}

/// #10127: `tm run` of an Architect carries the production deny as ONE
/// `--settings`, merged into the owner file, and removes every chat token
/// from the child; an owner file it cannot read refuses the run. Before the
/// fix the owner file rode bare and the tokens were inherited.
#[test]
fn run_launch_command_carries_the_architect_lockdown() {
    use crate::core::architect_lockdown::{ARCHITECT_ENV_UNSET, assert_production_deny};
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let carrier = tmp.path().join("owner.json");
    std::fs::write(&carrier, r#"{"deniedMcpServers":[{"serverName":"x"}]}"#).expect("carrier");
    let build = |settings: std::path::PathBuf| {
        run_launch_command(
            None,
            &tmp.path().join("repo"),
            &tmp.path().join("cfg"),
            None,
            &crate::core::launch_policy::LaunchFiles {
                mcp_config: None,
                settings: Some(settings),
            },
            &tmp.path().join("p.txt"),
            SessionProfile::Architect,
        )
    };
    let cmd = build(carrier.clone()).expect("the Architect command builds");
    let args: Vec<String> = cmd
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    assert_production_deny(&args);
    assert!(!args.contains(&carrier.display().to_string()), "{args:?}");
    for name in ARCHITECT_ENV_UNSET {
        assert!(
            cmd.get_envs().any(|(k, v)| k == *name && v.is_none()),
            "{name} must be removed"
        );
    }
    assert!(build(tmp.path().join("gone.json")).is_err());
}
