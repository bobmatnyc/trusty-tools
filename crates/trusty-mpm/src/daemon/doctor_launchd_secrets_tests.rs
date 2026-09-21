//! Tests for the `launchd_secrets` doctor row and its repair (#8236).
//!
//! Every fixture is a temp directory and every value an obvious fake. No test
//! here reads or writes `~/Library/LaunchAgents`, the real login Keychain, or
//! the real `~/.trusty-mpm`; the repair migrates into an injected
//! `MemoryKeyStore`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use trusty_common::credentials::{KeyStore, KeyStoreError, MemoryKeyStore};

use super::*;
use crate::core::doctor_repair::{RepairMode, StepStatus};
use crate::daemon::doctor_launchd_secrets_repair::repair_with_store;

/// A fake OpenRouter-shaped key. Not a credential; the `FAKE` run is the point.
const FAKE_API_KEY: &str = "sk-or-v1-0000000000000000000000000000FAKE";

/// A fake Telegram bot-token-shaped value, for the no-vendor-prefix path.
const FAKE_BOT_TOKEN: &str = "1234567890:AAFAKEFAKEFAKEFAKEFAKEFAKEFAKEFAKEFAKE";

/// Assert a rendered row, step or error never carries a fixture value.
fn assert_value_never_echoed(text: &str) {
    for value in [FAKE_API_KEY, FAKE_BOT_TOKEN] {
        assert!(
            !text.contains(value),
            "output must name the key, never the value: {text}"
        );
    }
    assert!(!text.contains("sk-or-"), "credential prefix leaked: {text}");
}

/// A home directory with a `Library/LaunchAgents` in it.
fn home_with_agents() -> tempfile::TempDir {
    let home = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(home.path().join("Library/LaunchAgents")).expect("mkdir");
    home
}

/// Write `body` as `<home>/Library/LaunchAgents/<name>` and return its path.
fn install(home: &Path, name: &str, body: &str) -> PathBuf {
    let path = home.join("Library/LaunchAgents").join(name);
    std::fs::write(&path, body).expect("write plist");
    path
}

/// A unit carrying one registry-mapped credential among ordinary tunables.
fn plist_with_credential() -> String {
    format!(
        "<plist version=\"1.0\">\n<dict>\n  \
         <key>Label</key>\n  <string>com.trusty.mpm</string>\n  \
         <key>EnvironmentVariables</key>\n  <dict>\n    \
         <key>PATH</key>\n    <string>/usr/bin</string>\n    \
         <key>OPENROUTER_API_KEY</key>\n    <string>{FAKE_API_KEY}</string>\n    \
         <key>RUST_LOG</key>\n    <string>info</string>\n  \
         </dict>\n</dict>\n</plist>\n"
    )
}

/// A store that accepts every write and serves nothing back.
///
/// Why: the read-back confirmation is what licenses the strip. A backend that
/// accepts a write it cannot serve must NOT license one.
struct WriteOnlyStore;

impl KeyStore for WriteOnlyStore {
    fn get(&self, _provider: &str) -> Option<String> {
        None
    }
    fn set(&self, _provider: &str, _value: &str) -> Result<(), KeyStoreError> {
        Ok(())
    }
    fn unset(&self, _provider: &str) -> Result<(), KeyStoreError> {
        Ok(())
    }
    fn list(&self) -> Vec<String> {
        Vec::new()
    }
}

/// Why: the scan is the whole diagnostic; a finding that carried the value
/// would make `tm doctor` itself a disclosure.
/// Test: this test.
#[test]
fn scan_names_the_key_not_the_value() {
    let home = home_with_agents();
    install(home.path(), "com.trusty.mpm.plist", &plist_with_credential());

    let findings = scan_launch_agents(home.path()).expect("scan");

    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].migratable, vec!["OPENROUTER_API_KEY".to_string()]);
    assert_value_never_echoed(&format!("{:?}", findings[0]));
}

