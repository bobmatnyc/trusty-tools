//! #8433 P0: a managed session's end retires its channel taint; nothing else does.

use super::super::record::{ManagedSessionId, StopCause};
use super::super::tests::make_manager_with_pane;
use super::*;
use crate::core::channels_taint::{TaintMark, TaintState, read_state, taint_managed_session};

fn mark() -> TaintMark {
    TaintMark {
        channel: "slack".into(),
        route: "owner-dm".into(),
        question_id: "Q-1".into(),
        tainted_at: chrono::Utc::now(),
    }
}

/// A manager with the taint dir installed and one tainted record; its id.
async fn tainted_manager(
    dir: &tempfile::TempDir,
    ws: &std::path::Path,
    taint: &std::path::Path,
) -> (SessionManager, ManagedSessionId) {
    let (mgr, _fake) = make_manager_with_pane(dir).await;
    assert!(mgr.install_taint_dir(taint.to_path_buf()));
    let record = mgr
        .create_with_id(
            ManagedSessionId::new(),
            "task".into(),
            Some(ws.to_path_buf()),
            None,
            Some(ws.to_path_buf()),
            None,
            None,
            crate::runtime::RuntimeKind::default(),
            false,
            false,
        )
        .await
        .expect("create");
    taint_managed_session(taint, &record.id.to_string(), "claude-a", &mark()).expect("taint");
    (mgr, record.id)
}

/// 🔴 #8433 (Dfc): a deliberate stop and a decommission each retire the taint;
/// an observed (auto-resumable) stop keeps it.
#[tokio::test]
async fn stop_and_decommission_retire_taint() {
    let dir = crate::test_support::hermetic_temp_dir();
    let ws = crate::test_support::hermetic_temp_dir();
    let home = crate::test_support::hermetic_temp_dir();
    let taint = crate::core::channels_taint::taint_dir(home.path());

    let (mgr, id) = tainted_manager(&dir, ws.path(), &taint).await;
    mgr.stop_with_cause(&id, StopCause::Unexpected)
        .await
        .expect("observed stop");
    assert!(
        read_state(&taint, "claude-a").is_tainted(),
        "an observed stop keeps the taint"
    );
    mgr.resume(&id).await.expect("resume");
    mgr.stop(&id).await.expect("stop");
    assert_eq!(
        read_state(&taint, "claude-a"),
        TaintState::Untainted,
        "a stop retires it"
    );

    let dir2 = crate::test_support::hermetic_temp_dir();
    let (mgr, id) = tainted_manager(&dir2, ws.path(), &taint).await;
    let root = crate::test_support::hermetic_temp_dir();
    mgr.decommission_with_root(&id, root.path(), None)
        .await
        .expect("decommission");
    assert_eq!(
        read_state(&taint, "claude-a"),
        TaintState::Untainted,
        "decommission retires it"
    );
    assert_eq!(
        crate::core::channels_taint::presence(&taint),
        crate::core::channels_taint::Presence::None,
        "the mark is gone too"
    );
}
