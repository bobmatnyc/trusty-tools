//! Tests for [`super`] — the `launchd_secrets` doctor row and repair (#8236).
//!
//! Every fixture credential here is an obvious fake, and every assertion that
//! a message is safe checks for the absence of that fake string. Nothing in
//! this file touches the real `~/Library/LaunchAgents`: each test builds its
//! own temp home.

use std::path::PathBuf;

use tempfile::TempDir;

use super::*;

/// An obviously fake OpenRouter-shaped key. Not a credential.
const FAKE_KEY: &str = "sk-or-v1-0000000000000000000000000000FAKE";

/// A trusty LaunchAgent carrying one credential entry beside two tunables.
fn exposed_plist() -> String {
    format!(
        "<plist version=\"1.0\">\n<dict>\n  <key>Label</key>\n  \
         <string>com.trusty.mpm</string>\n  <key>EnvironmentVariables</key>\n  \
         <dict>\n    <key>PATH</key>\n    <string>/usr/bin</string>\n    \
         <key>OPENROUTER_API_KEY</key>\n    <string>{FAKE_KEY}</string>\n    \
         <key>RUST_LOG</key>\n    <string>info</string>\n  </dict>\n</dict>\n</plist>\n"
    )
}

/// The same unit with no credential in it.
fn clean_plist() -> String {
    "<plist version=\"1.0\">\n<dict>\n  <key>EnvironmentVariables</key>\n  \
     <dict>\n    <key>PATH</key>\n    <string>/usr/bin</string>\n  </dict>\n\
     </dict>\n</plist>\n"
        .to_string()
}

/// Build a temp home with one `LaunchAgents` file.
fn home_with(name: &str, contents: &str) -> (TempDir, PathBuf) {
    let tmp = TempDir::new().expect("temp home");
    let agents = tmp.path().join("Library/LaunchAgents");
    std::fs::create_dir_all(&agents).expect("create LaunchAgents");
    let path = agents.join(name);
    std::fs::write(&path, contents).expect("write plist");
    (tmp, path)
}

/// Assert no rendered surface of a check or step echoes the fake credential.
fn assert_value_never_echoed(text: &str) {
    assert!(
        !text.contains(FAKE_KEY),
        "output must name the key, never the value: {text}"
    );
    assert!(
        !text.contains("sk-or-"),
        "output must not carry a credential prefix: {text}"
    );
}

/// Why: the scan is the shared input to the row and the repair; it must find
/// the key and must never carry the value forward.
/// Test: this test.
#[test]
fn scan_names_the_key_not_the_value() {
    let (tmp, path) = home_with("com.trusty.mpm.plist", &exposed_plist());
    let findings = scan_launch_agents(tmp.path()).expect("scan");
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].path, path);
    assert_eq!(findings[0].keys, vec!["OPENROUTER_API_KEY"]);
    assert_value_never_echoed(&format!("{findings:?}"));
}

/// Why: `--fix` may only rewrite files tm's own installers generate. An
/// operator's own agent is neither reported nor touched.
/// Test: this test.
#[test]
fn scan_ignores_foreign_plists() {
    let (tmp, _) = home_with("com.example.other.plist", &exposed_plist());
    assert!(scan_launch_agents(tmp.path()).expect("scan").is_empty());
}

/// Why: a host that never installed a LaunchAgent has no exposure, and a
/// missing directory must not read as an error.
/// Test: this test.
#[test]
fn scan_is_empty_without_a_launch_agents_dir() {
    let tmp = TempDir::new().expect("temp home");
    assert!(scan_launch_agents(tmp.path()).expect("scan").is_empty());
}

/// Why: an unparseable plist is the false-negative hazard — reporting it clean
/// is how a compromised host passes a diagnostic.
/// Test: this test.
#[test]
fn scan_reports_an_unparseable_plist() {
    let broken =
        "<key>EnvironmentVariables</key>\n<dict>\n<key>PATH</key>\n<string>/usr/bin</string>\n";
    let (tmp, _) = home_with("com.trusty.search.plist", broken);
    let findings = scan_launch_agents(tmp.path()).expect("scan");
    assert_eq!(findings.len(), 1);
    assert!(findings[0].keys.is_empty());
    assert!(findings[0].unreadable.is_some(), "must not read as clean");
}

/// The #8236 row guard: a plist generated from inputs containing a credential
/// must FAIL the diagnostic, and the message must name the key alone.
///
/// Why: the acceptance criterion in the issue — `tm doctor` fails a plist that
/// carries a credential-shaped value, and an agent printing the report cannot
/// expose one.
/// Test: this test.
#[test]
fn row_fails_and_names_the_key_not_the_value() {
    let (tmp, _) = home_with("com.trusty.mpm.plist", &exposed_plist());
    let check = check_launchd_plist_secrets(tmp.path());
    assert_eq!(check.status, CheckStatus::Fail);
    assert!(
        check.message.contains("OPENROUTER_API_KEY"),
        "{}",
        check.message
    );
    assert!(
        check.message.contains("tm doctor --fix"),
        "{}",
        check.message
    );
    assert!(check.message.contains("ROTATE"), "{}", check.message);
    assert_value_never_echoed(&check.message);
}

