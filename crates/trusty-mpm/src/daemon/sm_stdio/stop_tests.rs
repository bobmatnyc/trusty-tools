//! `tm sm serve --stdio` stops that are not a signal (#10464).
//!
//! Why: the EOF stop logged no late total, and a setup error returned after
//! the start had taken the spool's events into the buffer, losing them.
//! What: each test starts the real inject producer on a scratch data dir with
//! the bus absent, through `start_with_injects_at` (no process-global env),
//! and drives `run_sm_stdio_with`.
//! Test: this file.

use std::path::{Path, PathBuf};

use chrono::Utc;

use super::*;
use crate::daemon::action_events::{StartAt, inject::start_with_injects_at, spool};
use crate::daemon::state::DaemonState;
use crate::session_manager::inject_record::{Delivery, InjectRecorder};

/// The start on `data_dir`, with the bus socket absent so nothing is sent.
fn at(data_dir: &Path) -> StartAt {
    let sock = data_dir.join("absent.sock");
    StartAt {
        allowed: true,
        socket: Box::new(move || Ok(sock)),
        data_dir: Ok(data_dir.to_path_buf()),
    }
}

/// A state started on `data_dir` with the inject recorder installed.
fn started(data_dir: &Path) -> DaemonState {
    let state = DaemonState::new();
    assert!(start_with_injects_at(&state, at(data_dir)), "not installed");
    state
}

fn deliver(state: &DaemonState, n: usize) {
    let publisher = state.action_events().publisher().expect("attached");
    for i in 0..n {
        publisher.delivered(&Delivery {
            at: Utc::now(),
            tmux_name: "tm-proj-01",
            claude_session_id: None,
            text: &format!("inject {i}"),
        });
    }
}

/// The number of events in the spool under `data_dir`, read without claiming.
fn spooled(data_dir: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(spool::spool_dir(data_dir)) else {
        return 0;
    };
    entries
        .map(|e| e.expect("entry").path())
        .filter(|p: &PathBuf| p.extension().is_some_and(|x| x == "ndjson"))
        .map(|p| std::fs::read_to_string(p).expect("read").lines().count())
        .sum()
}

/// An EOF serve that returns at once.
async fn eof(state: &DaemonState) -> signal::Ended<anyhow::Result<()>> {
    run_sm_stdio_with(
        state,
        async { Ok(()) },
        |()| async { Ok(()) },
        std::future::pending::<()>(),
    )
    .await
}

/// The EOF stop logs the late total once. Injects delivered after the
/// settle are late; the power-of-two lines alone stop at 4.
#[tokio::test]
async fn sm_stdio_eof_logs_the_final_late_total_10464() {
    use tracing_subscriber::layer::SubscriberExt;
    let dir = tempfile::tempdir().expect("tempdir");
    let state = started(dir.path());
    state.action_events().shutdown().await;
    deliver(&state, 5);
    crate::test_support::enable_event_capture();
    let buffer = trusty_common::log_buffer::LogBuffer::new(64);
    let _capture = tracing::subscriber::set_default(tracing_subscriber::registry().with(
        trusty_common::log_buffer::LogBufferLayer::new(buffer.clone()),
    ));

    let ended = eof(&state).await;

    assert!(matches!(ended, signal::Ended::Served(Ok(()))), "{ended:?}");
    let lines = buffer.tail(64);
    let totals: Vec<_> = lines
        .iter()
        .filter(|l| l.contains("lost in total"))
        .collect();
    assert_eq!(totals.len(), 1, "{lines:?}");
    assert!(totals[0].contains("late_lost=5"), "{totals:?}");
}

/// A setup error after the start replayed the spool settles those events
/// back to the spool before it returns; none is lost.
#[tokio::test]
async fn sm_stdio_setup_error_still_settles_replayed_events_10464() {
    let dir = tempfile::tempdir().expect("tempdir");
    let first = started(dir.path());
    deliver(&first, 3);
    assert_eq!(first.action_events().shutdown().await.spooled, 3);
    let state = started(dir.path());
    let publisher = state.action_events().publisher().expect("attached");
    assert_eq!(publisher.buffered_len(), 3, "not replayed");
    assert_eq!(spooled(dir.path()), 0, "the start left the spool");

    let ended = run_sm_stdio_with(
        &state,
        async { Err::<(), _>(anyhow::anyhow!("setup failed")) },
        |()| async { Ok(()) },
        std::future::pending::<()>(),
    )
    .await;

    assert!(matches!(ended, signal::Ended::Served(Err(_))), "{ended:?}");
    assert_eq!(spooled(dir.path()), 3, "the replayed events were lost");
    let settled = state.action_events().shutdown().await;
    assert_eq!((settled.spooled, settled.lost), (3, 0), "{settled:?}");
}

/// Regression guard: `sm serve --stdio` spools at an EOF stop and the next
/// start replays the spool once. This passed before #10464; it fails if
/// `start_with_injects` starts without a spool.
#[tokio::test]
async fn sm_stdio_stop_spools_and_next_start_replays_10464() {
    let dir = tempfile::tempdir().expect("tempdir");
    let state = started(dir.path());
    deliver(&state, 2);

    let ended = eof(&state).await;

    assert!(matches!(ended, signal::Ended::Served(Ok(()))), "{ended:?}");
    assert_eq!(spooled(dir.path()), 2, "the stop did not spool");
    let next = started(dir.path());
    let publisher = next.action_events().publisher().expect("attached");
    assert_eq!(publisher.buffered_len(), 2, "not replayed");
    assert_eq!(spooled(dir.path()), 0, "the spool is still there");
    let again = started(dir.path());
    let publisher = again.action_events().publisher().expect("attached");
    assert_eq!(publisher.buffered_len(), 0, "replayed twice");
}
