//! Tests for the search TUI event loop's private poll glue (#9214).
//!
//! Why: `poll_daemon` is private to `event_loop`, so the sibling `tests.rs`
//! cannot reach it.
//! What: an unanswered `logs_tail` leaves the log watermark state alone.
//! Test: `cargo test -p trusty-common --features monitor-tui -- search_tui::event_loop::tests`.

use super::*;

/// #9214: a failed `logs_tail` is not "no new lines". On the first poll and on
/// a later one, the watermark, the first-poll flag and the log stay unchanged.
#[tokio::test]
async fn poll_daemon_keeps_the_log_watermark_when_logs_tail_fails() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("absent.sock");
    let mut client = SearchClient::new(&socket);

    let mut state = SearchTuiState::new(socket.display().to_string());
    poll_daemon(&mut state, &mut client).await;
    assert!(!state.daemon_status.is_online(), "no daemon answers");
    assert!(
        state.log_first_poll,
        "a failed tail must not end the first poll"
    );
    assert_eq!(state.log_watermark, None);
    assert!(state.log.is_empty());

    state.log_first_poll = false;
    state.log_watermark = Some("line 2".into());
    state.log.push_raw("line 1");
    let before: Vec<String> = state.log.iter().cloned().collect();
    poll_daemon(&mut state, &mut client).await;
    assert!(!state.log_first_poll);
    assert_eq!(state.log_watermark.as_deref(), Some("line 2"));
    assert_eq!(state.log.iter().cloned().collect::<Vec<_>>(), before);
}
