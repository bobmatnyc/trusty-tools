//! `session.get_agents` handler (DOC-39 §5.4). Split out of `protocol.rs`
//! purely to keep that production file under the crate's 500-SLOC cap — this
//! is a child module of `protocol` (declared via `#[path = ...] mod
//! protocol_agents;`), so it shares full access to `protocol`'s private
//! `SessionIdParams`/`parse` helpers exactly as if this handler were still
//! defined in that file.
//!
//! Why: §5.4 names `session.get_agents` "the principled endpoint" a
//! late-attaching UI client needs to ask "who is running right now?" without
//! folding the SSE ring-buffer replay itself — see
//! `session::registry::agents`' module docs for the fold this handler
//! forwards to.
//! What: [`get_agents`] forwards to `SessionRegistry::get_agents`, wrapping
//! its roster under an `"agents"` key — the same `{"<plural>": [...]}` shape
//! `session.list`/`session.get_goals` already use.
//! Test: `tests::*` in this module.

use super::*;

/// `session.get_agents(session_id) -> { agents: [AgentRosterEntry] }`
/// (DOC-39 §5.4).
///
/// Why: the query counterpart to the tool-attribution events every agent
/// loop's tool dispatch already emits — see module docs.
/// What: `-32007 session_not_found` if `session_id` is unknown; otherwise
/// `SessionRegistry::get_agents`'s roster, wrapped as `{"agents": [...]}`
/// (never a bare array, matching `session.list`'s `{"sessions": [...]}`
/// convention). An empty roster (no tool activity recorded yet) is a
/// success, not an error.
/// Test: `tests::get_agents_wraps_roster_under_agents_key`,
/// `tests::get_agents_unknown_session_maps_to_session_not_found`.
pub(super) async fn get_agents(
    registry: &SessionRegistry,
    params: Value,
    _ctx: ConnectionContext,
) -> Result<Value, RpcError> {
    let p: SessionIdParams = parse(params, "session.get_agents")?;
    Ok(json!({ "agents": registry.get_agents(&p.session_id)? }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    fn test_ctx() -> ConnectionContext {
        let (tx, _rx) = mpsc::unbounded_channel();
        ConnectionContext::new(tx)
    }

    /// A session with a recorded tool call reports it under `"agents"`, with
    /// the roster entry's core fields populated from real spawn-attribution
    /// state.
    #[tokio::test]
    async fn get_agents_wraps_roster_under_agents_key() {
        let registry = SessionRegistry::new();
        let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
        registry
            .record_tool_started(
                &session.id,
                "python-engineer",
                "eng-1",
                "bash",
                "c1",
                "cargo test",
            )
            .unwrap();

        let result = get_agents(&registry, json!({"session_id": session.id}), test_ctx())
            .await
            .unwrap();

        let agents = result["agents"].as_array().unwrap();
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0]["agent_id"], "eng-1");
        assert_eq!(agents[0]["name"], "python-engineer");
        assert_eq!(agents[0]["state"], "running");
        assert!(agents[0]["model"].is_null());
        assert!(agents[0]["task"].is_null());
        assert_eq!(agents[0]["todos"].as_array().unwrap().len(), 0);
        assert_eq!(agents[0]["files_changed"].as_array().unwrap().len(), 0);
    }

    /// A session with no tool activity yet returns an empty `"agents"`
    /// array, not an error.
    #[tokio::test]
    async fn get_agents_empty_session_returns_empty_array() {
        let registry = SessionRegistry::new();
        let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);

        let result = get_agents(&registry, json!({"session_id": session.id}), test_ctx())
            .await
            .unwrap();

        assert_eq!(result["agents"].as_array().unwrap().len(), 0);
    }

    /// An unknown session must map to `-32007 session_not_found`.
    #[tokio::test]
    async fn get_agents_unknown_session_maps_to_session_not_found() {
        let registry = SessionRegistry::new();
        let err = get_agents(&registry, json!({"session_id": "nope"}), test_ctx())
            .await
            .unwrap_err();
        assert_eq!(err.code, -32007);
    }
}
