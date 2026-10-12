//! #8433 P0: the `channel_taint` doctor row reports and never deletes.

use super::*;
use crate::core::channels_taint::{TaintMark, TaintRecord, taint_managed_session, write_taint};

fn mark() -> TaintMark {
    TaintMark {
        channel: "slack".into(),
        route: "owner-dm".into(),
        question_id: "Q-1".into(),
        tainted_at: chrono::Utc::now(),
        workspace: std::path::PathBuf::from("/"),
    }
}

fn files(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut out: Vec<_> = walk(dir);
    out.sort();
    out
}

fn walk(dir: &Path) -> Vec<std::path::PathBuf> {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .flat_map(|e| {
                    if e.path().is_dir() {
                        walk(&e.path())
                    } else {
                        vec![e.path()]
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

/// 🔴 #8433: orphaned files and marks, failed re-applies and unreadable files
/// are listed; a live session's taint is fine; nothing is deleted.
#[test]
fn doctor_lists_orphan_taint_files_and_deletes_none() {
    let home = tempfile::tempdir().expect("tempdir");
    let dir = channels_taint::taint_dir(home.path());
    let live: HashSet<String> = ["live-1".to_owned()].into();
    taint_managed_session(&dir, "live-1", "a", &mark()).expect("live");
    let ok = channel_taint_row(&dir, &live);
    assert_eq!(ok.status, CheckStatus::Ok, "{}", ok.message);

    taint_managed_session(&dir, "gone-1", "b", &mark()).expect("orphan");
    let hook = TaintRecord {
        channel: channels_taint::REAPPLY_FAILED_CHANNEL.into(),
        ..mark().record_for("c", "")
    };
    write_taint(&dir, &hook).expect("hook-side");
    std::fs::write(dir.join("d.json"), "garbage").expect("malformed");
    let before = files(&dir);

    let row = channel_taint_row(&dir, &live);
    assert_eq!(row.status, CheckStatus::Warn, "{}", row.message);
    for needle in [
        "b.json: orphaned",
        "c.json: failed re-apply",
        "d.json: unreadable",
        "managed/gone-1.json",
    ] {
        assert!(
            row.message.contains(needle),
            "{needle} missing: {}",
            row.message
        );
    }
    assert!(
        !row.message.contains("a.json"),
        "the live taint is not flagged: {}",
        row.message
    );
    assert_eq!(files(&dir), before, "the row deleted something");

    // #8433 fix round: the record sits in the anchored taint dir.
    let failures = dir.join(channels_taint::REAPPLY_FAILURES_FILE);
    std::fs::write(&failures, "{\"session_id\":\"e\"}\n").expect("double failure");
    assert_eq!(channel_taint_row(&dir, &live).status, CheckStatus::Fail);
}

/// 🔴 #8433 (r-078): a taint dir or a managed dir that cannot be listed
/// grades `Unknown`, never `Ok`.
#[test]
fn doctor_row_is_unknown_when_taint_cannot_be_listed() {
    use std::os::unix::fs::PermissionsExt as _;
    let home = tempfile::tempdir().expect("tempdir");
    let dir = channels_taint::taint_dir(home.path());
    let live: HashSet<String> = ["live-1".to_owned()].into();
    taint_managed_session(&dir, "live-1", "a", &mark()).expect("live");
    for blocked in [dir.clone(), dir.join("managed")] {
        std::fs::set_permissions(&blocked, std::fs::Permissions::from_mode(0o000)).expect("ro");
        let row = channel_taint_row(&dir, &live);
        std::fs::set_permissions(&blocked, std::fs::Permissions::from_mode(0o700)).expect("rw");
        assert_eq!(
            row.status,
            CheckStatus::Unknown,
            "{}: {}",
            blocked.display(),
            row.message
        );
    }
}
