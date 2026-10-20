//! The tier-3 client: one secret value for a process `tm secrets exec`
//! granted (DOC-74 §15.8, S9, #9071).
//!
//! Why: a program that `tm secrets exec` started holds an exec grant, and
//! needs one call to read a granted value. tm stays the only grant client
//! (ruling 34), and a live grant keeps the server alive, so this client
//! never grants, revokes or starts a server. It links no tokio and no
//! trusty-common, so any program can use it under the `api` feature.
//! What: [`GrantedClient`] reads the grant token from the fd whose number is
//! in [`TOKEN_FD_ENV`] without consuming it, so the command's siblings and
//! descendants read the same token (DOC-74 §10.5), checks the socket's
//! directory and mode, sends
//! one `secrets.resolve` frame and returns the [`SecretValue`]. [`resolve`]
//! does the same through one process-wide client. Every failure is a
//! [`GrantedError`]. Blocking, std only; an async caller wraps the call in
//! `spawn_blocking`. Nothing here logs.
//! Test: `granted_tests.rs` beside this file (fake servers),
//! `granted_client_resolves_a_granted_key_from_a_running_server` (the
//! in-process server), and the ignored
//! `granted_client_resolves_through_the_real_binary` in
//! `tests/granted_client_live.rs`.
//!
//! Governing document: DOC-74 §10.5, §15.8
//! (`docs/specs/DOC-74-secrets-integration.md`).

mod error;
mod token;
mod wire;

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

pub use error::{GrantedError, Refusal};

use crate::api::{SecretKey, SecretValue, TOKEN_FD_ENV};

/// The socket path under the home directory; the server's
/// `settings::SOCKET_SUBPATH`.
///
/// Test: `granted_socket_subpath_matches_server_settings`.
pub const SOCKET_SUBPATH: &str = ".trusty-tools/trusty-secrets/secrets.sock";

/// How long one resolve waits: the server's 120 s deadline plus its 15 s
/// client margin.
///
/// Why: #7524 P2-M1 — a client that gives up first can report a failure for
/// a call that then spends a one-shot grant. No override exists.
/// Test: `granted_resolve_wait_matches_the_server_deadline`.
pub const RESOLVE_WAIT: Duration = Duration::from_secs(135);

/// A handle that resolves keys under this process's exec grant.
///
/// Why: reading the token checks the fd every time, so a client reads it
/// once and holds it for every later resolve of the process.
/// What: the socket path, the token (never printed; `Debug` shows
/// `<redacted>`) and the resolve wait. No public constructor takes a token
/// string, so a caller is never led to put one in argv or env.
/// Test: `granted_client_sends_one_resolve_frame`,
/// `granted_client_debug_and_errors_never_show_token_or_value`.
pub struct GrantedClient {
    socket: PathBuf,
    token: token::Token,
    wait: Duration,
}

impl GrantedClient {
    /// The client for this process's grant: the home directory's socket and the token
    /// on the fd [`TOKEN_FD_ENV`] names.
    ///
    /// Why: `tm secrets exec` hands the token down on an inherited fd and
    /// puts only its number in the environment (Architect ruling Q1).
    /// What: reads the home directory as tm does (`$HOME`, else the user
    /// database), then the fd number, then the token with `pread` at offset
    /// 0. The fd stays open with its flags and offset unchanged, so every
    /// other process holding it (a sibling, a child) reads the same token.
    /// Dials nothing.
    ///
    /// # Errors
    ///
    /// [`GrantedError::HomeUnavailable`], [`GrantedError::NoGrant`],
    /// [`GrantedError::TokenUnreadable`], [`GrantedError::InvalidToken`].
    ///
    /// Test: `granted_env_without_home_is_an_error`,
    /// `granted_client_siblings_both_resolve`,
    /// `granted_client_resolves_through_the_real_binary`.
    pub fn from_env() -> Result<Self, GrantedError> {
        Self::from_env_parts(home_dir(), std::env::var_os(TOKEN_FD_ENV).as_deref())
    }

    /// [`Self::from_env`] on given values, so a test mutates no env.
    fn from_env_parts(home: Option<OsString>, fd: Option<&OsStr>) -> Result<Self, GrantedError> {
        let socket = default_socket(home)?;
        let fd = token::parse_token_fd(fd)?;
        let token = token::read_inherited(fd)?;
        Ok(Self::with_token(socket, token))
    }

