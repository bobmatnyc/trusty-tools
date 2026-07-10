//! In-process [`ManagedBackend`] over this daemon's own `SessionManager`.
//!
//! Why: the daemon's local proxy routes must reach a managed session WITHOUT a
//! network hop back into its own HTTP surface (a self-referential loopback call
//! would be fragile — an extra failure mode, a needless round trip, and a
//! bootstrap hazard before the listener is bound). [`DirectManagedBackend`]
//! reaches the session the same way every OTHER `managed_routes` handler does:
//! through `state.session_manager()` directly.
//! What: implements the three [`ManagedBackend`] primitives — `resolve` via the
//! shared [`resolve_target`] resolver over `SessionManager::list`, `send` via
//! `SessionManager::send_input`, and `activity` as a lightweight digest built
//! straight from the [`SessionRecord`] (no tmux capture, no LLM classifier — see
//! the module doc on `proxy::mod` for why that is a deliberate, documented
//! divergence from the richer `GET .../activity` endpoint).
//! Test: `tests.rs` (in-crate) and `tests/proxy_routes.rs` (HTTP-level) exercise
//! this backend end-to-end through the proxy routes.

use async_trait::async_trait;

use crate::client::proxy::{ActivityDigest, ManagedBackend};
use crate::client::resolve_target;
use crate::client::resolver::Resolvable;
use crate::daemon::state::DaemonState;
use crate::session_manager::record::{ManagedSessionId, SessionRecord};

impl Resolvable for SessionRecord {
    /// Why: [`resolve_target`] is the one canonical id/name/prefix resolver every
    /// surface uses; a managed session's daemon-internal [`SessionRecord`] must
    /// participate in it exactly like the wire-facing
    /// [`crate::client::http_client::ManagedSessionSummary`] does, so the local
    /// proxy backend resolves a fuzzy target with the SAME precedence rules as
    /// every other surface.
    /// What: `id_matches` compares the record's UUID (via `Display`) to the
    /// query; `resolve_name` returns the tmux name.
    /// Test: `resolve_direct_backend_matches_id_name_prefix` in `tests.rs`.
    fn id_matches(&self, query: &str) -> bool {
        self.id.to_string() == query
    }
    fn resolve_name(&self) -> &str {
        &self.tmux_name
    }
}

/// [`ManagedBackend`] reaching this daemon's OWN `SessionManager` in-process.
///
/// Why: this is the backend the local proxy routes (`daemon::managed_routes::proxy`)
/// bind [`crate::client::proxy::SessionProxy`] to — see that module's doc for why
/// this lets the state machine be `curl`-tested before a channel connects.
/// What: wraps `Arc<DaemonState>` so each method can call
/// `state.session_manager().await` fresh (matching every other `managed_routes`
/// handler's pattern; the manager itself is cached behind a `OnceCell`, so this
/// costs nothing beyond the async yield).
/// Test: `tests.rs`.
pub struct DirectManagedBackend {
    state: std::sync::Arc<DaemonState>,
}

impl DirectManagedBackend {
    /// Wrap `state` for in-process managed-session access.
    pub fn new(state: std::sync::Arc<DaemonState>) -> Self {
        Self { state }
    }
}

#[async_trait]
impl ManagedBackend for DirectManagedBackend {
    /// Resolve `target` against the live managed-session list.
    ///
    /// Why: mirrors [`crate::client::executor::managed`]'s `resolve_managed`
    /// EXACTLY in wording — the "not found" phrasing must match so
    /// `is_missing_session` behaves identically regardless of which backend a
    /// [`crate::client::proxy::SessionProxy`] is built over.
    async fn resolve(&self, target: &str) -> Result<(String, String), String> {
        let mgr = self.state.session_manager().await;
        let records = mgr.list().await;
        match resolve_target(&records, target) {
            Some(r) => Ok((r.id.to_string(), r.tmux_name.clone())),
            None => Err(format!("managed session {target} not found")),
        }
    }

    /// Inject `text` into the session identified by `id`.
    async fn send(&self, id: &str, text: &str) -> Result<(), String> {
        let parsed = parse_managed_id(id)?;
        let mgr = self.state.session_manager().await;
        mgr.send_input(&parsed, text).await.map_err(|e| e.to_string())
    }

    /// Build a lightweight digest straight from the session record.
    ///
    /// Why: deliberately NOT the LLM-classified `GET .../activity` digest — no
    /// tmux capture, no `OPENROUTER_API_KEY` requirement, so this local surface
    /// is hermetic and fast to test. A caller wanting the full classified digest
    /// still calls `GET /api/v1/sessions/managed/{id}/activity` directly.
    async fn activity(&self, id: &str) -> Result<ActivityDigest, String> {
        let parsed = parse_managed_id(id)?;
        let mgr = self.state.session_manager().await;
        let record = mgr
            .get(&parsed)
            .await
            .map_err(|_| format!("managed session {id} not found"))?;
        let summary = match &record.pending_decision {
            Some(d) => format!("blocked on decision: {d}"),
            None if record.task.trim().is_empty() => {
                format!("state={}", record.state)
            }
            None => format!("state={} task={}", record.state, record.task),
        };
        Ok(ActivityDigest {
            state: record.state.to_string(),
            summary,
            pending_decision: record.pending_decision,
        })
    }
}

/// Parse a canonical managed-session id string into a [`ManagedSessionId`].
///
/// Why: [`ManagedBackend::send`]/[`ManagedBackend::activity`] receive the
/// resolved id as a plain `String` (the trait is backend-agnostic); this
/// backend needs the typed [`ManagedSessionId`] `SessionManager` expects. A
/// caller always reaches this AFTER `resolve` succeeded, so a parse failure here
/// would indicate the store returned a non-UUID id — reported as an error rather
/// than a panic (no `unwrap` on a value that ultimately traces back to a wire
/// caller in `daemon::managed_routes::proxy`).
/// What: parses `id` as a `uuid::Uuid` and wraps it; `Err` carries a message
/// phrased WITHOUT the "not found" substring (an invalid id is a different
/// failure mode than a missing session, so it must not trip the auto-unfocus
/// path — see `is_missing_session`'s contract).
/// Test: `parse_managed_id_rejects_non_uuid` in `tests.rs`.
pub(super) fn parse_managed_id(id: &str) -> Result<ManagedSessionId, String> {
    id.parse::<uuid::Uuid>()
        .map(ManagedSessionId::from)
        .map_err(|_| format!("invalid managed session id: {id}"))
}