/// Why (#8236 item 2): a key with no registry mapping has nowhere to migrate
/// to, so it must be classified apart from one that has.
/// Test: this test.
#[test]
fn scan_splits_registry_mapped_keys_from_unmapped_ones() {
    let home = home_with_agents();
    install(
        home.path(),
        "com.trusty.agents.slack.plist",
        &format!(
            "<plist version=\"1.0\">\n<dict>\n  \
             <key>EnvironmentVariables</key>\n  <dict>\n    \
             <key>SLACK_APP_TOKEN</key>\n    <string>xapp-1-FAKEFAKEFAKEFAKE</string>\n    \
             <key>AWS_SECRET_ACCESS_KEY</key>\n    <string>{FAKE_BOT_TOKEN}</string>\n  \
             </dict>\n</dict>\n</plist>\n"
        ),
    );

    let findings = scan_launch_agents(home.path()).expect("scan");

    assert_eq!(findings[0].migratable, vec!["SLACK_APP_TOKEN".to_string()]);
    assert_eq!(
        findings[0].unmapped,
        vec!["AWS_SECRET_ACCESS_KEY".to_string()]
    );
}

/// Why (#8236 item 4): a binary plist decoded as text finds no `<key>`, so the
/// scanner would report the host CLEAN. Unknown with an actionable reason is
/// the only correct answer.
/// Test: this test.
#[test]
fn scan_reports_a_binary_plist_as_unknown() {
    let home = home_with_agents();
    let path = home.path().join("Library/LaunchAgents/com.trusty.mpm.plist");
    let mut bytes = b"bplist00".to_vec();
    bytes.extend_from_slice(&[0xd1, 0x01, 0x02]);
    std::fs::write(&path, bytes).expect("write");

    let findings = scan_launch_agents(home.path()).expect("scan");
    let why = findings[0].unreadable.clone().expect("unreadable");

    assert!(why.contains("BINARY"), "{why}");
    assert!(why.contains("plutil -convert xml1"), "{why}");
    assert!(findings[0].migratable.is_empty());
}

/// Why: a plist the parser cannot finish is exactly the file that may hold the
/// secret; it must never render as clean.
/// Test: this test.
#[test]
fn scan_reports_an_unparseable_plist() {
    let home = home_with_agents();
    install(
        home.path(),
        "com.trusty.mpm.plist",
        "<plist>\n<dict>\n<key>EnvironmentVariables</key>\n<dict>\n<key>A</key>\n",
    );

    let findings = scan_launch_agents(home.path()).expect("scan");

    assert!(findings[0].unreadable.is_some());
    assert_value_never_echoed(&format!("{:?}", findings[0]));
}

/// Why: a plist tm did not generate belongs to the operator.
/// Test: this test.
#[test]
fn scan_ignores_foreign_plists() {
    let home = home_with_agents();
    install(home.path(), "com.example.other.plist", &plist_with_credential());

    assert!(scan_launch_agents(home.path()).expect("scan").is_empty());
}

/// Why: a host with no LaunchAgents has no exposure, and that is not an error.
/// Test: this test.
#[test]
fn scan_is_empty_without_a_launch_agents_dir() {
    let home = tempfile::tempdir().expect("tempdir");
    assert!(scan_launch_agents(home.path()).expect("scan").is_empty());
}

/// Why: a directory that cannot be listed means the scan did not run, which
/// the caller must hear as UNKNOWN rather than as a clean host.
/// Test: this test.
#[test]
fn scan_errors_when_the_directory_cannot_be_listed() {
    let home = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(home.path().join("Library")).expect("mkdir");
    std::fs::write(home.path().join("Library/LaunchAgents"), b"not a dir").expect("write");

    assert!(scan_launch_agents(home.path()).is_err());
}

