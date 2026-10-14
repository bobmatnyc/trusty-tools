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
        // #8433 B2: the hook's owner-less write, from marks it could not list.
        "c.json: failed re-apply, tainted by the SessionStart hook (no owner",
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

/// The delivery trace in `dir` holding `lines`.
fn trace(dir: &Path, lines: &[crate::core::channels_delivery::DeliveryLine]) {
    for line in lines {
        crate::core::channels_delivery::record_delivery(dir, line).expect("trace line");
    }
}

/// 🔴 #9873 (F13, Q5): a `pending` line with no settle, the daemon died or
/// the settle failed mid-send, is a `Fail` naming the question and asker.
#[test]
fn delivery_pending_without_settle_is_a_doctor_fail() {
    use crate::core::channels_delivery::{DeliveryLine, DeliveryState};
    let home = tempfile::tempdir().expect("tempdir");
    let dir = channels_taint::taint_dir(home.path());
    let live: HashSet<String> = ["live-1".to_owned()].into();
    taint_managed_session(&dir, "live-1", "a", &mark()).expect("live");
    trace(
        &dir,
        &[
            DeliveryLine::new(DeliveryState::Pending, "6", "live-1", None),
            DeliveryLine::new(DeliveryState::Delivered, "6", "live-1", None),
        ],
    );
    let ok = channel_taint_row(&dir, &live);
    assert_eq!(
        ok.status,
        CheckStatus::Ok,
        "a settled delivery: {}",
        ok.message
    );
    trace(
        &dir,
        &[DeliveryLine::new(
            DeliveryState::Pending,
            "7",
            "live-1",
            None,
        )],
    );
    let row = channel_taint_row(&dir, &live);
    assert_eq!(row.status, CheckStatus::Fail, "{}", row.message);
    assert!(
        row.message
            .contains("Q-7 to asker live-1: delivery unconfirmed"),
        "{}",
        row.message
    );
}

/// 🔴 #9873 (AC2, Q4): a delivery that failed after `resolve` is a `Fail`
/// naming the question id and the asker.
#[test]
fn delivery_failed_line_is_a_doctor_fail_naming_question_and_asker() {
    use crate::core::channels_delivery::{DeliveryLine, DeliveryState};
    let home = tempfile::tempdir().expect("tempdir");
    let dir = channels_taint::taint_dir(home.path());
    let live: HashSet<String> = ["live-1".to_owned()].into();
    taint_managed_session(&dir, "live-1", "a", &mark()).expect("live");
    trace(
        &dir,
        &[
            DeliveryLine::new(DeliveryState::Pending, "7", "live-1", None),
            DeliveryLine::new(DeliveryState::Failed, "7", "live-1", Some("send_failed")),
        ],
    );
    let row = channel_taint_row(&dir, &live);
    assert_eq!(row.status, CheckStatus::Fail, "{}", row.message);
    assert!(
        row.message
            .contains("Q-7 to asker live-1: delivery failed (send_failed)"),
        "{}",
        row.message
    );
}

/// 🔴 #9873 (F18): a delivery trace that cannot be read grades `Unknown`,
/// never `Ok`.
#[test]
fn doctor_row_is_unknown_when_delivery_log_unreadable() {
    use std::os::unix::fs::PermissionsExt as _;
    let home = tempfile::tempdir().expect("tempdir");
    let dir = channels_taint::taint_dir(home.path());
    let live: HashSet<String> = ["live-1".to_owned()].into();
    taint_managed_session(&dir, "live-1", "a", &mark()).expect("live");
    let path = dir.join(crate::core::channels_delivery::DELIVERY_FILE);
    std::fs::write(&path, "{\"truncated\n").expect("torn trace");
    let row = channel_taint_row(&dir, &live);
    assert_eq!(row.status, CheckStatus::Unknown, "{}", row.message);
    std::fs::write(&path, "").expect("empty trace");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).expect("chmod");
    let row = channel_taint_row(&dir, &live);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("chmod");
    if !nix::unistd::geteuid().is_root() {
        assert_eq!(row.status, CheckStatus::Unknown, "{}", row.message);
    }
}

/// 🔴 #9873 (Q4a): a question the table records as resolved with no
/// delivery line at all (the daemon stopped between `resolve` and the
/// taint) is a `Fail` naming it and its asker.
#[cfg(feature = "channels")]
#[test]
fn resolved_question_without_a_delivery_line_is_a_doctor_fail() {
    use crate::channels::question_table::{
        Ask, Conversation, QuestionTable, ResolveOutcome, questions_path,
    };
    let home = tempfile::tempdir().expect("tempdir");
    let dir = channels_taint::taint_dir(home.path());
    let live: HashSet<String> = HashSet::new();
    let table = QuestionTable::open_at(&questions_path(home.path())).expect("table");
    let conversation = Conversation {
        dm_channel_id: "D0OWNERDM".into(),
        thread_ts: "1700000000.000100".into(),
    };
    let id = table.reserve().expect("reserve");
    let ask = Ask {
        channel: trusty_channels::policy::Channel::Slack,
        route: "owner-0".into(),
        recipient: "U0OWNER1".into(),
        asker_managed_session: "m-arch".into(),
        conversation: conversation.clone(),
        message_ts: "1700000000.000100".into(),
    };
    table.open(id, ask).expect("open");
    let ok = channel_taint_row(&dir, &live);
    assert_eq!(
        ok.status,
        CheckStatus::Ok,
        "an open question: {}",
        ok.message
    );
    assert_eq!(
        table.resolve(id, "U0OWNER1", &conversation, "yes"),
        ResolveOutcome::Resolved
    );
    let row = channel_taint_row(&dir, &live);
    assert_eq!(row.status, CheckStatus::Fail, "{}", row.message);
    assert!(
        row.message.contains(&format!(
            "Q-{id} to asker m-arch: resolved with no delivery record"
        )),
        "{}",
        row.message
    );
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
