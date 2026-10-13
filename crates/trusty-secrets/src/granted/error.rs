//! The tier-3 client's errors (#9071).
//!
//! Why: every failure of a granted resolve must reach the caller as a typed
//! error. None may read as an empty value or a default, because a caller
//! that sees `Ok` injects what it got.
//! What: [`GrantedError`], one variant per failure branch, and [`Refusal`],
//! the server's JSON-RPC error. No variant carries a token, a value or a
//! request field; a decode failure drops serde's message, which can quote
//! its input.
//! Test: `granted_client_debug_and_errors_never_show_token_or_value`.

use std::fmt;
use std::io;
use std::path::PathBuf;

/// Why a granted resolve produced no value.
///
/// Why: the caller of a tier-3 resolve must tell "no grant" from "server
/// gone" from "refused", and must never mistake any of them for a value.
/// What: the token branches (`NoGrant`, `TokenUnreadable`, `InvalidToken`,
/// `Rearm`), the dial branches (`HomeUnavailable`, `UntrustedSocket`,
/// `Unavailable`, `Transport`, `Timeout`) and the answer branches
/// (`Refused`, `EmptyResponse`, `Decode`, `KeyMismatch`).
/// Test: `granted_client_debug_and_errors_never_show_token_or_value`,
/// `granted_client_refusal_is_an_error_with_the_wire_kind`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum GrantedError {
    /// `TRUSTY_SECRETS_TOKEN_FD` is unset, not a decimal fd number, or names
    /// stdin, stdout or stderr.
    #[error("no exec grant: TRUSTY_SECRETS_TOKEN_FD is unset or not an fd")]
    NoGrant,
    /// The fd is not open, not a pipe, or holds no token yet.
    #[error("the grant token could not be read")]
    TokenUnreadable(#[source] io::Error),
    /// The fd held something other than 64 lowercase hex digits.
    #[error("the grant token is malformed")]
    InvalidToken,
    /// The token could not be put back on its fd for child processes.
    #[error("the grant token could not be passed on to child processes")]
    Rearm(#[source] io::Error),
    /// No home directory is known, or it is empty, so the socket path is
    /// unknown.
    #[error("home directory is unavailable")]
    HomeUnavailable,
    /// The socket or its directory failed the ownership and mode check.
    #[error("refusing socket {path}: {reason}")]
    UntrustedSocket {
        /// The socket path.
        path: PathBuf,
        /// Which check failed; fixed text.
        reason: &'static str,
    },
    /// Nothing serves the socket. The client never starts a server.
    #[error("trusty-secrets is not serving {path}")]
    Unavailable {
        /// The socket path.
        path: PathBuf,
        /// The stat or connect error.
        #[source]
        source: io::Error,
    },
    /// The request or the answer did not cross the socket.
    #[error("the request did not cross the socket")]
    Transport(#[source] io::Error),
    /// The server did not answer within the resolve wait.
    #[error("trusty-secrets did not answer in time")]
    Timeout,
    /// The server answered with a JSON-RPC error.
    #[error("{0}")]
    Refused(Refusal),
    /// The server answered with neither a result nor an error.
    #[error("the server answered without a result")]
    EmptyResponse,
    /// The answer was not JSON, named another request, or did not decode.
    #[error("the answer did not decode")]
    Decode,
    /// The answer named a key other than the one requested.
    #[error("the answer named another key")]
    KeyMismatch,
}

impl GrantedError {
    /// The same error again, for a later caller of the process-wide client.
    ///
    /// What: an I/O source is rebuilt from its kind and text, since
    /// `io::Error` is not `Clone`.
    pub(crate) fn replay(&self) -> Self {
        let copy = |e: &io::Error| io::Error::new(e.kind(), e.to_string());
        match self {
            Self::NoGrant => Self::NoGrant,
            Self::TokenUnreadable(e) => Self::TokenUnreadable(copy(e)),
            Self::InvalidToken => Self::InvalidToken,
            Self::Rearm(e) => Self::Rearm(copy(e)),
            Self::HomeUnavailable => Self::HomeUnavailable,
            Self::UntrustedSocket { path, reason } => Self::UntrustedSocket {
                path: path.clone(),
                reason,
            },
            Self::Unavailable { path, source } => Self::Unavailable {
                path: path.clone(),
                source: copy(source),
            },
            Self::Transport(e) => Self::Transport(copy(e)),
            Self::Timeout => Self::Timeout,
            Self::Refused(refusal) => Self::Refused(refusal.clone()),
            Self::EmptyResponse => Self::EmptyResponse,
            Self::Decode => Self::Decode,
            Self::KeyMismatch => Self::KeyMismatch,
        }
    }
}

/// A JSON-RPC error the server answered with.
///
/// What: the wire code, the server's fixed message, and the `data.kind`
/// string (`grant_refused` for every grant refusal); `kind` is `None` when
/// the error carries none, e.g. the router's own errors. `Display` is
/// `[<code>] <message>`.
/// Test: `granted_client_refusal_is_an_error_with_the_wire_kind`,
/// `granted_client_server_error_is_a_refusal`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Refusal {
    /// The JSON-RPC error code, e.g. `-32082` for `grant_refused`.
    pub code: i64,
    /// The server's fixed error text.
    pub message: String,
    /// `error.data.kind`, when present.
    pub kind: Option<String>,
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {}", self.code, self.message)
    }
}
