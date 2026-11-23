//! `serve_until` flushes the inject producer on a stop signal (#9167 AC5).
//!
//! Why: `tm sm serve --stdio` buffers delivered-inject events; a signal must
//! not lose them.
//! What: a real publisher on a real listener, one buffered event, a stop
//! that is already resolved against a served future that never ends.
//! Test: this file.

use std::time::Duration;

use chrono::Utc;
use tokio::io::AsyncReadExt;
use trusty_common::control_bus::{HarnessEvent, HarnessPayload};

use super::*;
use crate::daemon::action_events::start_with;
use crate::session_manager::inject_record::{Delivery, InjectRecorder};

/// A signal flushes the buffered inject event to the bus, and the call ends
/// with `Signalled` (the caller then exits). Without the flush the listener
/// reads nothing.
#[tokio::test]
async fn a_signal_flushes_the_buffered_inject_event_9167() {
    let dir = tempfile::tempdir().expect("tempdir");
    let sock = dir.path().join("bus.sock");
    let state = DaemonState::new();
    assert!(start_with(&state, true, Duration::from_secs(3600), || Ok(
        sock.clone()
    )));
    let publisher = state.action_events().publisher().expect("attached");
    publisher.delivered(&Delivery {
        at: Utc::now(),
        tmux_name: "tm-proj-01",
        claude_session_id: None,
        text: "hello",
    });
    assert_eq!(publisher.buffered_len(), 1);
    let listener = trusty_common::uds::bind_hardened(&sock).expect("bind");
    let read = tokio::spawn(async move {
        let (mut conn, _) = listener.accept().await.expect("accept");
        let mut buf = Vec::new();
        conn.read_to_end(&mut buf).await.expect("read");
        buf
    });

    let ended = serve_until(&state, std::future::ready(()), std::future::pending::<()>()).await;

    assert_eq!(ended, Ended::Signalled);
    let bytes = tokio::time::timeout(Duration::from_secs(10), read)
        .await
        .expect("nothing reached the bus within 10 s")
        .expect("join");
    let frames: Vec<HarnessEvent> = String::from_utf8_lossy(&bytes)
        .lines()
        .map(|l| serde_json::from_str(l).expect("frame"))
        .collect();
    assert_eq!(frames.len(), 1, "{frames:?}");
    assert!(matches!(frames[0].payload, HarnessPayload::Message(_)));
    assert_eq!(publisher.buffered_len(), 0);
    assert_eq!(publisher.dropped(), 0);
}

/// #10468: a real SIGHUP to this process takes the settle path. The buffered
/// event reaches the bus and the serve ends `Signalled`. Before the fix SIGHUP
/// was not a stop signal: the stop never resolved (or the process died).
#[cfg(unix)]
#[tokio::test]
async fn a_sighup_settles_the_buffered_inject_event_10468() {
    let dir = tempfile::tempdir().expect("tempdir");
    let sock = dir.path().join("bus.sock");
    let state = DaemonState::new();
    assert!(start_with(&state, true, Duration::from_secs(3600), || Ok(
        sock.clone()
    )));
    let publisher = state.action_events().publisher().expect("attached");
    publisher.delivered(&Delivery {
        at: Utc::now(),
        tmux_name: "tm-proj-01",
        claude_session_id: None,
        text: "hello",
    });
    assert_eq!(publisher.buffered_len(), 1);
    let listener = trusty_common::uds::bind_hardened(&sock).expect("bind");
    let read = tokio::spawn(async move {
        let (mut conn, _) = listener.accept().await.expect("accept");
        let mut buf = Vec::new();
        conn.read_to_end(&mut buf).await.expect("read");
        buf
    });

    // The production stop, installed before the signal is raised.
    let stop =
        crate::daemon::stop_signal::StopSignals::install_with(tokio::signal::unix::signal, false)
            .recv();
    // SAFETY: raising a signal this process has a handler for.
    assert_eq!(unsafe { libc::raise(libc::SIGHUP) }, 0);
    let ended = tokio::time::timeout(
        Duration::from_secs(10),
        serve_until(
            &state,
            async move {
                assert_eq!(stop.await, "SIGHUP");
            },
            std::future::pending::<()>(),
        ),
    )
    .await
    .expect("SIGHUP did not stop the serve within 10 s");

    assert_eq!(ended, Ended::Signalled);
    let bytes = tokio::time::timeout(Duration::from_secs(10), read)
        .await
        .expect("nothing reached the bus within 10 s")
        .expect("join");
    assert_eq!(String::from_utf8_lossy(&bytes).lines().count(), 1);
    assert_eq!(publisher.buffered_len(), 0);
    assert_eq!(publisher.dropped(), 0);
}

/// EOF ends the serve with its own result and does not flush here: the
/// caller's EOF path flushes.
#[tokio::test]
async fn eof_returns_the_served_result_9167() {
    let state = DaemonState::new();
    let ended = serve_until(&state, std::future::pending::<()>(), async { 7 }).await;
    assert_eq!(ended, Ended::Served(7));
}
