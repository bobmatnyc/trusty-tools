//! Socket-backed liveness probing: is anything serving this path, right now?
//!
//! Why (#5085): `try_wait()` answers "has this child been REAPED", which is not
//! the same question as "is it SERVING". A SIGKILLed daemon closes its listener
//! during `do_exit` and only becomes reapable once the kernel finishes tearing
//! it down — measured at up to 32.6 µs, with the socket dying first in 35 of 40
//! rounds. For that window a dead child reads alive and the caller is handed a
//! socket path nothing listens on, with nothing anywhere reporting a problem.
//! That was the fail-open PR #5119 closed, and the promotion has to carry the
//! probe or it reintroduces it at the shared layer for every future service.
//!
//! What: [`SocketVerdict`] and the two probes over it. The three-state shape is
//! the load-bearing part — see the type's docs for why a timeout must never be
//! grounds for eviction.
//!
//! Moved up from `supervisor/probe.rs` in #5182 without changing a line of its
//! logic: `uds::bind_singleton_hardened` has to answer the same question before
//! it takes over a socket file, and it is not behind the `uds-supervisor`
//! feature. Two copies of this classification would be two places to get the
//! ECONNREFUSED case wrong. `supervisor` re-exports these, so its callers are
//! unaffected; `wait_for_socket` stays there because it is keyed to
//! `ServiceTimeouts`.
//!
//! Test: `probe_verdict_reports_not_serving_for_a_missing_socket`,
//! `probe_verdict_reports_not_serving_for_a_stale_socket_file`,
//! `probe_verdict_reports_serving_for_a_bound_socket`.

use std::path::Path;
use std::time::Duration;

use tokio::net::UnixStream;

/// What a connect attempt says about whether anything is serving a socket.
///
/// Why (#5085): the spawn path only ever needed "did it answer, yes or no", but
/// eviction needs a third answer. Evicting a child because one connect did not
/// complete turns a load spike into a respawn storm — the child is killed, a
/// replacement is spawned into the same contention, and the cycle repeats
/// (#3631 is the record of a give-up rule that never fired; this is its
/// mirror-image). Separating "I could not tell" from a definite answer keeps
/// eviction off the ambiguous cases.
///
/// **What ECONNREFUSED does NOT tell you, and why it is the design contract
/// here rather than a footnote.** ECONNREFUSED is not exclusively "no
/// listener". On macOS/BSD a bound listener whose accept queue is full answers
/// connects with ECONNREFUSED too (`unp_connect` rejects once
/// `so_qlen >= so_qlimit`) — measured directly: `listen(1)`, never accept, six
/// non-blocking connects, connect 0 OK and connects 1–5 all ECONNREFUSED.
/// Linux instead queues to the backlog and only then refuses. So on macOS a
/// saturated backlog is indistinguishable from a dead one, and this
/// classification alone does not make the difference safe.
///
/// What makes it safe is a property of the supervised service, which is
/// therefore a REQUIREMENT this supervisor places on anything it supervises:
/// **accept promptly and off the request path.** `trusty-bm25-daemon` satisfies
/// it — tokio's default backlog is 1024 and its accept loop spawns a task per
/// connection rather than serving inline, so the queue does not saturate. A
/// service that accepts inline, or sets a small backlog, breaks the assumption
/// and will be evicted under load. Check that before putting a new service
/// behind this supervisor.
///
/// What: `NotServing` covers exactly ENOENT and ECONNREFUSED. A timeout or any
/// other errno (EPERM, and Linux's EAGAIN) is `Inconclusive` and leaves the
/// child alone.
///
/// `#[non_exhaustive]`: a fourth state (separating "refused" from "absent" for
/// metrics) is plausible, and external crates match this with a wildcard arm.
///
/// Test: `probe_verdict_reports_not_serving_for_a_missing_socket`,
/// `probe_verdict_reports_not_serving_for_a_stale_socket_file`,
/// `probe_verdict_reports_serving_for_a_bound_socket`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SocketVerdict {
    /// A connection was accepted — something is serving this path.
    Serving,
    /// The kernel proved no listener is bound (ENOENT / ECONNREFUSED).
    NotServing,
    /// The probe could not settle the question; assume the child is fine.
    Inconclusive,
}

/// Quick non-blocking probe — opens a `UnixStream`, immediately closes it.
///
/// Why: one answer to "is something listening on this socket right now?"
/// without depending on any service's wire protocol and without spending more
/// than a few ms. Deliberately a bare `UnixStream::connect` rather than
/// [`super::connect_hardened`]: the classification keys off the raw errno,
/// and a permission failure would arrive as a `UdsSecurityError` that carries
/// no errno to classify. Permission verification happens where it decides
/// something — the adoption path — not here.
/// What: attempts the connect with `timeout` and classifies the outcome per
/// [`SocketVerdict`].
/// Test: the three `probe_verdict_*` tests.
pub async fn probe_socket_verdict(path: &Path, timeout: Duration) -> SocketVerdict {
    match tokio::time::timeout(timeout, UnixStream::connect(path)).await {
        Ok(Ok(_)) => SocketVerdict::Serving,
        Ok(Err(e)) => match e.kind() {
            std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused => {
                SocketVerdict::NotServing
            }
            _ => SocketVerdict::Inconclusive,
        },
        Err(_elapsed) => SocketVerdict::Inconclusive,
    }
}

/// Whether a socket is definitely accepting connections right now.
///
/// Why: the spawn and adoption paths only care about the affirmative case —
/// "may I skip the spawn?" — for which anything short of a completed connect
/// must read as no.
/// What: true iff [`probe_socket_verdict`] returns [`SocketVerdict::Serving`].
/// Test: `probe_verdict_reports_not_serving_for_a_missing_socket`,
/// `probe_verdict_reports_serving_for_a_bound_socket` — both assert this
/// function alongside the verdict it wraps.
pub async fn socket_is_serving(path: &Path, timeout: Duration) -> bool {
    probe_socket_verdict(path, timeout).await == SocketVerdict::Serving
}
