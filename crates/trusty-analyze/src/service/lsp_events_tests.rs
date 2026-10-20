//! Unit tests for the LSP event producer (#9078, L4).
//!
//! Why: the handler-level tests in `tests/lsp_rpc.rs` cover the wire; these
//! pin the parts no handler reaches on demand: path display, the bound on the
//! buffer, the drop guard and the flush task's log rate.
//! Test: this file.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::io::AsyncReadExt;
use trusty_common::control_bus::{
    ActionEvent, CallPhase, HarnessEvent, HarnessPayload, PushFlushOutcome,
    DEFAULT_PUSH_BUFFER_CAPACITY,
};

use super::*;

fn absent() -> (tempfile::TempDir, Arc<LspEventPublisher>) {
    let dir = tempfile::tempdir().expect("tempdir");
    let publisher = Arc::new(LspEventPublisher::new(dir.path().join("none.sock")));
    (dir, publisher)
}

#[test]
fn paths_are_shown_home_relative() {
    let p = LspEventPublisher::new("/x.sock").with_home(Some(PathBuf::from("/home/u")));
    assert_eq!(p.shown(Path::new("/home/u/ws/src/a.rs")), "~/ws/src/a.rs");
    assert_eq!(p.shown(Path::new("/home/u")), "~");
    assert_eq!(p.shown(Path::new("/home/uv/a.rs")), "/home/uv/a.rs");
    assert_eq!(p.shown(Path::new("/opt/ws/a.rs")), "/opt/ws/a.rs");
    let none = LspEventPublisher::new("/x.sock").with_home(None);
    assert_eq!(none.shown(Path::new("/home/u/a.rs")), "/home/u/a.rs");
}

/// D9: with no bus, 10,000 events cost well under a second and the buffer
/// stays at its bound, counting what it dropped.
#[test]
fn ten_thousand_events_with_no_bus_are_fast_and_bounded() {
    let (_dir, publisher) = absent();
    let started = Instant::now();
    for _ in 0..5_000 {
        let call = LspCall::start_on(
            &publisher,
            TOOL_REFERENCES,
            Path::new("/w/a.rs"),
            None,
            Vec::new(),
        );
        call.finish(&Ok::<(), LspError>(()), |_| Vec::new());
    }
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(publisher.buffered_len(), DEFAULT_PUSH_BUFFER_CAPACITY);
    assert_eq!(
        publisher.dropped(),
        (10_000 - DEFAULT_PUSH_BUFFER_CAPACITY) as u64
    );
}

/// Flush `publisher` into a hardened listener bound at `sock` and parse what
/// arrived. `publisher` dials nothing until the flush, so it may be built
/// first.
async fn delivered(publisher: &LspEventPublisher, sock: &Path) -> Vec<HarnessEvent> {
    let listener = trusty_common::uds::bind_hardened(sock).expect("bind");
    let read = tokio::spawn(async move {
        let (mut conn, _) = listener.accept().await.expect("accept");
        let mut buf = String::new();
        conn.read_to_string(&mut buf).await.expect("read");
        buf
    });
    let outcome = publisher.flush().await;
    assert!(outcome.error.is_none(), "{:?}", outcome.error);
    read.await
        .expect("join")
        .lines()
        .map(|l| serde_json::from_str(l).expect("frame"))
        .collect()
}

fn phase(ev: &HarnessEvent) -> (CallPhase, String) {
    match &ev.payload {
        HarnessPayload::Action(ActionEvent::Tool { phase, meta, .. }) => (
            *phase,
            meta.attrs["outcome"].as_str().unwrap_or("").to_string(),
        ),
        other => panic!("not a Tool action: {other:?}"),
    }
}

#[tokio::test]
async fn a_dropped_call_ends_errored_cancelled() {
    let dir = tempfile::tempdir().expect("tempdir");
    let sock = dir.path().join("bus.sock");
    let publisher = Arc::new(LspEventPublisher::new(&sock));
    let call = LspCall::start_on(
        &publisher,
        TOOL_DIAGNOSTICS,
        Path::new("/w/a.rs"),
        None,
        Vec::new(),
    );
    drop(call);
    let frames = delivered(&publisher, &sock).await;
    assert_eq!(frames.len(), 2);
    assert_eq!(phase(&frames[1]), (CallPhase::Errored, "cancelled".into()));
    assert_eq!(frames[1].parent_id, Some(frames[0].id));
}

#[tokio::test]
async fn flush_health_warns_once_per_outage_and_rate_limits_drops() {
    let (_dir, publisher) = absent();
    let call = LspCall::start_on(
        &publisher,
        TOOL_REFERENCES,
        Path::new("/a.rs"),
        None,
        Vec::new(),
    );
    drop(call);
    let down = publisher.flush().await;
    assert!(down.error.is_some());
    let up = PushFlushOutcome {
        sent: 2,
        error: None,
    };
    let t0 = Instant::now();
    let mut h = FlushHealth::default();
    assert_eq!(h.report(&down, 0, t0), vec![Notice::Down]);
    assert_eq!(h.report(&down, 0, t0), vec![], "one warning per outage");
    assert_eq!(h.report(&up, 0, t0), vec![Notice::Recovered]);
    assert_eq!(h.report(&up, 5, t0), vec![Notice::Dropped(5)]);
    let soon = t0 + Duration::from_secs(1);
    assert_eq!(h.report(&up, 9, soon), vec![], "rate-limited");
    let later = t0 + DROP_WARN_EVERY;
    assert_eq!(h.report(&up, 9, later), vec![Notice::Dropped(9)]);
}