/// Why: the row is the operator-visible half of the whole ticket.
/// Test: this test.
#[test]
fn row_fails_and_names_the_key_not_the_value() {
    let home = home_with_agents();
    install(home.path(), "com.trusty.mpm.plist", &plist_with_credential());

    let row = check_launchd_plist_secrets(home.path());

    assert_eq!(row.status, CheckStatus::Fail);
    assert!(row.detail.contains("OPENROUTER_API_KEY"), "{}", row.detail);
    assert!(row.detail.contains("ROTATE"), "{}", row.detail);
    assert_value_never_echoed(&row.detail);
}

/// Why (#8236 item 3): the row reports each plist's MODE, and flags anything
/// wider than `0600` — the #8236 host's plist was `0644`.
/// Test: this test.
#[cfg(unix)]
#[test]
fn row_flags_a_world_readable_plist() {
    use std::os::unix::fs::PermissionsExt;
    let home = home_with_agents();
    let path = install(home.path(), "com.trusty.mpm.plist", &plist_with_credential());
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("chmod");

    let row = check_launchd_plist_secrets(home.path());

    assert!(row.detail.contains("0644"), "{}", row.detail);
}

/// Why (#8236 item 2): the row has to SAY that an unmapped key will not be
/// stripped, or an operator reads a partial fix as a complete one.
/// Test: this test.
#[test]
fn row_says_an_unmapped_key_is_not_stripped() {
    let home = home_with_agents();
    install(
        home.path(),
        "com.trusty.mpm.plist",
        "<plist>\n<dict>\n<key>EnvironmentVariables</key>\n<dict>\n\
         <key>AWS_SECRET_ACCESS_KEY</key>\n<string>fake</string>\n</dict>\n</dict>\n</plist>\n",
    );

    let row = check_launchd_plist_secrets(home.path());

    assert_eq!(row.status, CheckStatus::Fail);
    assert!(row.detail.contains("will NOT remove"), "{}", row.detail);
}

/// Why: a clean host must report clean, or the row is noise.
/// Test: this test.
#[test]
fn row_is_ok_when_clean() {
    let home = home_with_agents();
    let path = install(
        home.path(),
        "com.trusty.mpm.plist",
        "<plist>\n<dict>\n<key>EnvironmentVariables</key>\n<dict>\n\
         <key>PATH</key>\n<string>/usr/bin</string>\n</dict>\n</dict>\n</plist>\n",
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("chmod");
    }
    let _ = &path;

    assert_eq!(check_launchd_plist_secrets(home.path()).status, CheckStatus::Ok);
}

/// Why: "could not tell" must never render as healthy.
/// Test: this test.
#[test]
fn row_is_unknown_when_a_plist_cannot_be_parsed() {
    let home = home_with_agents();
    install(
        home.path(),
        "com.trusty.mpm.plist",
        "<plist>\n<dict>\n<key>EnvironmentVariables</key>\n<dict>\n<key>A</key>\n",
    );

    assert_eq!(
        check_launchd_plist_secrets(home.path()).status,
        CheckStatus::Unknown
    );
}

/// Why: same, for the directory-level failure.
/// Test: this test.
#[test]
fn row_is_unknown_when_the_directory_cannot_be_listed() {
    let home = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(home.path().join("Library")).expect("mkdir");
    std::fs::write(home.path().join("Library/LaunchAgents"), b"not a dir").expect("write");

    assert_eq!(
        check_launchd_plist_secrets(home.path()).status,
        CheckStatus::Unknown
    );
}

/// Why: ranking Fail above Unknown must not DROP the unjudged file.
/// Test: this test.
#[test]
fn row_fail_still_names_the_plists_it_could_not_judge() {
    let home = home_with_agents();
    install(home.path(), "com.trusty.mpm.plist", &plist_with_credential());
    install(
        home.path(),
        "com.trusty.zebra.plist",
        "<plist>\n<dict>\n<key>EnvironmentVariables</key>\n<dict>\n<key>A</key>\n",
    );

    let row = check_launchd_plist_secrets(home.path());

    assert_eq!(row.status, CheckStatus::Fail);
    assert!(row.detail.contains("com.trusty.zebra.plist"), "{}", row.detail);
}

