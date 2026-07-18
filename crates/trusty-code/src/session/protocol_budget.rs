//! `session.get_context_budget` handler (issue #3015). Split out of
//! `protocol.rs` purely to keep that production file under the crate's
//! 500-SLOC cap — this is a child module of `protocol` (declared via
//! `#[path = ...] mod protocol_budget;`), so it shares full access to
//! `protocol`'s private `SessionIdParams`/`parse` helpers exactly as if this
//! handler were still defined in that file.
//!
//! Why: `Event::ContextBudget` (`crate::events`) is a per-turn STREAM value
//! (epic #2343 "Infinite Sessions") — a UI status bar that attaches after a
//! turn already ran, or reconnects mid-session, has no way to ask "what is
//! the working-context budget right now"; it can only ever see events that
//! arrive while it happens to be attached. This is the query half PR #3014's
//! GUI status bar needs (it currently renders "budget: unavailable" waiting
//! for exactly this API): a client asks once, instead of hoping it was
//! subscribed at the right moment. Mirrors `protocol_readiness` exactly.
//! What: [`get_context_budget`] forwards to
//! `SessionRegistry::get_context_budget`, returning its `ContextBudgetQuery`
//! (`{"status":"recorded", ...snapshot fields}` or
//! `{"status":"never_recorded"}`) verbatim as the JSON-RPC result.
//! Test: `tests::*` in this module.

use super::*;

/// `session.get_context_budget(session_id) -> ContextBudgetQuery` (issue
/// #3015).
///
/// Why: the query counterpart to the per-turn `Event::ContextBudget` stream
/// — see module docs.
/// What: `-32007 session_not_found` if `session_id` is unknown; otherwise
/// `SessionRegistry::get_context_budget`'s `ContextBudgetQuery` verbatim —
/// either the cached snapshot (`status: "recorded"`) or the typed "nothing
/// recorded yet" result (`status: "never_recorded"`), never a bare `null`.
/// Test: `tests::get_context_budget_returns_recorded_snapshot_after_recording`,
/// `tests::get_context_budget_never_recorded_session_returns_never_recorded`,
/// `tests::get_context_budget_unknown_session_maps_to_session_not_found`.
pub(super) async fn get_context_budget(
    registry: &SessionRegistry,
    params: Value,
    _ctx: ConnectionContext,
) -> Result<Value, RpcError> {
    let p: SessionIdParams = parse(params, "session.get_context_budget")?;
    Ok(json!(registry.get_context_budget(&p.session_id)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    fn test_ctx() -> ConnectionContext {
        let (tx, _rx) = mpsc::unbounded_channel();
        ConnectionContext::new(tx)
    }

    fn measurement() -> crate::agent_loop::ContextBudgetSnapshot {
        crate::agent_loop::ContextBudgetSnapshot {
            context_window: 200_000,
            overhead_tokens: 50_000,
            overhead_cap_tokens: 80_000,
            working_context_pct: 75,
            overhead_pct: 25,
            within_budget: true,
            fired: false,
            rounds: 1,
        }
    }

    /// A session with no recorded measurement returns the typed
    /// `never_recorded` result, not an error or a bare `null`.
    #[tokio::test]
    async fn get_context_budget_never_recorded_session_returns_never_recorded() {
        let registry = SessionRegistry::new();
        let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);

        let result = get_context_budget(&registry, json!({"session_id": session.id}), test_ctx())
            .await
            .unwrap();

        assert_eq!(result["status"], "never_recorded");
    }

    /// Once a measurement has been recorded, `session.get_context_budget`
    /// returns it with `status: "recorded"` and the snapshot fields
    /// alongside.
    #[tokio::test]
    async fn get_context_budget_returns_recorded_snapshot_after_recording() {
        let registry = SessionRegistry::new();
        let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
        registry
            .record_context_budget(&session.id, &measurement())
            .unwrap();

        let result = get_context_budget(&registry, json!({"session_id": session.id}), test_ctx())
            .await
            .unwrap();

        assert_eq!(result["status"], "recorded");
        assert_eq!(result["working_context_pct"], 75);
        assert_eq!(result["within_budget"], true);
        assert_eq!(result["context_window_tokens"], 200_000);
    }

    /// An unknown session must map to `-32007 session_not_found`.
    #[tokio::test]
    async fn get_context_budget_unknown_session_maps_to_session_not_found() {
        let registry = SessionRegistry::new();
        let err = get_context_budget(&registry, json!({"session_id": "nope"}), test_ctx())
            .await
            .unwrap_err();
        assert_eq!(err.code, -32007);
    }
}
