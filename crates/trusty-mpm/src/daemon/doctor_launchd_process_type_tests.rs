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
        process_type_of(DEPLOY_SUPERVISOR),
        Ok(Some(EXPECTED_PROCESS_TYPE.to_owned())),
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
    assert_eq!(process_type_of(xml), Ok(Some("Interactive".to_owned())));
    assert_eq!(process_type_of(KEYLESS_DAEMON), Ok(None));
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

/// #8415: `<string/>`, a non-string value, or anything between the key and
/// its `<string>` cannot be judged — Unknown, never a guessed value.
#[test]
fn process_type_of_rejects_malformed_values() {
    for xml in [
        "<dict><key>ProcessType</key><string/></dict>",
        "<dict><key>ProcessType</key><integer>1</integer></dict>",
        "<dict><key>ProcessType</key><key>Other</key><string>Interactive</string></dict>",
        "<dict><key>ProcessType</key><string>Interactive</dict>",
    ] {
        assert!(process_type_of(xml).is_err(), "{xml}");
    }
    let home = home_with(&[(
        MPM_SUPERVISOR,
        b"<dict><key>ProcessType</key><string/></dict>".as_slice(),
    )]);
    let row = check_launchd_process_type(home.path());
    assert_eq!(row.status, CheckStatus::Unknown, "{}", row.message);
    assert_eq!(
        process_type_of("<key>ProcessType</key>\n\t  <string>Interactive</string>"),
        Ok(Some("Interactive".to_owned())),
        "whitespace between the key and its <string> is allowed"
    );
}

/// #8415: launchd compares the value as written, so padding inside the
/// `<string>` is not `Interactive` — the row warns instead of passing.
#[test]
fn padded_process_type_is_not_interactive() {
    let xml = "<dict><key>ProcessType</key><string> Interactive </string></dict>";
    assert_eq!(process_type_of(xml), Ok(Some(" Interactive ".to_owned())));
    let home = home_with(&[(MPM_SUPERVISOR, xml.as_bytes())]);
    let row = check_launchd_process_type(home.path());
    assert_eq!(row.status, CheckStatus::Warn, "{}", row.message);
}

/// #8415: CoreFoundation keeps the last of duplicated keys; so does the row.
#[test]
fn process_type_of_takes_the_last_duplicate_key() {
    let xml = "<dict><key>ProcessType</key><string>Interactive</string>\
               <key>ProcessType</key><string>Background</string></dict>";
    assert_eq!(process_type_of(xml), Ok(Some("Background".to_owned())));
}

/// #8415: the printed commands must survive a path with a space in it.
#[test]
fn remedy_quotes_a_path_with_a_space() {
    let row = build_process_type_check(&[PlistReading {
        label: MPM_SUPERVISOR,
        path: PathBuf::from("/Users/a b/Library/LaunchAgents/x.plist"),
        reading: declared(Some("Background")),
    }]);
    assert!(
        row.message
            .contains("-string Interactive '/Users/a b/Library/LaunchAgents/x.plist'"),
        "{}",
        row.message
    );
    assert!(
        row.message
            .contains("launchctl bootout gui/$(id -u) '/Users/a b/Library/LaunchAgents/x.plist'"),
        "{}",
        row.message
    );
    assert_eq!(shell_quote(Path::new("/it's")), "'/it'\\''s'");
}

/// #8415 owner rule: a test build never reads the operator's real
/// `~/Library/LaunchAgents`, even through `run_doctor`.
#[test]
fn test_builds_never_read_the_real_launch_agents() {
    let real = PathBuf::from("/Users/operator");
    let dir = launch_agents_dir_from(&real, None, true);
    assert!(!dir.path.starts_with(&real), "{dir:?}");
    assert!(dir.path.starts_with(std::env::temp_dir()), "{dir:?}");
    let row = check_launchd_process_type_in(&dir);
    assert_eq!(row.status, CheckStatus::Ok, "{}", row.message);
}

/// #8415: the production arm reads `<home>/Library/LaunchAgents`. A fake
/// home only — nothing here touches the real directory.
#[test]
fn production_reads_library_launch_agents_under_home() {
    let dir = launch_agents_dir_from(Path::new("/fake/home"), None, false);
    assert_eq!(
        dir,
        AgentsDir {
            path: PathBuf::from("/fake/home/Library/LaunchAgents"),
            from_env: false,
        }
    );
}

/// #8415: `TRUSTY_MPM_LAUNCH_AGENTS_DIR` points the row at another directory,
/// and an empty value is ignored.
#[test]
fn launch_agents_dir_honours_the_env_override() {
    let home = home_with(&[]);
    let agents = home.path().join("elsewhere");
    std::fs::create_dir_all(&agents).expect("mkdir");
    std::fs::write(
        agents.join(format!("{MPM_SUPERVISOR}.plist")),
        PRE_8415_SUPERVISOR,
    )
    .expect("write plist");
    let dir = launch_agents_dir_from(
        Path::new("/Users/operator"),
        Some(agents.clone().into()),
        false,
    );
    assert_eq!(dir.path, agents);
    assert!(dir.from_env);
    let row = check_launchd_process_type_in(&dir);
    assert_eq!(row.status, CheckStatus::Fail, "{}", row.message);
    assert_ne!(
        launch_agents_dir_from(Path::new("/Users/operator"), Some("".into()), false).path,
        agents
    );

    // #8415: an Ok row read through the override names the directory.
    let empty = home.path().join("empty");
    std::fs::create_dir_all(&empty).expect("mkdir");
    let row = check_launchd_process_type_in(&launch_agents_dir_from(
        Path::new("/Users/operator"),
        Some(empty.clone().into()),
        false,
    ));
    assert_eq!(row.status, CheckStatus::Ok, "{}", row.message);
    assert!(row.message.contains("no tm LaunchAgent"), "{}", row.message);
    assert!(
        row.message.contains(&format!("`{}`", empty.display())),
        "{}",
        row.message
    );
    assert!(
        row.message
            .contains("(from `TRUSTY_MPM_LAUNCH_AGENTS_DIR`)"),
        "{}",
        row.message
    );
}

/// #8415: the injected override outranks the env var and the home default,
/// and the first call wins. The path does not exist; nothing is created.
#[test]
fn override_outranks_every_other_source() {
    let dir = std::env::temp_dir().join(format!(
        "tm-lib-test-no-launch-agents-{}",
        std::process::id()
    ));
    override_launch_agents_dir(dir.clone());
    override_launch_agents_dir(PathBuf::from("/ignored"));
    let got = launch_agents_dir(Path::new("/Users/operator"));
    assert_eq!(got.path, dir);
    assert!(!got.from_env);
}
