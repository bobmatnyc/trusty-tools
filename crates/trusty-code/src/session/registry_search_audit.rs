//! `session.get_search_audit`'s retained search/recall audit trail (issue
//! #3072). Split out of `registry.rs` purely to keep that production file
//! under the crate's 500-SLOC cap — this is a child module of `registry`
//! (declared via `#[path = ...] mod search_audit;`), so it shares full
//! access to `SessionRegistry`'s private `lock` helper exactly as if these
//! methods were still defined in that file.
//!
//! Why: DOC-39 §4.7 (Search tab, 10d) and #3027 (the 8b search/recall
//! monitor card) both need a REST-pollable history of `search_code`/
//! `recall_session` activity, but `Event::SearchPerformed`/
//! `Event::MemoryRecalled` (`crate::events`) are emitted ONLY on the SSE
//! stream (`GET /sessions/{id}/events`) — a client that attaches late, or
//! never opens an SSE connection at all (DOC-39 §2.1 forbids the GUI from
//! becoming an ad hoc SSE consumer for this), cannot see them. This module
//! is the daemon-owned, ALWAYS-RETAINED (bounded, not ring-evicted) list
//! that closes that gap — the same shape `registry_agents.rs` used for
//! #2962's `session.get_agents`, adapted for a growing history rather than a
//! small per-agent roster.
//!
//! **Why a dedicated cap distinct from the ring buffer:** the ring buffer
//! (`SessionEntry::ring`, `DEFAULT_RING_CAPACITY` = 1000) evicts EVERY event
//! kind together, so a session dominated by tool-call chatter can age a
//! search/recall record out of the ring long before `SEARCH_AUDIT_CAP`
//! search/recall records have happened — exactly the silent-data-loss shape
//! the #2962 code-critic HIGH flagged for `session.get_agents` (see
//! `registry_agents.rs`'s module docs). Storing search/recall records in
//! their OWN bounded list, independent of ring pressure from unrelated
//! events, avoids that: a session's search/recall history survives exactly
//! [`SEARCH_AUDIT_CAP`] records regardless of how much other traffic passes
//! through the ring in between.
//! What: [`push_search_audit`](SessionRegistry::push_search_audit) appends
//! one [`SearchAuditRecord`] onto `SessionEntry::search_audit`, evicting the
//! OLDEST record first once the list is at [`SEARCH_AUDIT_CAP`] — called
//! from `record_search_performed`/`record_memory_recalled`
//! (`registry_events.rs`) immediately before they call `self.record(...)` to
//! emit the matching SSE event, mirroring `record_context_budget`'s
//! cache-then-emit ordering. [`get_search_audit`](SessionRegistry::get_search_audit)
//! returns the retained list verbatim, oldest-first — `[]` for a session
//! with no search/recall activity yet (never an error), `-32007
//! session_not_found` for an unknown session.
//! Test: `tests::*` in this module (registry-level);
//! `session::protocol_search_audit::tests::*` (RPC-level wiring).

use super::*;

/// Maximum number of [`SearchAuditRecord`]s retained per session — the
/// OLDEST record is dropped once a session's `SEARCH_AUDIT_CAP`+1'th
/// search/recall fires. Chosen generously above what a single interactive
/// session realistically produces (DOC-39 §4.7's audit list is a debugging
/// aid, not a permanent record — `session.get_transcript`/the durable memory
/// palace are the sources of truth for long-term history) while still
/// bounding per-session memory on a long-running or looping agent.
pub(super) const SEARCH_AUDIT_CAP: usize = 200;

impl SessionRegistry {
    /// Append one [`SearchAuditRecord`] onto `id`'s retained search/recall
    /// audit trail, evicting the oldest record first once at
    /// [`SEARCH_AUDIT_CAP`] (issue #3072).
    ///
    /// Why: the write half of this module's gap-closing list — see the
    /// module docs for why this is a DEDICATED cap rather than a fold over
    /// the shared ring buffer.
    /// What: a no-op if `id` is unknown, mirroring `SessionRegistry::record`'s
    /// same "no hard error, caller already checked existence" contract —
    /// every call site here is `record_search_performed`/
    /// `record_memory_recalled`, both of which already call `ensure_exists`
    /// before reaching this.
    /// Test: `tests::push_search_audit_evicts_oldest_once_at_cap`.
    pub(super) fn push_search_audit(&self, id: &str, record: SearchAuditRecord) {
        let mut sessions = self.lock();
        if let Some(entry) = sessions.get_mut(id) {
            if entry.search_audit.len() >= SEARCH_AUDIT_CAP {
                entry.search_audit.pop_front();
            }
            entry.search_audit.push_back(record);
        }
    }

