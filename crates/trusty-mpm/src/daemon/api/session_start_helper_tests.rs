//! #10751: an Architect and its `-poll` helper share one directory by design,
//! so their `SessionStart` must link the Architect, not raise the #3764 alarm
//! that two `claude` sessions in one cwd still raise.

use std::sync::Arc;

use tracing::instrument::WithSubscriber;
use tracing_subscriber::layer::SubscriberExt;

use crate::core::hook::HookEvent;
use crate::daemon::api::HookPost;
use crate::daemon::rpc::sessions_legacy_ops::ingest_hook;
use crate::daemon::state::DaemonState;
use crate::session_manager::{ManagedSessionId, SessionKind};

const CLAUDE_ID: &str = "550e8400-e29b-41d4-a716-446655410751";
const COLLISION: &str = "Active managed sessions share the same cwd";

/// An isolated daemon with one Active managed session per `kinds` entry, all
/// working in `ws`, each stamped with its kind.
async fn state_with_kinds(
    root: &std::path::Path,
    ws: &std::path::Path,
    kinds: &[SessionKind],
) -> (Arc<DaemonState>, Vec<ManagedSessionId>) {
    let state = Arc::new(DaemonState::with_root_isolated_managed(root.to_path_buf()).await);
    let mgr = state.session_manager().await;
    let mut ids = Vec::new();
    for kind in kinds {
        let rec = mgr
            .create(
                "helper task".into(),
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
        let mut store = mgr.store.write().await;
        let mut record = store.cached_get(&rec.id).expect("seeded record");
        record.kind = *kind;
        store.upsert(record).await.expect("stamp kind");
        ids.push(rec.id);
    }
    (state, ids)
}

/// Send one `SessionStart` for `ws` and return every log line it emitted.
async fn session_start_lines(state: &Arc<DaemonState>, ws: &std::path::Path) -> Vec<String> {
    let buffer = trusty_common::log_buffer::LogBuffer::new(64);
    let subscriber = tracing_subscriber::registry().with(
        trusty_common::log_buffer::LogBufferLayer::new(buffer.clone()),
    );
    let post = HookPost {
        session_id: CLAUDE_ID.to_owned(),
        event: HookEvent::SessionStart,
        payload: serde_json::json!({ "cwd": ws.display().to_string() }),
    };
    ingest_hook(state, post)
        .with_subscriber(subscriber)
        .await
        .expect("SessionStart accepted");
    buffer.tail(64)
}

/// The `claude_session_id` each record in `ids` carries after the hook.
async fn linked_ids(state: &DaemonState, ids: &[ManagedSessionId]) -> Vec<Option<String>> {
    let mgr = state.session_manager().await;
    let mut out = Vec::new();
    for id in ids {
        out.push(mgr.get(id).await.expect("get").claude_session_id);
    }
    out
}

/// 🔴 #10751: an Architect plus its poller in one cwd — the Architect is
/// linked and no ERROR is logged.
#[tokio::test]
#[serial_test::serial]
async fn session_start_links_the_architect_beside_its_poller_without_error() {
    crate::test_support::enable_event_capture();
    let root = crate::test_support::hermetic_temp_dir();
    let ws = crate::test_support::hermetic_temp_dir();
    let kinds = [SessionKind::Architect, SessionKind::ArchitectAux];
    let (state, ids) = state_with_kinds(root.path(), ws.path(), &kinds).await;

    let lines = session_start_lines(&state, ws.path()).await;

    assert!(
        !lines
            .iter()
            .any(|l| l.contains("ERROR") || l.contains(COLLISION)),
        "an Architect beside its poller is no collision: {lines:#?}"
    );
    assert_eq!(
        linked_ids(&state, &ids).await,
        vec![Some(CLAUDE_ID.to_owned()), None],
        "the Architect is linked; the poller, which runs no claude, is not"
    );
}

/// 🔴 #10751: two claude sessions in one cwd still skip attribution and log
/// the #3764 ERROR, whether or not one of them is the Architect.
#[tokio::test]
#[serial_test::serial]
async fn session_start_still_refuses_two_claude_sessions_in_one_cwd() {
    crate::test_support::enable_event_capture();
    for kinds in [
        [SessionKind::Architect, SessionKind::Ordinary],
        [SessionKind::Ordinary, SessionKind::Ordinary],
    ] {
        let root = crate::test_support::hermetic_temp_dir();
        let ws = crate::test_support::hermetic_temp_dir();
        let (state, ids) = state_with_kinds(root.path(), ws.path(), &kinds).await;

        let lines = session_start_lines(&state, ws.path()).await;

        let line = lines
            .iter()
            .find(|l| l.contains(COLLISION))
            .unwrap_or_else(|| panic!("{kinds:?}: the collision is reported: {lines:#?}"));
        assert!(line.contains("ERROR"), "{kinds:?}: at ERROR: {line}");
        assert_eq!(
            linked_ids(&state, &ids).await,
            vec![None, None],
            "{kinds:?}: neither session is linked"
        );
    }
}
