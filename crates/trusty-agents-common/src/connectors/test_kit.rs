//! [`ConnectorTestKit`] — shared conformance assertions any
//! [`super::WorkstreamConnector`] implementation can run against itself
//! (DOC-44 Phase 1 deliverable "both connectors, trait passing integration
//! tests", issue #3007 test-layer (a)).
//!
//! Why: `trusty-mpm`'s and `trusty-code`'s connector test suites both need
//! to prove the SAME trait contract — "a freshly created session appears in
//! `list_sessions`", "an unknown id is `NotFound`, not a panic or a
//! different error kind" — against two structurally different backends.
//! Writing that assertion logic twice risks the two suites drifting apart
//! (one backend's test tightens a check the other's never gets). Landing it
//! ONCE here, and calling it from both `crates/trusty-mpm/src/connectors/
//! tm_tests.rs` and `crates/trusty-code/tests/connector_e2e.rs`, keeps the
//! contract singly-sourced. This is deliberately NOT behind `#[cfg(test)]`:
//! `trusty-mpm`'s and `trusty-code`'s connector tests live in DIFFERENT
//! crates (an integration test in `trusty-code/tests/*.rs` is its own crate
//! at compile time) and so cannot see `trusty-agents-common`'s `#[cfg(test)]`
//! items — only this crate's normal `[dependencies]` surface, which
//! `[dev-dependencies]` cannot special-case around a *downstream* crate's
//! test build. Shipping a tiny, `assert!`-based helper module in the normal
//! build is the same tradeoff `condition-based-waiting`-style test kits make
//! workspace-wide.
//! What: each function takes `&dyn WorkstreamConnector` (object-safe callers
//! can pass either an `Arc<dyn WorkstreamConnector>` `.as_ref()` or a
//! concrete `&impl WorkstreamConnector`) and panics via `assert!`/
//! `assert_eq!` on a contract violation, exactly like a normal test
//! assertion — callers wrap these in their own `#[tokio::test]` functions.
//! Test: `test_kit::tests` runs every assertion against the same
//! `MockConnector` `connector::tests` uses, proving the kit itself is
//! correct before any real backend depends on it.

use super::connector::WorkstreamConnector;
use super::error::ConnectorError;
use super::types::{AgentSpec, CreateSessionReq};

/// Shared conformance assertions for any [`WorkstreamConnector`] impl.
///
/// Why/What: see module docs.
pub struct ConnectorTestKit;

impl ConnectorTestKit {
    /// Assert the create -> list -> status -> send_input happy path.
    ///
    /// Why: this is the minimum viable lifecycle every backend must support
    /// — a caller that creates a session must be able to find it again and
    /// drive it.
    /// What: creates a session via `req`, asserts it has a non-empty id,
    /// asserts `list_sessions` includes it, asserts `session_status` returns
    /// a matching id, then sends `input` and asserts success. Returns the
    /// created [`super::types::SessionInfo`] so the caller can run further
    /// backend-specific assertions (e.g. tm's attach-cmd shape) against the
    /// same session without creating a second one.
    /// Test: `test_kit::tests::assert_basic_lifecycle_passes_against_mock`.
    pub async fn assert_basic_lifecycle(
        connector: &dyn WorkstreamConnector,
        req: CreateSessionReq,
        input: &str,
    ) -> super::types::SessionInfo {
        let info = connector
            .create_session(req)
            .await
            .expect("create_session must succeed for a well-formed request");
        assert!(
            !info.id.is_empty(),
            "create_session must return a non-empty session id"
        );

        let listed = connector
            .list_sessions()
            .await
            .expect("list_sessions must succeed");
        assert!(
            listed.iter().any(|s| s.id == info.id),
            "list_sessions must include the just-created session {}",
            info.id
        );

        let status = connector
            .session_status(&info.id)
            .await
            .expect("session_status must succeed for a known id");
        assert_eq!(
            status.id, info.id,
            "session_status must echo back the id it was asked about"
        );

        connector
            .send_input(&info.id, input)
            .await
            .expect("send_input must succeed for a known id");

        info
    }

    /// Assert `session_status` on an unknown id is `ConnectorError::NotFound`.
    ///
    /// Why: a backend that returns `Backend`/`Transport` for an unknown id
    /// instead of `NotFound` breaks the lead agent's ability to distinguish
    /// "gone" from "the daemon is unreachable" (DOC-44 §2.2's workstream
    /// status tracking depends on this).
    /// What: calls `session_status(unknown_id)` and asserts the error is
    /// `NotFound`.
    /// Test: `test_kit::tests::assert_status_not_found_passes_against_mock`.
    pub async fn assert_status_not_found_for_unknown_id(
        connector: &dyn WorkstreamConnector,
        unknown_id: &str,
    ) {
        let err = connector
            .session_status(unknown_id)
            .await
            .expect_err("session_status on an unknown id must error");
        assert!(
            err.is_not_found(),
            "expected ConnectorError::NotFound for unknown id {unknown_id}, got {err:?}"
        );
    }

