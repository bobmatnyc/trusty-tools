//! #8453: the real launch of a supervisor project, and of a PM project.
//!
//! Why: the prompt composer, the style selector and the settings writer each
//! branch on the profile; only the launch proves they agree in one session.
//! What: runs `prepare_session_inner` against a scratch `$HOME` whose
//! `.trusty-mpm/config.toml` is the user-level allowlist, and reads back the
//! settings and the instruction stash (`.trusty-mpm/last-instructions.md`).
//! Test: this is the test module.

use super::tests::EnvVarGuard;
use super::*;
use crate::core::session_profile::{SUPERVISOR_MODEL, SessionProfile};
use tempfile::tempdir;

/// Launch `project` with `home` as `$HOME`; return the profile, the settings
/// and the stash.
fn launch_in(project: &Path, home: &Path) -> (SessionProfile, serde_json::Value, String) {
    let _home = EnvVarGuard::set("HOME", home);
    let fw = crate::core::paths::FrameworkPaths::under(home);
    let report = prepare_session_inner(
        &fw,
        project,
        None,
        true,
        None,
        None,
        HostInputs::with_home(Some(home)),
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
    (report.profile, settings, stash)
}

/// A scratch `$HOME` whose user config allow-lists `project`.
fn home_allowing(project: &Path) -> tempfile::TempDir {
    let home = tempdir().unwrap();
    let root = home.path().join(".trusty-mpm");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        root.join("config.toml"),
        format!(
            "[supervisor]\nprojects = [{:?}]\n",
            project.display().to_string()
        ),
    )
    .unwrap();
    home
}

/// Write the project's `.trusty-mpm.toml` profile.
fn set_profile(project: &Path, profile: &str) {
    std::fs::write(
        project.join(".trusty-mpm.toml"),
        format!("profile = \"{profile}\"\n"),
    )
    .unwrap();
}

#[test]
#[serial_test::serial]
fn a_supervisor_launch_gets_the_supervisor_prompt_style_and_model() {
    let tmp = tempdir().unwrap();
    set_profile(tmp.path(), "supervisor");
    let home = home_allowing(tmp.path());
    let (profile, settings, stash) = launch_in(tmp.path(), home.path());
    assert_eq!(profile, SessionProfile::Supervisor);
    assert_eq!(
        settings["outputStyle"],
        crate::core::session_profile::SUPERVISOR_OUTPUT_STYLE_ID
    );
    assert_eq!(settings["model"], SUPERVISOR_MODEL);
    assert!(
        stash.contains(&crate::core::session_profile::supervisor_prompt()),
        "{stash}"
    );
    assert!(!stash.contains("STRICTLY FORBIDDEN FROM DOING ANY WORK DIRECTLY"));
    assert!(!stash.contains("## Delegation Authority"), "{stash}");
}

#[test]
#[serial_test::serial]
fn a_project_only_switch_launches_as_a_pm() {
    // #3981: no user-level allowlist entry, so the file alone changes nothing.
    let tmp = tempdir().unwrap();
    set_profile(tmp.path(), "supervisor");
    let home = tempdir().unwrap();
    let (profile, settings, stash) = launch_in(tmp.path(), home.path());
    assert_eq!(profile, SessionProfile::Pm);
    assert_eq!(
        settings["outputStyle"],
        crate::core::bundle::DEFAULT_OUTPUT_STYLE_ID
    );
    assert!(settings.get("model").is_none(), "{settings}");
    assert!(!stash.contains("# Trusty Fleet Supervisor"));
}

#[test]
#[serial_test::serial]
fn a_pm_launch_writes_no_model_and_keeps_the_pm_style() {
    let tmp = tempdir().unwrap();
    let home = tempdir().unwrap();
    let (profile, settings, stash) = launch_in(tmp.path(), home.path());
    assert_eq!(profile, SessionProfile::Pm);
    assert_eq!(
        settings["outputStyle"],
        crate::core::bundle::DEFAULT_OUTPUT_STYLE_ID
    );
    assert!(settings.get("model").is_none(), "{settings}");
    assert!(!stash.contains("# Trusty Fleet Supervisor"));
}

#[test]
#[serial_test::serial]
fn flipping_back_to_pm_removes_the_model_tm_wrote() {
    let tmp = tempdir().unwrap();
    set_profile(tmp.path(), "supervisor");
    let home = home_allowing(tmp.path());
    let (_, settings, _) = launch_in(tmp.path(), home.path());
    assert_eq!(settings["model"], SUPERVISOR_MODEL);
    set_profile(tmp.path(), "pm");
    let (profile, settings, _) = launch_in(tmp.path(), home.path());
    assert_eq!(profile, SessionProfile::Pm);
    assert!(settings.get("model").is_none(), "{settings}");
}

#[test]
#[serial_test::serial]
fn a_user_set_model_is_never_touched() {
    let tmp = tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".claude")).unwrap();
    std::fs::write(
        tmp.path().join(".claude").join("settings.json"),
        r#"{"model": "sonnet"}"#,
    )
    .unwrap();
    set_profile(tmp.path(), "supervisor");
    let home = home_allowing(tmp.path());
    // A supervisor launch keeps the user's value …
    let (profile, settings, _) = launch_in(tmp.path(), home.path());
    assert_eq!(profile, SessionProfile::Supervisor);
    assert_eq!(settings["model"], "sonnet");
    // … and so does the PM launch after it.
    set_profile(tmp.path(), "pm");
    let (_, settings, _) = launch_in(tmp.path(), home.path());
    assert_eq!(settings["model"], "sonnet");
}
