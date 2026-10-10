//! How long the server lets a request run, and how long a client waits for
//! its reply (#7524 P2-M1).
//!
//! Why: the client waited 30 s, while one vendor CLI call may take 60 s and
//! a 1Password set is two of them, a Keeper set about ten. The client gave
//! up with a transport timeout, and the server then committed the write.
//! The two numbers must come from one table, so the server's deadline is
//! always shorter than the client's wait for the same method.
//! What: [`request_deadline`] — [`WRITE_DEADLINE`] for `set`, `delete` and
//! `copy`, which reach vendor CLIs, and [`READ_DEADLINE`] for every other
//! method. [`client_wait`] is that deadline plus [`CLIENT_MARGIN`], which
//! covers the connect, the index publish and the audit record that follow
//! the last CLI call. The router sets the deadline on the thread a body runs
//! on (`store::deadline`); the CLI runner refuses or stops a call past it.
//! #9572: the router stops waiting for the body [`BODY_GRACE`] after it.
//! Test: `client_wait_exceeds_the_server_deadline_for_every_method`,
//! `server_request_past_its_deadline_is_a_definite_error_and_commits_nothing`.

use std::time::Duration;

use crate::api::methods::method;

/// The deadline for `set`, `delete` and `copy`: room for one 60 s CLI call
/// that waits on a desktop unlock prompt, plus the calls around it.
pub(crate) const WRITE_DEADLINE: Duration = Duration::from_secs(120);

/// The deadline for every other method; none of them runs a vendor CLI.
pub(crate) const READ_DEADLINE: Duration = Duration::from_secs(15);

/// How much longer a client waits than the server's deadline.
pub(crate) const CLIENT_MARGIN: Duration = Duration::from_secs(15);

/// How long past the deadline the router still waits for a body (#9572).
///
/// Why: a body that honours the deadline answers just after it: the CLI
/// runner kills the CLI at the deadline, and `copy` then names the keys it
/// did not finish. A router that stopped waiting at the deadline itself
/// would replace that answer with a bare `deadline_exceeded`.
/// What: two seconds, well inside [`CLIENT_MARGIN`], so the client still
/// receives the router's answer for a body that never returns.
/// Test: `server_copy_past_its_deadline_starts_no_further_key`,
/// `client_wait_exceeds_the_server_deadline_for_every_method`.
pub(crate) const BODY_GRACE: Duration = Duration::from_secs(2);

/// The whole-operation deadline the server gives one request of `name`.
pub(crate) fn request_deadline(name: &str) -> Duration {
    match name {
        method::SET | method::DELETE | method::COPY => WRITE_DEADLINE,
        _ => READ_DEADLINE,
    }
}

/// How long a client waits for the reply to one request of `name`.
pub(crate) fn client_wait(name: &str) -> Duration {
    request_deadline(name) + CLIENT_MARGIN
}
