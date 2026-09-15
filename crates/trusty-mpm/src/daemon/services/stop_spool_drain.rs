//! Replay the `SubagentStop` records a hook could not deliver (#6556).
//!
//! Why: the hook is the only process that ever learns a subagent stopped, and it
//! learns it once. When this daemon was down, every attempt that hook made
//! failed and it parked the stop in
//! [`crate::core::stop_spool`] instead of losing it. Without a reader that park
//! is a write-only file: the delegation still sits `Running` until the six-hour
//! `RUNNING_STALE_AFTER_SECS` sweep, holding a builder slot and a checkout. This
//! is that reader, and the reap loop is where it runs — the same loop that owns
//! the staleness sweep this fix exists to beat.
//!
//! What: [`drain_unposted_stops`] replays each parked body through the same
//! [`crate::daemon::rpc::sessions_legacy_ops::ingest_hook`] a live POST would
//! have reached, so a recovered stop terminalizes its delegation by exactly the
//! path the hook's own POST does. Every record is discarded after its replay,
//! including one the daemon refuses — a body this daemon cannot accept will not
//! become acceptable on the next tick, and leaving it would wedge the directory
//! against [`crate::core::stop_spool::MAX_UNPOSTED_STOPS`].
//!
//! Test: the module's `#[cfg(test)]` suite.

use std::sync::Arc;

use crate::core::stop_spool;
use crate::daemon::api::HookPost;
use crate::daemon::state::DaemonState;

/// Replay and discard every parked stop under this daemon's framework root.
///
/// Why: see the module header.
/// What: reads `<framework-root>/unposted-stops/` oldest first, deserializes
/// each body as a [`HookPost`], and ingests it. Returns how many were replayed
/// successfully. Unparsable and refused records are discarded too, and counted
/// separately only in the log — the caller's number is "stops recovered".
/// Test: `a_parked_stop_terminalizes_its_delegation`,
/// `a_corrupt_record_is_discarded_rather_than_wedging_the_drain`,
/// `an_empty_spool_drains_nothing`.
pub async fn drain_unposted_stops(state: &Arc<DaemonState>) -> usize {
    let root = state.framework_root().to_path_buf();
    let parked = stop_spool::read_unposted_stops(&root);
    if parked.is_empty() {
        return 0;
    }
    let mut replayed = 0usize;
    let mut discarded = 0usize;
    for (path, body) in parked {
        match body.and_then(|v| serde_json::from_value::<HookPost>(v).ok()) {
            Some(post) => {
                match crate::daemon::rpc::sessions_legacy_ops::ingest_hook(state, post).await {
                    Ok(_) => replayed += 1,
                    Err(e) => {
                        discarded += 1;
                        tracing::warn!(
                            record = %path.display(),
                            "stop-spool: the daemon refused a parked SubagentStop ({e}); discarding \
                             it rather than retrying a body that cannot become acceptable (#6556)"
                        );
                    }
                }
            }
            None => {
                discarded += 1;
                tracing::warn!(
                    record = %path.display(),
                    "stop-spool: a parked SubagentStop is not a readable hook body; discarding \
                     it (#6556)"
                );
            }
        }
        stop_spool::discard_unposted_stop(&path);
    }
    tracing::info!(
        "stop-spool: recovered {replayed} SubagentStop record(s) the hook could not deliver, \
         discarded {discarded} (#6556)"
    );
    replayed
}

#[cfg(test)]
mod stop_spool_drain_tests {
    use super::*;
    use crate::core::agent::{Delegation, DelegationStatus};
    use crate::core::paths::FrameworkPaths;
    use crate::core::session::SessionId;

    /// A hermetic daemon whose framework root is a temp dir, plus one session.
    fn hermetic() -> (Arc<DaemonState>, tempfile::TempDir, SessionId) {
        let dir = tempfile::tempdir().expect("temp dir");
        let paths = FrameworkPaths::under(dir.path());
        let state = Arc::new(DaemonState::with_paths(&paths));
        (state, dir, SessionId(uuid::Uuid::new_v4()))
    }

    /// One running delegation the parked stop will terminalize.
    fn running(state: &DaemonState, session: SessionId, agent_id: &str) {
        let mut d = Delegation::observed(session, "rust-engineer", "task", Some("toolu_1".into()));
        d.status = DelegationStatus::Running;
        d.agent_id = Some(agent_id.to_string());
        state.upsert_delegation(d);
    }

    /// The body `tm hook` parks: the exact `POST /hooks` envelope.
    fn parked_body(session: SessionId, agent_id: &str) -> serde_json::Value {
        serde_json::json!({
            "session_id": session.0.to_string(),
            "event": "SubagentStop",
            "payload": {"agent_id": agent_id, "tool": "Agent"},
        })
    }

    /// 🔴 REGRESSION (#6556): the park is only worth writing if something reads
    /// it. A daemon that was down when the agent stopped recovers the stop on its
    /// next reap tick, instead of the record sitting `Running` for six hours.
    #[tokio::test]
    async fn a_parked_stop_terminalizes_its_delegation() {
        let (state, dir, session) = hermetic();
        running(&state, session, "agent-1");
        let root = FrameworkPaths::under(dir.path()).root;
        stop_spool::record_unposted_stop(&root, &parked_body(session, "agent-1")).expect("parked");

        assert_eq!(drain_unposted_stops(&state).await, 1);

        let records = state.delegations_for(session);
        assert_eq!(records.len(), 1);
        assert!(
            !records[0].status.is_live(),
            "the recovered stop ended the record, status {:?}",
            records[0].status
        );
        assert!(
            stop_spool::read_unposted_stops(&root).is_empty(),
            "a replayed record must not replay again"
        );
    }

    /// A record this daemon cannot read is discarded, not retried forever —
    /// otherwise one corrupt file fills the spool to its cap and every later
    /// stop is dropped at the writer.
    #[tokio::test]
    async fn a_corrupt_record_is_discarded_rather_than_wedging_the_drain() {
        let (state, dir, _session) = hermetic();
        let root = FrameworkPaths::under(dir.path()).root;
        let spool = stop_spool::unposted_stops_dir(&root);
        std::fs::create_dir_all(&spool).expect("dir");
        std::fs::write(spool.join("0000-0-0.json"), "{ not json").expect("write");
        // Valid JSON, but not a hook body — the other way a record is unusable.
        std::fs::write(spool.join("0001-0-0.json"), r#"{"event":"SubagentStop"}"#).expect("write");

        assert_eq!(drain_unposted_stops(&state).await, 0);
        assert!(stop_spool::read_unposted_stops(&root).is_empty());
    }

    /// The common case: nothing parked, no work, no log noise.
    #[tokio::test]
    async fn an_empty_spool_drains_nothing() {
        let (state, _dir, _session) = hermetic();
        assert_eq!(drain_unposted_stops(&state).await, 0);
    }
}
