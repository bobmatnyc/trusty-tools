//! `session.get_readiness` handler (DOC-39 §5.6 Slice D). Split out of
//! `protocol.rs` purely to keep that production file under the crate's
//! 500-SLOC cap — this is a child module of `protocol` (declared via
//! `#[path = ...] mod protocol_readiness;`), so it shares full access to
//! `protocol`'s private `SessionIdParams`/`parse` helpers exactly as if this
//! handler were still defined in that file.
//!
//! Why: `Event::IndexReadiness` (`crate::events`) is emitted exactly once, at
//! task start, from `task::executor`'s background-indexing observer. A UI
//! client that attaches to a session AFTER that one-time probe fired has no
//! way to ask "what is the readiness state right now" — only a stream it may
//! have missed. This is the query half a previous PR (#2861) left unshipped:
//! the event-only side existed, but nothing let a late-attaching client
//! retrieve the state it never streamed.
//! What: [`get_readiness`] forwards to `SessionRegistry::get_readiness`,
//! returning its `ReadinessQuery` (`{"status":"probed", ...snapshot fields}`
//! or `{"status":"never_probed"}`) verbatim as the JSON-RPC result.
//! Test: `tests::*` in this module.

use super::*;

/// `session.get_readiness(session_id) -> ReadinessQuery` (DOC-39 §5.6 Slice D).
///
/// Why: the query counterpart to the fire-once `Event::IndexReadiness` — see
/// module docs.
/// What: `-32007 session_not_found` if `session_id` is unknown; otherwise
/// `SessionRegistry::get_readiness`'s `ReadinessQuery` verbatim — either the
/// cached snapshot (`status: "probed"`) or the typed "nothing recorded yet"
/// result (`status: "never_probed"`), never a bare `null`.
/// Test: `tests::get_readiness_returns_probed_snapshot_after_recording`,
/// `tests::get_readiness_never_probed_session_returns_never_probed`,
/// `tests::get_readiness_unknown_session_maps_to_session_not_found`.
pub(super) async fn get_readiness(
    registry: &SessionRegistry,
    params: Value,
    _ctx: ConnectionContext,
) -> Result<Value, RpcError> {
    let p: SessionIdParams = parse(params, "session.get_readiness")?;
    Ok(json!(registry.get_readiness(&p.session_id)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    fn test_ctx() -> ConnectionContext {
        let (tx, _rx) = mpsc::unbounded_channel();
        ConnectionContext::new(tx)
    }

    /// A session with no recorded probe returns the typed `never_probed`
    /// result, not an error or a bare `null`.
    #[tokio::test]
    async fn get_readiness_never_probed_session_returns_never_probed() {
        let registry = SessionRegistry::new();
        let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);

        let result = get_readiness(&registry, json!({"session_id": session.id}), test_ctx())
            .await
            .unwrap();

        assert_eq!(result["status"], "never_probed");
    }

    /// Once a probe has been recorded, `session.get_readiness` returns it
    /// with `status: "probed"` and the snapshot fields alongside.
    #[tokio::test]
    async fn get_readiness_returns_probed_snapshot_after_recording() {
        let registry = SessionRegistry::new();
        let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
        registry
            .record_index_readiness(&session.id, None, "no daemon")
            .unwrap();

        let result = get_readiness(&registry, json!({"session_id": session.id}), test_ctx())
            .await
            .unwrap();

        assert_eq!(result["status"], "probed");
        assert_eq!(result["state"], "unavailable");
        assert_eq!(result["summary"], "no daemon");
    }

    /// An unknown session must map to `-32007 session_not_found`.
    #[tokio::test]
    async fn get_readiness_unknown_session_maps_to_session_not_found() {
        let registry = SessionRegistry::new();
        let err = get_readiness(&registry, json!({"session_id": "nope"}), test_ctx())
            .await
            .unwrap_err();
        assert_eq!(err.code, -32007);
    }
}
