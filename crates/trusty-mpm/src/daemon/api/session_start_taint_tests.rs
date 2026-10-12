//! #8433 P0 (Dfc, D09): the daemon re-applies a managed session's channel
//! taint to the Claude id a `SessionStart` reports, and fails the call when
//! it cannot.

use std::sync::Arc;

use crate::core::channels_taint::{self, TaintMark, TaintState};
use crate::core::hook::HookEvent;
use crate::daemon::api::HookPost;
use crate::daemon::rpc::sessions_legacy_ops::ingest_hook;
use crate::daemon::state::DaemonState;

/// An isolated daemon with one Active managed session working in `ws`.
async fn state_with_active(
    root: &std::path::Path,
    ws: &std::path::Path,
) -> (Arc<DaemonState>, String) {
    let state = Arc::new(DaemonState::with_root_isolated_managed(root.to_path_buf()).await);
    let mgr = state.session_manager().await;
    let rec = mgr
        .create(
            "taint task".into(),
            Some(ws.to_path_buf()),
            None,
            None,
            None,
            None,
        )
        .await
        .expect("create");
    mgr.stop(&rec.id).await.expect("stop");
    mgr.resume(&rec.id).await.expect("resume");
    (state, rec.id.to_string())
}

fn taint_dir(state: &DaemonState) -> std::path::PathBuf {
    channels_taint::taint_dir(&state.user_home().expect("pinned home"))
}

fn start(id: &str, ws: &std::path::Path) -> HookPost {
    HookPost {
        session_id: id.to_owned(),
        event: HookEvent::SessionStart,
        payload: serde_json::json!({ "cwd": ws.display().to_string() }),
    }
}

fn mark() -> TaintMark {
    TaintMark {
        channel: "slack".into(),
        route: "owner-dm".into(),
        question_id: "Q-1".into(),
        tainted_at: chrono::Utc::now(),
    }
}

const OLD: &str = "550e8400-e29b-41d4-a716-446655440001";
const NEW: &str = "550e8400-e29b-41d4-a716-446655440002";

/// 🔴 #8433 (Dfc): a tainted managed session's new Claude id reads tainted
/// once the daemon has answered its `SessionStart`.
#[tokio::test]
async fn session_start_with_new_id_reapplies_taint_for_tainted_managed_session() {
    let root = crate::test_support::hermetic_temp_dir();
    let ws = crate::test_support::hermetic_temp_dir();
    let (state, managed) = state_with_active(root.path(), ws.path()).await;
    let dir = taint_dir(&state);
    channels_taint::taint_managed_session(&dir, &managed, OLD, &mark()).expect("taint");
    assert_eq!(channels_taint::read_state(&dir, NEW), TaintState::Untainted);

    ingest_hook(&state, start(NEW, ws.path()))
        .await
        .expect("SessionStart accepted");

    assert!(
        channels_taint::read_state(&dir, NEW).is_tainted(),
        "the new id is tainted"
    );
    assert!(
        channels_taint::read_state(&dir, OLD).is_tainted(),
        "the old id stays tainted"
    );
}

/// 🔴 #8433: an untainted managed session's `SessionStart` writes nothing.
#[tokio::test]
async fn session_start_for_untainted_managed_session_writes_nothing() {
    let root = crate::test_support::hermetic_temp_dir();
    let ws = crate::test_support::hermetic_temp_dir();
    let (state, _managed) = state_with_active(root.path(), ws.path()).await;
    let dir = taint_dir(&state);

    ingest_hook(&state, start(NEW, ws.path()))
        .await
        .expect("SessionStart accepted");

    assert_eq!(channels_taint::read_state(&dir, NEW), TaintState::Untainted);
    assert_eq!(
        channels_taint::presence(&dir),
        channels_taint::Presence::None
    );
}

/// 🔴 #8433 (D09): a re-apply the daemon cannot complete fails the call, and
/// the id is never left reading "untainted" by a success answer.
#[tokio::test]
async fn session_start_reapply_failure_fails_the_ingest() {
    let root = crate::test_support::hermetic_temp_dir();
    let ws = crate::test_support::hermetic_temp_dir();
    let (state, managed) = state_with_active(root.path(), ws.path()).await;
    let dir = taint_dir(&state);
    channels_taint::taint_managed_session(&dir, &managed, OLD, &mark()).expect("taint");
    // A mark the daemon cannot read: it cannot tell, so it must fail.
    std::fs::write(
        dir.join("managed").join(format!("{managed}.json")),
        "garbage",
    )
    .expect("corrupt");

    let err = ingest_hook(&state, start(NEW, ws.path()))
        .await
        .expect_err("must fail");
    assert!(err.to_string().contains("re-apply"), "{err}");
}
