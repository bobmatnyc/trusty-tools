//! #8453: the real launch of a supervisor project, and of a PM project.
//!
//! Why: the prompt composer, the style selector and the settings writer each
//! branch on the profile; only the launch proves they agree in one session.
//! What: runs `prepare_session_inner` and reads back the settings and the
//! instruction stash (`.trusty-mpm/last-instructions.md`).
//! Test: this is the test module.

use super::tests::EnvVarGuard;
use super::*;
use tempfile::tempdir;

/// Launch `project` with a scratch `$HOME`; return its settings and stash.
fn launch(project: &Path) -> (serde_json::Value, String) {
    let tmp_home = tempdir().unwrap();
    let _home = EnvVarGuard::set("HOME", tmp_home.path());
    let fw = crate::core::paths::FrameworkPaths::under(tmp_home.path());
    prepare_session_inner(
        &fw,
        project,
        None,
        true,
        None,
        None,
        HostInputs::with_home(Some(tmp_home.path())),
    )
    .expect("the launch prepares");
    let settings = serde_json::from_str(
        &std::fs::read_to_string(project.join(".claude").join("settings.json")).unwrap(),
    )
    .unwrap();
    let stash = std::fs::read_to_string(
        crate::core::harness_root::harness_dir(project).join("last-instructions.md"),
    )
    .unwrap();
    (settings, stash)
}

#[test]
#[serial_test::serial]
fn a_supervisor_launch_gets_the_supervisor_prompt_style_and_model() {
    let tmp = tempdir().unwrap();
    std::fs::write(
        tmp.path().join(".trusty-mpm.toml"),
        "profile = \"supervisor\"\n",
    )
    .unwrap();
    let (settings, stash) = launch(tmp.path());
    assert_eq!(
        settings["outputStyle"],
        crate::core::session_profile::SUPERVISOR_OUTPUT_STYLE_ID
    );
    assert_eq!(
        settings["model"],
        crate::core::session_profile::SUPERVISOR_MODEL
    );
    assert!(
        stash.contains(&crate::core::session_profile::supervisor_prompt()),
        "{stash}"
    );
    assert!(!stash.contains("STRICTLY FORBIDDEN FROM DOING ANY WORK DIRECTLY"));
    assert!(!stash.contains("## Delegation Authority"), "{stash}");
}

#[test]
#[serial_test::serial]
fn a_pm_launch_writes_no_model_and_keeps_the_pm_style() {
    let tmp = tempdir().unwrap();
    let (settings, stash) = launch(tmp.path());
    assert_eq!(
        settings["outputStyle"],
        crate::core::bundle::DEFAULT_OUTPUT_STYLE_ID
    );
    assert!(settings.get("model").is_none(), "{settings}");
    assert!(!stash.contains("# Trusty Fleet Supervisor"));
}
