//! `GET /api/agents/:name/chat-history` — read back the durable persona chat
//! log so the GUI can rehydrate its chat view on reload (#4278).
//!
//! Why: every persona chat turn is already persisted — `spawn_persist_turn`
//! (`ctrl::pm_task::dispatch::persona_memory`) appends the user prompt and the
//! assistant response to a trusty-memory `chat_session` keyed
//! `persona-{agent}` (DOC-54 §9.1). Nothing could READ it back over HTTP, so
//! a plain page reload discarded the visible conversation while the durable
//! log sat untouched — a data-loss bug, not a missing feature. The two
//! surfaces that looked like they might serve are both wrong for this:
//! `GET /api/tasks` returns `PmResponse` envelopes that carry NO request text
//! (see `lib/taskHistory.ts`), so it can never restore a prompt; and
//! `GET /api/workstreams/:name/history` returns `ws:`-tagged drawers, which
//! the owner ruled a digest rather than the turn history this issue requires.
//! The `chat_session` projection is the only store holding both halves of a
//! turn, which is why the owner named it as the read source.
//!
//! What: resolves the agent's bound palace from `[[stores]].primary().palace`
//! (the same mapping `agent_kg` performs), dials `chat_session_recall` through
//! the `tools/call` envelope, and returns a BOUNDED slice of the flat
//! `Vec<ChatMessage>` history, newest-end first, with a cursor for fetching
//! older messages on demand. The bound is the owner's volume requirement: the
//! `persona-{agent}` session is continuous and never rolls over, so hydrating
//! every turn on launch does not scale.
//!
//! Honest limits, stated rather than papered over:
//!   - The bound is applied HERE, not upstream. `chat_session_recall` has no
//!     `limit`/`offset` parameter (`tools/chat_definitions.rs`), so the daemon
//!     still returns the whole session over the socket and this route slices
//!     it. That caps what the browser renders and parses, not what crosses the
//!     Unix socket; bounding the daemon read needs a trusty-memory change and
//!     is deliberately out of scope here.
//!   - `ChatMessage` is `{role, content}` with NO per-message timestamp
//!     (`trusty_common::memory_core::store::chat_sessions::types`), so the
//!     response carries only the session's `updated_at`. The client must not
//!     invent per-turn times it was never given.
//!   - Only turns that reached [`spawn_persist_turn`] are here. That call sits
//!     on the SUCCESS path in `persona.rs`, so an errored or cancelled turn is
//!     never appended, and a stream that never completed has no final response
//!     to append. `available: true` therefore means "this is the persisted
//!     log", never "this is every turn that ever happened" — the distinction
//!     the owner asked to have verified before trusting this source.
//!
//! An agent binding no palace persists nothing, so it has nothing to
//! rehydrate. That is an ordinary state, reported as `200` +
//! `available: false` + a reason (the `costs`/`agent_kg` precedent), never an
//! error the GUI has to render as a failure.
//!
//! Test: `super::tests::chat_history`.

use std::path::{Path, PathBuf};

use axum::{
    Json,
    extract::{Path as AxumPath, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::{Value, json};

use super::agent_kg::{describe_failure, parse_stores};
use super::agent_patch::resolve_agent_paths;
use super::state::AppState;
use crate::ctrl::pm_task::dispatch::persona_memory::session_id_for;
use crate::stores::status::PROBE_TIMEOUT;

/// Messages returned when the client names no `limit` — roughly 50 turns.
const DEFAULT_LIMIT: usize = 100;
/// Hard ceiling on one page, so a hand-crafted `limit` cannot ask the GUI to
/// render an unbounded session in one paint.
const MAX_LIMIT: usize = 500;

/// `?limit=<n>&before=<n>`.
///
/// `before` is the number of most-recent messages the client ALREADY holds —
/// an offset counted from the newest end, not a timestamp. A cursor counted
/// from the end is stable under appends: new turns land beyond the window the
/// client is paging backwards through, so lazy-loading older messages cannot
/// skip or duplicate one because a turn arrived mid-scroll.
#[derive(Debug, Default, Deserialize)]
pub(super) struct HistoryQuery {
    limit: Option<usize>,
    before: Option<usize>,
}

/// `GET /api/agents/:name/chat-history` — HTTP entry point.
pub(super) async fn agent_chat_history_route(
    State(_state): State<AppState>,
    AxumPath(name): AxumPath<String>,
    Query(q): Query<HistoryQuery>,
) -> Response {
    chat_history_at(
        &crate::agents::agents_dir_candidates(),
        &name,
        trusty_common::memory_rpc::resolve_memory_socket()
            .ok()
            .as_deref(),
        q.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT),
        q.before.unwrap_or(0),
    )
    .await
}

/// Core read against explicit agents dirs + a trusty-memory socket.
///
/// Why: the socket is injected so tests exercise every branch against a mock
/// daemon on a temp socket instead of the developer's live one — the
/// `agent_kg::kg_proxy_at` / `agent_stores::stores_at` pattern.
/// What: `400` on an invalid agent name, `404` on an unknown agent, `500` only
/// when the agent's own config cannot be read. Every other outcome — no palace
/// bound, daemon undiscoverable, daemon unreachable, session never created —
/// is a `200` carrying `available: false` and a reason, because each of those
/// means "nothing to rehydrate", which the chat view renders as an empty
/// conversation rather than an error.
/// Test: `chat_history_returns_bounded_newest_slice`,
/// `chat_history_before_cursor_pages_backwards`,
/// `chat_history_empty_when_no_palace_bound`,
/// `chat_history_empty_when_session_absent`,
/// `chat_history_degrades_when_memory_unreachable`,
/// `chat_history_unknown_agent_404`, `chat_history_rejects_traversal_name`.
pub(super) async fn chat_history_at(
    dirs: &[PathBuf],
    name: &str,
    memory_socket: Option<&Path>,
    limit: usize,
    before: usize,
) -> Response {
    if name.is_empty() || name.contains(['/', '\\']) || name == "." || name == ".." {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "invalid agent name" })),
        )
            .into_response();
    }
    let Some((path, _package_dir)) = resolve_agent_paths(dirs, name) else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "unknown agent", "name": name })),
        )
            .into_response();
    };
    let raw = match tokio::fs::read_to_string(&path).await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(?e, agent = name, path = %path.display(), "chat_history_at: read failed");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "failed to read agent config" })),
            )
                .into_response();
        }
    };

    let session_id = session_id_for(name);
    let (stores, config_error) = parse_stores(&raw);
    let Some(palace) = stores.primary().and_then(|b| b.palace.clone()) else {
        return unavailable(
            None,
            &session_id,
            "this agent binds no memory palace (`[[stores]].palace` is unset), so no chat turns are persisted",
            config_error,
        );
    };

    let session = match memory_socket {
        None => Err("trusty-memory daemon not discoverable (is it running?)".to_string()),
        Some(socket) => fetch_session(socket, &palace, &session_id).await,
    };
    match session {
        Err(reason) => unavailable(Some(palace), &session_id, &reason, config_error),
        Ok(session) => {
            let mut body = page(&palace, &session_id, &session, limit, before);
            if let Some(err) = config_error {
                body["config_error"] = Value::String(err);
            }
            (StatusCode::OK, Json(body)).into_response()
        }
    }
}

