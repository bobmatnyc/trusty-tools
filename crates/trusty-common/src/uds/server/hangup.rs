//! Detect a client that hung up while its request is still being handled
//! (#11256).
//!
//! Why not a read returning EOF: [`crate::uds::send_framed_request`] shuts its
//! WRITE half after sending the frame, so the server's read side reaches EOF on
//! every healthy call. Only a full close also shuts the server's write side, so
//! "write closed" is the signal that the answer can no longer be delivered.

use std::time::Duration;

use tokio::io::Interest;
use tokio::net::UnixStream;

/// How often the watcher re-reads the stream's cached readiness.
///
/// The reactor records the peer's close as it happens; the poll only bounds
/// how late the handler learns of it. 50 ms is small against a request worth
/// cancelling and costs one wake-up per interval per read-only request.
const HANGUP_POLL: Duration = Duration::from_millis(50);

/// Resolve once the peer has closed its end of `stream` fully.
///
/// What: loops on the write readiness the reactor holds for the socket and
/// returns `Ok(())` when it reports write-closed (EPOLLHUP on Linux, `EV_EOF`
/// on the kqueue write filter on macOS). A peer that only half-closed keeps
/// the write side open, so it never resolves for a client still waiting.
///
/// # Errors
///
/// The readiness query's own I/O error. The caller treats it as a lost
/// connection, never as success.
///
/// Test: `a_read_only_handler_is_cancelled_when_its_client_hangs_up_11256`,
/// `a_half_closed_client_still_gets_its_read_only_answer_11256`.
pub(super) async fn peer_hung_up(stream: &UnixStream) -> std::io::Result<()> {
    loop {
        if stream.ready(Interest::WRITABLE).await?.is_write_closed() {
            return Ok(());
        }
        tokio::time::sleep(HANGUP_POLL).await;
    }
}