    /// Assert `send_input` on an unknown id is `ConnectorError::NotFound`.
    ///
    /// Why: mirrors [`Self::assert_status_not_found_for_unknown_id`] for the
    /// mutating half of the surface — a backend must not silently swallow an
    /// inject-into-nothing.
    /// What: calls `send_input(unknown_id, ...)` and asserts the error is
    /// `NotFound`.
    /// Test: `test_kit::tests::assert_send_not_found_passes_against_mock`.
    pub async fn assert_send_not_found_for_unknown_id(
        connector: &dyn WorkstreamConnector,
        unknown_id: &str,
    ) {
        let err = connector
            .send_input(unknown_id, "probe")
            .await
            .expect_err("send_input on an unknown id must error");
        assert!(
            err.is_not_found(),
            "expected ConnectorError::NotFound for unknown id {unknown_id}, got {err:?}"
        );
    }

    /// Assert `delegate` on a backend that does not support it returns
    /// `ConnectorError::NotSupported` (never a panic, never a bare
    /// `Backend`/`Transport` error).
    ///
    /// Why: DOC-44 locked decision 3 — tcode's `delegate` must fail closed
    /// with a typed, callable-distinguishable error, not an ad-hoc message.
    /// What: calls `delegate(session_id, spec)` and asserts the error is
    /// `NotSupported`.
    /// Test: `test_kit::tests::assert_delegate_not_supported_passes_against_mock`.
    pub async fn assert_delegate_not_supported(
        connector: &dyn WorkstreamConnector,
        session_id: &str,
        spec: &AgentSpec,
    ) {
        let err = connector
            .delegate(session_id, spec)
            .await
            .expect_err("delegate must error on a backend that does not support it");
        assert!(
            matches!(err, ConnectorError::NotSupported(_)),
            "expected ConnectorError::NotSupported, got {err:?}"
        );
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use async_trait::async_trait;

    use super::super::types::{
        AttachHandle, BackendParams, DelegateHandle, SessionInfo, SessionStatus,
    };
    use super::*;

    /// Reuses the same minimal contract `connector::tests::MockConnector`
    /// exercises, kept as a private duplicate here rather than exported from
    /// `connector` — the mock is test-only scaffolding, not part of the
    /// public API either module should expose.
    struct MockConnector;

    #[async_trait]
    impl WorkstreamConnector for MockConnector {
        async fn create_session(
            &self,
            req: CreateSessionReq,
        ) -> Result<SessionInfo, ConnectorError> {
            Ok(SessionInfo {
                id: "mock-1".into(),
                name: "mock-session".into(),
                state: "active".into(),
                task: Some(req.task),
            })
        }

        async fn list_sessions(&self) -> Result<Vec<SessionInfo>, ConnectorError> {
            Ok(vec![SessionInfo {
                id: "mock-1".into(),
                name: "mock-session".into(),
                state: "active".into(),
                task: None,
            }])
        }

        async fn session_status(&self, session_id: &str) -> Result<SessionStatus, ConnectorError> {
            if session_id == "mock-1" {
                Ok(SessionStatus {
                    id: session_id.to_string(),
                    state: "active".into(),
                    pending_decision: None,
                })
            } else {
                Err(ConnectorError::NotFound(session_id.to_string()))
            }
        }

        async fn send_input(&self, session_id: &str, _input: &str) -> Result<(), ConnectorError> {
            if session_id == "mock-1" {
                Ok(())
            } else {
                Err(ConnectorError::NotFound(session_id.to_string()))
            }
        }

        async fn attach(&self, session_id: &str) -> Result<AttachHandle, ConnectorError> {
            Ok(AttachHandle::ShellCommand(format!(
                "mock attach -t {session_id}"
            )))
        }

        async fn delegate(
            &self,
            _session_id: &str,
            _agent_spec: &AgentSpec,
        ) -> Result<DelegateHandle, ConnectorError> {
            Err(ConnectorError::NotSupported("mock never delegates".into()))
        }
    }

    fn mock_req() -> CreateSessionReq {
        CreateSessionReq {
            task: "do the thing".into(),
            name_hint: None,
            agent: None,
            backend: BackendParams::Tcode {
                project: PathBuf::from("/tmp/proj"),
            },
        }
    }

    #[tokio::test]
    async fn assert_basic_lifecycle_passes_against_mock() {
        let connector = MockConnector;
        let info = ConnectorTestKit::assert_basic_lifecycle(&connector, mock_req(), "hello").await;
        assert_eq!(info.id, "mock-1");
    }

    #[tokio::test]
    async fn assert_status_not_found_passes_against_mock() {
        let connector = MockConnector;
        ConnectorTestKit::assert_status_not_found_for_unknown_id(&connector, "nope").await;
    }

    #[tokio::test]
    async fn assert_send_not_found_passes_against_mock() {
        let connector = MockConnector;
        ConnectorTestKit::assert_send_not_found_for_unknown_id(&connector, "nope").await;
    }

    #[tokio::test]
    async fn assert_delegate_not_supported_passes_against_mock() {
        let connector = MockConnector;
        let spec = AgentSpec {
            agent_name: "research".into(),
            task: "find the bug".into(),
            tier: None,
        };
        ConnectorTestKit::assert_delegate_not_supported(&connector, "mock-1", &spec).await;
    }
}
