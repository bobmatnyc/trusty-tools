//! Tests for `tm doctor --fix-launchd-secrets` (#8236).
//!
//! Every plist is a temp file under a temp `$HOME`, every store is a
//! `MemoryKeyStore`, and the one credential is the synthetic
//! `sk-test-not-real`. No test reads `~/Library/LaunchAgents`, the real store,
//! or the real environment.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use trusty_common::credentials::{KeyStore, MemoryKeyStore};

use super::*;
use crate::daemon::doctor_launchd_secrets::CHECK_NAME;
use crate::daemon::doctor_launchd_secrets_repair::repair_with_store;

/// The synthetic credential. Not a key; the point is that it never surfaces.
const FAKE_KEY: &str = "sk-test-not-real";

/// A temp `$HOME` holding one trusty plist with `FAKE_KEY` at `mode`.
fn home_with_plist(mode: u32) -> (tempfile::TempDir, PathBuf) {
    let home = tempfile::tempdir().expect("tempdir");
    let dir = home.path().join("Library/LaunchAgents");
    std::fs::create_dir_all(&dir).expect("mkdir");
    let path = dir.join("com.trusty.mpm.plist");
    let body = format!(
        "<plist version=\"1.0\">\n<dict>\n  \
         <key>Label</key>\n  <string>com.trusty.mpm</string>\n  \
         <key>EnvironmentVariables</key>\n  <dict>\n    \
         <key>OPENROUTER_API_KEY</key>\n    <string>{FAKE_KEY}</string>\n    \
         <key>RUST_LOG</key>\n    <string>info</string>\n  \
         </dict>\n</dict>\n</plist>\n"
    );
    std::fs::write(&path, body).expect("write plist");
    set_mode(&path, mode).expect("chmod fixture");
    (home, path)
}

/// The scoped repair against an injected store and injected chmod.
fn run(
    home: &Path,
    mode: RepairMode,
    store: Arc<MemoryKeyStore>,
    chmod: &dyn Fn(&Path, u32) -> std::io::Result<()>,
) -> Vec<RepairStep> {
    tighten_modes(
        repair_with_store(home, mode, store),
        mode,
        chmod,
        &read_mode,
    )
}

/// Assert no rendered step carries the value or its vendor prefix.
fn assert_no_value(steps: &[RepairStep]) {
    let rendered = format!("{steps:?}");
    assert!(!rendered.contains(FAKE_KEY), "value leaked: {rendered}");
    assert!(!rendered.contains("sk-test"), "prefix leaked: {rendered}");
}

/// Why: the scoped flag exists so an operator can fix ONE exposure; every
/// step it produces must belong to the `launchd_secrets` class.
/// Test: this test.
#[test]
fn scoped_repair_runs_only_the_launchd_secrets_class() {
    let (home, _path) = home_with_plist(0o600);
    let steps = run(
        home.path(),
        RepairMode::Apply,
        Arc::new(MemoryKeyStore::new()),
        &set_mode,
    );
    assert_eq!(steps.len(), 1, "{steps:?}");
    assert!(steps.iter().all(|s| s.check == CHECK_NAME), "{steps:?}");
}

/// Why (#8236): `write_atomic` keeps the old mode, so a stripped plist stayed
/// `0644` and the row stayed at WARN. The scoped repair narrows it and says so.
/// Test: this test.
#[cfg(unix)]
#[test]
fn scoped_repair_tightens_a_wide_plist_to_0600() {
    let (home, path) = home_with_plist(0o644);
    let store = Arc::new(MemoryKeyStore::new());

    let planned = run(home.path(), RepairMode::DryRun, store.clone(), &set_mode);
    assert_eq!(planned[0].status, StepStatus::Planned);
    assert!(
        planned[0].what.contains("then tighten mode 0644 to 0600"),
        "{}",
        planned[0].what
    );
    assert_eq!(read_mode(&path), Some(0o644), "a dry run must not chmod");

    let steps = run(home.path(), RepairMode::Apply, store.clone(), &set_mode);
    assert_eq!(steps[0].status, StepStatus::Applied { backup: None });
    assert!(
        steps[0].what.contains("tightened mode 0644 to 0600"),
        "{}",
        steps[0].what
    );
    assert_eq!(read_mode(&path), Some(0o600));
    assert_eq!(store.get("openrouter").as_deref(), Some(FAKE_KEY));
}

/// Why (#8236 Fail-Open Check): a chmod that fails must turn the step into a
/// failure, never leave the stripped plist reported as fixed.
/// Test: this test.
#[cfg(unix)]
#[test]
fn scoped_repair_fails_when_the_chmod_fails() {
    let (home, path) = home_with_plist(0o644);
    let refuse = |_: &Path, _: u32| Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied));

    let steps = run(
        home.path(),
        RepairMode::Apply,
        Arc::new(MemoryKeyStore::new()),
        &refuse,
    );

    match &steps[0].status {
        StepStatus::Failed(why) => assert!(why.contains("0644"), "{why}"),
        other => panic!("a failed chmod must report Failed, got {other:?}"),
    }
    assert_eq!(read_mode(&path), Some(0o644));
    assert_no_value(&steps);
}

/// Why: a chmod that returns `Ok` without narrowing the mode is the same
/// defect as one that errors — the re-read catches it.
/// Test: this test.
#[cfg(unix)]
#[test]
fn scoped_repair_fails_when_the_mode_stays_wide() {
    let (home, _path) = home_with_plist(0o644);
    let no_op = |_: &Path, _: u32| Ok(());

    let steps = run(
        home.path(),
        RepairMode::Apply,
        Arc::new(MemoryKeyStore::new()),
        &no_op,
    );

    assert!(
        matches!(&steps[0].status, StepStatus::Failed(why) if why.contains("still 0644")),
        "{:?}",
        steps[0].status
    );
}

/// Why: the scoped repair prints key names and outcomes only. Every field the
/// step printer renders — check, path, what, status — is in the `Debug` form.
/// Test: this test.
#[test]
fn scoped_repair_output_never_carries_a_value() {
    for mode in [RepairMode::DryRun, RepairMode::Apply] {
        let (home, _path) = home_with_plist(0o644);
        let steps = run(
            home.path(),
            mode,
            Arc::new(MemoryKeyStore::new()),
            &set_mode,
        );
        assert!(!steps.is_empty());
        assert!(
            steps.iter().all(|s| s.what.contains("OPENROUTER_API_KEY")),
            "the step must name the key: {steps:?}"
        );
        assert_no_value(&steps);
    }
}
