//! Tests for [`super::respond`]: the payload-failure arm (#10960).

use std::ffi::OsString;

use trusty_mpm::core::config::MpmConfig;
use trusty_mpm::core::session_profile::{
    ARCHITECT_WRITTEN_PROFILE_ID, PROJECT_DIR_ENV, SESSION_PROFILE_ENV,
};

use super::respond;

/// A granted Architect project holding a standing-rules file.
fn architect_project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join(".trusty-mpm.toml"),
        "profile = \"supervisor\"\n",
    )
    .expect("write project config");
    std::fs::create_dir_all(dir.path().join("local")).expect("mkdir local");
    std::fs::write(dir.path().join("local/standing-rules.md"), "- rule one\n")
        .expect("write rules");
    dir
}

/// `respond` for an Architect session at `project` (stamp, allowlist and file).
fn architect_respond(project: &std::path::Path, stdin: Option<String>) -> super::Response {
    let env = |name: &str| match name {
        SESSION_PROFILE_ENV => Some(OsString::from(ARCHITECT_WRITTEN_PROFILE_ID)),
        PROJECT_DIR_ENV => Some(project.as_os_str().to_owned()),
        _ => None,
    };
    let config = || {
        let mut config = MpmConfig::default();
        config.architect.get_or_insert_default().projects = vec![project.to_path_buf()];
        config
    };
    respond(env, stdin, config)
}

#[test]
fn an_unreadable_payload_warns_and_injects_nothing() {
    let project = architect_project();
    // Control: a readable payload in the same session injects the rules, so the
    // empty stdout below comes from the payload arm, not a missing grant.
    let payload = r#"{"hook_event_name":"UserPromptSubmit"}"#.to_owned();
    let granted = architect_respond(project.path(), Some(payload));
    assert!(
        granted.stdout.is_some_and(|out| out.contains("- rule one")),
        "the Architect grant injects the rules"
    );
    for stdin in [Some("not json".to_owned()), None] {
        let got = architect_respond(project.path(), stdin.clone());
        assert_eq!(got.stdout, None, "nothing injected for {stdin:?}");
        let stderr = got.stderr.expect("a stderr warning");
        assert!(
            stderr.contains("no readable payload") && stderr.contains("no rules injected"),
            "{stdin:?}: {stderr}"
        );
    }
}
