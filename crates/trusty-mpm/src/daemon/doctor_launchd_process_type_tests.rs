//! Tests for the `launchd_process_type` doctor row (#8415).
//!
//! Every fixture is a temp directory; no test reads the real
//! `~/Library/LaunchAgents` or calls `launchctl`.

use super::*;

/// The supervisor plist as `tctl install` wrote it before #8415.
const PRE_8415_SUPERVISOR: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.trusty.mpm.supervisor</string>
    <key>RunAtLoad</key>
    <true/>
    <key>ProcessType</key>
    <string>Background</string>
</dict>
</plist>"#;

/// A daemon plist with no `ProcessType` key, as hand-written installs carry.
const KEYLESS_DAEMON: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.trusty.mpm</string>
</dict>
</plist>"#;

/// The supervisor template this repository ships for the manual sed recipe.
const DEPLOY_SUPERVISOR: &str =
    include_str!("../../deploy/supervisor/com.trusty.mpm.supervisor.plist");

fn reading(label: &'static str, reading: ProcessTypeReading) -> PlistReading {
    PlistReading {
        label,
        path: PathBuf::from(format!("/tmp/LaunchAgents/{label}.plist")),
        reading,
    }
}

fn declared(v: Option<&str>) -> ProcessTypeReading {
    ProcessTypeReading::Declared(v.map(str::to_owned))
}

/// A home whose LaunchAgents holds `files` as `(label, contents)`.
fn home_with(files: &[(&str, &[u8])]) -> tempfile::TempDir {
    let home = tempfile::tempdir().expect("tempdir");
    let agents = home.path().join("Library/LaunchAgents");
    std::fs::create_dir_all(&agents).expect("mkdir");
    for (label, body) in files {
        std::fs::write(agents.join(format!("{label}.plist")), body).expect("write plist");
    }
    home
}

/// REGRESSION (#8415): the plist every pre-#8415 install carries fails the row
/// and names the exact `plutil` remedy for its path.
#[test]
fn background_supervisor_plist_fails() {
    let home = home_with(&[(MPM_SUPERVISOR, PRE_8415_SUPERVISOR.as_bytes())]);
    let row = check_launchd_process_type(home.path());
    assert_eq!(row.status, CheckStatus::Fail, "{}", row.message);
    assert!(row.message.contains(MPM_SUPERVISOR), "{}", row.message);
    assert!(
        row.message
            .contains("plutil -replace ProcessType -string Interactive"),
        "{}",
        row.message
    );
    assert!(row.message.contains("#8415"), "{}", row.message);
}

/// REGRESSION (#8415): the template this repo ships passes its own row. Fails
/// on the pre-fix template, which declared `Background`.
#[test]
fn deploy_supervisor_template_passes_the_row() {
    assert_eq!(
        process_type_of(DEPLOY_SUPERVISOR).as_deref(),
        Some(EXPECTED_PROCESS_TYPE),
        "{DEPLOY_SUPERVISOR}"
    );
    let home = home_with(&[(MPM_SUPERVISOR, DEPLOY_SUPERVISOR.as_bytes())]);
    let row = check_launchd_process_type(home.path());
    assert_eq!(row.status, CheckStatus::Ok, "{}", row.message);
}

/// A daemon plist with no key runs under launchd's throttled `Standard`
/// default, which the daemon's tmux servers inherit: warn, not fail.
#[test]
fn keyless_daemon_plist_warns_standard_default() {
    let row = build_process_type_check(&[
        reading(MPM, declared(None)),
        reading(MPM_SUPERVISOR, ProcessTypeReading::NotInstalled),
    ]);
    assert_eq!(row.status, CheckStatus::Warn, "{}", row.message);
    assert!(row.message.contains("Standard"), "{}", row.message);
}

#[test]
fn interactive_plists_pass() {
    let row = build_process_type_check(&[
        reading(MPM, declared(Some("Interactive"))),
        reading(MPM_SUPERVISOR, declared(Some("Interactive"))),
    ]);
    assert_eq!(row.status, CheckStatus::Ok, "{}", row.message);
}

#[test]
fn no_plists_pass() {
    let home = tempfile::tempdir().expect("tempdir");
    let row = check_launchd_process_type(home.path());
    assert_eq!(row.status, CheckStatus::Ok, "{}", row.message);
    assert!(row.message.contains("no tm LaunchAgent"), "{}", row.message);
}

#[test]
fn binary_plist_is_unknown() {
    let row = build_process_type_check(&[reading(
        MPM,
        ProcessTypeReading::Unjudged("binary plist".to_owned()),
    )]);
    assert_eq!(row.status, CheckStatus::Unknown, "{}", row.message);
}

#[test]
fn process_type_of_ignores_commented_keys() {
    let xml = "<dict><!-- <key>ProcessType</key><string>Background</string> -->\
               <key>ProcessType</key><string>Interactive</string></dict>";
    assert_eq!(process_type_of(xml).as_deref(), Some("Interactive"));
    assert_eq!(process_type_of(KEYLESS_DAEMON), None);
}

#[test]
fn reading_classifies_absent_keyless_and_binary_plists() {
    let home = home_with(&[
        (MPM, KEYLESS_DAEMON.as_bytes()),
        (MPM_SUPERVISOR, b"bplist00\x00\x01"),
    ]);
    let agents = home.path().join("Library/LaunchAgents");
    assert_eq!(
        read_plist(&agents.join("absent.plist")),
        ProcessTypeReading::NotInstalled
    );
    assert_eq!(
        read_plist(&agents.join(format!("{MPM}.plist"))),
        declared(None)
    );
    assert!(matches!(
        read_plist(&agents.join(format!("{MPM_SUPERVISOR}.plist"))),
        ProcessTypeReading::Unjudged(_)
    ));
}

#[test]
fn check_reads_plists_under_the_given_home() {
    let home = home_with(&[
        (MPM, KEYLESS_DAEMON.as_bytes()),
        (MPM_SUPERVISOR, PRE_8415_SUPERVISOR.as_bytes()),
    ]);
    let row = check_launchd_process_type(home.path());
    assert_eq!(row.status, CheckStatus::Fail, "{}", row.message);
    assert!(
        row.message.contains(&home.path().display().to_string()),
        "the remedy must name the real plist path: {}",
        row.message
    );
    assert!(
        row.message.contains("Standard"),
        "the warn must ride along: {}",
        row.message
    );
}