/// Why: the clean path is the steady state and must not nag.
/// Test: this test.
#[test]
fn row_is_ok_when_clean() {
    let (tmp, _) = home_with("com.trusty.mpm.plist", &clean_plist());
    assert_eq!(
        check_launchd_plist_secrets(tmp.path()).status,
        CheckStatus::Ok
    );
}

/// Why: a check that could not run has not passed (#4005). A parse failure must
/// never render as a green row.
/// Test: this test.
#[test]
fn row_is_unknown_when_a_plist_cannot_be_parsed() {
    let broken = "<key>EnvironmentVariables</key>\n<dict>\n<key>PATH</key>\n";
    let (tmp, _) = home_with("com.trusty.mpm.plist", broken);
    let check = check_launchd_plist_secrets(tmp.path());
    assert_eq!(check.status, CheckStatus::Unknown);
    assert!(check.message.contains("UNKNOWN"), "{}", check.message);
}

/// Why: `--fix` without `--yes` must describe and write nothing — the file on
/// disk has to be byte-identical after a dry run.
/// Test: this test.
#[test]
fn repair_plans_without_writing() {
    let (tmp, path) = home_with("com.trusty.mpm.plist", &exposed_plist());
    let steps = repair_launchd_plist_secrets(tmp.path(), RepairMode::DryRun);
    assert_eq!(steps.len(), 1);
    assert_eq!(steps[0].status, StepStatus::Planned);
    assert_value_never_echoed(&steps[0].what);
    assert_eq!(
        std::fs::read_to_string(&path).expect("read"),
        exposed_plist()
    );
}

/// Why: the remediation half of the issue's acceptance list — an existing
/// install's plist is rewritten WITHOUT the credential and with every other
/// entry intact.
/// Test: this test.
#[test]
fn repair_removes_the_entry() {
    let (tmp, path) = home_with("com.trusty.mpm.plist", &exposed_plist());
    let steps = repair_launchd_plist_secrets(tmp.path(), RepairMode::Apply);
    assert_eq!(steps.len(), 1);
    assert_eq!(steps[0].status, StepStatus::Applied { backup: None });

    let after = std::fs::read_to_string(&path).expect("read");
    assert!(!after.contains(FAKE_KEY), "the value must be gone");
    assert!(!after.contains("OPENROUTER_API_KEY"));
    assert!(after.contains("<key>PATH</key>"));
    assert!(after.contains("<string>info</string>"));

    // And the row it answers now passes.
    assert_eq!(
        check_launchd_plist_secrets(tmp.path()).status,
        CheckStatus::Ok
    );

    // No sibling file was created — a backup would be a second readable copy.
    let agents: Vec<PathBuf> = std::fs::read_dir(tmp.path().join("Library/LaunchAgents"))
        .expect("list")
        .flatten()
        .map(|e| e.path())
        .collect();
    assert_eq!(agents, vec![path]);
}

/// The Fail-Open guard: a remediation that could not run is an ERROR, never a
/// warning followed by a pass.
///
/// Why: a `--fix` that silently skipped the file it could not parse would leave
/// the operator believing the host was remediated.
/// Test: this test.
#[test]
fn repair_fails_loudly_on_an_unparseable_plist() {
    let broken =
        "<key>EnvironmentVariables</key>\n<dict>\n<key>OPENROUTER_API_KEY</key>\n</dict>\n";
    let (tmp, _) = home_with("com.trusty.mpm.plist", broken);
    let steps = repair_launchd_plist_secrets(tmp.path(), RepairMode::Apply);
    assert_eq!(steps.len(), 1);
    match &steps[0].status {
        StepStatus::Failed(why) => assert!(why.contains("no value element"), "was: {why}"),
        other => panic!("an unparseable plist must fail, got {other:?}"),
    }
    assert!(!steps[0].changed(), "a failure must not count as applied");
}

/// The same guard for the write half.
///
/// Why: an unwritable plist (a read-only file, a locked volume) must surface as
/// an error the operator can act on, not as a silent no-op.
/// Test: this test.
#[cfg(unix)]
#[test]
fn repair_fails_loudly_when_the_plist_is_unwritable() {
    use std::os::unix::fs::PermissionsExt;

    let (tmp, path) = home_with("com.trusty.mpm.plist", &exposed_plist());
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).expect("chmod");
    let dir = path.parent().expect("parent").to_path_buf();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).expect("chmod dir");

    let steps = repair_launchd_plist_secrets(tmp.path(), RepairMode::Apply);

    // Restore write permission so the TempDir can clean itself up.
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).expect("restore dir");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("restore");

    assert_eq!(steps.len(), 1);
    assert!(
        matches!(steps[0].status, StepStatus::Failed(_)),
        "an unwritable plist must fail, got {:?}",
        steps[0].status
    );
}

/// Why: a clean host must produce no `--fix` output at all, so the repair list
/// stays a list of things that need doing.
/// Test: this test.
#[test]
fn repair_produces_no_steps_for_a_clean_host() {
    let (tmp, _) = home_with("com.trusty.mpm.plist", &clean_plist());
    assert!(repair_launchd_plist_secrets(tmp.path(), RepairMode::Apply).is_empty());
}
