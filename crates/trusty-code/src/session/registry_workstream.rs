//! `SessionRegistry::bind_workstream` (DOC-48 §4.1/§5.3, issue #3298). Split
//! out of `registry.rs` for the same 500-SLOC-cap reason as
//! `registry_events.rs`/`registry_goals.rs` — this is a child module of
//! `registry` (declared via `#[path = ...] mod workstream_binding;`), so it
//! shares full access to `SessionRegistry`'s private `lock`/`record` helpers
//! and `SessionEntry`'s fields exactly as if this method were still defined
//! in that file.
//!
//! Why: `session::protocol::create` and `task::protocol::task_run` both need
//! to stamp a freshly-minted session's immutable workstream binding once
//! they've resolved it (either an explicit `workstream_id` param, §4.1, or
//! §4.2's ambient active-workstream default) and PERSISTED it into
//! [`crate::workstreams::store::WorkstreamStore`] (via
//! `WorkstreamStore::bind_session`) — this is the registry-side half that
//! also makes the binding observable on the `Session` snapshot
//! (`session.status`/`session.list`) and publishes the DOC-48 §5.3
//! `SessionAdded` event.
//!
//! Test: `registry_tests::bind_workstream_sets_field_and_publishes_session_added`,
//! `registry_tests::bind_workstream_unknown_session_errors`.

use super::*;

impl SessionRegistry {
    /// Stamp a freshly-minted session's immutable workstream binding, and
    /// publish `Event::SessionAdded`.
    ///
    /// Why: `session::protocol::create` and `task::protocol::task_run` are
    /// the ONLY callers, each calling this at most once — immediately after
    /// `Self::create` mints the session's id and the caller has separately
    /// persisted the binding into the workstream store — so there is no
    /// setter that changes `Session.workstream_id` a second time; this
    /// method itself does not enforce that (it would silently overwrite),
    /// the immutability guarantee comes from the call sites never invoking
    /// it twice for the same session (a REUSED session on `task.run` never
    /// calls this again — see `task::protocol::task_run`'s docs on the
    /// mismatch rejection it performs instead).
    /// What: `Err(session_not_found)` if `id` is unknown; otherwise sets
    /// `entry.session.workstream_id = Some(workstream_id)` and records+
    /// publishes `Event::SessionAdded { session_id, workstream_id,
    /// binding_time }` through the normal per-session ring buffer/live bus
    /// (`Self::record`) — see that event's own docs for why this is what
    /// lets `crate::workstreams::sse::aggregate_live`'s dynamic membership
    /// re-check pick it up with no changes.
    pub fn bind_workstream(
        &self,
        id: &str,
        workstream_id: crate::workstreams::model::WorkstreamId,
    ) -> Result<(), RpcError> {
        {
            let mut sessions = self.lock();
            let entry = sessions
                .get_mut(id)
                .ok_or_else(|| RpcError::session_not_found(id))?;
            entry.session.workstream_id = Some(workstream_id);
        }
        self.record(
            id,
            Event::SessionAdded {
                session_id: id.to_string(),
                workstream_id: workstream_id.to_string(),
                binding_time: Utc::now(),
            },
        );
        Ok(())
    }

    /// Record+publish `Event::SessionActivityUpdate` for `id`.
    ///
    /// Why: `Self::set_run_outcome`'s sole activity-signal hook (module
    /// docs) — split out here purely so that method's own diff stays small
    /// enough for `registry.rs`'s 500-SLOC cap; behaviourally this is still
    /// just `Self::record` with the event's shape.
    /// What: `last_turn_at` is stamped `Utc::now()` at call time;
    /// `has_running_task` is passed in by the caller (which already holds
    /// the lock needed to read `entry.execution.is_some()`, so this helper
    /// does not re-lock).
    pub(super) fn publish_activity_update(&self, id: &str, has_running_task: bool) {
        self.record(
            id,
            Event::SessionActivityUpdate {
                session_id: id.to_string(),
                last_turn_at: Utc::now(),
                has_running_task,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `bind_workstream` must set the field and publish `SessionAdded` on
    /// the session's own ring buffer/live bus.
    #[tokio::test]
    async fn bind_workstream_sets_field_and_publishes_session_added() {
        let registry = SessionRegistry::new();
        let session = registry.create("t".to_string(), None, ProjectBinding::None);
        let ws_id = crate::workstreams::model::WorkstreamId::new();

        registry.bind_workstream(&session.id, ws_id).expect("bind");

        let status = registry.status(&session.id).expect("status");
        assert_eq!(status.workstream_id, Some(ws_id));

        let events = registry.replay(&session.id).expect("replay");
        assert!(events.iter().any(|e| matches!(
            &e.event,
            Event::SessionAdded { workstream_id, .. } if *workstream_id == ws_id.to_string()
        )));
    }

    /// An unknown session id must map to `session_not_found`.
    #[tokio::test]
    async fn bind_workstream_unknown_session_errors() {
        let registry = SessionRegistry::new();
        let ws_id = crate::workstreams::model::WorkstreamId::new();
        let err = registry.bind_workstream("nope", ws_id).unwrap_err();
        assert_eq!(err.code, -32007);
    }
}