/// The `200` + `available: false` body every "nothing to rehydrate" path returns.
///
/// Shape-identical to the success body so a client branches only on
/// `available`, never on whether a field is present.
fn unavailable(
    palace: Option<String>,
    session_id: &str,
    reason: &str,
    config_error: Option<String>,
) -> Response {
    let mut body = json!({
        "available": false,
        "reason": reason,
        "palace": palace,
        "session_id": session_id,
        "messages": [],
        "total": 0,
        "has_more": false,
        "updated_at": Value::Null,
    });
    if let Some(err) = config_error {
        body["config_error"] = Value::String(err);
    }
    (StatusCode::OK, Json(body)).into_response()
}

/// Slice the newest `limit` messages ending `before` from the end.
///
/// Why: the owner's volume requirement — a continuous session that never rolls
/// over must not hydrate in full on launch. The window is
/// `[total - before - limit, total - before)`, clamped, so `before` walks
/// backwards one page at a time and `has_more` says whether anything older
/// remains.
/// What: pure over the decoded session value; a `history` that is absent or
/// not an array reads as an empty session rather than an error, because the
/// daemon's own contract already guarantees the field.
/// Test: `chat_history_returns_bounded_newest_slice`,
/// `chat_history_before_cursor_pages_backwards`.
fn page(palace: &str, session_id: &str, session: &Value, limit: usize, before: usize) -> Value {
    let history = session
        .get("history")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let total = history.len();
    let end = total.saturating_sub(before);
    let start = end.saturating_sub(limit);
    json!({
        "available": true,
        "palace": palace,
        "session_id": session_id,
        "messages": history[start..end].to_vec(),
        "total": total,
        "has_more": start > 0,
        "updated_at": session.get("updated_at").cloned().unwrap_or(Value::Null),
    })
}

/// Fetch the session, collapsing every failure into the reason a client renders.
///
/// Why: `chat_session_recall` is absent from trusty-memory's direct-dispatch
/// `TOOL_METHODS` allow-list, so it is only reachable wrapped in `tools/call`
/// — the same envelope `persona_memory::call_memory_tool` writes through. That
/// arm answers `result.content[0].text` as a JSON STRING, so the payload needs
/// decoding back out; it also reports EVERY tool failure as `-32603`, so a
/// session that simply does not exist yet cannot be told apart by code and is
/// matched on the daemon's own "session not found" wording instead.
/// What: `Ok(session)` when the daemon answers; `Err(reason)` for an absent
/// session, any other refusal, or a transport failure. Bounded by
/// [`PROBE_TIMEOUT`], the ceiling every cross-daemon read in this crate uses.
/// Test: `chat_history_empty_when_session_absent`,
/// `chat_history_degrades_when_memory_unreachable`.
async fn fetch_session(socket: &Path, palace: &str, session_id: &str) -> Result<Value, String> {
    let answer = trusty_common::memory_rpc::call_memory_tool_at_with_timeout(
        socket,
        "tools/call",
        json!({
            "name": "chat_session_recall",
            "arguments": { "palace": palace, "session_id": session_id },
        }),
        PROBE_TIMEOUT,
    )
    .await
    .map_err(|e| {
        let described = describe_failure(&e, palace);
        if described.contains("not found") || described.contains("does not exist") {
            format!("no persisted chat session `{session_id}` yet")
        } else {
            described
        }
    })?;

    let text = answer
        .pointer("/content/0/text")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            "trusty-memory answered chat_session_recall without `content[0].text`".to_string()
        })?;
    serde_json::from_str::<Value>(text)
        .map_err(|e| format!("trusty-memory returned an undecodable chat session: {e}"))
}
