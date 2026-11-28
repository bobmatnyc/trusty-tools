//! #10102: `tm run`'s sandbox step; #12077: it writes nothing.

use super::run_sandbox;
use crate::core::session_profile::SessionProfile;

/// A repo, home and tm-owned config dir under one canonical temp dir.
fn fixture() -> (
    tempfile::TempDir,
    std::path::PathBuf,
    std::path::PathBuf,
    std::path::PathBuf,
) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = std::fs::canonicalize(tmp.path()).expect("canonical");
    let repo = root.join("projects/a/repo");
    std::fs::create_dir_all(repo.join(".claude")).expect("mkdir");
    let home = root.join("home");
    let config = root.join("projects/a/claude-config");
    (tmp, repo, home, config)
}

/// #12077: `tm run` writes no sandbox key into the repo or its config dir.
#[test]
fn run_sandbox_writes_no_sandbox_settings_12077() {
    let (_tmp, repo, home, config) = fixture();
    run_sandbox(&repo, Some(&home), &config, SessionProfile::Pm).expect("not refused");
    assert!(
        !repo.join(".claude/settings.json").exists(),
        "no project write"
    );
    assert!(!config.join("settings.json").exists(), "no user-tier write");
    assert!(!repo.join(".trusty-mpm/sandbox-roles.json").exists());
}

/// #12077: a local override that turns the sandbox off no longer refuses the
/// run, and is left as it was.
#[test]
fn run_sandbox_leaves_a_local_override_untouched_12077() {
    let (_tmp, repo, home, config) = fixture();
    let local = repo.join(".claude/settings.local.json");
    let planted = r#"{"sandbox":{"enabled":false}}"#;
    std::fs::write(&local, planted).expect("plant");
    run_sandbox(&repo, Some(&home), &config, SessionProfile::Pm).expect("not refused");
    assert_eq!(std::fs::read_to_string(&local).expect("read"), planted);
}

/// #12077: with no home resolved `tm run` goes ahead and no sandbox file is
/// written.
#[test]
fn run_sandbox_goes_ahead_with_no_home_12077() {
    let (_tmp, repo, _home, config) = fixture();
    run_sandbox(&repo, None, &config, SessionProfile::Pm).expect("no home is not refused");
    assert!(
        !repo.join(".claude/settings.json").exists(),
        "no project write"
    );
    assert!(!config.join("settings.json").exists(), "no user-tier write");
    assert!(!repo.join(".trusty-mpm/sandbox-roles.json").exists());
}

/// #12077: `tm run` carries the role's sandbox only when opted in, as ONE
/// inline `--settings` with the switch set, and writes no sandbox key either
/// way.
#[test]
fn run_carries_the_sandbox_inline_only_when_opted_in_12077() {
    let (_tmp, repo, home, config) = fixture();
    assert_eq!(
        super::run_sandbox_payload(false, &repo, Some(&home), SessionProfile::Pm)
            .expect("never refused"),
        None
    );
    let payload = super::run_sandbox_payload(true, &repo, Some(&home), SessionProfile::Pm)
        .expect("not refused")
        .expect("an opted-in payload");
    let files = crate::core::launch_policy::LaunchFiles {
        mcp_config: None,
        settings: None,
        sandbox: Some(payload),
    };
    let cmd = super::run_launch_command(
        None,
        &repo,
        &config,
        None,
        &files,
        &repo.join("p.txt"),
        SessionProfile::Pm,
    )
    .expect("the command builds");
    let args: Vec<String> = cmd
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let values: Vec<&String> = args
        .windows(2)
        .filter(|w| w[0] == "--settings")
        .map(|w| &w[1])
        .collect();
    assert_eq!(values.len(), 1, "{args:?}");
    let inline: serde_json::Value = serde_json::from_str(values[0]).expect("inline JSON");
    assert_eq!(
        inline.pointer("/sandbox/enabled"),
        Some(&serde_json::json!(true))
    );
    let switch = crate::core::claude_sandbox::SANDBOX_OPT_IN_ENV;
    assert!(
        cmd.get_envs()
            .any(|(k, v)| k == switch && v == Some(std::ffi::OsStr::new("1"))),
        "the switch reaches claude"
    );
    run_sandbox(&repo, Some(&home), &config, SessionProfile::Pm).expect("not refused");
    for file in [
        repo.join(".claude/settings.json"),
        repo.join(".claude/settings.local.json"),
        home.join(".claude/settings.json"),
        config.join("settings.json"),
        repo.join(".trusty-mpm/sandbox-roles.json"),
    ] {
        assert!(!file.exists(), "{} was written", file.display());
    }
}

/// #12077 Fail-Open Check: `tm run` with the switch on and no home is refused
/// with an error naming the switch, and a default run with no home is not; a
/// default run's `claude` never inherits the switch.
#[test]
fn run_refuses_an_opt_in_with_no_home_12077() {
    let (_tmp, repo, _home, config) = fixture();
    let err = super::run_sandbox_payload(true, &repo, None, SessionProfile::Pm)
        .expect_err("an opt-in with no home is refused");
    assert!(err.to_string().contains("TM_CLAUDE_SANDBOX"), "{err}");
    assert_eq!(
        super::run_sandbox_payload(false, &repo, None, SessionProfile::Pm).expect("default"),
        None
    );
    let files = crate::core::launch_policy::LaunchFiles::default();
    let cmd = super::run_launch_command(
        None,
        &repo,
        &config,
        None,
        &files,
        &repo.join("p.txt"),
        SessionProfile::Pm,
    )
    .expect("the command builds");
    let switch = crate::core::claude_sandbox::SANDBOX_OPT_IN_ENV;
    assert!(
        cmd.get_envs().any(|(k, v)| k == switch && v.is_none()),
        "a default run removes the switch"
    );
}
