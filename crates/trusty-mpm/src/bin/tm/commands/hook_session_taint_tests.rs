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
        workspace: std::path::PathBuf::from("/"),
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
        settle_session_start(Some(home.path()), "new", None, &Delivery::Accepted),
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
        settle_session_start(Some(home.path()), "new", None, &no_answer()),
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
        let err =
            settle_session_start(Some(home.path()), "new", None, &delivery).expect_err("fails");
        assert!(err.contains("tainted this session itself"), "{err}");
        assert_eq!(
            read_state(&dir, "new"),
            TaintState::Tainted(channels_taint::TaintReason::Present)
        );
        let again =
            settle_session_start(Some(home.path()), "new", None, &delivery).expect_err("fails");
        assert!(again.contains("already reads channel-tainted"), "{again}");
    }
}

/// 🔴 #8433 (D09): a hook-side write that fails too is recorded inside the
/// anchored taint dir and fails; an unknown home fails closed.
#[test]
fn double_failure_is_recorded_and_fails() {
    let home = tempfile::tempdir().expect("tempdir");
    let dir = taint_dir(home.path());
    taint_managed_session(&dir, "m-1", "old", &mark()).expect("taint");
    std::fs::create_dir_all(dir.join("new.json").join("occupied")).expect("block the target");
    let err = settle_session_start(Some(home.path()), "new", None, &no_answer());
    // The blocked target reads tainted (unreadable), so the hook says so.
    assert!(err.is_err());
    let blocked = tempfile::tempdir().expect("tempdir");
    let dir = taint_dir(blocked.path());
    taint_managed_session(&dir, "m-1", "old", &mark()).expect("taint");
    // #8433 fix round: the record sits in the taint dir, under the anchor. It
    // exists already, so an append works while the dir refuses new files.
    let failures = dir.join(channels_taint::REAPPLY_FAILURES_FILE);
    std::fs::write(&failures, "").expect("an empty record");
    let err = settle_session_start(Some(blocked.path()), "bad/id", None, &no_answer())
        .expect_err("fails");
    assert!(
        err.contains("already reads"),
        "a bad id reads tainted: {err}"
    );
    std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o500))
        .expect("ro");
    let err =
        settle_session_start(Some(blocked.path()), "new2", None, &no_answer()).expect_err("fails");
    std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700))
        .expect("rw");
    assert!(err.contains("own taint write failed"), "{err}");
    let log = std::fs::read_to_string(&failures).expect("the double failure is recorded");
    assert!(log.contains("new2"), "{log}");
    assert!(
        settle_session_start(None, "new", None, &no_answer()).is_err(),
        "unknown home fails"
    );
}

/// A home whose managed session `m-1` is tainted in workspace `ws`.
fn tainted_workspace(ws: &std::path::Path) -> tempfile::TempDir {
    let home = tempfile::tempdir().expect("tempdir");
    let mark = TaintMark {
        workspace: ws.to_path_buf(),
        ..mark()
    };
    taint_managed_session(&taint_dir(home.path()), "m-1", "old", &mark).expect("taint");
    home
}

/// 🔴 #8433 (critic HIGH 2): an outage or a refusal at a cwd no taint covers
/// taints nothing and passes, so one taint does not spread machine-wide.
#[test]
fn outage_at_an_unrelated_cwd_taints_nothing() {
    let ws = tempfile::tempdir().expect("ws");
    let other = tempfile::tempdir().expect("other");
    let home = tainted_workspace(ws.path());
    for delivery in [no_answer(), Delivery::Refused("403 Forbidden".into())] {
        assert_eq!(
            settle_session_start(Some(home.path()), "new", Some(other.path()), &delivery),
            Ok(())
        );
        assert_eq!(
            read_state(&taint_dir(home.path()), "new"),
            TaintState::Untainted
        );
    }
}

