//! A watch transition is readable where `tm events` reads (#12049).
//!
//! Why: the loop tests drive a fake emitter, so none saw that the daemon's
//! emitter handed `ci_transition` to the bus only, while `tm events` read the
//! daemon's hook ring, which never holds it.
//! What: one tick over [`daemon_ports`] with a fake `gh` reader and a waker
//! that succeeds, then `GET /events/daemon/poll` through the daemon router —
//! the route `tm events` calls — and the spool file the bus-down path writes.
//! Test: the tests below.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::Request;
use tower::ServiceExt;

use super::{BusEmitter, RecordingEmitter, daemon_ports};
use crate::core::pr_watch::gh::{PrReader, PrSnapshot, parse_pr_view};
use crate::core::pr_watch::table::{Table, WatchRow};
use crate::core::pr_watch::verdict::Verdict;
use crate::daemon::DaemonState;
use crate::daemon::action_events::{spool, start_with_spool};
use crate::daemon::services::pr_watch_loop::{
    EVENT_KIND, Emitter, PrWatchLoop, Transition, Waker, transition_event,
};

/// Required context `ci` succeeded on head `h1`.
struct GreenReader;

impl PrReader for GreenReader {
    fn view(&self, _repo: &str, _pr: u64) -> anyhow::Result<PrSnapshot> {
        parse_pr_view(
            r#"{"state":"OPEN","headRefOid":"h1","baseRefName":"main","statusCheckRollup":[{"name":"ci","status":"COMPLETED","conclusion":"SUCCESS"}]}"#,
        )
    }
    fn required(&self, _repo: &str, _base: &str) -> anyhow::Result<Option<Vec<String>>> {
        Ok(Some(vec!["ci".into()]))
    }
}

struct OkWake;

#[async_trait::async_trait]
impl Waker for OkWake {
    async fn wake(&self, _owner: &str, _line: &str) -> anyhow::Result<()> {
        Ok(())
    }
}

fn row() -> WatchRow {
    WatchRow {
        repo: "o/r".into(),
        pr: 7,
        head: "h1".into(),
        base: "main".into(),
        required: vec!["ci".into()],
        owner: "pm-a".into(),
        project: None,
        verdict: Some(Verdict::Pending),
        emitted_key: None,
        ledger_key: None,
        last_woken_key: None,
        seq: 0,
        added_at: "2026-11-27T00:00:00Z".into(),
    }
}

/// `GET /events/daemon/poll` through the daemon router, as `tm events` asks.
async fn daemon_poll(state: &Arc<DaemonState>) -> serde_json::Value {
    let resp = crate::daemon::api::router(Arc::clone(state))
        .oneshot(
            Request::builder()
                .uri("/events/daemon/poll")
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("router answers");
    assert_eq!(resp.status(), 200, "the route tm events reads must exist");
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .expect("body");
    serde_json::from_slice(&bytes).expect("json body")
}

/// The `ci_transition` keys in a poll body.
fn transition_keys(body: &serde_json::Value) -> Vec<String> {
    body["events"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|e| e["payload"]["event"]["kind"] == EVENT_KIND)
        .filter_map(|e| {
            e["payload"]["event"]["data"]["key"]
                .as_str()
                .map(String::from)
        })
        .collect()
}

/// Every `.ndjson` spool line under `dir`, read without claiming the files.
fn spool_text(dir: &std::path::Path) -> String {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return String::new();
    };
    entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "ndjson"))
        .filter_map(|p| std::fs::read_to_string(p).ok())
        .collect()
}

/// 🔴 #12049 REGRESSION: a green transition the watch emits through the
/// production ports is readable at `GET /events/daemon/poll`, once across two
/// ticks, and is in the spool file the bus-down path writes.
#[tokio::test]
async fn a_watch_transition_is_readable_where_tm_events_reads_12049() {
    let dir = tempfile::tempdir().expect("tempdir");
    // No publisher attached: the bus is down, so the emit spools.
    let state = DaemonState::shared();
    let table = Table::at(dir.path().join("table"));
    table.upsert(row()).expect("add");
    let mut ports = daemon_ports(Arc::clone(&state), dir.path(), Arc::new(GreenReader));
    ports.waker = Arc::new(OkWake);
    let mut watch = PrWatchLoop::new(table, ports, 60);

    let first = watch.tick(100).await;
    assert_eq!((first.woken, first.failed), (1, 0), "{first:?}");
    watch.tick(200).await;

    let keys = transition_keys(&daemon_poll(&state).await);
    assert_eq!(
        keys,
        vec!["o/r#7@h1:green#1".to_string()],
        "tm events must show it once"
    );
    let spooled = spool_text(&spool::spool_dir(dir.path()));
    assert!(
        spooled.contains(EVENT_KIND) && spooled.contains("o/r#7@h1:green#1"),
        "the bus-down fallback file must hold it: {spooled}"
    );
}

/// 🔴 #12049 FAIL-CLOSED: a publish the stopped bus buffer refuses is an emit
/// error — the ring records nothing and the watch delivers no wake.
#[tokio::test]
async fn a_late_publish_is_an_emit_error_and_records_nothing_12049() {
    let dir = tempfile::tempdir().expect("tempdir");
    let state = Arc::new(DaemonState::new());
    let sock = dir.path().join("absent.sock");
    assert!(start_with_spool(
        &state,
        true,
        Duration::from_secs(3600),
        || Ok(sock),
        Some(dir.path().join("spool")),
    ));
    state.action_events().shutdown().await;
    let emitter = RecordingEmitter {
        inner: BusEmitter {
            publisher: state.action_events().publisher().cloned(),
            spool_dir: Some(dir.path().join("spool")),
        },
        state: Arc::clone(&state),
    };
    let t = Transition {
        kind: crate::core::pr_watch::verdict::Kind::Green,
        head: "h1".into(),
        verdict: Verdict::Green,
        key: "o/r#7@h1:green#1".into(),
        seq: 1,
        required: vec!["ci".into()],
    };
    let err = emitter
        .emit(transition_event(&row(), &t))
        .await
        .expect_err("a settled buffer must refuse the emit");
    assert!(err.to_string().contains("settled"), "{err:#}");
    assert!(state.action_events().recent_daemon_events().is_empty());

    let table = Table::at(dir.path().join("table"));
    table.upsert(row()).expect("add");
    let mut ports = daemon_ports(Arc::clone(&state), dir.path(), Arc::new(GreenReader));
    ports.waker = Arc::new(OkWake);
    let report = PrWatchLoop::new(table.clone(), ports, 60).tick(100).await;
    assert_eq!((report.woken, report.failed), (0, 1), "{report:?}");
    let kept = table.list().expect("list").remove(0);
    assert_eq!(kept.emitted_key, None, "a refused emit advances nothing");
    assert!(transition_keys(&daemon_poll(&state).await).is_empty());
}
