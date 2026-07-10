//! `session.set_goal`/`session.clear_goal`/`session.get_goals` handlers
//! (#2350, epic #2343 pillar 3). Split out of `protocol.rs` purely to keep
//! that production file under the crate's 500-SLOC cap — this is a child
//! module of `protocol` (declared via `#[path = ...] mod protocol_goals;`),
//! so it shares full access to `protocol`'s private `SessionIdParams`/`parse`
//! helpers exactly as if these handlers were still defined in that file.
//!
//! Why: `agent_loop::goals::GoalSlots` (#2347) already lets the PM's own
//! model write goals mid-conversation via the `set_goal`/`clear_goal` tools;
//! this is the OPERATOR-facing half of the same shared state, reachable over
//! `session.*` JSON-RPC exactly like every other session operation (vision
//! spec Axiom 4 — the daemon owns sessions, clients only ever go through the
//! API).
//! What: [`set_goal`]/[`clear_goal`] forward to
//! `SessionRegistry::set_goal`/`clear_goal`, which write with
//! `GoalSource::Operator` (last-write-wins against a model write to the same
//! slot — see `SessionRegistry::set_goal`'s docs for the full design
//! decision on sessions with no transcript yet). [`get_goals`] forwards to
//! `SessionRegistry::get_goals`, returning `{"goals": [...]}`.
//! Test: `tests::*` in this module.

use super::*;

/// `params` shape for `session.set_goal`.
#[derive(Deserialize)]
struct SetGoalParams {
    session_id: String,
    slot: usize,
    text: String,
}

/// `params` shape for `session.clear_goal`.
#[derive(Deserialize)]
struct ClearGoalParams {
    session_id: String,
    slot: usize,
}

/// `session.set_goal(session_id, slot, text) -> {}` (#2350).
///
/// Why: the operator-facing entry point onto the session's 5 fixed goal
/// slots — see module docs.
/// What: `-32007 session_not_found` if `session_id` is unknown;
/// `-32003 invalid_argument` if the session has no transcript yet (see
/// `SessionRegistry::set_goal`'s docs for this design decision) or `slot` is
/// out of range (`0` or `> 5`). On success, writes with
/// `GoalSource::Operator` and returns `{}`.
/// Test: `tests::set_goal_writes_operator_goal`,
/// `tests::set_goal_out_of_range_slot_maps_to_invalid_argument`,
/// `tests::set_goal_unknown_session_maps_to_session_not_found`.
pub(super) async fn set_goal(
    registry: &SessionRegistry,
    params: Value,
    _ctx: ConnectionContext,
) -> Result<Value, RpcError> {
    let p: SetGoalParams = parse(params, "session.set_goal")?;
    registry.set_goal(&p.session_id, p.slot, &p.text)?;
    Ok(json!({}))
}

/// `session.clear_goal(session_id, slot) -> {}` (#2350).
///
/// Why: the operator-facing counterpart to `set_goal` — see module docs.
/// What: same error mapping as [`set_goal`]. Clearing an already-empty slot
/// is idempotent success (mirrors `GoalSlots::clear`'s own convention).
/// Test: `tests::clear_goal_clears_operator_goal`,
/// `tests::clear_goal_out_of_range_slot_maps_to_invalid_argument`.
pub(super) async fn clear_goal(
    registry: &SessionRegistry,
    params: Value,
    _ctx: ConnectionContext,
) -> Result<Value, RpcError> {
    let p: ClearGoalParams = parse(params, "session.clear_goal")?;
    registry.clear_goal(&p.session_id, p.slot)?;
    Ok(json!({}))
}

