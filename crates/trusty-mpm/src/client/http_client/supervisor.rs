//! The Architect registration call for [`DaemonClient`] (#8942).
//!
//! Why: `tm fleet init` must tell "no daemon to register with" (a warning;
//! init still succeeds) apart from "the daemon refused" (a failed step), so
//! the answer is typed instead of an `anyhow` string.
//! What: [`DaemonClient::register_supervisor`] posts the registration and
//! returns the report or a [`RegisterCallError`].
//! Test: `a_registration_call_to_an_absent_socket_is_unreachable`.

use super::DaemonClient;
use crate::session_manager::{RegistrationReport, SupervisorRegistration};

/// Why a registration call produced no report.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RegisterCallError {
    /// No daemon accepted the connection.
    #[error("the daemon is unreachable: {0}")]
    Unreachable(String),
    /// The daemon answered with a non-success status and this reason.
    #[error("the daemon refused the registration ({status}): {reason}")]
    Refused {
        /// The HTTP status, or its projection from the socket error code.
        status: u16,
        /// The daemon's refusal text.
        reason: String,
    },
    /// The exchange failed after a connection existed, or the answer did not
    /// parse (including a daemon that predates the route).
    #[error("the registration call failed: {0}")]
    Failed(String),
}

impl DaemonClient {
    /// `POST /api/v1/sessions/managed/supervisor` (#8942); see the module doc.
    ///
    /// Test: `a_registration_call_to_an_absent_socket_is_unreachable`.
    pub async fn register_supervisor(
        &self,
        reg: &SupervisorRegistration,
    ) -> Result<RegistrationReport, RegisterCallError> {
        let resp = match self
            .post("/api/v1/sessions/managed/supervisor")
            .json(reg)
            .send()
            .await
        {
            Ok(resp) => resp,
            Err(e) if e.is_connect() => return Err(RegisterCallError::Unreachable(e.to_string())),
            Err(e) => return Err(RegisterCallError::Failed(format!("{e:#}"))),
        };
        let status = resp.status();
        if !status.is_success() {
            let reason = resp.text().await.unwrap_or_default();
            return Err(RegisterCallError::Refused {
                status: status.as_u16(),
                reason,
            });
        }
        resp.json()
            .await
            .map_err(|e| RegisterCallError::Failed(format!("unreadable answer: {e:#}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An absent socket is `Unreachable`, never `Failed` or `Refused`: that is
    /// the one answer `tm fleet init` treats as a warning.
    #[tokio::test]
    async fn a_registration_call_to_an_absent_socket_is_unreachable() {
        let dir = crate::test_support::hermetic_temp_dir();
        let client = DaemonClient::over_socket(dir.path().join("absent.sock"));
        let reg = SupervisorRegistration {
            dir: dir.path().to_path_buf(),
            session: "tm-arch".into(),
            poll_session: None,
            collector_session: None,
        };
        let err = client
            .register_supervisor(&reg)
            .await
            .expect_err("no daemon");
        assert!(matches!(err, RegisterCallError::Unreachable(_)), "{err:?}");
    }
}
