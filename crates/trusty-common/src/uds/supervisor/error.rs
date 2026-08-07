//! Failures [`super::UdsServiceSupervisor`] reports to its caller (#5089).
//!
//! Why: `Bm25Supervisor` returned `anyhow::Result` because it lived in a
//! binary-adjacent crate. Promoted into `trusty-common` the same failures cross
//! a library boundary, where this workspace's convention is a structured
//! `thiserror` enum — and where a caller deciding whether to retry, degrade, or
//! surface a 5xx needs to tell "the binary is missing" from "it bound too
//! slowly" from "something untrusted is already on that path".
//! What: one variant per way `ensure_running` can decline to hand back a socket.
//! Test: exercised through `tests.rs`'s spawn-failure and untrusted-socket
//! cases; the `Display` strings are the operator-facing contract.

use std::path::PathBuf;
use std::time::Duration;

/// Why a supervised service could not be made available.
///
/// `#[non_exhaustive]`: on an ENUM the attribute constrains *matching* — an
/// external crate needs a wildcard arm (E0004) — which is the intended cost of
/// keeping variant additions non-breaking once `trusty-common` publishes.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SupervisorError {
    /// The caller's spawn-spec closure failed — typically because the service's
    /// binary could not be located.
    #[error("resolve the spawn command for {service} instance {key}: {source}")]
    SpawnSpec {
        /// Service label.
        service: String,
        /// Instance key.
        key: String,
        /// Underlying failure.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },

    /// A directory the spec asked for could not be created.
    #[error("create directory {path} for {service} instance {key}: {source}")]
    CreateDir {
        /// Service label.
        service: String,
        /// Instance key.
        key: String,
        /// Directory that could not be created.
        path: PathBuf,
        /// Underlying OS error.
        #[source]
        source: std::io::Error,
    },

    /// `Command::spawn` failed.
    #[error("spawn {program} for {service} instance {key}: {source}")]
    Spawn {
        /// Service label.
        service: String,
        /// Instance key.
        key: String,
        /// Binary that could not be executed.
        program: PathBuf,
        /// Underlying OS error.
        #[source]
        source: std::io::Error,
    },

    /// The child started but never bound its socket inside the service's
    /// `spawn_probe` budget.
    #[error(
        "{service} instance {key} did not bind {socket} within {budget:?} — \
         if this service loads a model at startup, its ServiceTimeouts::spawn_probe \
         is too small"
    )]
    SpawnTimeout {
        /// Service label.
        service: String,
        /// Instance key.
        key: String,
        /// Socket the child was expected to bind.
        socket: PathBuf,
        /// The budget that elapsed.
        budget: Duration,
    },

    /// Something is serving the socket, but it does not pass the
    /// [`crate::uds::verify_socket_for_connect`] check — so adopting it would
    /// mean trusting a socket whose permissions are not the credential
    /// ADR-0034 §3 says they are.
    #[error("refusing to adopt {socket} for {service} instance {key}: {source}")]
    UntrustedSocket {
        /// Service label.
        service: String,
        /// Instance key.
        key: String,
        /// The socket that was refused.
        socket: PathBuf,
        /// Which property failed. Boxed: `UdsSecurityError` is the largest
        /// payload in this enum, and unboxed it pushes `SupervisorError` past
        /// clippy's `result_large_err` threshold for every fallible non-async
        /// caller.
        #[source]
        source: Box<crate::uds::UdsSecurityError>,
    },

    /// A runtime-derived [`crate::uds::ServiceTimeouts`] pair fails the one
    /// relation the type enforces. Only [`crate::uds::ServiceTimeouts::try_new`]
    /// produces this — the `const fn` constructor turns the same condition into
    /// a build error.
    #[error(
        "SIGTERM patience {sigterm_patience:?} must strictly exceed the supervised \
         child's shutdown-flush budget {shutdown_flush:?}, or the SIGKILL lands \
         mid-flush and discards acked writes"
    )]
    InvalidTimeouts {
        /// The patience that was too short.
        sigterm_patience: Duration,
        /// The child's declared flush budget.
        shutdown_flush: Duration,
    },

    /// The socket path does not fit the kernel's address buffer, so no child
    /// could ever bind it.
    #[error("socket path for {service} instance {key} is unusable: {source}")]
    SocketPath {
        /// Service label.
        service: String,
        /// Instance key.
        key: String,
        /// The underlying budget failure. Boxed for the same reason as
        /// [`SupervisorError::UntrustedSocket`]'s.
        #[source]
        source: Box<crate::uds::UdsSecurityError>,
    },
}
