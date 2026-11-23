//! `StopSignals` registers SIGHUP with SIGTERM and SIGINT (#10468).
//!
//! Test: this file.

use std::time::Duration;

use super::*;

/// The installed set includes SIGHUP when the process did not inherit it
/// ignored.
#[tokio::test]
async fn install_registers_sighup_with_term_and_int_10468() {
    let stop = StopSignals::install_with(tokio::signal::unix::signal, false);
    assert_eq!(stop.names(), ["SIGTERM", "SIGINT", "SIGHUP"]);
}

/// `nohup` leaves SIGHUP ignored; installing must not override that.
#[tokio::test]
async fn an_inherited_ignored_sighup_is_left_ignored_10468() {
    let stop = StopSignals::install_with(tokio::signal::unix::signal, true);
    assert_eq!(stop.names(), ["SIGTERM", "SIGINT"]);
}

/// The Fail-Open Check: every registration fails, so nothing is registered
/// and `recv` never resolves on its own.
#[tokio::test]
async fn a_failed_registration_is_skipped_and_never_stops_10468() {
    let stop = StopSignals::install_with(|_| Err(std::io::Error::other("refused")), false);
    assert!(stop.names().is_empty());
    let ended = tokio::time::timeout(Duration::from_millis(200), stop.recv()).await;
    assert!(ended.is_err(), "recv resolved with no signal registered");
}