/// 🔴 #8433 (critic HIGH 2): an outage under the tainted workspace taints the
/// new id for the managed session that owns the taint.
#[test]
fn outage_at_the_tainted_workspace_taints_the_new_id_for_its_managed_session() {
    let ws = tempfile::tempdir().expect("ws");
    std::fs::create_dir_all(ws.path().join("sub")).expect("sub");
    let home = tainted_workspace(ws.path());
    let dir = taint_dir(home.path());
    let err = settle_session_start(
        Some(home.path()),
        "new",
        Some(&ws.path().join("sub")),
        &no_answer(),
    )
    .expect_err("fails");
    assert!(err.contains("tainted this session itself"), "{err}");
    let record: channels_taint::TaintRecord =
        serde_json::from_slice(&std::fs::read(dir.join("new.json")).expect("read")).expect("json");
    assert_eq!(
        record.managed_session, "m-1",
        "owned by the managed session"
    );
}

/// 🔴 #8433 (critic HIGH 2): a taint the hook wrote is retired with the
/// managed session that owns it.
#[test]
fn a_hook_written_taint_retires_with_its_managed_session() {
    let ws = tempfile::tempdir().expect("ws");
    let home = tainted_workspace(ws.path());
    let dir = taint_dir(home.path());
    let _ = settle_session_start(Some(home.path()), "new", Some(ws.path()), &no_answer());
    assert!(read_state(&dir, "new").is_tainted());
    channels_taint::retire(&dir, "m-1").expect("retire");
    assert_eq!(read_state(&dir, "new"), TaintState::Untainted);
    assert_eq!(
        channels_taint::presence(&dir),
        channels_taint::Presence::None
    );
}

/// 🔴 #8433 (B2, r-078): marks that cannot be listed may cover any cwd, so an
/// outage even at an unrelated cwd taints the new id with no owner and fails.
#[test]
fn outage_with_unlistable_marks_taints_the_new_id_with_no_owner() {
    let ws = tempfile::tempdir().expect("ws");
    let unrelated = tempfile::tempdir().expect("unrelated");
    let home = tainted_workspace(ws.path());
    let dir = taint_dir(home.path());
    let managed = dir.join("managed");
    std::fs::set_permissions(
        &managed,
        std::os::unix::fs::PermissionsExt::from_mode(0o000),
    )
    .expect("block the marks");
    let result = settle_session_start(
        Some(home.path()),
        "new",
        Some(unrelated.path()),
        &no_answer(),
    );
    std::fs::set_permissions(
        &managed,
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )
    .expect("restore");
    let err = result.expect_err("fails");
    assert!(err.contains("tainted this session itself"), "{err}");
    assert!(read_state(&dir, "new").is_tainted());
    let record: channels_taint::TaintRecord =
        serde_json::from_slice(&std::fs::read(dir.join("new.json")).expect("read")).expect("json");
    assert_eq!(record.managed_session, "", "no owner");
}

/// 🔴 #8433 (r-078): a socket that accepts and then times out the exchange is no
/// answer, and the event is never sent again over HTTP (#8531).
#[tokio::test]
async fn socket_exchange_failure_is_no_answer_without_http_fallback() {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let tmp = tempfile::tempdir().expect("tempdir");
    let socket = tmp.path().join("mpm.sock");
    // The hardened dial trusts only a 0600 socket in a 0700 directory.
    let unix = trusty_common::uds::bind_hardened(&socket).expect("bind socket");
    // Accept and never answer: the 2 s exchange timeout fails the call.
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((stream, _)) = unix.accept().await {
            held.push(stream);
        }
    });
    let http = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind http");
    let url = format!("http://{}", http.local_addr().expect("addr"));
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = http.accept().await {
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf).await;
            let _ = stream
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
                .await;
        }
    });
    let delivery = deliver_session_start(Some(&socket), &url, &serde_json::json!({})).await;
    assert!(matches!(delivery, Delivery::NoAnswer(_)), "{delivery:?}");
}
