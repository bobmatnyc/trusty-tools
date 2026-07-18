//! `session.get_search_audit` handler (issue #3072). Split out of
//! `protocol.rs` purely to keep that production file under the crate's
//! 500-SLOC cap — this is a child module of `protocol` (declared via
//! `#[path = ...] mod protocol_search_audit;`), so it shares full access to
//! `protocol`'s private `SessionIdParams`/`parse` helpers exactly as if this
//! handler were still defined in that file.
//!
//! Why: DOC-39 §4.7 (Search tab, 10d) and #3027 (the 8b search/recall
//! monitor card) both need a REST-pollable history of search/recall
//! activity, but `Event::SearchPerformed`/`Event::MemoryRecalled` are
//! emitted ONLY on the SSE stream — a late-attaching or SSE-avoiding client
//! (DOC-39 §2.1) has no way to see them. This is the query half; see
//! `session::registry_search_audit` module docs for the retained-list design
//! and cap.
//! What: [`get_search_audit`] forwards to
//! `SessionRegistry::get_search_audit`, wrapping its result under a
//! `"search_audit"` key — the same `{"<plural>": [...]}` shape
//! `session.get_agents`/`session.get_goals` already use.
//! Test: `tests::*` in this module.

use super::*;

/// `session.get_search_audit(session_id) -> { search_audit:
/// [SearchAuditRecord] }` (issue #3072).
///
/// Why: the query counterpart to the search/recall telemetry every
/// `search_code`/`recall_session` call already emits on the SSE stream — see
/// module docs.
/// What: `-32007 session_not_found` if `session_id` is unknown; otherwise
/// `SessionRegistry::get_search_audit`'s retained list, oldest-first, under
/// `"search_audit"`. An empty list (no search/recall activity recorded yet)
/// is a success, not an error.
/// Test: `tests::get_search_audit_wraps_records_under_search_audit_key`,
/// `tests::get_search_audit_empty_session_returns_empty_array`,
/// `tests::get_search_audit_unknown_session_maps_to_session_not_found`.
pub(super) async fn get_search_audit(
    registry: &SessionRegistry,
    params: Value,
    _ctx: ConnectionContext,
) -> Result<Value, RpcError> {
    let p: SessionIdParams = parse(params, "session.get_search_audit")?;
    Ok(json!({ "search_audit": registry.get_search_audit(&p.session_id)? }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    fn test_ctx() -> ConnectionContext {
        let (tx, _rx) = mpsc::unbounded_channel();
        ConnectionContext::new(tx)
    }

    /// A session with a recorded search reports it under `"search_audit"`,
    /// with the record's core fields populated.
    #[tokio::test]
    async fn get_search_audit_wraps_records_under_search_audit_key() {
        let registry = SessionRegistry::new();
        let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
        registry
            .record_search_performed(
                &session.id,
                "pm",
                "pm-1",
                &crate::tools::SearchTelemetry {
                    lane: "grep".to_string(),
                    query: "where is auth".to_string(),
                    hit_count: Some(3),
                    hits: vec![],
                    latency_ms: 5,
                },
            )
            .unwrap();

        let result = get_search_audit(&registry, json!({"session_id": session.id}), test_ctx())
            .await
            .unwrap();

        let records = result["search_audit"].as_array().unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0]["kind"], "search");
        assert_eq!(records[0]["query"], "where is auth");
        assert_eq!(records[0]["hit_count"], 3);
    }

    /// A session with no search/recall activity yet returns an empty
    /// `"search_audit"` array, not an error.
    #[tokio::test]
    async fn get_search_audit_empty_session_returns_empty_array() {
        let registry = SessionRegistry::new();
        let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);

        let result = get_search_audit(&registry, json!({"session_id": session.id}), test_ctx())
            .await
            .unwrap();

        assert_eq!(result["search_audit"].as_array().unwrap().len(), 0);
    }

    /// An unknown session must map to `-32007 session_not_found`.
    #[tokio::test]
    async fn get_search_audit_unknown_session_maps_to_session_not_found() {
        let registry = SessionRegistry::new();
        let err = get_search_audit(&registry, json!({"session_id": "nope"}), test_ctx())
            .await
            .unwrap_err();
        assert_eq!(err.code, -32007);
    }
}
