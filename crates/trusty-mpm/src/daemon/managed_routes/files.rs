//! Per-session file-change view over the existing hook-event ring buffer (#94).
//!
//! Why: the daemon already records every coding-relevant `FileChanged` event —
//! the filesystem watcher synthesises them in
//! [`crate::daemon::watcher::FileWatcher::record_change`] and posted hooks land
//! through [`crate::daemon::services::hook_service`]. What the GUI and other
//! clients lacked was a stable per-session read surface over that record; the
//! alternative, a second `HashMap<SessionId, Vec<FileChange>>` ledger, would
//! drift from the ring buffer it duplicated. This module adds the view and no
//! new state.
//! What: [`router`] registers `GET /api/v1/sessions/managed/{id}/files`.
//! [`files_core`] resolves the path id, filters the ring buffer to that
//! session's `FileChanged` records, projects each onto a [`FileChangeEntry`],
//! applies the optional `?since=<unix_timestamp>` lower bound, and collapses
//! entries that would render identically. An id no store knows answers 404 with
//! a [`FilesNotFound`] body.
//! Test: `files_tests` — `files_route_returns_only_that_sessions_records`,
//! `files_route_since_excludes_earlier_records`,
//! `files_route_collapses_duplicate_reports`,
//! `files_route_unknown_session_is_a_typed_404`.

use std::collections::HashSet;
use std::sync::Arc;

use axum::Router;
use axum::extract::{Path as AxumPath, Query, State};
use axum::response::IntoResponse;
use axum::routing::get;
use serde::{Deserialize, Serialize};

use crate::core::hook::{HookEvent, HookEventRecord};
use crate::core::session::SessionId;
use crate::daemon::rpc::managed::outcome::RouteOutcome;
use crate::daemon::state::DaemonState;

use super::parse_id;

/// Query parameters for `GET /api/v1/sessions/managed/{id}/files`.
///
/// Why: a polling client re-asks for the same session repeatedly and wants only
/// what it has not seen.
/// What: `since` is a Unix timestamp in SECONDS, matching the `timestamp` field
/// each [`FileChangeEntry`] reports. The bound is INCLUSIVE — a caller passing
/// the newest timestamp it holds sees that second again rather than losing an
/// entry recorded later in the same second, and the dedup key below is exactly
/// the tuple that lets it discard the repeat.
/// Test: `files_route_since_excludes_earlier_records`.
#[derive(Debug, Default, Deserialize)]
pub struct FilesQuery {
    /// Lower bound, in Unix seconds, on the entries returned. Inclusive.
    pub since: Option<i64>,
}

/// One file change, as this route reports it.
///
/// Why: the ring buffer holds an opaque hook payload whose shape varies by
/// producer; clients need one stable projection of it.
/// What: `path` and `timestamp` are always present. `operation` carries
/// `payload.operation` when the producer set it and is `null` otherwise — the
/// watcher records a bare path, so a synthesised change reports no operation.
/// `additions`/`deletions` are read from the payload and are likewise `null`
/// when absent; this route never computes diff statistics of its own.
/// `Eq + Hash` is what [`files_core`] deduplicates on: two records that would
/// render as the same entry collapse to one.
/// Test: `files_route_collapses_duplicate_reports`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct FileChangeEntry {
    /// Absolute path of the changed file.
    pub path: String,
    /// Producer-reported operation (`"write"`, `"delete"`, …), or `null`.
    pub operation: Option<String>,
    /// When the daemon recorded the change, as a Unix timestamp in seconds.
    pub timestamp: i64,
    /// Lines added, when the event payload carried the figure.
    pub additions: Option<u64>,
    /// Lines removed, when the event payload carried the figure.
    pub deletions: Option<u64>,
}

/// Response body for `GET /api/v1/sessions/managed/{id}/files`.
///
/// Why: echoing the session and the applied `since` lets a polling client
/// confirm what it actually asked for without tracking request state.
/// What: `files` is ordered oldest-first. `count` is `files.len()`, reported so
/// a caller can check for truncation without walking the array.
/// Test: `files_route_returns_only_that_sessions_records`.
#[derive(Debug, Serialize)]
pub struct FilesResponse {
    /// The session id the entries belong to, as supplied in the path.
    pub session: String,
    /// The `?since=` bound that was applied, or `null` when none was given.
    pub since: Option<i64>,
    /// Number of entries in `files`.
    pub count: usize,
    /// The changes, oldest first.
    pub files: Vec<FileChangeEntry>,
}

/// The 404 body for a session id no store knows.
///
/// Why: the sibling managed routes answer a miss with a bare string, which a
/// client has to pattern-match. This route is a polling surface, so its refusal
/// is JSON like its success: `error` mirrors the daemon-wide
/// `{"error": …}` shape that [`crate::daemon::error::DaemonError`] emits.
/// Test: `files_route_unknown_session_is_a_typed_404`.
#[derive(Debug, Serialize)]
pub struct FilesNotFound {
    /// Human-readable reason.
    pub error: String,
    /// The session id that was not found, echoed back.
    pub session: String,
}