/// Why: a dry run must plan and write nothing.
/// Test: this test.
#[test]
fn repair_plans_without_writing() {
    let home = home_with_agents();
    let path = install(home.path(), "com.trusty.mpm.plist", &plist_with_credential());
    let before = std::fs::read_to_string(&path).expect("read");

    let steps = repair_with_store(
        home.path(),
        RepairMode::DryRun,
        Arc::new(MemoryKeyStore::new()),
    );

    assert_eq!(steps.len(), 1);
    assert_eq!(steps[0].status, StepStatus::Planned);
    assert_eq!(std::fs::read_to_string(&path).expect("read"), before);
}

/// Why (#8236 items 1 and 2): the repair migrates the value into the store,
/// confirms it by read-back, and only then removes it — atomically.
/// Test: this test.
#[test]
fn repair_migrates_then_removes() {
    let home = home_with_agents();
    let path = install(home.path(), "com.trusty.mpm.plist", &plist_with_credential());
    let store = Arc::new(MemoryKeyStore::new());

    let steps = repair_with_store(home.path(), RepairMode::Apply, store.clone());

    assert_eq!(steps[0].status, StepStatus::Applied { backup: None });
    assert_eq!(store.get("openrouter").as_deref(), Some(FAKE_API_KEY));
    let after = std::fs::read_to_string(&path).expect("read");
    assert!(!after.contains("OPENROUTER_API_KEY"), "{after}");
    assert!(after.contains("RUST_LOG"), "{after}");
    assert_value_never_echoed(&after);
    assert_value_never_echoed(&format!("{:?}", steps[0]));
}

/// Why (#8236 item 2): an import that cannot be confirmed must leave the plist
/// byte-identical. `WriteOnlyStore` accepts the write and serves nothing back.
/// Test: this test.
#[test]
fn repair_leaves_the_plist_untouched_when_the_import_fails() {
    let home = home_with_agents();
    let path = install(home.path(), "com.trusty.mpm.plist", &plist_with_credential());
    let before = std::fs::read_to_string(&path).expect("read");

    let steps = repair_with_store(home.path(), RepairMode::Apply, Arc::new(WriteOnlyStore));

    assert!(
        matches!(steps[0].status, StepStatus::Failed(_)),
        "{:?}",
        steps[0].status
    );
    assert_eq!(std::fs::read_to_string(&path).expect("read"), before);
    assert_value_never_echoed(&format!("{:?}", steps[0]));
}

/// Why (#8236 item 2): stripping a key with no store entry breaks the feature
/// it configures, so the repair refuses and says why.
/// Test: this test.
#[test]
fn repair_keeps_an_unmapped_key_and_says_so() {
    let home = home_with_agents();
    let path = install(
        home.path(),
        "com.trusty.mpm.plist",
        "<plist>\n<dict>\n<key>EnvironmentVariables</key>\n<dict>\n\
         <key>AWS_SECRET_ACCESS_KEY</key>\n<string>fake</string>\n</dict>\n</dict>\n</plist>\n",
    );
    let before = std::fs::read_to_string(&path).expect("read");

    let steps = repair_with_store(
        home.path(),
        RepairMode::Apply,
        Arc::new(MemoryKeyStore::new()),
    );

    assert!(
        matches!(steps[0].status, StepStatus::Refused(_)),
        "{:?}",
        steps[0].status
    );
    assert_eq!(std::fs::read_to_string(&path).expect("read"), before);
}

/// Why (#8236 item 4): `--fix` must refuse a binary plist rather than rewrite
/// it as text, which would destroy the unit.
/// Test: this test.
#[test]
fn repair_refuses_a_binary_plist() {
    let home = home_with_agents();
    let path = home.path().join("Library/LaunchAgents/com.trusty.mpm.plist");
    let mut bytes = b"bplist00".to_vec();
    bytes.extend_from_slice(&[0xd1, 0x01]);
    std::fs::write(&path, &bytes).expect("write");

    let steps = repair_with_store(
        home.path(),
        RepairMode::Apply,
        Arc::new(MemoryKeyStore::new()),
    );

    assert!(
        matches!(steps[0].status, StepStatus::Failed(_)),
        "{:?}",
        steps[0].status
    );
    assert_eq!(std::fs::read(&path).expect("read"), bytes);
}

