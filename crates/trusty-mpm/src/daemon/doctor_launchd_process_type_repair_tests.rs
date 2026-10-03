//! Tests for the `launchd_process_type` repair (#8562).
//!
//! Every fixture is a temp directory; no test reads or writes the real
//! `~/Library/LaunchAgents`, and the repair under test never calls `launchctl`.

use super::repair::{repair_process_type_in, set_process_type_interactive};
use super::*;
use crate::core::doctor_repair::{RepairMode, StepStatus};

/// A hand-written daemon plist: no `ProcessType`, a nested env dict, and a
/// comment that quotes the key.
const KEYLESS_DAEMON: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.trusty.mpm</string>
    <!-- <key>ProcessType</key><string>Background</string> -->
    <key>EnvironmentVariables</key>
    <dict>
        <key>RUST_LOG</key>
        <string>info</string>
    </dict>
</dict>
</plist>
"#;

/// The supervisor plist as `tctl install` wrote it before #8415.
const BACKGROUND_SUPERVISOR: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.trusty.mpm.supervisor</string>
    <key>ProcessType</key>
    <string>Background</string>
    <key>RunAtLoad</key>
    <true/>
</dict>
</plist>"#;

/// A LaunchAgents directory holding `files` as `(label, contents)`.
fn agents_with(files: &[(&str, &[u8])]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    for (label, body) in files {
        std::fs::write(dir.path().join(format!("{label}.plist")), body).expect("write plist");
    }
    dir
}

fn plist(dir: &tempfile::TempDir, label: &str) -> PathBuf {
    dir.path().join(format!("{label}.plist"))
}

fn row_for(dir: &tempfile::TempDir) -> DoctorCheck {
    check_launchd_process_type_in(&AgentsDir {
        path: dir.path().to_path_buf(),
        from_env: false,
    })
}

/// A dry run names both stale plists and writes nothing.
#[test]
fn dry_run_plans_without_writing() {
    let dir = agents_with(&[
        (MPM, KEYLESS_DAEMON.as_bytes()),
        (MPM_SUPERVISOR, BACKGROUND_SUPERVISOR.as_bytes()),
    ]);
    let steps = repair_process_type_in(dir.path(), RepairMode::DryRun);
    assert_eq!(steps.len(), 2, "{steps:?}");
    assert!(steps.iter().all(|s| s.status == StepStatus::Planned));
    let daemon = std::fs::read_to_string(plist(&dir, MPM)).expect("read");
    assert_eq!(daemon, KEYLESS_DAEMON, "a dry run must not write");
}

/// REGRESSION (#8562): the daemon plist had no supported write path. After the
/// repair it declares `Interactive` and the rest survives. The row reports
/// "written; pending reload" until a boot after the write, then passes.
#[test]
fn apply_adds_the_key_and_the_row_reports_pending_reload() {
    let dir = agents_with(&[(MPM, KEYLESS_DAEMON.as_bytes())]);
    assert_eq!(row_for(&dir).status, CheckStatus::Warn);

    let steps = repair_process_type_in(dir.path(), RepairMode::Apply);
    assert_eq!(steps.len(), 1, "{steps:?}");
    assert_eq!(steps[0].status, StepStatus::Applied { backup: None });

    let after = std::fs::read_to_string(plist(&dir, MPM)).expect("read");
    assert_eq!(
        process_type_of(&after),
        Ok(Some("Interactive".into())),
        "{after}"
    );
    assert!(after.contains("<key>RUST_LOG</key>"), "{after}");
    assert!(
        after.contains("<!-- <key>ProcessType</key><string>Background</string> -->"),
        "the comment is left alone: {after}"
    );
    let row = row_for(&dir);
    assert_eq!(row.status, CheckStatus::Warn, "{}", row.message);
    assert!(
        row.message.contains("written; pending reload"),
        "{}",
        row.message
    );
    let rebooted = std::time::SystemTime::now() + std::time::Duration::from_secs(86_400);
    let dir = AgentsDir {
        path: dir.path().to_path_buf(),
        from_env: false,
    };
    let row = check_launchd_process_type_at(&dir, Some(rebooted));
    assert_eq!(row.status, CheckStatus::Ok, "{}", row.message);
}

/// A `Background` value is replaced in place; nothing else changes.
#[test]
fn apply_replaces_a_background_value_and_keeps_the_rest() {
    let out = set_process_type_interactive(BACKGROUND_SUPERVISOR).expect("edit");
    assert_eq!(
        out,
        BACKGROUND_SUPERVISOR.replace(
            "<string>Background</string>",
            "<string>Interactive</string>"
        )
    );
}