/// Register the route on its own sub-router.
///
/// Why: mirrors `provision_status::router()` and `sync_assets::router()` —
/// the route table for a cohesive surface lives beside the handler rather than
/// in `api.rs`. Merged into the daemon router, so it inherits the router-wide
/// same-origin guard and the daemon's loopback-only bind exactly as the sibling
/// managed routes do; it adds no listener and no guard exemption of its own.
/// Test: every test in `files_tests` drives the real `api::router`.
pub fn router() -> Router<Arc<DaemonState>> {
    Router::new().route(
        "/api/v1/sessions/managed/{id}/files",
        get(get_session_files),
    )
}

/// `GET /api/v1/sessions/managed/{id}/files` — this session's file changes.
///
/// Test: `files_tests`.
pub async fn get_session_files(
    State(state): State<Arc<DaemonState>>,
    AxumPath(id_str): AxumPath<String>,
    Query(query): Query<FilesQuery>,
) -> impl IntoResponse {
    files_core(&state, &id_str, query.since).await
}

/// Project one hook record onto a [`FileChangeEntry`].
///
/// Why: the payload is opaque JSON, and a record whose `path` is missing or
/// empty carries nothing a client could act on.
/// What: returns `None` for such a record so it is skipped rather than reported
/// as a change to the empty path.
/// Test: exercised by every `files_tests` case that posts a `FileChanged` event.
fn entry_from(record: &HookEventRecord) -> Option<FileChangeEntry> {
    let path = record.payload.get("path")?.as_str()?;
    if path.is_empty() {
        return None;
    }
    Some(FileChangeEntry {
        path: path.to_owned(),
        operation: record
            .payload
            .get("operation")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
        timestamp: record.at.timestamp(),
        additions: record
            .payload
            .get("additions")
            .and_then(serde_json::Value::as_u64),
        deletions: record
            .payload
            .get("deletions")
            .and_then(serde_json::Value::as_u64),
    })
}

/// Every session-id key space this managed id's `FileChanged` records can sit
/// under, or `None` when no store knows the id.
///
/// Why: a `FileChanged` record is keyed by the [`SessionId`] its producer used,
/// and two producers use two different ids for one managed session. The watcher
/// keys by the daemon-registered session id, which shares the managed id's UUID
/// — the daemon already reads the two as one value (`ManagedSessionId(id.0)` in
/// `daemon::mcp_backend`). A Claude Code session instead announces itself on
/// `SessionStart`, is auto-registered under its OWN UUID, and is bound to the
/// managed record through `claude_session_id` (#1744). Reading only the first
/// key space would return an empty list for exactly the sessions a GUI asks
/// about, so this unions both — the same two-key-space union `delegations_for`
/// already performs for delegations (#4141).
/// What: 404s (returns `None`) only when NEITHER the managed store NOR the
/// daemon session registry knows the id, so "not found" means unknown to the
/// daemon rather than unknown to one of its two stores.
/// Test: `files_route_unknown_session_is_a_typed_404`.
async fn key_spaces_for(
    state: &Arc<DaemonState>,
    id: &crate::session_manager::ManagedSessionId,
) -> Option<Vec<SessionId>> {
    let own = SessionId(id.0);
    let record = state.session_manager().await.get(id).await.ok();
    if record.is_none() && state.session(own).is_none() {
        return None;
    }
    let mut spaces = vec![own];
    // #94: the Claude-UUID half, present once the #1744 correlation has bound a
    // Claude Code session to this managed record.
    if let Some(claude) = record.as_ref().and_then(|r| r.claude_session_id.as_deref())
        && let Ok(uuid) = uuid::Uuid::parse_str(claude)
        && SessionId(uuid) != own
    {
        spaces.push(SessionId(uuid));
    }
    Some(spaces)
}

/// The body of `GET .../{id}/files`, with no transport attached.
///
/// Why: the managed routes return a [`RouteOutcome`] so one implementation can
/// serve both axum and the Unix socket (#6288); this route follows that shape
/// even though only the HTTP projection is registered today.
/// What: resolves the id, reads the ring buffer for each key space
/// [`key_spaces_for`] reports, keeps `FileChanged` records at or after `since`,
/// collapses entries that would render identically, and sorts oldest-first. The
/// sort is stable, so entries recorded in the same second keep arrival order.
/// Test: `files_tests`.
pub(crate) async fn files_core(
    state: &Arc<DaemonState>,
    id_str: &str,
    since: Option<i64>,
) -> RouteOutcome {
    let id = match parse_id(id_str) {
        Ok(id) => id,
        Err((code, msg)) => return RouteOutcome::text(code.as_u16(), msg),
    };
    let Some(key_spaces) = key_spaces_for(state, &id).await else {
        return RouteOutcome::json(
            404,
            &FilesNotFound {
                error: format!("session {id_str} not found"),
                session: id_str.to_owned(),
            },
        );
    };

    let mut seen: HashSet<FileChangeEntry> = HashSet::new();
    let mut files: Vec<FileChangeEntry> = Vec::new();
    for key in key_spaces {
        for record in state.hook_events_for(key) {
            if record.event != HookEvent::FileChanged {
                continue;
            }
            let Some(entry) = entry_from(&record) else {
                continue;
            };
            if since.is_some_and(|bound| entry.timestamp < bound) {
                continue;
            }
            if seen.insert(entry.clone()) {
                files.push(entry);
            }
        }
    }
    files.sort_by_key(|entry| entry.timestamp);

    RouteOutcome::ok(&FilesResponse {
        session: id_str.to_owned(),
        since,
        count: files.len(),
        files,
    })
}

#[cfg(test)]
#[path = "files_tests.rs"]
mod files_tests;
