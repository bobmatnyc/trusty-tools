//! #8433 P0 (D09): settling a `SessionStart` the daemon did not confirm.

use super::*;
use trusty_mpm::core::channels_taint::{
    TaintMark, TaintState, read_state, taint_dir, taint_managed_session,
};

fn mark() -> TaintMark {
    TaintMark {
        channel: "slack".into(),
        route: "owner-dm".into(),
        question_id: "Q-1".into(),
        tainted_at: chrono::Utc::now(),
    }
}

fn no_answer() -> Delivery {
    Delivery::NoAnswer("connection refused".into())
}

/// #8433: a confirmed `SessionStart` settles nothing, tainted machine or not.
#[test]
fn accepted_delivery_settles_nothing() {
    let home = tempfile::tempdir().expect("tempdir");
    taint_managed_session(&taint_dir(home.path()), "m-1", "old", &mark()).expect("taint");
    assert_eq!(
        settle_session_start(Some(home.path()), "new", &Delivery::Accepted),
        Ok(())
    );
    assert_eq!(
        read_state(&taint_dir(home.path()), "new"),
        TaintState::Untainted
    );
}

/// 🔴 #8433: with no taint anywhere there is nothing to re-apply — the hook
/// writes nothing and succeeds, as before this change.
#[test]
fn daemon_unreachable_with_no_taint_anywhere_writes_nothing() {
    let home = tempfile::tempdir().expect("tempdir");
    assert_eq!(
        settle_session_start(Some(home.path()), "new", &no_answer()),
        Ok(())
    );
    assert!(!taint_dir(home.path()).exists(), "nothing was created");
}

/// 🔴 #8433 (D09): an unconfirmed or refused `SessionStart` on a machine with
/// a live taint taints the new id itself and fails the hook.
#[test]
fn unconfirmed_delivery_with_taint_present_taints_the_new_id_and_fails() {
    for delivery in [
        no_answer(),
        Delivery::Refused("500: re-apply failed".into()),
    ] {
        let home = tempfile::tempdir().expect("tempdir");
        let dir = taint_dir(home.path());
        taint_managed_session(&dir, "m-1", "old", &mark()).expect("taint");
        let err = settle_session_start(Some(home.path()), "new", &delivery).expect_err("fails");
        assert!(err.contains("tainted this session itself"), "{err}");
        assert_eq!(
            read_state(&dir, "new"),
            TaintState::Tainted(channels_taint::TaintReason::Present)
        );
        let again = settle_session_start(Some(home.path()), "new", &delivery).expect_err("fails");
        assert!(again.contains("already reads channel-tainted"), "{again}");
    }
}

/// 🔴 #8433 (D09): a hook-side write that fails too is recorded and fails;
/// an unknown home fails closed.
#[test]
fn double_failure_is_recorded_and_fails() {
    let home = tempfile::tempdir().expect("tempdir");
    let dir = taint_dir(home.path());
    taint_managed_session(&dir, "m-1", "old", &mark()).expect("taint");
    std::fs::create_dir_all(dir.join("new.json").join("occupied")).expect("block the target");
    let err = settle_session_start(Some(home.path()), "new", &no_answer());
    // The blocked target reads tainted (unreadable), so the hook says so.
    assert!(err.is_err());
    let blocked = tempfile::tempdir().expect("tempdir");
    let dir = taint_dir(blocked.path());
    taint_managed_session(&dir, "m-1", "old", &mark()).expect("taint");
    let failures = dir
        .parent()
        .expect("channels")
        .join(channels_taint::REAPPLY_FAILURES_FILE);
    let err =
        settle_session_start(Some(blocked.path()), "bad/id", &no_answer()).expect_err("fails");
    assert!(
        err.contains("already reads"),
        "a bad id reads tainted: {err}"
    );
    std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o500))
        .expect("ro");
    let err = settle_session_start(Some(blocked.path()), "new2", &no_answer()).expect_err("fails");
    std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700))
        .expect("rw");
    assert!(err.contains("own taint write failed"), "{err}");
    let log = std::fs::read_to_string(&failures).expect("the double failure is recorded");
    assert!(log.contains("new2"), "{log}");
    assert!(
        settle_session_start(None, "new", &no_answer()).is_err(),
        "unknown home fails"
    );
}
