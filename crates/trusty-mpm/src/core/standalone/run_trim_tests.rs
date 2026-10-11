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
        None,
        &tmp.path().join("p.txt"),
        profile,
    );
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
fn run_launch_command_trims_a_pm_and_not_a_supervisor() {
    let deny = [
        "--disallowedTools",
        "Workflow,DesignSync,mcp__claude-in-chrome",
    ];
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

    let (args, env) = launched(SessionProfile::Supervisor);
    assert!(!args.iter().any(|a| a == "--disallowedTools"), "{args:?}");
    assert!(!env.iter().any(|(k, _)| k == "CLAUDE_CODE_DISABLE_ARTIFACT"));
    assert!(env.contains(&(
        "TRUSTY_MPM_SESSION_PROFILE".to_owned(),
        Some("supervisor".to_owned())
    )));
}