/// `session.get_goals(session_id) -> { goals: [GoalSlotRecord] }` (#2350).
///
/// Why: lets an operator/UI inspect the current goal state (from either
/// source) without pulling the larger `session.get_transcript` payload.
/// What: `-32007 session_not_found` if `session_id` is unknown; otherwise
/// `{"goals": [...]}` — `[]` for a session with no transcript yet, matching
/// `session.get_transcript`'s never-run convention.
/// Test: `tests::get_goals_returns_goals_key`,
/// `tests::get_goals_on_never_run_session_is_empty`,
/// `tests::get_goals_unknown_session_maps_to_session_not_found`.
pub(super) async fn get_goals(
    registry: &SessionRegistry,
    params: Value,
    _ctx: ConnectionContext,
) -> Result<Value, RpcError> {
    let p: SessionIdParams = parse(params, "session.get_goals")?;
    Ok(json!({ "goals": registry.get_goals(&p.session_id)? }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    fn test_ctx() -> ConnectionContext {
        let (tx, _rx) = mpsc::unbounded_channel();
        ConnectionContext::new(tx)
    }

    /// Seed `id`'s `pm_transcript` the same way a real `task.run` would, so
    /// `set_goal`/`clear_goal` get past the "no transcript yet" guard.
    fn seed_pm_transcript(registry: &SessionRegistry, id: &str) {
        let transcript = registry
            .begin_pm_transcript(id, "you are the pm", "first task")
            .unwrap();
        registry.store_pm_transcript(id, transcript);
    }

    /// `set_goal` on a session with a seeded transcript writes the slot,
    /// visible via `get_goals` with `source: "operator"`.
    #[tokio::test]
    async fn set_goal_writes_operator_goal() {
        let registry = SessionRegistry::new();
        let session = registry.create("t".to_string(), None, None);
        seed_pm_transcript(&registry, &session.id);

        let result = set_goal(
            &registry,
            json!({"session_id": session.id, "slot": 2, "text": "ship it"}),
            test_ctx(),
        )
        .await
        .unwrap();
        assert_eq!(result, json!({}));

        let goals = get_goals(&registry, json!({"session_id": session.id}), test_ctx())
            .await
            .unwrap();
        assert_eq!(goals["goals"][0]["slot"], 2);
        assert_eq!(goals["goals"][0]["text"], "ship it");
        assert_eq!(goals["goals"][0]["source"], "operator");
    }

    /// `set_goal` with an out-of-range slot must map to `-32003
    /// invalid_argument`.
    #[tokio::test]
    async fn set_goal_out_of_range_slot_maps_to_invalid_argument() {
        let registry = SessionRegistry::new();
        let session = registry.create("t".to_string(), None, None);
        seed_pm_transcript(&registry, &session.id);

        let err = set_goal(
            &registry,
            json!({"session_id": session.id, "slot": 6, "text": "x"}),
            test_ctx(),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, -32003);
    }

    /// `set_goal` on an unknown session must map to `-32007
    /// session_not_found`.
    #[tokio::test]
    async fn set_goal_unknown_session_maps_to_session_not_found() {
        let registry = SessionRegistry::new();
        let err = set_goal(
            &registry,
            json!({"session_id": "nope", "slot": 1, "text": "x"}),
            test_ctx(),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, -32007);
    }

    /// `clear_goal` empties a previously `set_goal`-written slot.
    #[tokio::test]
    async fn clear_goal_clears_operator_goal() {
        let registry = SessionRegistry::new();
        let session = registry.create("t".to_string(), None, None);
        seed_pm_transcript(&registry, &session.id);
        set_goal(
            &registry,
            json!({"session_id": session.id, "slot": 3, "text": "temp"}),
            test_ctx(),
        )
        .await
        .unwrap();

        clear_goal(
            &registry,
            json!({"session_id": session.id, "slot": 3}),
            test_ctx(),
        )
        .await
        .unwrap();

        let goals = get_goals(&registry, json!({"session_id": session.id}), test_ctx())
            .await
            .unwrap();
        assert_eq!(goals["goals"].as_array().unwrap().len(), 0);
    }

    /// `clear_goal` with an out-of-range slot must map to `-32003
    /// invalid_argument`.
    #[tokio::test]
    async fn clear_goal_out_of_range_slot_maps_to_invalid_argument() {
        let registry = SessionRegistry::new();
        let session = registry.create("t".to_string(), None, None);
        seed_pm_transcript(&registry, &session.id);

        let err = clear_goal(
            &registry,
            json!({"session_id": session.id, "slot": 0}),
            test_ctx(),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, -32003);
    }

    /// `get_goals` must wrap its result under a `"goals"` key.
    #[tokio::test]
    async fn get_goals_returns_goals_key() {
        let registry = SessionRegistry::new();
        let session = registry.create("t".to_string(), None, None);
        let result = get_goals(&registry, json!({"session_id": session.id}), test_ctx())
            .await
            .unwrap();
        assert!(result["goals"].is_array());
    }

    /// `get_goals` on a session that has never run a task returns `[]`, not
    /// an error.
    #[tokio::test]
    async fn get_goals_on_never_run_session_is_empty() {
        let registry = SessionRegistry::new();
        let session = registry.create("t".to_string(), None, None);
        let result = get_goals(&registry, json!({"session_id": session.id}), test_ctx())
            .await
            .unwrap();
        assert_eq!(result["goals"].as_array().unwrap().len(), 0);
    }

    /// `get_goals` on an unknown session must map to `-32007
    /// session_not_found`.
    #[tokio::test]
    async fn get_goals_unknown_session_maps_to_session_not_found() {
        let registry = SessionRegistry::new();
        let err = get_goals(&registry, json!({"session_id": "nope"}), test_ctx())
            .await
            .unwrap_err();
        assert_eq!(err.code, -32007);
    }
}
