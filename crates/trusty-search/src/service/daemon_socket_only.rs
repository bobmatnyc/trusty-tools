//! How the daemon, which serves its socket only (#9214), waits for its stop.
//!
//! Why: split out of `daemon.rs`, which sits near the 500-SLOC cap, so the
//! wait is one function a test can drive with both of its inputs ready.
//! What: [`await_socket_only_stop`].
//! Test: `a_socket_only_stop_returns_ok_when_the_serve_loop_also_ended`.

use super::DaemonError;
use tokio::task::{JoinError, JoinHandle};
use tokio_util::sync::CancellationToken;

/// Wait until the daemon is told to stop, or until its RPC serve loop ends.
///
/// Why (#9214): the socket is the daemon's only door, so a serve
/// loop that ends on its own leaves a daemon that answers nothing — that is an
/// error. But a normal stop cancels the drain and ALSO ends the serve loop, so
/// both futures can be ready at once. An unbiased `select!` then picked the
/// serve-loop arm about half the time and turned a clean stop into an error,
/// which exits non-zero instead of through #1746's `exit(0)`.
/// What: polls the drain first (`biased`), and treats a serve-loop exit as a
/// clean stop whenever the drain is already cancelled. Returns the result and,
/// when the serve-loop arm ran, its join result (the handle cannot be awaited
/// twice).
/// Test: `a_socket_only_stop_returns_ok_when_the_serve_loop_also_ended`.
pub(super) async fn await_socket_only_stop(
    drain: &CancellationToken,
    rpc_task: &mut JoinHandle<()>,
) -> (Result<(), DaemonError>, Option<Result<(), JoinError>>) {
    tokio::select! {
        biased;
        _ = drain.cancelled() => (Ok(()), None),
        joined = rpc_task => {
            let result = if drain.is_cancelled() {
                Ok(())
            } else {
                Err(DaemonError::Server("the rpc socket stopped serving".to_string()))
            };
            (result, Some(joined))
        }
    }
}
