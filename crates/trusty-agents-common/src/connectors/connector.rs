//! [`WorkstreamConnector`] — the tool-agnostic session-control trait (DOC-44
//! §2.1/§5.2, issue #3007, twin Phase 1).
//!
//! Why: DOC-44 (`docs/specs/DOC-44-engineering-lead-twin-orchestration.md`,
//! still unmerged in PR #3006 at the time this trait landed — see this
//! module's crate-level docs) defines a Connector as "a thin, stateless tool
//! that translates unified control commands into tool-specific I/O" — NOT an
//! agent, with no memory or decision-making of its own. Living in
//! `trusty-agents-common` (a dependency leaf both `trusty-mpm` and
//! `trusty-code` already depend on) rather than in either concrete harness
//! crate is what makes a single trait object work across both: neither
//! harness crate can depend on the other, so the shared abstraction has to
//! live one level below both.
//! What: six operations — `create_session`, `list_sessions`,
//! `session_status`, `send_input`, `attach`, `delegate` — matching the
//! issue's locked scope (a superset sketch in DOC-44 §5.2 also lists
//! `detach`/`workstream_status`; those are deferred to a later phase, not
//! part of this ticket). `#[async_trait]` makes the trait dyn-safe
//! (`Arc<dyn WorkstreamConnector>`), matching the `ManagedBackend`
//! (`crates/trusty-mpm/src/client/proxy.rs`) and `AgentRunner`
//! (`crate::runner`) precedent already established in this workspace.
//! Test: `connector::tests` exercises a minimal in-memory mock implementation
//! to prove the trait is object-safe and every method is callable through
//! `Arc<dyn WorkstreamConnector>`. Full backend coverage lives in
//! `trusty-mpm`'s and `trusty-code`'s own connector test suites (see
//! `crates/trusty-mpm/src/connectors/tm_tests.rs` and
//! `crates/trusty-code/tests/connector_e2e.rs`).

use async_trait::async_trait;

use super::error::ConnectorError;
use super::types::{
    AgentSpec, AttachHandle, CreateSessionReq, DelegateHandle, SessionInfo, SessionStatus,
};

/// Tool-agnostic session-control surface — one implementation per harness.
///
/// Why: see module docs. The lead agent (DOC-44 Layer 2, not built by this
/// ticket) will hold `Arc<dyn WorkstreamConnector>` per tool and call these
/// methods without knowing whether it is talking to tm or tcode.
/// What: every method returns `Result<_, ConnectorError>` — no `unwrap`, no
/// panics on a backend-reported failure. `delegate` is deliberately NOT
/// universal: tcode has no delegate surface at all (DOC-44 locked decision
/// 3) and its implementation returns
/// [`ConnectorError::NotSupported`] rather than a runtime-only failure,
/// so a caller can distinguish "this backend never supports this" from "this
/// particular call failed."
/// Test: `connector::tests::mock_connector_is_dyn_compatible`.
#[async_trait]
pub trait WorkstreamConnector: Send + Sync {
    /// Create a new session.
    ///
    /// Why: the entry point for bringing a new workstream into existence in
    /// this backend.
    /// What: `req.backend` must carry the [`super::BackendParams`] variant
    /// this backend implements — a mismatched variant is
    /// [`ConnectorError::InvalidRequest`], never a panic (see
    /// [`super::BackendParams`]'s docs).
    async fn create_session(&self, req: CreateSessionReq) -> Result<SessionInfo, ConnectorError>;

    /// List every session this backend currently owns.
    ///
    /// Why: the lead's fan-out poll (DOC-44 §2.2) reads this per tool to
    /// build its workstream ledger.
    /// What: returns every session, regardless of lifecycle state — callers
    /// filter on [`SessionInfo::state`] themselves (see that field's docs on
    /// why it stays a raw backend-native string).
    async fn list_sessions(&self) -> Result<Vec<SessionInfo>, ConnectorError>;

    /// Point lookup for one session's current status.
    ///
    /// Why: cheaper than re-listing when the caller already has an id.
    /// What: [`ConnectorError::NotFound`] for an unknown `session_id`.
    async fn session_status(&self, session_id: &str) -> Result<SessionStatus, ConnectorError>;

    /// Inject text into a session (a prompt, an answer, a command).
    ///
    /// Why: the primary way a caller drives a session forward without
    /// attaching to its interactive surface.
    /// What: [`ConnectorError::NotFound`] for an unknown `session_id`.
    async fn send_input(&self, session_id: &str, input: &str) -> Result<(), ConnectorError>;

    /// Obtain a handle for attaching to a session's interactive surface.
    ///
    /// Why: tm and tcode expose fundamentally different attach mechanisms —
    /// see [`super::AttachHandle`]'s docs for why this returns an enum
    /// rather than a single shape.
    /// What: [`ConnectorError::NotFound`] for an unknown `session_id`.
    async fn attach(&self, session_id: &str) -> Result<AttachHandle, ConnectorError>;

    /// Delegate work to a sub-agent within a session.
    ///
    /// Why: tm's `agent_delegate` gates the request through the caller's
    /// circuit breaker and RECORDS the delegation for audit — it does NOT
    /// execute the sub-agent itself (the tm backend's `delegate`
    /// implementation documents this loudly; see
    /// `crates/trusty-mpm/src/connectors/tm.rs`). tcode has no delegate
    /// surface at all in this phase; its implementation always returns
    /// [`ConnectorError::NotSupported`] (DOC-44 locked decision 3 —
    /// deliberately NOT a new tcode endpoint, that would be scope creep for
    /// this ticket).
    /// What: [`ConnectorError::NotFound`] for an unknown `session_id`;
    /// [`ConnectorError::NotSupported`] for a backend with no delegate
    /// surface.
    async fn delegate(
        &self,
        session_id: &str,
        agent_spec: &AgentSpec,
    ) -> Result<DelegateHandle, ConnectorError>;
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;

    use super::*;

    /// Minimal in-memory mock proving the trait is dyn-safe and every method
    /// is reachable through `Arc<dyn WorkstreamConnector>`.
    ///
    /// Why: the trait's entire purpose is to be stored as
    /// `Arc<dyn WorkstreamConnector>` by a future lead agent (DOC-44 Layer
    /// 2) — a compile-time regression here (e.g. an accidentally
    /// non-object-safe method signature) would only surface downstream,
    /// which is a bad place to discover it.
    /// What: every method returns a canned value or the appropriate error.
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

    #[tokio::test]
    async fn mock_connector_is_dyn_compatible() {
        let connector: Arc<dyn WorkstreamConnector> = Arc::new(MockConnector);

        let req = CreateSessionReq {
            task: "do the thing".into(),
            name_hint: None,
            agent: None,
            backend: super::super::types::BackendParams::Tcode {
                project: std::path::PathBuf::from("/tmp/proj"),
            },
        };
        let info = connector.create_session(req).await.unwrap();
        assert_eq!(info.id, "mock-1");

        let listed = connector.list_sessions().await.unwrap();
        assert_eq!(listed.len(), 1);

        let status = connector.session_status("mock-1").await.unwrap();
        assert_eq!(status.state, "active");

        assert!(connector.session_status("nope").await.is_err());

        connector.send_input("mock-1", "hi").await.unwrap();

        let attach = connector.attach("mock-1").await.unwrap();
        assert!(matches!(attach, AttachHandle::ShellCommand(_)));

        let spec = AgentSpec {
            agent_name: "research".into(),
            task: "find the bug".into(),
            tier: None,
        };
        let err = connector.delegate("mock-1", &spec).await.unwrap_err();
        assert!(matches!(err, ConnectorError::NotSupported(_)));
    }
}