    /// `session.get_search_audit(session_id) -> { search_audit:
    /// [SearchAuditRecord] }` (issue #3072).
    ///
    /// Why: the query surface DOC-39 §4.7's Search tab and #3027's monitor
    /// card poll instead of buffering the SSE stream client-side — see
    /// module docs.
    /// What: `-32007 session_not_found` if `id` is unknown; otherwise the
    /// retained `SearchAuditRecord` list, oldest-first, verbatim. A session
    /// with no search/recall activity yet returns an empty list, not an
    /// error — the same "empty means nothing yet" convention
    /// `session.get_agents`/`session.get_goals` use.
    /// Test: `tests::get_search_audit_empty_session_returns_empty_list`,
    /// `tests::get_search_audit_returns_records_after_recording`,
    /// `tests::get_search_audit_unknown_session_errors`.
    pub fn get_search_audit(&self, id: &str) -> Result<Vec<SearchAuditRecord>, RpcError> {
        let sessions = self.lock();
        let entry = sessions
            .get(id)
            .ok_or_else(|| RpcError::session_not_found(id))?;
        Ok(entry.search_audit.iter().cloned().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn search_record(query: &str) -> SearchAuditRecord {
        SearchAuditRecord::Search {
            agent: "pm".to_string(),
            agent_id: "pm-1".to_string(),
            lane: "grep".to_string(),
            query: query.to_string(),
            hit_count: Some(3),
            latency_ms: 10,
            at: Utc::now(),
        }
    }

    /// A session with no search/recall activity yet returns an empty list,
    /// not an error.
    #[test]
    fn get_search_audit_empty_session_returns_empty_list() {
        let registry = SessionRegistry::new();
        let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);

        let records = registry.get_search_audit(&session.id).unwrap();

        assert!(records.is_empty());
    }

    /// Records pushed via `push_search_audit` are returned, in push order.
    #[test]
    fn get_search_audit_returns_records_after_recording() {
        let registry = SessionRegistry::new();
        let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);

        registry.push_search_audit(&session.id, search_record("first"));
        registry.push_search_audit(&session.id, search_record("second"));

        let records = registry.get_search_audit(&session.id).unwrap();

        assert_eq!(records.len(), 2);
        assert!(matches!(&records[0], SearchAuditRecord::Search { query, .. } if query == "first"));
        assert!(
            matches!(&records[1], SearchAuditRecord::Search { query, .. } if query == "second")
        );
    }

    /// Once a session is at `SEARCH_AUDIT_CAP` records, the next push evicts
    /// the OLDEST record rather than growing unbounded — the retention
    /// contract this module exists to enforce.
    #[test]
    fn push_search_audit_evicts_oldest_once_at_cap() {
        let registry = SessionRegistry::new();
        let session = registry.create("t".to_string(), None, crate::binding::ProjectBinding::None);

        for i in 0..SEARCH_AUDIT_CAP {
            registry.push_search_audit(&session.id, search_record(&format!("q-{i}")));
        }
        // One more push past the cap.
        registry.push_search_audit(&session.id, search_record("q-overflow"));

        let records = registry.get_search_audit(&session.id).unwrap();

        assert_eq!(records.len(), SEARCH_AUDIT_CAP);
        // The very first record ("q-0") must have been evicted.
        assert!(
            !records
                .iter()
                .any(|r| matches!(r, SearchAuditRecord::Search { query, .. } if query == "q-0"))
        );
        // The overflow record must be present, as the newest (last) entry.
        assert!(matches!(
            records.last(),
            Some(SearchAuditRecord::Search { query, .. }) if query == "q-overflow"
        ));
    }

    /// An unknown session must map to `-32007 session_not_found`.
    #[test]
    fn get_search_audit_unknown_session_errors() {
        let registry = SessionRegistry::new();
        let err = registry.get_search_audit("nope").unwrap_err();
        assert_eq!(err.code, -32007);
    }

    /// `push_search_audit` on an unknown session is a silent no-op, mirroring
    /// `SessionRegistry::record`'s contract — it never panics or surfaces an
    /// error to a caller that already validated existence.
    #[test]
    fn push_search_audit_unknown_session_is_noop() {
        let registry = SessionRegistry::new();
        // Must not panic.
        registry.push_search_audit("nope", search_record("x"));
    }
}