/// Why: an unparseable plist is a failure, never a silent pass.
/// Test: this test.
#[test]
fn repair_fails_loudly_on_an_unparseable_plist() {
    let home = home_with_agents();
    install(
        home.path(),
        "com.trusty.mpm.plist",
        "<plist>\n<dict>\n<key>EnvironmentVariables</key>\n<dict>\n<key>A</key>\n",
    );

    let steps = repair_with_store(
        home.path(),
        RepairMode::Apply,
        Arc::new(MemoryKeyStore::new()),
    );

    assert!(matches!(steps[0].status, StepStatus::Failed(_)));
}

/// Why (#8236 item 1): a write that cannot land must leave the original intact
/// and report Failed — not a partial file.
/// Test: this test.
#[cfg(unix)]
#[test]
fn repair_fails_loudly_when_the_plist_is_unwritable() {
    use std::os::unix::fs::PermissionsExt;
    let home = home_with_agents();
    let path = install(home.path(), "com.trusty.mpm.plist", &plist_with_credential());
    let before = std::fs::read_to_string(&path).expect("read");
    let dir = home.path().join("Library/LaunchAgents");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).expect("chmod");

    let steps = repair_with_store(
        home.path(),
        RepairMode::Apply,
        Arc::new(MemoryKeyStore::new()),
    );

    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).expect("chmod back");
    assert!(
        matches!(steps[0].status, StepStatus::Failed(_)),
        "{:?}",
        steps[0].status
    );
    assert_eq!(std::fs::read_to_string(&path).expect("read"), before);
}

/// Why: a directory that cannot be listed is a failure the operator must see.
/// Test: this test.
#[test]
fn repair_fails_loudly_when_the_directory_cannot_be_listed() {
    let home = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(home.path().join("Library")).expect("mkdir");
    std::fs::write(home.path().join("Library/LaunchAgents"), b"not a dir").expect("write");

    let steps = repair_with_store(
        home.path(),
        RepairMode::Apply,
        Arc::new(MemoryKeyStore::new()),
    );

    assert_eq!(steps.len(), 1);
    assert!(matches!(steps[0].status, StepStatus::Failed(_)));
}

/// Why: a clean host must produce no steps, so `--fix` prints nothing for it.
/// Test: this test.
#[test]
fn repair_produces_no_steps_for_a_clean_host() {
    let home = home_with_agents();
    install(
        home.path(),
        "com.trusty.mpm.plist",
        "<plist>\n<dict>\n<key>EnvironmentVariables</key>\n<dict>\n\
         <key>PATH</key>\n<string>/usr/bin</string>\n</dict>\n</dict>\n</plist>\n",
    );

    assert!(
        repair_with_store(
            home.path(),
            RepairMode::Apply,
            Arc::new(MemoryKeyStore::new())
        )
        .is_empty()
    );
}

/// Why (#8236 item 2): running `--fix` twice must be a no-op the second time,
/// not a second rewrite or a failure.
/// Test: this test.
#[test]
fn repair_is_idempotent() {
    let home = home_with_agents();
    let path = install(home.path(), "com.trusty.mpm.plist", &plist_with_credential());
    let store = Arc::new(MemoryKeyStore::new());

    let first = repair_with_store(home.path(), RepairMode::Apply, store.clone());
    let after_first = std::fs::read_to_string(&path).expect("read");
    let second = repair_with_store(home.path(), RepairMode::Apply, store);

    assert_eq!(first[0].status, StepStatus::Applied { backup: None });
    assert!(second.is_empty(), "{second:?}");
    assert_eq!(std::fs::read_to_string(&path).expect("read"), after_first);
}