/// A second run, an already-correct plist and an absent one produce no step.
#[test]
fn repair_is_silent_when_already_interactive_or_absent() {
    let dir = agents_with(&[(MPM_SUPERVISOR, BACKGROUND_SUPERVISOR.as_bytes())]);
    assert_eq!(
        repair_process_type_in(dir.path(), RepairMode::Apply).len(),
        1
    );
    assert!(repair_process_type_in(dir.path(), RepairMode::Apply).is_empty());
}

/// A binary plist cannot be judged, so it is refused and left untouched.
#[test]
fn repair_refuses_a_binary_plist() {
    let dir = agents_with(&[(MPM, b"bplist00\x00\x01")]);
    let steps = repair_process_type_in(dir.path(), RepairMode::Apply);
    assert!(
        matches!(&steps[..], [s] if matches!(s.status, StepStatus::Refused(_))),
        "{steps:?}"
    );
    assert_eq!(
        std::fs::read(plist(&dir, MPM)).expect("read"),
        b"bplist00\x00\x01"
    );
}

/// A symlinked plist is refused: a rename would replace the link.
#[cfg(unix)]
#[test]
fn repair_refuses_a_symlinked_plist() {
    let dir = agents_with(&[]);
    let target = dir.path().join("real.xml");
    std::fs::write(&target, KEYLESS_DAEMON).expect("write");
    std::os::unix::fs::symlink(&target, plist(&dir, MPM)).expect("symlink");
    let steps = repair_process_type_in(dir.path(), RepairMode::Apply);
    assert!(
        matches!(&steps[..], [s] if matches!(s.status, StepStatus::Refused(_))),
        "{steps:?}"
    );
    assert_eq!(
        std::fs::read_to_string(&target).expect("read"),
        KEYLESS_DAEMON
    );
}

/// A plist whose top-level dict is not closed as expected is refused.
#[test]
fn an_unexpected_layout_is_refused() {
    let err = set_process_type_interactive("<plist><array></array></plist>");
    assert!(err.is_err(), "{err:?}");
}

/// Owner ruling 2026-09-28 (#8562): the repair never reloads launchd, so every
/// step and the row's remedy say when launchd applies the class for that label:
/// when it next LOADS it, never on a kickstart or crash respawn, and — for the
/// supervisor — not on a daemon restart.
#[test]
fn every_step_says_when_launchd_loads_the_new_class() {
    let dir = agents_with(&[
        (MPM, KEYLESS_DAEMON.as_bytes()),
        (MPM_SUPERVISOR, BACKGROUND_SUPERVISOR.as_bytes()),
    ]);
    for mode in [RepairMode::DryRun, RepairMode::Apply] {
        let steps = repair_process_type_in(dir.path(), mode);
        assert_eq!(steps.len(), 2, "{steps:?}");
        for s in &steps {
            let label = if s.path.ends_with(format!("{MPM}.plist")) {
                MPM
            } else {
                MPM_SUPERVISOR
            };
            assert!(s.what.contains("not reloaded"), "{}", s.what);
            assert!(
                s.what
                    .contains(&format!("applies it when it next loads `{label}`")),
                "{}",
                s.what
            );
            assert!(
                s.what.contains("launchctl bootout") && s.what.contains("launchctl bootstrap"),
                "{}",
                s.what
            );
            assert!(s.what.contains("crash respawn"), "{}", s.what);
            assert!(!s.what.contains("next daemon restart"), "{}", s.what);
            assert_eq!(
                s.what.contains("tm daemon restart does not reload"),
                label == MPM_SUPERVISOR,
                "{}",
                s.what
            );
        }
    }
    let stale = agents_with(&[(MPM_SUPERVISOR, BACKGROUND_SUPERVISOR.as_bytes())]);
    let row = row_for(&stale);
    assert!(
        row.message.contains("tm doctor --fix --yes"),
        "{}",
        row.message
    );
    assert!(
        row.message.contains("tm daemon restart does not reload"),
        "{}",
        row.message
    );
}

/// #8562: a write that fails is reported as `Failed`, and the plist is left
/// byte-identical.
#[cfg(unix)]
#[test]
fn a_write_failure_is_reported_and_leaves_the_plist_unchanged() {
    use std::os::unix::fs::PermissionsExt;
    let dir = agents_with(&[(MPM_SUPERVISOR, BACKGROUND_SUPERVISOR.as_bytes())]);
    let set_mode = |mode| {
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(mode)).expect("chmod");
    };
    set_mode(0o555);
    let steps = repair_process_type_in(dir.path(), RepairMode::Apply);
    set_mode(0o755);
    assert!(
        matches!(&steps[..], [s] if matches!(s.status, StepStatus::Failed(_))),
        "{steps:?}"
    );
    assert_eq!(
        std::fs::read(plist(&dir, MPM_SUPERVISOR)).expect("read"),
        BACKGROUND_SUPERVISOR.as_bytes()
    );
}
