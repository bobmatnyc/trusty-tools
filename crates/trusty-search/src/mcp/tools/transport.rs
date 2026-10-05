//! The MCP bridge's one way to reach the daemon: its Unix socket (#9168).
//!
//! Why: ADR-0032 retires the daemon's loopback HTTP listener, and every route
//! the bridge used already has a `search.*` twin on the socket. One call path
//! keeps the refusal mapping — not-ready, unavailable, invalid params — in a
//! single place, exactly as the four HTTP verb helpers it replaces did, and it
//! never builds a URL: a missing socket is an error naming the socket.
//! What: [`McpServer::call`] and [`McpServer::call_scoped`] send one frame
//! through [`crate::service::daemon_client::DaemonClient`] and fold a
//! [`DaemonCallError`] into a [`DispatchError`]. A `not found` refusal on the
//! session's advertised index becomes `INDEX_NOT_READY` (#4715); an
//! unavailable refusal carrying the daemon's 503 body in `data` becomes
//! `INDEX_UNAVAILABLE` (#5350); an invalid-params refusal stays a parameter
//! error; anything else is a transport error whose text names the socket.
//! Writes and chat carry no client-side limit; every other method is bounded
//! by the daemon's query deadline plus admission headroom.
//! Test: `tests_transport.rs`, plus every dispatch test that drives a mock
//! socket daemon.

use std::time::Duration;

use serde_json::Value;

use super::unavailable::classify_unavailable;
use super::{types::DispatchError, McpServer};
use crate::service::daemon_client::{DaemonCallError, DEFAULT_CALL_TIMEOUT};
use crate::service::query_timeout::query_timeout_secs;
use crate::service::rpc::chat::METHOD_CHAT;
use crate::service::rpc::writes::{
    METHOD_INDEX_CREATE, METHOD_INDEX_DELETE, METHOD_INDEX_FILE_PUT, METHOD_INDEX_FILE_REMOVE,
    METHOD_INDEX_REINDEX,
};

/// The methods the bridge sends with no client-side limit (#9168).
///
/// Why: the daemon puts no deadline on any of them — the writes run under
/// `bulk_guarded`/`unguarded` in `service::rpc::writes`, and `search.chat`
/// waits on a model. The HTTP bridge this replaced set no timeout either. A
/// client limit would report a timeout for work the daemon still finishes, so
/// a retried `delete_index` would then answer `not found`.
/// What: index create/delete/reindex, file put/remove, and chat.
/// Test: `writes_and_chat_outlast_the_query_budget`.
const UNBOUNDED_METHODS: &[&str] = &[
    METHOD_INDEX_CREATE,
    METHOD_INDEX_DELETE,
    METHOD_INDEX_REINDEX,
    METHOD_INDEX_FILE_PUT,
    METHOD_INDEX_FILE_REMOVE,
    METHOD_CHAT,
];

/// No client-side limit. `tokio::time::timeout` maps a deadline it cannot
/// represent to "never", so this waits until the daemon answers or the socket
/// closes.
const NO_CLIENT_LIMIT: Duration = Duration::MAX;

/// Time a query may spend in the daemon's admission queue before its own
/// deadline starts.
const ADMISSION_HEADROOM: Duration = Duration::from_secs(30);

/// The client-side budget for every method not in [`UNBOUNDED_METHODS`].
///
/// Why: the daemon bounds a query at `TRUSTY_QUERY_TIMEOUT_SECS` once it is
/// admitted, and admission can queue. A fixed 60 s budget cut a query off
/// before the daemon's own deadline whenever an operator raised that value
/// past 60 s.
/// What: the deadline `raw` names plus [`ADMISSION_HEADROOM`], never below
/// [`DEFAULT_CALL_TIMEOUT`] — 60 s at the 30 s default, as before.
/// Test: `query_budget_tracks_the_daemon_deadline`.
pub(crate) fn query_call_budget(raw: Option<&str>) -> Duration {
    Duration::from_secs(query_timeout_secs(raw))
        .saturating_add(ADMISSION_HEADROOM)
        .max(DEFAULT_CALL_TIMEOUT)
}

impl McpServer {
    /// Call `method` with `params` and return its result.
    ///
    /// # Errors
    ///
    /// The [`DispatchError`] [`Self::dispatch_error`] derives from the failure.
    pub(super) async fn call(&self, method: &str, params: Value) -> Result<Value, DispatchError> {
        self.call_scoped(method, params, None).await
    }

    /// [`Self::call`] for an index-scoped method, so a `not found` refusal on
    /// the session's advertised index reads as `INDEX_NOT_READY` (#4715).
    ///
    /// Why: the daemon answers "no such id" for a never-built index and a
    /// mistyped one alike; only the bridge knows which id it advertised.
    /// What: identical to [`Self::call`] except that `index_id` is offered to
    /// [`McpServer::classify_index_miss`] on a `not found` refusal. A method in
    /// [`UNBOUNDED_METHODS`] is sent with no client-side limit; every other
    /// method gets the client's query budget ([`query_call_budget`]).
    ///
    /// # Errors
    ///
    /// The [`DispatchError`] [`Self::dispatch_error`] derives from the failure.
    ///
    /// Test: `tools_call_search_on_unindexed_pin_returns_structured_not_ready`.
    pub(super) async fn call_scoped(
        &self,
        method: &str,
        params: Value,
        index_id: Option<&str>,
    ) -> Result<Value, DispatchError> {
        // #9168: writes and chat get no client limit; everything else uses the
        // query budget `McpServer::new` set on the client.
        let result = if UNBOUNDED_METHODS.contains(&method) {
            self.daemon
                .call_with_timeout(method, params, NO_CLIENT_LIMIT)
                .await
        } else {
            self.daemon.call(method, params).await
        };
        result.map_err(|e| self.dispatch_error(&e, index_id))
    }

    /// Fold one daemon failure into the error the dispatcher reports.
    ///
    /// Why: the HTTP helpers applied the same three classifications on every
    /// verb; keeping them in one function keeps every tool consistent.
    /// What: not-found on the advertised index → `IndexNotReady`; a structured
    /// unavailable refusal → `IndexUnavailable`; invalid params →
    /// `InvalidParams` with the daemon's own message; otherwise `Transport`
    /// whose text names the socket. Never yields `Ok`.
    /// Test: `a_refusal_names_the_socket_not_a_url`,
    /// `invalid_params_refusal_stays_a_parameter_error`.
    pub(super) fn dispatch_error(
        &self,
        e: &DaemonCallError,
        index_id: Option<&str>,
    ) -> DispatchError {
        if e.is_not_found() {
            if let Some(not_ready) = self.classify_index_miss(index_id) {
                return not_ready;
            }
        }
        if let Some(unavailable) = classify_unavailable(e) {
            return unavailable;
        }
        if e.is_invalid_params() {
            return DispatchError::InvalidParams(e.message().unwrap_or("bad request").to_owned());
        }
        DispatchError::Transport(self.describe(e))
    }

    /// Operator-facing text for a failure, always naming the socket.
    ///
    /// Why: every [`DaemonCallError`] but `Refused` already names the socket;
    /// a refusal names only the method, and a reader of an MCP error must be
    /// able to tell WHICH daemon refused.
    pub(super) fn describe(&self, e: &DaemonCallError) -> String {
        match e {
            DaemonCallError::Refused { .. } => {
                format!("{e} (daemon socket {})", self.daemon.socket().display())
            }
            other => other.to_string(),
        }
    }
}