    /// The client for the token on `fd`, which this call consumes.
    ///
    /// What: the same checks and read as [`Self::from_env`], on an fd the
    /// caller owns; `fd` closes here. Fd 0-2 is [`GrantedError::NoGrant`].
    ///
    /// # Errors
    ///
    /// As [`Self::from_env`].
    ///
    /// Test: `granted_client_checks_the_token_before_dialing`,
    /// `granted_client_refuses_a_token_fd_of_the_wrong_shape`.
    pub fn from_token_fd(fd: OwnedFd) -> Result<Self, GrantedError> {
        let socket = default_socket(home_dir())?;
        let token = token::read_owned(fd)?;
        Ok(Self::with_token(socket, token))
    }

    fn with_token(socket: PathBuf, token: token::Token) -> Self {
        Self {
            socket,
            token,
            wait: RESOLVE_WAIT,
        }
    }

    /// The same client, dialing `socket` instead of the home directory's.
    #[must_use]
    pub fn with_socket(mut self, socket: impl Into<PathBuf>) -> Self {
        self.socket = socket.into();
        self
    }

    /// A test-only shorter wait.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_wait(mut self, wait: Duration) -> Self {
        self.wait = wait;
        self
    }

    /// The socket this client dials.
    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// Read `key` under this process's grant.
    ///
    /// Why: the one value-returning call of tier 3 (DOC-74 §15.8).
    /// What: checks the socket, sends one `secrets.resolve` frame, reads one
    /// answer within [`RESOLVE_WAIT`], and returns the value only when the
    /// answer names `key`. Never retries and never starts a server.
    ///
    /// # Errors
    ///
    /// Every [`GrantedError`] dial and answer branch.
    ///
    /// Test: `granted_client_sends_one_resolve_frame`,
    /// `granted_client_never_spawns_a_server`,
    /// `granted_client_refusal_is_an_error_with_the_wire_kind`,
    /// `granted_client_times_out_on_a_silent_server`.
    pub fn resolve(&self, key: &SecretKey) -> Result<SecretValue, GrantedError> {
        wire::resolve(&self.socket, self.token.expose(), key, self.wait)
    }
}

impl fmt::Debug for GrantedClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GrantedClient")
            .field("socket", &self.socket)
            .field("token", &"<redacted>")
            .field("wait", &self.wait)
            .finish()
    }
}

/// The home directory, found as tm's grant client finds it (`dirs::home_dir`).
///
/// Why: #9071 — tm granted on the socket under its home directory, so the
/// client looks in the same place: `$HOME`, else the user database.
fn home_dir() -> Option<OsString> {
    std::env::home_dir().map(PathBuf::into_os_string)
}

/// The home directory's socket path; no home, or an empty one, is an error.
fn default_socket(home: Option<OsString>) -> Result<PathBuf, GrantedError> {
    match home {
        Some(home) if !home.is_empty() => Ok(PathBuf::from(home).join(SOCKET_SUBPATH)),
        _ => Err(GrantedError::HomeUnavailable),
    }
}

/// Read `key` under this process's grant, through one process-wide client.
///
/// Why: one client per process checks and reads the token fd once; every
/// call shares the client the first call built.
/// What: builds the client with [`GrantedClient::from_env`] on first use.
/// If that failed, every later call returns the same error and the fd is
/// not read again, so a later call never hides the first cause.
///
/// # Errors
///
/// As [`GrantedClient::from_env`] and [`GrantedClient::resolve`].
///
/// Test: `granted_resolve_replays_the_first_init_error`,
/// `granted_client_resolves_through_the_real_binary`.
pub fn resolve(key: &SecretKey) -> Result<SecretValue, GrantedError> {
    static CLIENT: OnceLock<Result<GrantedClient, GrantedError>> = OnceLock::new();
    resolve_in(&CLIENT, GrantedClient::from_env, key)
}

/// [`resolve`] over a given cell, so a test owns its cell.
fn resolve_in(
    cell: &OnceLock<Result<GrantedClient, GrantedError>>,
    init: impl FnOnce() -> Result<GrantedClient, GrantedError>,
    key: &SecretKey,
) -> Result<SecretValue, GrantedError> {
    match cell.get_or_init(init) {
        Ok(client) => client.resolve(key),
        Err(error) => Err(error.replay()),
    }
}

#[cfg(test)]
#[path = "granted_tests.rs"]
mod tests;
