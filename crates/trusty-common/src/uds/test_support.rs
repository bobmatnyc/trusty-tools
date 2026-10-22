//! Hardened socket binds for test servers (#7215).
//!
//! Why: [`super::connect_hardened`] refuses a socket whose directory is not
//! `0700` or whose mode is not `0600`. A test stub bound with a bare
//! `UnixListener::bind` in a `tempfile` directory fails that check, so each
//! consumer's tests carried its own `set_permissions` pair to get past it.
//! What: [`bind_hardened_stub`] binds a stub socket through the production
//! [`super::bind_hardened`] path, so a stub is hardened exactly as a daemon is.
//! A test that must bind at a path the code under test chose calls
//! [`super::bind_hardened`] directly. Behind the `uds-test-support` feature so
//! no production build compiles it.
//! Test: `bind_hardened_stub_hardens_a_umask_wide_tempdir_for_connect_hardened`,
//! `bind_hardened_stub_reports_a_symlinked_directory_rather_than_binding`
//! (`uds/tests.rs`).

use std::path::{Path, PathBuf};

use tokio::net::UnixListener;

use super::{UdsSecurityError, bind_hardened};

/// The file name [`bind_hardened_stub`] binds inside the directory it is given.
pub const STUB_SOCKET_NAME: &str = "stub.sock";

/// Bind a stub listener at `dir/`[`STUB_SOCKET_NAME`], with `dir` at `0700`
/// and the socket at `0600`.
///
/// Why: a test stub must be one [`super::connect_hardened`] accepts, and
/// getting there by hand took a chmod pair per test.
/// What: [`super::bind_hardened`] on `dir.join(STUB_SOCKET_NAME)`, which
/// narrows an existing `dir` owned by this uid to `0700` and publishes the
/// socket only once it is `0600`. Returns the listener and the socket path.
/// Must run inside a Tokio runtime, as `UnixListener::bind` does.
///
/// # Errors
///
/// Any [`UdsSecurityError`] from [`super::bind_hardened`] — for instance a
/// symlinked `dir`, or a socket already at the path.
///
/// Test: `bind_hardened_stub_hardens_a_umask_wide_tempdir_for_connect_hardened`,
/// `bind_hardened_stub_reports_a_symlinked_directory_rather_than_binding`.
pub fn bind_hardened_stub(dir: &Path) -> Result<(UnixListener, PathBuf), UdsSecurityError> {
    let socket = dir.join(STUB_SOCKET_NAME);
    let listener = bind_hardened(&socket)?;
    Ok((listener, socket))
}
