//! Tests for the `local/quiet-sessions.txt` init step (#10861). Every test
//! runs under a temp home, so nothing reaches the operator's `~/.trusty-mpm`.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use super::quiet_list::{DEST, HEADER, step};
use super::tests::NO_TMUX;
use super::{InitReport, Step, init, resolve_dir};

/// A full `tm fleet init --no-launch` under `home`; the Architect directory.
fn init_under(home: &Path) -> (PathBuf, InitReport) {
    let dir = resolve_dir(None, home).unwrap();
    let report = init(&dir, home, false, NO_TMUX).unwrap();
    (dir, report)
}

/// The report's steps that name the list.
fn list_steps(report: &InitReport) -> Vec<&Step> {
    report
        .steps
        .iter()
        .filter(|s| {
            let (Step::Changed(t) | Step::Unchanged(t) | Step::Skipped(t) | Step::Failed(t)) = s;
            t.contains(DEST)
        })
        .collect()
}

/// Created on a first init, and every reader parses it as no sessions.
#[test]
fn quiet_list_init_creates_an_empty_list() {
    let home = tempfile::tempdir().unwrap();
    let (dir, report) = init_under(home.path());
    let path = dir.join(DEST);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), HEADER);
    assert!(
        trusty_architect::quiet::read_quiet_sessions_checked(&path)
            .unwrap()
            .is_empty()
    );
    assert!(
        matches!(list_steps(&report)[..], [Step::Changed(_)]),
        "{}",
        report.render()
    );
}

/// A re-run reports the list unchanged and leaves the owner's edit, mtime included.
#[test]
fn quiet_list_rerun_leaves_an_edited_list_unchanged() {
    let home = tempfile::tempdir().unwrap();
    let (dir, _) = init_under(home.path());
    let path = dir.join(DEST);
    std::fs::write(&path, "tm-parked\n").unwrap();
    let before = std::fs::metadata(&path).unwrap().modified().unwrap();
    let (_, report) = init_under(home.path());
    assert!(
        matches!(list_steps(&report)[..], [Step::Unchanged(t)] if t == DEST),
        "{}",
        report.render()
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "tm-parked\n");
    assert_eq!(
        std::fs::metadata(&path).unwrap().modified().unwrap(),
        before
    );
}

/// A pre-#10861 list at the root is not shadowed by a new empty one.
#[test]
fn quiet_list_legacy_file_is_skipped_not_shadowed() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("quiet-sessions.txt"), "tm-old\n").unwrap();
    let got = step(dir.path());
    assert!(
        matches!(&got, Step::Skipped(t) if t.contains("move it to")),
        "{got:?}"
    );
    assert!(!dir.path().join(DEST).exists());
}

/// Error arm: a write that fails is a `Failed` step, never a silent success.
#[test]
fn quiet_list_write_failure_is_a_failed_step() {
    let dir = tempfile::tempdir().unwrap();
    let local = dir.path().join("local");
    std::fs::create_dir(&local).unwrap();
    std::fs::set_permissions(&local, std::fs::Permissions::from_mode(0o555)).unwrap();
    let got = step(dir.path());
    std::fs::set_permissions(&local, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(
        matches!(&got, Step::Failed(t) if t.starts_with("cannot create")),
        "{got:?}"
    );
    assert!(!dir.path().join(DEST).exists());
}
