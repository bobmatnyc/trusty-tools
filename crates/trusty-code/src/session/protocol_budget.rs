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

    // -- issue #3868: lifetime_compaction_alarm_count --

    fn telemetry_temp_dir(label: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "tcode-protocol-budget-alarm-test-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A recorded threshold-compaction alarm fire is reflected on
    /// `session.get_context_budget`'s `lifetime_compaction_alarm_count`
    /// field (issue #3868's core acceptance criterion).
    ///
    /// (#3868 fix) The alarm-log read now happens in `get_context_budget`
    /// itself (the query path), not `record_context_budget` (the every-turn
    /// write path) — so the RPC call, not just the record call, must run
    /// with the data-dir env var still set. `with_data_dir_env_fut` wraps
    /// the WHOLE async sequence for exactly that reason.
    #[tokio::test]
    async fn get_context_budget_reflects_lifetime_compaction_alarm_count() {
        let dir = telemetry_temp_dir("reflects");
        let registry = SessionRegistry::new();
        let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);

        let result = crate::agent_loop::telemetry::with_data_dir_env_fut(&dir, async {
            crate::agent_loop::telemetry::record_compaction_alarm(&dir);
            registry
                .record_context_budget(&session.id, &measurement())
                .unwrap();
            get_context_budget(&registry, json!({"session_id": session.id}), test_ctx())
                .await
                .unwrap()
        })
        .await;

        assert_eq!(result["status"], "recorded");
        assert!(
            result["lifetime_compaction_alarm_count"].as_u64().unwrap() >= 1,
            "expected a non-zero alarm count, got {result:?}"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A session that never records an alarm fire has
    /// `lifetime_compaction_alarm_count == 0` — the `cadence: None`
    /// regression guard (issue #3868 acceptance criteria: "cadence: None
    /// runs never increment this counter").
    #[tokio::test]
    async fn get_context_budget_alarm_count_zero_without_a_fire() {
        let dir = telemetry_temp_dir("zero");
        let registry = SessionRegistry::new();
        let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);

        let result = crate::agent_loop::telemetry::with_data_dir_env_fut(&dir, async {
            registry
                .record_context_budget(&session.id, &measurement())
                .unwrap();
            get_context_budget(&registry, json!({"session_id": session.id}), test_ctx())
                .await
                .unwrap()
        })
        .await;

        assert_eq!(result["lifetime_compaction_alarm_count"], 0);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// The alarm count is read fresh from the durable log every time, so a
    /// BRAND NEW `SessionRegistry` (simulating a daemon restart) querying a
    /// brand new session still sees fires recorded by an earlier registry
    /// instance — the durability requirement the old in-memory `Transcript`
    /// counter failed (issue #3868: "persists across a session restart").
    #[tokio::test]
    async fn get_context_budget_alarm_count_survives_fresh_registry() {
        let dir = telemetry_temp_dir("survives-restart");

        let result = crate::agent_loop::telemetry::with_data_dir_env_fut(&dir, async {
            crate::agent_loop::telemetry::record_compaction_alarm(&dir);
            crate::agent_loop::telemetry::record_compaction_alarm(&dir);

            // A FRESH registry — nothing in-memory carried over from
            // whatever recorded the fires above.
            let fresh_registry = SessionRegistry::new();
            let session =
                fresh_registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);
            fresh_registry
                .record_context_budget(&session.id, &measurement())
                .unwrap();

            get_context_budget(
                &fresh_registry,
                json!({"session_id": session.id}),
                test_ctx(),
            )
            .await
            .unwrap()
        })
        .await;

        assert_eq!(
            result["lifetime_compaction_alarm_count"], 2,
            "a fresh registry must read the SAME durable count, not reset to 0"
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}
